//! A small client for the local interface of the Discord app (RPC over a named pipe) and for the one request to
//! discord.com the login needs. It can do only what Discord Plus needs: connect, sign in, ask for the voice
//! channel, set the activity ("Playing …").
//!
//! How the interface works (documentation: https://docs.discord.com/developers/topics/rpc):
//! - The Discord app opens `\\.\pipe\discord-ipc-0` when it starts (`-1`, `-2` … if Discord runs more than once).
//! - Every message is a frame: 4 bytes opcode, 4 bytes length (both little-endian), then JSON.
//!   Opcode 0 = handshake, 1 = command or answer, 2 = close, 3/4 = ping/pong.
//! - After the handshake `{"v": 1, "client_id": …}` Discord answers with the event READY.
//! - SET_ACTIVITY works at once. What reads data of the account (GET_SELECTED_VOICE_CHANNEL) only after signing in:
//!     1. AUTHORIZE: Discord shows "… wants to access your account" in the app – "Authorize" yields a code
//!     2. the code is exchanged for a token at discord.com (this needs the client secret): `token`
//!     3. AUTHENTICATE with the token – then the commands are free until the connection ends
//!   The token lasts about a week; the refresh token gets a new one without asking.
//!
//! Without a review by Discord only the owner of the app and up to 50 listed testers may use it this way – exactly
//! right for an app of one's own.

use std::fs::File;
use std::io::{Read, Write};

use serde_json::{Value, json};

const TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const SCOPES: [&str; 2] = ["rpc", "identify"];
/// Must be listed in the Discord app under OAuth2 → Redirects.
pub const REDIRECT_URI: &str = "http://localhost";

const HANDSHAKE: u32 = 0;
const FRAME: u32 = 1;
const CLOSE: u32 = 2;
const PING: u32 = 3;
const PONG: u32 = 4;
/// No message of Discord is anywhere near this size; a larger length means the stream is out of step.
const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Discord refused a command (`code` as in the documentation, e.g. 4006 = not signed in), or discord.com
    /// refused the login (`code` None).
    Discord { message: String, code: Option<i64> },
    /// Discord is not running, or the connection broke.
    Gone(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Discord { message, .. } | Error::Gone(message) => f.write_str(message),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Gone(e.to_string())
    }
}

