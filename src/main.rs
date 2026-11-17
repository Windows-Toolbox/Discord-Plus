//! Discord Plus – extensions for Discord that run in the background.
//!
//! So far one function, the call status: it shows "In a call" in Discord as soon as you are on a call or in a
//! voice chat. Discord shows server voice chats only to people with access to the server, private calls to nobody.
//! So this program sets an activity of its own (Rich Presence, "Playing …") – everybody sees it who otherwise sees
//! what you play.
//!
//! How it works: every few seconds the program asks the Discord app which voice channel you are in
//! (GET_SELECTED_VOICE_CHANNEL) – that includes private and group calls. When something changes, it sets the
//! activity anew (SET_ACTIVITY) or clears it. For that it has to be signed in once: "Connect" on its page makes
//! Discord ask in the app. The program never asks by itself – so nothing pops up in the middle of a game.
//!
//! The files:
//!   main.rs      settings, the page, and the worker that keeps the connection to Discord
//!   discord.rs   talks to the Discord app (named pipe) and to discord.com (login)
//!   activity.rs  what is shown in Discord, from the voice channel and the settings – also for the preview
//!
//! Everything else – the pipe to the Toolbox, loading and saving, the tray menu, single instance – is the kit's job
//! (Toolbox\DESIGN.md 5). New functions become a section of the page and a step of the same worker: one
//! connection to Discord, used by one thread.
#![windows_subsystem = "windows"]

mod activity;
mod discord;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kit::app::{self, Program, Runtime};
use kit::page::Builder;
use kit::proto::{Health, MenuEntry, Status, Tray};
use kit::{process, t};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use activity::{BESIDE_NAME, Channel, KINDS, Place};
use discord::{Connection, Error, Grant};

/// Between two questions while Discord runs.
const POLL: Duration = Duration::from_secs(3);
/// Until the next try when Discord does not run or something is missing.
const WAIT: Duration = Duration::from_secs(15);
const PORTAL: &str = "https://discord.com/developers/applications";
/// The autostart entry of the Python version; taken over on the first start (kit::autostart).
const OLD_AUTOSTART: &str = "WindowsToolbox DiscordPlus";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShowFor {
    #[default]
    Both,
    /// Only calls (direct and group).
    Calls,
    /// Only voice chats on servers.
    Servers,
}

/// The settings. A new setting = one field here + one line in `page`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// From the Developer Portal (OAuth2 → Client ID).
    pub client_id: String,
    /// From there too (OAuth2 → Reset Secret) – it stays on this PC.
    pub client_secret: String,
    /// Off = show nothing (the program keeps running). On by default: it is what the program is for.
    pub call_status: bool,
    pub show_for: ShowFor,
    /// Empty = the standard text of the chosen language ("In a call").
    pub text_call: String,
    /// The same for servers ("In a voice chat").
    pub text_server: String,
    /// "… with Anna, Tom" and the profile picture – all your friends then see them.
    pub names: bool,
    /// Server and channel, or the group's name.
    pub place: bool,
    /// "3 people" (not for calls of two).
    pub count: bool,
    /// "12:34 elapsed" – counts from the moment the program notices the call.
    pub duration: bool,
    /// One of `activity::KINDS`.
    pub kind: u8,
    /// What stands next to your name in the member list (`activity::BESIDE_NAME`).
    pub beside_name: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            call_status: true,
            show_for: ShowFor::Both,
            text_call: String::new(),
            text_server: String::new(),
            names: false,
            place: false,
            count: true,
            duration: true,
            kind: 0,
            beside_name: 2,
        }
    }
}

