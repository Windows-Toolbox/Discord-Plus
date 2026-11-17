# Discord Plus

Extensions for Discord that run in the background – a program of the
[Windows Toolbox](https://github.com/Windows-Toolbox/Toolbox).

> **Downloaded from the releases?** The `.exe` is not signed (signing costs money). Windows SmartScreen may warn
> once at the first start: *More info → Run anyway*.

So far one function, the **call status**: Discord shows “In a call” as soon as you are on a call or in a voice
chat – also to friends who cannot see the server or the call. Discord itself shows server voice chats only to
people with access to the server, and private calls to nobody. So this program sets an activity of its own (Rich
Presence, “Playing …”); everybody sees it who otherwise sees what you play.

## Using it

Add the folder in the Toolbox (**Add program…**, or **Get programs**). Everything else – the one-time setup with
its step-by-step instructions, what is shown, the preview – is on the program's page there.

Without the Toolbox the program runs too: a double-click starts it, and after a moment it shows a tray icon of its
own (status, “Show call status”, Quit). Its settings then stand in `data\settings.json`.

- Works only with the Discord app on this PC (not in the browser).
- The program never asks in Discord by itself – only “Connect” on its page does. Nothing pops up mid-game.
- Client ID and Client Secret stay on this PC (`data\settings.json`), the sign-in in `data\login.json`.

Why an activity and not a custom status (the text under your name)? A custom status could only be set with your
account's token, and Discord bans tools that act as your account. An activity comes from an app of your own, which
is what Discord allows – that is also why the setup asks for one: Discord takes activities only from registered
apps, and the app's name is what stands after “Playing”. Without a review by Discord only its owner and up to 50
listed testers may use it this way – exactly right for you alone.

## When something does not work

What went wrong stands in the status line on the program's page and in `data\log.txt`.

| Message | Cause |
|---|---|
| Discord isn't running – waiting for the Discord app | The Discord app is closed (the browser does not count). The program waits and connects by itself. |
| Discord rejects the Client ID: Invalid Client ID | The Client ID was not copied completely. |
| Connecting failed: … `invalid_client` | The Client Secret is wrong – or it was reset in the portal since; then enter the new one. |
| Connecting failed: … `Invalid "redirect_uri"` | `http://localhost` is missing under OAuth2 → Redirects (step 3 of the setup). |
| Not connected – click “Connect” on the program's page | Never connected, or the sign-in ran out or was revoked in Discord → *Connect*. |
| discord.com cannot be reached – … | No internet while the sign-in had to be renewed. It is renewed by itself once the internet is back. |

The activity is there, but nobody sees it? In Discord under *Settings → Activity Privacy*, “Share my activity”
has to be on.

## Building

`build.ps1` builds the program and installs it into `dist\` – that folder is the installed program. Requirement:
[Rust](https://rustup.rs). `release.ps1` publishes a signed release (needs the Toolbox repo next to this one).

The shared library (`toolbox-kit`) comes from the Toolbox repo; cargo fetches it. With the Toolbox repo next to
this one and the small `.cargo\config.toml` in the folder above both (Toolbox `DESIGN.md`, section 3), the local
kit is used instead.

| File | What it holds |
|---|---|
| `src\main.rs` | settings, the page, the worker that keeps the connection to Discord, taking over the Python version's files |
| `src\discord.rs` | the Discord app's local interface (named pipe) and the login at discord.com |
| `src\activity.rs` | what is shown in Discord, from the voice channel and the settings – also the preview |
| `assets\i18n.json` | every text, German and English |

A new setting is one field in `Settings` and one line in `page`. A new function becomes a section of the page and
a step of the same worker (one connection to Discord, used by one thread).

## What the Python version could do – the port's checklist

Written from the Python version in the Toolbox repo (`programs\discord_plus\`: `discord_plus.py`, `discord_ipc.py`,
`discord_plus_settings.py`, `discord_plus_gui.py`, `i18n.json`, `README.md`) – every setting, menu entry and special
case – and checked line by line against this code on 2026-11-17 (Toolbox `DESIGN.md`, section 14).

✔ in this version, covered by a test 
· ◐ in this version, **not yet tried** with the real Discord app or the real Toolbox 
· ☐ **still missing** 
· ↻ works differently on purpose (why) 
· ✘ dropped on purpose (why)

The Python version was set up on the developer's PC and connected to Discord on 2026-10-13: pipe, handshake,
AUTHORIZE, the token at discord.com with `http://localhost` and AUTHENTICATE worked there. This version was tried
with the real Discord app and the real Toolbox on 2026-11-18 (“by hand”): with the sign-in taken over from the
Python version, which was still valid – so Connect and renewing the token have not run for real yet (◐).

**Talking to Discord** (`discord.rs`)

- ✔ find the pipe `discord-ipc-0` … `-9`; handshake `{"v": 1, "client_id": …}`; READY (by hand ✔), or Discord's
  refusal (e.g. 4000 “Invalid Client ID”) as “Discord refuses the client ID: …” (◐)
- ✔ frames: opcode + length + JSON; answer ping with pong; skip events nobody asked for; notice a closed
  connection; a length over 16 MB counts as a broken connection (new)
- ◐ AUTHORIZE (scopes `rpc`, `identify`) → code → token at discord.com (`redirect_uri` `http://localhost`) →
  AUTHENTICATE; the user's id (to leave them out of the list), display name (else user name) and the app's name
  are kept in `data\login.json` (AUTHENTICATE with a kept token: by hand ✔)
- ✔ reading discord.com's answers: token, refusal with Discord's reason, a broken answer, an answer without token
- ◐ the user agent: the Python version sent `windows-toolbox discord_plus (…, 1.0)` because Cloudflare in front of
  discord.com sometimes blocked Python's default one; the kit sends `windows-toolbox`. Shows at the first Connect.
- ◐ renew the token when it ends within a day or Discord refuses it; an answer without a new refresh token keeps
  the old one; Discord's “no” (expired, revoked) clears the token → “Connect” again
- ↻ no internet while renewing: the Python version said “Discord is not running” (wrong); this one keeps the
  token and tries it, and when that old token has expired too, it says “discord.com cannot be reached – the
  sign-in is renewed as soon as the internet is back” (✔ test; ◐ with a real cable pulled)
- ✔ GET_SELECTED_VOICE_CHANNEL every 3 s; GET_GUILD (timeout 5) once per server for its name – no name, no
  problem; SET_ACTIVITY with the program's pid, only on a change (Discord allows 5 changes in 20 s); none = cleared
  (by hand: a server voice chat and a private call shown, cleared after leaving)
- ◐ Discord refuses a command: disconnect, “Discord reports: … (code …)”, 15 s; Discord not running or the
  connection lost: “Discord is not running – waiting for the Discord app”, 15 s – never asking the user
- ✔ a sign-in made for another client ID is not used; a changed client ID opens a new connection
- ◐ Stop while Discord is asking for “Authorize”: the worker waits for that answer before it stops (the Python
  version too). To try what Discord does when nobody clicks.

**Reading the call** (`activity.rs`, all ✔)

- channel type 1 = private call, 3 = group call, everything else = server
- the others: everybody in the channel except the user; nickname before display name before user name, else “?”;
  profile picture `cdn.discordapp.com/avatars/<id>/<hash>.png?size=256`
- “Discord reports: …” on the page: private call with … / group call with … / voice chat server · channel –
  with …; “nobody else”; “not in a call or voice chat”
- the duration counts from the moment the program first sees a channel; another channel starts it anew

**What is shown** (`activity.rs`, all ✔)

- call status on/off – off clears the activity, the status says “Paused – shows nothing in Discord”
- show for calls and servers / only calls / only servers
- own text for calls and for servers; shorter than 2 characters → the standard text
- ↻ an empty text field means the standard text of the current language, and the field's label names it. The
  Python version wrote the standard text into the field and recognised it in every language; the carry-over
  turns such an old standard text into an empty field, so it keeps following the language.
- names (“… with Anna, Tom and Lea”, “and 1 more” from the fourth on); the profile picture (with the name as its
  tooltip) only in a call with exactly one other person
- place: “server · channel” on servers, the group's name in group calls, nothing in a private call
- number of people: from three on, and on servers always (“alone”)
- the second line only when it has at least 2 characters; at most 128 characters per line (“…”)
- duration (`timestamps.start`); the kind (Playing, Listening to, Watching, Competing in); what stands next to the
  name (app name or text); numbers Discord does not know fall back to the first

**The settings file**

- ✔ every value is checked on its own: a wrong one takes its default, the others stay (the kit; Python: `pruefen`)
- ✔ blanks around client ID and secret do not count; a secret of blanks is none

**The page**

- ✔ status: the line with its colour, “Discord reports: …” (only while signed in), “Stopped – without the program,
  Discord shows nothing”. ↻ “Ready” is green now (Python: neutral) – the kit has three colours.
- ✔ setup: the instructions (steps 1–5 and how to test), *Open Developer Portal*, *Copy http://localhost* with a
  note, Client ID, Client Secret as dots, *Connect*, *Sign out* only while signed in; below it “Connected as … –
  app …” / “Not set up yet” / “Please click Authorize in Discord” / “Not connected yet” / “has to be running”
- ◐ Connect right after typing the secret: a field hands its text over when it loses focus, and the click on
  *Connect* does that before the button counts. The Python version handed typed fields over explicitly. To try.
- ✔ Connect while Discord is already asking: *Connect* is not there then, and a second click before the worker
  took the first asks nothing more (the Python version asked Discord a second time)
- ✔ call status: every setting above, applied at once. ↻ the warning about names only while names are on.
- ✔ the preview for a call and a server, with the app's name once known, or “not shown”; “roughly”
- ✔ good to know (plus a line about *Start with Windows*)
- ✔ advanced: open log, open settings file, reset everything (the credentials stay, with a note). ↻ the reset
  button is not red: kit buttons have no colours.
- ✔ Start / Stop / Restart, open folder, README – the Toolbox's frame around every program's page
- ✘ “Connect” starting the program by itself: a page button reaches a stopped program only to change settings.
  The page says “first Start, then Connect” instead.
- ✘ the note “DiscordPlus.exe is missing – run Setup.exe”: there is no starter any more, only the one exe.
- ✘ the note “the entry starts a different location”: the kit repairs the entry itself after a move, and leaves
  it alone while the other location still exists.

**Start with Windows**

- ✔ the entry of the Python version (`WindowsToolbox DiscordPlus`) is taken over the first time the workers run –
  also when the program was started for its page and then switched on with Start (test in the kit; by hand ✔ –
  started from its page, which the first version of this port missed)
- ✔ the switch *Start with Windows* in the Toolbox's frame (Toolbox tests; by hand ✔: one entry in Task Manager)
- ✔ “switched off in Windows (Task Manager → Startup) – switching on allows it again”; an entry that starts another
  copy says which, and switching on takes it over
- ✔ removing the program from the Toolbox removes the entry (in the Python version: Remove and `Uninstall.exe`)

**Around it**

- ✔ single instance; a second start switches the first one on
- ✔ the Python version still running at the switch (“which one to keep” → this one): the Toolbox asks it to quit on
  its port and then starts this one, so Discord never shows the activity twice (Toolbox test; ◐ by hand)
- ✔ tray: the status as tooltip (at most 127 characters) and first line, “Show call status” with a check mark,
  Settings (a click on the icon; Python: a double click), Quit – under the Toolbox's icon, or an icon of its own
  without the Toolbox
- ✔ the language follows the Toolbox at once, including the standard texts shown in Discord and the tray menu
- ✔ errors go to `data\log.txt` (↻ Python: the Toolbox's `data\logs\discord_plus.log`): connected as …, token
  refused, connecting failed, every new error once
- ✔ settings and sign-in of the Python version are carried over once (test ✔; with the real files by hand ✔ – the
  Python version had renewed its token meanwhile, so it simply went on working)
- ✔ quitting or stopping clears the activity in Discord (the connection closes)
- ✔ Windows shows “Discord Plus” with its logo and “Windows Toolbox” as publisher (compiled into the exe; the same
  `.ico` as the Python version; by hand ✔ in Task Manager → Startup)
- ✘ the status field with the activity last sent: nothing read it
- ✔ the logo's source: `assets\logo_source.png`, next to the `.ico` made from it. ✘ `build_logo.py`: Python
  leaves the project, and the `.ico` is done; a new logo would need a new script anyway.
- ✔ README: why an activity and an app of one's own, the table of common messages, “nobody sees it → Activity
  Privacy” (above)