/// One frame as bytes.
fn frame(op: u32, data: &Value) -> Vec<u8> {
    let body = data.to_string().into_bytes();
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Reads one frame: opcode and JSON.
fn read_frame(from: &mut impl Read) -> Result<(u32, Value), Error> {
    let mut head = [0u8; 8];
    from.read_exact(&mut head)?;
    let op = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len > MAX_FRAME {
        return Err(Error::Gone("Discord sent something that is no message".into()));
    }
    let mut body = vec![0u8; len];
    from.read_exact(&mut body)?;
    Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

/// A connection to the running Discord app. Used by one thread only.
pub struct Connection {
    pub client_id: String,
    pipe: File,
    nonce: u64,
}

impl Connection {
    /// `Error::Gone` if Discord is not running, `Error::Discord` if it does not accept the client id.
    pub fn open(client_id: &str) -> Result<Connection, Error> {
        let pipe = (0..10)
            .find_map(|n| std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{n}")).ok())
            .ok_or_else(|| Error::Gone("Discord is not running (no discord-ipc pipe)".into()))?;
        let mut c = Connection { client_id: client_id.to_string(), pipe, nonce: 0 };
        c.pipe.write_all(&frame(HANDSHAKE, &json!({"v": 1, "client_id": client_id})))?;
        let (op, answer) = read_frame(&mut c.pipe)?;
        if op == CLOSE || answer["evt"] != "READY" {
            // e.g. {"code": 4000, "message": "Invalid Client ID"}
            return Err(Error::Discord { message: answer["message"].as_str().unwrap_or("Discord refused the connection").to_string(), code: answer["code"].as_i64() });
        }
        Ok(c)
    }

    /// Sends a command and waits for the answer to it. Returns the answer's `data`.
    fn command(&mut self, cmd: &str, args: Value) -> Result<Value, Error> {
        self.nonce += 1;
        let nonce = format!("{}-{}", std::process::id(), self.nonce);
        self.pipe.write_all(&frame(FRAME, &json!({"cmd": cmd, "args": args, "nonce": nonce})))?;
        loop {
            let (op, mut answer) = read_frame(&mut self.pipe)?;
            match op {
                PING => self.pipe.write_all(&frame(PONG, &answer))?,
                CLOSE => return Err(Error::Gone(answer["message"].as_str().unwrap_or("Discord closed the connection").to_string())),
                _ if answer["nonce"] == nonce.as_str() => {
                    if answer["evt"] == "ERROR" {
                        let data = &answer["data"];
                        return Err(Error::Discord { message: data["message"].as_str().unwrap_or("unknown error").to_string(), code: data["code"].as_i64() });
                    }
                    return Ok(answer["data"].take());
                }
                // Anything else (events nobody subscribed to) is skipped.
                _ => {}
            }
        }
    }

    /// AUTHORIZE – Discord asks in the app. Waits until the user clicks there; returns the code.
    pub fn authorize(&mut self) -> Result<String, Error> {
        let data = self.command("AUTHORIZE", json!({"client_id": self.client_id, "scopes": SCOPES}))?;
        data["code"].as_str().map(str::to_string).ok_or_else(|| Error::Discord { message: "Discord gave no code".into(), code: None })
    }

    /// AUTHENTICATE – returns `{"user": …, "application": {"name": …}, "expires": …}`.
    pub fn authenticate(&mut self, access_token: &str) -> Result<Value, Error> {
        self.command("AUTHENTICATE", json!({"access_token": access_token}))
    }

    /// The voice channel or call the user is in right now – or `Null`.
    pub fn voice_channel(&mut self) -> Result<Value, Error> {
        self.command("GET_SELECTED_VOICE_CHANNEL", json!({}))
    }

    pub fn guild(&mut self, guild_id: &str) -> Result<Value, Error> {
        self.command("GET_GUILD", json!({"guild_id": guild_id, "timeout": 5}))
    }

    /// Sets this program's activity – `None` clears it. It also disappears by itself as soon as the connection or
    /// the program (pid) has ended.
    pub fn set_activity(&mut self, activity: Option<&Value>) -> Result<(), Error> {
        let mut args = json!({"pid": std::process::id()});
        if let Some(activity) = activity {
            args["activity"] = activity.clone();
        }
        self.command("SET_ACTIVITY", args).map(|_| ())
    }
}

/// How a token is asked for.
pub enum Grant<'a> {
    /// The code from AUTHORIZE.
    Code(&'a str),
    /// A new token for an old one, without asking in Discord.
    Refresh(&'a str),
}

/// What discord.com answered: `access_token`, `refresh_token`, `expires_in` (seconds).
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
}

/// Reads discord.com's answer to a token request.
fn read_token(status: u32, body: &str) -> Result<Token, Error> {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let refused = |reason: String| Error::Discord { message: format!("discord.com refuses the login: {reason}"), code: None };
    if !(200..300).contains(&status) {
        let reason = v["error_description"].as_str().or(v["error"].as_str()).map(str::to_string).unwrap_or(format!("HTTP {status}"));
        return Err(refused(reason));
    }
    let access_token = v["access_token"].as_str().ok_or_else(|| refused("the answer holds no token".into()))?.to_string();
    Ok(Token { access_token, refresh_token: v["refresh_token"].as_str().map(str::to_string), expires_in: v["expires_in"].as_u64().unwrap_or(0) })
}

/// Exchanges a code or a refresh token for a token at discord.com. `Error::Discord` carries Discord's reason,
/// `Error::Gone` means: no internet.
pub fn token(client_id: &str, client_secret: &str, grant: Grant) -> Result<Token, Error> {
    let mut fields = vec![("client_id", client_id), ("client_secret", client_secret)];
    match grant {
        Grant::Code(code) => fields.extend([("grant_type", "authorization_code"), ("code", code), ("redirect_uri", REDIRECT_URI)]),
        Grant::Refresh(token) => fields.extend([("grant_type", "refresh_token"), ("refresh_token", token)]),
    }
    let answer = kit::http::post_form(TOKEN_URL, &fields, 1024 * 1024).map_err(|e| Error::Gone(format!("discord.com cannot be reached: {e}")))?;
    read_token(answer.status, &answer.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        let bytes = frame(FRAME, &json!({"cmd": "X"}));
        assert_eq!(&bytes[..8], &[1, 0, 0, 0, 11, 0, 0, 0]);
        assert_eq!(&bytes[8..], br#"{"cmd":"X"}"#);
        let mut two = bytes.clone();
        two.extend(frame(PING, &json!({"n": "ä"})));
        let mut stream = &two[..];
        assert_eq!(read_frame(&mut stream).unwrap(), (FRAME, json!({"cmd": "X"})));
        assert_eq!(read_frame(&mut stream).unwrap(), (PING, json!({"n": "ä"})));
        assert!(matches!(read_frame(&mut stream), Err(Error::Gone(_))), "the stream ended");
        // A length that cannot be a message.
        let mut nonsense = &[1u8, 0, 0, 0, 255, 255, 255, 255][..];
        assert!(matches!(read_frame(&mut nonsense), Err(Error::Gone(_))));
    }

    #[test]
    fn discord_com_s_answers() {
        let t = read_token(200, r#"{"access_token":"a","refresh_token":"r","expires_in":604800,"token_type":"Bearer"}"#).unwrap();
        assert_eq!((t.access_token.as_str(), t.refresh_token.as_deref(), t.expires_in), ("a", Some("r"), 604800));
        let refused = |status, body| read_token(status, body).err().unwrap().to_string();
        assert!(refused(400, r#"{"error":"invalid_grant","error_description":"Invalid \"code\" in request."}"#).contains("Invalid \"code\""));
        assert!(refused(401, r#"{"error":"invalid_client"}"#).contains("invalid_client"));
        assert!(refused(503, "<html>").contains("HTTP 503"));
        assert!(refused(200, "{}").contains("no token"));
    }
}