impl Settings {
    fn has_credentials(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

/// The sign-in at Discord (`data\login.json`). Written only by the worker – apart from the settings, so the two
/// never overwrite each other.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
struct Login {
    /// The client id the token belongs to.
    client_id: String,
    access_token: String,
    refresh_token: String,
    /// Unix time in seconds at which the access token ends.
    expires: u64,
    user: String,
    /// Name of the Discord app.
    app: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn login_file() -> PathBuf {
    app::data_dir().join("login.json")
}

fn save_login(login: &Login) {
    let file = login_file();
    let written = std::fs::create_dir_all(app::data_dir()).and_then(|_| std::fs::write(&file, serde_json::to_string_pretty(login).unwrap_or_default()));
    if let Err(e) = written {
        app::log(&format!("the sign-in could not be saved: {e}"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Starting,
    /// Client id or secret are missing.
    Setup,
    DiscordMissing,
    SignedOut,
    /// The token has to be renewed, but discord.com cannot be reached.
    Offline,
    /// Discord is asking the user to authorize.
    Asking,
    Ready,
    InCall,
    Failed,
}

/// What the worker last found out – for the status line, the tray and the page.
#[derive(Debug, Clone, PartialEq)]
struct Report {
    phase: Phase,
    message: String,
    /// What Discord reports right now ("Private call with Anna") – only while signed in. For testing.
    heard: Option<String>,
    user: String,
    app: String,
}

impl Report {
    fn starting() -> Report {
        Report { phase: Phase::Starting, message: t!("message.starting"), heard: None, user: String::new(), app: String::new() }
    }

    fn health(&self) -> Health {
        match self.phase {
            Phase::InCall | Phase::Ready => Health::Ok,
            Phase::Failed => Health::Bad,
            _ => Health::Warn,
        }
    }
}

/// What the page asks the worker to do – only the worker talks to Discord.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Order {
    Connect,
    SignOut,
}

struct Plus {
    settings: Settings,
    report: Report,
    orders: Vec<Order>,
    /// The worker is at work (the workers are switched on).
    running: bool,
}

impl Program for Plus {
    type Settings = Settings;
    const META: kit::Meta = kit::meta!();

    fn page(&self, p: &mut Builder, s: &Settings) {
        let r = &self.report;
        let signed_in = self.running && !r.user.is_empty() && !matches!(r.phase, Phase::Setup | Phase::SignedOut);
        p.section(t!("page.status"), |p| {
            if self.running {
                p.status(r.message.clone(), r.health());
                if let Some(heard) = &r.heard {
                    p.text(t!("page.heard", text = heard));
                }
            } else {
                p.text(t!("page.stopped"));
            }
        });
        p.section(t!("page.setup"), |p| {
            p.text(t!("page.setup.text"));
            p.buttons(|b| {
                b.button(t!("page.portal"), "portal").button(t!("page.copy_redirect"), "copy_redirect");
            });
            p.entry(t!("page.client_id"), "client_id");
            p.secret(t!("page.client_secret"), "client_secret");
            p.buttons(|b| {
                // Not while Discord asks already: a second click would make it ask a second time.
                if r.phase != Phase::Asking {
                    b.button(t!("page.connect"), "connect");
                }
                if signed_in {
                    b.button(t!("page.sign_out"), "sign_out");
                }
            });
            if signed_in {
                let text = if r.app.is_empty() { t!("page.connected", user = r.user) } else { t!("page.connected_app", user = r.user, app = r.app) };
                p.status(text, Health::Ok);
            } else if !s.has_credentials() {
                p.status(t!("page.not_set_up"), Health::Warn);
            } else if r.phase == Phase::Asking {
                p.status(t!("message.authorize"), Health::Warn);
            } else if self.running {
                p.status(t!("page.not_connected"), Health::Warn);
            } else {
                p.text(t!("page.must_run"));
            }
        });
        p.section(t!("page.call_status"), |p| {
            p.text(t!("page.call_status.text"));
            p.switch(t!("page.show"), "call_status");
            p.choice(t!("page.show_for"), "show_for", &[("both", t!("page.show_for.both")), ("calls", t!("page.show_for.calls")), ("servers", t!("page.show_for.servers"))]);
            p.entry(t!("page.text_call", standard = t!("standard.text_call")), "text_call");
            p.entry(t!("page.text_server", standard = t!("standard.text_server")), "text_server");
            p.switch(t!("page.names"), "names");
            if s.names {
                p.status(t!("page.names.warning"), Health::Warn);
            }
            p.switch(t!("page.place"), "place");
            p.switch(t!("page.count"), "count");
            p.switch(t!("page.duration"), "duration");
            let kinds: Vec<(u8, String)> = KINDS.iter().map(|k| (*k, t!(&format!("kind.{k}")))).collect();
            p.choice(t!("page.kind"), "kind", &kinds);
            p.choice(t!("page.beside_name"), "beside_name", &[(BESIDE_NAME[0], t!("page.beside_name.app")), (BESIDE_NAME[1], t!("page.beside_name.text"))]);
            p.text(t!("page.beside_name.info"));
        });
        p.section(t!("page.preview"), |p| {
            p.text(activity::preview(s, Some(r.app.as_str()).filter(|a| !a.is_empty())));
            p.text(t!("page.preview.text"));
        });
        p.section(t!("page.good_to_know"), |p| p.text(t!("page.notes")));
        p.section(t!("page.advanced"), |p| {
            p.buttons(|b| {
                b.button(t!("page.open_log"), "open_log").button(t!("page.open_settings"), "open_settings").button(t!("page.reset"), "reset");
            });
        });
    }

    fn status(&self, running: bool) -> Status {
        if running { Status { running, state: self.report.health(), text: self.report.message.clone() } } else { Status::default() }
    }

    fn tray(&self) -> Tray {
        let check = if self.settings.call_status { "✓ " } else { "" };
        Tray { line: self.report.message.clone(), menu: vec![MenuEntry { label: format!("{check}{}", t!("menu.call_status")), action: "toggle".into() }], live: vec![] }
    }

    fn applied(&mut self, s: &Settings) {
        self.settings = s.clone();
    }

    fn action(&mut self, name: &str, s: &mut Settings) -> Option<String> {
        match name {
            "portal" => {
                process::open(PORTAL);
            }
            "copy_redirect" => {
                return process::copy_text(discord::REDIRECT_URI).then(|| t!("toast.copied", text = discord::REDIRECT_URI));
            }
            "connect" if !s.has_credentials() => return Some(t!("toast.credentials_missing")),
            // The worker makes the connection; it only works while the program runs.
            "connect" if !self.running => return Some(t!("toast.start_first")),
            // Asked already (a double click, or Discord is asking right now): once is enough.
            "connect" if self.report.phase == Phase::Asking || self.orders.contains(&Order::Connect) => {}
            "connect" => self.orders.push(Order::Connect),
            "sign_out" => self.orders.push(Order::SignOut),
            "toggle" => s.call_status = !s.call_status,
            "open_log" => {
                process::open(&app::data_dir().join("log.txt").to_string_lossy());
            }
            "open_settings" => {
                process::open(&app::data_dir().join("settings.json").to_string_lossy());
            }
            // The credentials stay – otherwise the whole setup would have to be repeated.
            "reset" => {
                *s = Settings { client_id: s.client_id.clone(), client_secret: s.client_secret.clone(), ..Default::default() };
                return Some(t!("toast.reset"));
            }
            _ => {}
        }
        None
    }
}

// ---------- the worker ----------

/// The connection to Discord and everything known through it. Lives on the worker's thread.
struct Session {
    conn: Option<Connection>,
    /// AUTHENTICATE worked on this connection.
    signed_in: bool,
    /// The user's id (from AUTHENTICATE) – not to show oneself as "with …".
    me: Option<String>,
    /// Why the last "Connect" failed (stays until the next one).
    connect_error: Option<String>,
    /// The last renewal of the token found no internet (discord.com could not be reached).
    offline: bool,
    /// guild id → name (Discord is asked once per server).
    servers: BTreeMap<String, String>,
    /// Channel id and start time of the current call.
    call: Option<(String, u64)>,
    /// The activity last sent to Discord (sent again only when it changes: Discord allows at most 5 changes in
    /// 20 seconds).
    shown: Option<Value>,
    login: Login,
    report: Report,
}

impl Session {
    fn new() -> Session {
        let login = std::fs::read_to_string(login_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Session { conn: None, signed_in: false, me: None, connect_error: None, offline: false, servers: BTreeMap::new(), call: None, shown: None, login, report: Report::starting() }
    }

    fn say(&mut self, phase: Phase, message: String, channel: Option<&Channel>) {
        if phase == Phase::Failed && message != self.report.message {
            app::log(&message);
        }
        let heard = self.signed_in.then(|| activity::channel_text(channel));
        self.report = Report { phase, message, heard, user: self.login.user.clone(), app: self.login.app.clone() };
    }

    /// Makes the connection to Discord if there is none yet (for this client id).
    fn connect(&mut self, s: &Settings) -> Result<&mut Connection, Error> {
        let client_id = s.client_id.trim();
        if self.conn.as_ref().is_some_and(|c| c.client_id != client_id) {
            self.disconnect();
        }
        if self.conn.is_none() {
            self.conn = Some(Connection::open(client_id)?);
            self.signed_in = false;
            self.shown = None;
        }
        Ok(self.conn.as_mut().expect("just made"))
    }

    /// Closing the connection also clears the activity in Discord.
    fn disconnect(&mut self) {
        self.conn = None;
        self.signed_in = false;
        self.shown = None;
        self.call = None;
    }

    fn save_token(&mut self, s: &Settings, token: discord::Token) {
        self.login.client_id = s.client_id.trim().to_string();
        self.login.access_token = token.access_token;
        if let Some(refresh) = token.refresh_token {
            self.login.refresh_token = refresh;
        }
        self.login.expires = now() + token.expires_in;
        save_login(&self.login);
    }

    /// A new token through the refresh token (without asking in Discord). `false`: "Connect" is needed again.
    fn refresh(&mut self, s: &Settings) -> bool {
        if self.login.refresh_token.is_empty() {
            return false;
        }
        match discord::token(s.client_id.trim(), s.client_secret.trim(), Grant::Refresh(&self.login.refresh_token)) {
            Ok(token) => {
                self.offline = false;
                self.save_token(s, token);
                true
            }
            // Discord's own "no": expired or revoked – connect anew.
            Err(Error::Discord { message, .. }) => {
                self.offline = false;
                app::log(&format!("refreshing the token: {message}"));
                self.login.access_token.clear();
                false
            }
            // No internet: the token is kept and tried again later.
            Err(Error::Gone(message)) => {
                if !self.offline {
                    app::log(&format!("refreshing the token: {message}"));
                }
                self.offline = true;
                false
            }
        }
    }

    fn signed_in_as(&mut self, answer: &Value) {
        self.signed_in = true;
        self.connect_error = None;
        let user = &answer["user"];
        self.me = user["id"].as_str().map(str::to_string);
        let name = user["global_name"].as_str().or(user["username"].as_str()).unwrap_or_default().to_string();
        let app = answer["application"]["name"].as_str().unwrap_or_default().to_string();
        if (&self.login.user, &self.login.app) != (&name, &app) {
            self.login.user = name;
            self.login.app = app;
            save_login(&self.login);
        }
    }

    /// Signs in with the saved token – renewed first if it ends within a day or Discord refuses it.
    /// `Ok(false)`: "Connect" is needed.
    fn sign_in(&mut self, s: &Settings) -> Result<bool, Error> {
        self.offline = false;
        if self.login.client_id != s.client_id.trim() || self.login.access_token.is_empty() {
            return Ok(false);
        }
        if self.login.expires.saturating_sub(now()) < 24 * 3600 && !self.refresh(s) && self.login.access_token.is_empty() {
            return Ok(false);
        }
        let token = self.login.access_token.clone();
        match self.connect(s)?.authenticate(&token) {
            Ok(answer) => {
                self.signed_in_as(&answer);
                return Ok(true);
            }
            Err(Error::Discord { message, .. }) => app::log(&format!("Discord refused the token ({message}) – it is renewed")),
            Err(gone) => return Err(gone),
        }
        if !self.refresh(s) {
            return Ok(false);
        }
        let token = self.login.access_token.clone();
        let answer = self.connect(s)?.authenticate(&token)?;
        self.signed_in_as(&answer);
        Ok(true)
    }

    /// "Connect" on the page: Discord asks in its app – this waits until the user clicks there. `publish` shows
    /// "please click Authorize" on the page in the meantime.
    fn authorize(&mut self, s: &Settings, publish: &dyn Fn(&Report)) {
        if !s.has_credentials() {
            return self.say(Phase::Setup, t!("message.credentials_missing"), None);
        }
        match self.connect(s) {
            Ok(_) => {}
            Err(Error::Gone(_)) => return self.say(Phase::DiscordMissing, t!("message.start_discord"), None),
            Err(Error::Discord { message, .. }) => return self.say(Phase::Failed, t!("message.client_id_refused", error = message), None),
        }
        self.say(Phase::Asking, t!("message.authorize"), None);
        publish(&self.report);
        let signed = (|| {
            let code = self.connect(s)?.authorize()?;
            let token = discord::token(s.client_id.trim(), s.client_secret.trim(), Grant::Code(&code))?;
            let access = token.access_token.clone();
            self.save_token(s, token);
            let answer = self.connect(s)?.authenticate(&access)?;
            self.signed_in_as(&answer);
            Ok::<(), Error>(())
        })();
        match signed {
            Ok(()) => app::log(&format!("connected to Discord as {}", self.login.user)),
            Err(e) => {
                if matches!(e, Error::Gone(_)) {
                    self.disconnect();
                }
                app::log(&format!("connecting failed: {e}"));
                self.connect_error = Some(e.to_string());
            }
        }
    }

    fn sign_out(&mut self) {
        self.login = Login { client_id: self.login.client_id.clone(), ..Default::default() };
        save_login(&self.login);
        self.disconnect();
    }

    /// One round: where is the user right now, and does the display in Discord fit? Returns how long to wait.
    fn step(&mut self, s: &Settings) -> Duration {
        if !s.has_credentials() {
            self.disconnect();
            self.say(Phase::Setup, t!("message.setup"), None);
            return WAIT;
        }
        match self.look(s) {
            Ok(wait) => wait,
            Err(Error::Discord { message, code }) => {
                self.disconnect();
                self.say(Phase::Failed, t!("message.discord_error", error = message, code = code.map_or("–".to_string(), |c| c.to_string())), None);
                WAIT
            }
            Err(Error::Gone(_)) => {
                self.disconnect();
                self.say(Phase::DiscordMissing, t!("message.discord_missing"), None);
                WAIT
            }
        }
    }

    fn look(&mut self, s: &Settings) -> Result<Duration, Error> {
        self.connect(s)?;
        if !self.signed_in && !self.sign_in(s)? {
            let (phase, message) = self.why_signed_out();
            self.say(phase, message, None);
            return Ok(WAIT);
        }
        let answer = self.connect(s)?.voice_channel()?;
        let mut channel = activity::read_channel(&answer, self.me.as_deref());
        if let Some(channel) = channel.as_mut().filter(|c| c.place == Place::Server) {
            if let Some(guild) = channel.guild_id.clone() {
                channel.server = self.server_name(s, &guild);
            }
        }
        self.show(s, channel.as_ref())?;
        Ok(POLL)
    }

    /// Why the program is not signed in, for the status line.
    fn why_signed_out(&self) -> (Phase, String) {
        match &self.connect_error {
            Some(error) => (Phase::Failed, t!("message.connect_failed", error = error)),
            // Not "click Connect": that would need the internet as well, and the token is still good to renew.
            None if self.offline => (Phase::Offline, t!("message.offline")),
            None => (Phase::SignedOut, t!("message.not_connected")),
        }
    }

    fn server_name(&mut self, s: &Settings, guild: &str) -> String {
        if !self.servers.contains_key(guild) {
            // A server Discord does not tell the name of simply has none here; a broken connection shows at the
            // next question.
            let name = self.connect(s).and_then(|c| c.guild(guild)).ok().and_then(|g| g["name"].as_str().map(str::to_string)).unwrap_or_default();
            self.servers.insert(guild.to_string(), name);
        }
        self.servers[guild].clone()
    }

    /// Sets or clears the activity to fit the channel – only when it changes.
    fn show(&mut self, s: &Settings, channel: Option<&Channel>) -> Result<(), Error> {
        match channel {
            None => self.call = None,
            // A new call: the duration counts from now.
            Some(c) if self.call.as_ref().is_none_or(|(id, _)| id != &c.id) => self.call = Some((c.id.clone(), now())),
            Some(_) => {}
        }
        let new = activity::activity(channel, s, self.call.as_ref().map_or(0, |(_, start)| *start));
        if new != self.shown {
            self.connect(s)?.set_activity(new.as_ref())?;
            self.shown = new.clone();
        }
        if !s.call_status {
            self.say(Phase::Ready, t!("message.paused"), channel);
        } else if let Some(act) = &new {
            let text = [act["details"].as_str(), act["state"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            self.say(Phase::InCall, t!("message.showing", text = text), channel);
        } else if channel.is_some() {
            self.say(Phase::Ready, t!("message.not_shown"), channel);
        } else {
            self.say(Phase::Ready, t!("message.no_call"), channel);
        }
        Ok(())
    }
}

fn work(rt: Runtime<Plus>) {
    let publish = |report: &Report| {
        rt.with(|p, _| p.report = report.clone());
        rt.changed();
    };
    let mut taken_over = false;
    loop {
        // While the program only serves its page, the worker sleeps here.
        rt.wait_running();
        // The first time the program really runs: the autostart entry of the Python version now starts this
        // program. Here and not in `main` – started for its page and then switched on with Start, the process
        // never passes `main` with its workers on.
        if !std::mem::replace(&mut taken_over, true) {
            take_over_autostart();
        }
        let mut session = Session::new();
        rt.with(|p, _| {
            p.running = true;
            p.orders.clear();
        });
        publish(&session.report);
        while rt.running() {
            let (settings, orders) = rt.with(|p, s| (s.clone(), std::mem::take(&mut p.orders)));
            for order in orders {
                match order {
                    Order::Connect => session.authorize(&settings, &publish),
                    Order::SignOut => session.sign_out(),
                }
            }
            let wait = session.step(&settings);
            publish(&session.report);
            // Ends early when a setting changes, a button is pressed or the program is stopped.
            rt.sleep(wait);
        }
        // Stopped: the connection closes, and with it the activity disappears from Discord.
        session.disconnect();
        rt.with(|p, _| {
            p.running = false;
            p.report = Report::starting();
        });
        rt.changed();
    }
}

// ---------- from the Python version ----------

/// The settings of the Python version (`data\discord_plus.json` of the classic Toolbox) in today's names.
fn translate_settings(old: &Value) -> Value {
    // A text that is just the old standard text – in whichever language – becomes "the standard text".
    const STANDARD: [&str; 4] = ["Im Call", "In a call", "Im Voice-Chat", "In a voice chat"];
    let text = |key: &str| old[key].as_str().map(str::trim).filter(|t| !STANDARD.contains(t)).unwrap_or_default().to_string();
    let show_for = match old["anzeigen_bei"].as_str() {
        Some("privat") => "calls",
        Some("server") => "servers",
        _ => "both",
    };
    let mut new = serde_json::json!({"show_for": show_for, "text_call": text("text_privat"), "text_server": text("text_server")});
    for (old_key, new_key) in [("client_id", "client_id"), ("client_secret", "client_secret"), ("call_status", "call_status"), ("namen", "names"), ("ort", "place"), ("anzahl", "count"), ("dauer", "duration"), ("art", "kind"), ("neben_namen", "beside_name")] {
        if !old[old_key].is_null() {
            new[new_key] = old[old_key].clone();
        }
    }
    new
}

/// The sign-in of the Python version (`data\discord_plus_anmeldung.json`).
fn translate_login(old: &Value) -> Login {
    let text = |key: &str| old[key].as_str().unwrap_or_default().to_string();
    Login { client_id: text("client_id"), access_token: text("access_token"), refresh_token: text("refresh_token"), expires: old["ablauf"].as_f64().unwrap_or(0.0) as u64, user: text("nutzer"), app: text("app") }
}

/// On the very first start (no `data\settings.json` yet): settings and sign-in of the Python version are carried
/// over once, if a classic Toolbox lies in one of the folders above (Toolbox\DESIGN.md 11).
fn carry_over(program_dir: &Path, data: &Path) {
    if data.join("settings.json").exists() {
        return;
    }
    let read = |file: PathBuf| -> Option<Value> { serde_json::from_str(std::fs::read_to_string(file).ok()?.trim_start_matches('\u{feff}')).ok() };
    for above in program_dir.ancestors().skip(1).take(4) {
        for old_data in [above.join("data"), above.join("Toolbox").join("data")] {
            let Some(old) = read(old_data.join("discord_plus.json")) else { continue };
            let _ = std::fs::create_dir_all(data);
            let _ = std::fs::write(data.join("settings.json"), serde_json::to_string_pretty(&translate_settings(&old)).unwrap_or_default());
            if let Some(login) = read(old_data.join("discord_plus_anmeldung.json")) {
                let _ = std::fs::write(data.join("login.json"), serde_json::to_string_pretty(&translate_login(&login)).unwrap_or_default());
            }
            app::log(&format!("settings carried over from {}", old_data.display()));
            return;
        }
    }
}

/// The autostart entry of the Python version (`WindowsToolbox DiscordPlus`) is replaced by one for this program –
/// unless that one was switched off in Windows; then nothing starts, as before.
fn take_over_autostart() {
    if let Ok(exe) = std::env::current_exe()
        && kit::autostart::take_over(OLD_AUTOSTART, Plus::META.id, &exe)
    {
        app::log("the autostart entry of the Python version was taken over");
    }
}

fn main() {
    kit::texts!();
    app::init(&Plus::META);
    carry_over(&app::program_dir(), &app::data_dir());
    let plus = Plus { settings: Settings::default(), report: Report::starting(), orders: vec![], running: false };
    app::run(plus, work);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plus(running: bool) -> Plus {
        kit::texts!();
        kit::i18n::set_language("en");
        Plus { settings: Settings::default(), report: Report::starting(), orders: vec![], running }
    }

    fn page(plus: &Plus, s: &Settings) -> String {
        let v = serde_json::to_value(s).unwrap();
        let mut b = Builder::new(&v);
        plus.page(&mut b, s);
        serde_json::to_string(&b.finish()).unwrap()
    }

    /// Every key the page names is a field of the settings (Toolbox\DESIGN.md 13).
    #[test]
    fn page_keys_are_settings() {
        let plus = plus(true);
        kit::page::check_keys::<Settings>(|p| plus.page(p, &Settings::default())).unwrap();
    }

    /// Every text id used in the code exists, in every language.
    #[test]
    fn texts_are_complete() {
        kit::i18n::check_ids(env!("CARGO_MANIFEST_DIR")).unwrap();
        // The ids that are put together while running.
        plus(false);
        for id in KINDS.iter().map(|k| format!("kind.{k}")).chain(["standard.text_call".to_string(), "standard.text_server".to_string()]) {
            assert!(kit::i18n::has(&id), "{id}");
        }
    }

    #[test]
    fn the_page_tells_where_the_setup_stands() {
        let mut p = plus(false);
        let mut s = Settings::default();
        assert!(page(&p, &s).contains("Not set up yet"));
        s.client_id = "1".into();
        s.client_secret = " ".into();
        assert!(page(&p, &s).contains("Not set up yet"), "a secret of blanks is none");
        s.client_secret = "x".into();
        assert!(page(&p, &s).contains("has to be running to connect"));

        p.running = true;
        p.report = Report { phase: Phase::SignedOut, message: "m".into(), heard: None, user: String::new(), app: String::new() };
        let text = page(&p, &s);
        assert!(text.contains("Not connected yet") && !text.contains("sign_out"));
        p.report = Report { phase: Phase::Ready, message: "Connected – you're not in a call".into(), heard: Some("not in a call or voice chat".into()), user: "Max".into(), app: "Phone call".into() };
        let text = page(&p, &s);
        assert!(text.contains("Connected as Max – app “Phone call”") && text.contains("sign_out"));
        assert!(text.contains("Discord reports: not in a call or voice chat"));
        assert!(text.contains("Playing Phone call"), "the preview uses the app's name");
        // The warning about names only while names are shown.
        assert!(!text.contains("All your friends see this"));
        s.names = true;
        assert!(page(&p, &s).contains("All your friends see this"));
    }

    #[test]
    fn status_and_tray() {
        let mut p = plus(true);
        p.report = Report { phase: Phase::InCall, message: "Showing: In a call".into(), heard: None, user: String::new(), app: String::new() };
        assert_eq!(p.status(true), Status { running: true, state: Health::Ok, text: "Showing: In a call".into() });
        assert_eq!(p.status(false), Status::default());
        p.report.phase = Phase::Failed;
        assert_eq!(p.status(true).state, Health::Bad);
        p.report.phase = Phase::DiscordMissing;
        assert_eq!(p.status(true).state, Health::Warn);
        p.settings.call_status = true;
        assert_eq!(p.tray().menu[0], MenuEntry { label: "✓ Show call status".into(), action: "toggle".into() });
        p.settings.call_status = false;
        assert_eq!(p.tray().menu[0].label, "Show call status");
    }

    #[test]
    fn buttons() {
        let mut p = plus(false);
        let mut s = Settings { names: true, text_call: "On the phone".into(), ..Default::default() };
        // Connect needs the credentials and a running program; then the worker gets the order.
        assert!(p.action("connect", &mut s).unwrap().contains("Client ID"));
        s.client_id = "1".into();
        s.client_secret = "x".into();
        assert!(p.action("connect", &mut s).unwrap().contains("Start"));
        p.running = true;
        assert_eq!((p.action("connect", &mut s), p.orders.as_slice()), (None, &[Order::Connect][..]));
        p.action("sign_out", &mut s);
        assert_eq!(p.orders.last(), Some(&Order::SignOut));
        // A second Connect before the worker took the first, or while Discord asks, asks nothing more.
        p.action("connect", &mut s);
        assert_eq!(p.orders.iter().filter(|o| **o == Order::Connect).count(), 1);
        p.orders.clear();
        p.report.phase = Phase::Asking;
        p.action("connect", &mut s);
        assert!(p.orders.is_empty());
        assert!(!page(&p, &s).contains("\"connect\""), "no Connect button while Discord asks");
        p.report.phase = Phase::SignedOut;
        assert!(page(&p, &s).contains("\"connect\""));
        // The tray entry switches the call status.
        p.action("toggle", &mut s);
        assert!(!s.call_status);
        // Reset keeps the credentials.
        assert!(p.action("reset", &mut s).is_some());
        assert_eq!(s, Settings { client_id: "1".into(), client_secret: "x".into(), ..Default::default() });
    }

    #[test]
    fn the_python_version_s_files() {
        let old = json!({"client_id": " 123 ", "client_secret": "s", "call_status": false, "anzeigen_bei": "privat", "text_privat": "Im Call", "text_server": "Zocken",
            "namen": true, "ort": true, "anzahl": false, "dauer": false, "art": 3, "neben_namen": 0});
        let new: Settings = serde_json::from_value(translate_settings(&old)).unwrap();
        assert_eq!(
            new,
            Settings { client_id: " 123 ".into(), client_secret: "s".into(), call_status: false, show_for: ShowFor::Calls, text_call: String::new(), text_server: "Zocken".into(), names: true, place: true, count: false, duration: false, kind: 3, beside_name: 0 }
        );
        // What the old file lacks keeps its default.
        assert_eq!(serde_json::from_value::<Settings>(translate_settings(&json!({}))).unwrap(), Settings::default());
        let login = translate_login(&json!({"client_id": "123", "access_token": "a", "refresh_token": "r", "ablauf": 1790000000.5, "nutzer": "Max", "app": "Phone call"}));
        assert_eq!(login, Login { client_id: "123".into(), access_token: "a".into(), refresh_token: "r".into(), expires: 1790000000, user: "Max".into(), app: "Phone call".into() });

        // The files are found in a classic Toolbox above the program's folder – once.
        let root = std::env::temp_dir().join(format!("discord-plus-carry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (program, data) = (root.join("Discord Plus").join("dist"), root.join("Discord Plus").join("dist").join("data"));
        std::fs::create_dir_all(root.join("Toolbox").join("data")).unwrap();
        std::fs::create_dir_all(&program).unwrap();
        std::fs::write(root.join("Toolbox/data/discord_plus.json"), old.to_string()).unwrap();
        std::fs::write(root.join("Toolbox/data/discord_plus_anmeldung.json"), r#"{"client_id":"123","access_token":"a","nutzer":"Max"}"#).unwrap();
        carry_over(&program, &data);
        let carried: Settings = serde_json::from_str(&std::fs::read_to_string(data.join("settings.json")).unwrap()).unwrap();
        assert_eq!((carried.kind, carried.show_for), (3, ShowFor::Calls));
        assert!(std::fs::read_to_string(data.join("login.json")).unwrap().contains("\"user\": \"Max\""));
        std::fs::write(data.join("settings.json"), "{}").unwrap();
        carry_over(&program, &data);
        assert_eq!(std::fs::read_to_string(data.join("settings.json")).unwrap(), "{}", "never a second time");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The worker's steps that need no Discord: without credentials nothing is tried; a sign-in for another client
    /// id is not used.
    #[test]
    fn a_session_without_discord() {
        plus(false);
        let mut session = Session { conn: None, signed_in: false, me: None, connect_error: None, offline: false, servers: BTreeMap::new(), call: None, shown: None, login: Login::default(), report: Report::starting() };
        assert_eq!(session.step(&Settings::default()), WAIT);
        assert_eq!(session.report.phase, Phase::Setup);
        session.login = Login { client_id: "other".into(), access_token: "a".into(), ..Default::default() };
        let s = Settings { client_id: "mine".into(), client_secret: "x".into(), ..Default::default() };
        assert_eq!(session.sign_in(&s), Ok(false));

        // Why it is not signed in: no internet for the renewal is not "click Connect".
        assert_eq!(session.why_signed_out().0, Phase::SignedOut);
        session.offline = true;
        assert_eq!(session.why_signed_out(), (Phase::Offline, "discord.com cannot be reached – the sign-in is renewed as soon as the internet is back".to_string()));
        session.connect_error = Some("x".into());
        assert_eq!(session.why_signed_out().0, Phase::Failed);
    }
}
