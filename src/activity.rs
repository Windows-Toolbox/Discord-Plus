//! What friends see in Discord: from the voice channel Discord reports and the settings to the activity for
//! SET_ACTIVITY – and the same as a preview for the settings page. Plain data in, plain data out.

use kit::t;
use serde_json::{Value, json};

use crate::{Settings, ShowFor};

/// The kinds of activity Discord allows over this interface (SET_ACTIVITY): Playing, Listening to, Watching,
/// Competing in. Shown as `t!("kind.<number>")`.
pub const KINDS: [u8; 4] = [0, 2, 3, 5];
/// `status_display_type` – what stands next to the name in the member list: 0 = the app's name, 2 = the text.
pub const BESIDE_NAME: [u8; 2] = [0, 2];
/// Discord takes at most 128 characters per line, and at least 2.
const LINE: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    /// A private call with one person.
    Call,
    Group,
    Server,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Other {
    pub name: String,
    pub avatar: Option<String>,
}

/// Where the user is, as far as the display needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub place: Place,
    pub id: String,
    /// Name of the channel or of the group.
    pub name: String,
    pub guild_id: Option<String>,
    /// Name of the server; filled in by the caller (it costs a question to Discord).
    pub server: String,
    /// Everybody in the channel except the user.
    pub others: Vec<Other>,
}

/// The answer to GET_SELECTED_VOICE_CHANNEL → a channel; `None` if the user is in none. `me`: the user's id.
pub fn read_channel(data: &Value, me: Option<&str>) -> Option<Channel> {
    if !data.is_object() {
        return None;
    }
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let mut others = vec![];
    for state in data["voice_states"].as_array().into_iter().flatten() {
        let user = &state["user"];
        if me.is_some() && user["id"].as_str() == me {
            continue;
        }
        // nick = nickname (server or friends list), else display name, else user name
        let name = text(&state["nick"]).or(text(&user["global_name"])).or(text(&user["username"])).unwrap_or_else(|| "?".into());
        let avatar = match (user["id"].as_str(), user["avatar"].as_str()) {
            (Some(id), Some(avatar)) if !avatar.is_empty() => Some(format!("https://cdn.discordapp.com/avatars/{id}/{avatar}.png?size=256")),
            _ => None,
        };
        others.push(Other { name, avatar });
    }
    // Discord's channel types: 1 = direct message, 3 = group; everything else is a server.
    let place = match data["type"].as_i64() {
        Some(1) => Place::Call,
        Some(3) => Place::Group,
        _ => Place::Server,
    };
    Some(Channel { place, id: text(&data["id"]).unwrap_or_default(), name: text(&data["name"]).unwrap_or_default(), guild_id: text(&data["guild_id"]), server: String::new(), others })
}

/// For the page: what Discord reports right now, e.g. "Private call with Anna".
pub fn channel_text(channel: Option<&Channel>) -> String {
    let Some(channel) = channel else { return t!("channel.none") };
    let names: Vec<&str> = channel.others.iter().map(|o| o.name.as_str()).collect();
    let names = if names.is_empty() { t!("channel.nobody") } else { names.join(", ") };
    match channel.place {
        Place::Server => t!("channel.server", place = join(&[&channel.server, &channel.name]), names = names),
        Place::Call => t!("channel.call", names = names),
        Place::Group => t!("channel.group", names = names),
    }
}

/// The parts that are not empty, with a dot between them.
fn join(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= LINE { text.to_string() } else { text.chars().take(LINE - 1).chain(std::iter::once('…')).collect() }
}

/// `["Anna", "Tom", "Lea", "Ben"]` → "Anna, Tom, Lea and 1 more".
fn names_list(names: &[&str]) -> String {
    const SHOWN: usize = 3;
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [first @ .., last] if names.len() <= SHOWN => t!("shown.and", first = first.join(", "), last = last),
        _ => t!("shown.more", names = names[..SHOWN].join(", "), n = names.len() - SHOWN),
    }
}

/// The first line for calls or servers: the user's own text, or – if that is empty or too short for Discord – the
/// standard text in the current language.
pub fn line_text(own: &str, standard: &str) -> String {
    let own = own.trim();
    if own.chars().count() < 2 { kit::i18n::text(standard) } else { own.to_string() }
}

/// The activity for SET_ACTIVITY – or `None` if nothing is to be shown.
/// `start`: Unix time in seconds since when the user is in this call.
pub fn activity(channel: Option<&Channel>, s: &Settings, start: u64) -> Option<Value> {
    let channel = channel.filter(|_| s.call_status)?;
    let private = channel.place != Place::Server;
    if (s.show_for == ShowFor::Calls && !private) || (s.show_for == ShowFor::Servers && private) {
        return None;
    }
    let others = &channel.others;
    let mut text = if private { line_text(&s.text_call, "standard.text_call") } else { line_text(&s.text_server, "standard.text_server") };
    if s.names && !others.is_empty() {
        let names: Vec<&str> = others.iter().map(|o| o.name.as_str()).collect();
        text = t!("shown.with", text = text, names = names_list(&names));
    }

    let mut second = vec![];
    if s.place {
        match channel.place {
            Place::Server => second.push(join(&[&channel.server, &channel.name])),
            Place::Group => second.push(channel.name.clone()),
            Place::Call => {}
        }
    }
    let people = others.len() + 1;
    if s.count && (people > 2 || !private) {
        second.push(if people == 1 { t!("shown.alone") } else { t!("shown.people", n = people) });
    }
    let second = join(&second.iter().map(String::as_str).collect::<Vec<_>>());

    // Numbers that are not among the allowed ones (a settings file edited by hand) fall back to the first.
    let kind = if KINDS.contains(&s.kind) { s.kind } else { KINDS[0] };
    let beside = if BESIDE_NAME.contains(&s.beside_name) { s.beside_name } else { BESIDE_NAME[0] };
    let mut act = json!({"type": kind, "status_display_type": beside, "details": shorten(&text)});
    if second.chars().count() >= 2 {
        act["state"] = shorten(&second).into();
    }
    if s.duration {
        act["timestamps"] = json!({"start": start});
    }
    if let (true, [only]) = (s.names, &others[..]) {
        if let Some(avatar) = &only.avatar {
            act["assets"] = json!({"large_image": avatar, "large_text": shorten(&only.name)});
        }
    }
    Some(act)
}

/// Roughly what it looks like in the profile – for a call with Anna and a voice chat on a server.
/// `app`: the name of the user's Discord app, once known.
pub fn preview(s: &Settings, app: Option<&str>) -> String {
    let app = app.map(str::to_string).unwrap_or_else(|| t!("preview.app"));
    let s = Settings { call_status: true, ..s.clone() };
    let other = |name: &str, avatar: bool| Other { name: name.into(), avatar: avatar.then(|| "picture".into()) };
    let call = Channel { place: Place::Call, id: "1".into(), name: String::new(), guild_id: None, server: String::new(), others: vec![other("Anna", true)] };
    let server = Channel { place: Place::Server, id: "2".into(), name: t!("preview.channel"), guild_id: None, server: t!("preview.server_name"), others: vec![other("Anna", false), other("Tom", false), other("Lea", false)] };
    let mut blocks = vec![];
    for (title, channel) in [(t!("preview.call"), call), (t!("preview.server"), server)] {
        let Some(act) = activity(Some(&channel), &s, 0) else {
            blocks.push(t!("preview.not_shown", title = title));
            continue;
        };
        let verb = t!(&format!("kind.{}", act["type"]));
        let details = act["details"].as_str().unwrap_or_default();
        let mut lines = vec![format!("{title}:"), format!("      {verb} {app}"), format!("      {details}")];
        if let Some(state) = act["state"].as_str() {
            lines.push(format!("      {state}"));
        }
        if act.get("timestamps").is_some() {
            lines.push(format!("      {}", t!("preview.elapsed")));
        }
        if act.get("assets").is_some() {
            lines.push(format!("      {}", t!("preview.picture")));
        }
        let beside = if act["status_display_type"] == 0 { app.as_str() } else { details };
        lines.push(format!("      {}", t!("preview.beside", verb = verb, beside = beside)));
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english() {
        kit::texts!();
        kit::i18n::set_language("en");
    }

    fn other(name: &str) -> Other {
        Other { name: name.into(), avatar: None }
    }

    fn server(others: &[&str]) -> Channel {
        Channel { place: Place::Server, id: "9".into(), name: "General".into(), guild_id: Some("7".into()), server: "My server".into(), others: others.iter().map(|n| other(n)).collect() }
    }

    fn call() -> Channel {
        Channel { place: Place::Call, id: "1".into(), name: String::new(), guild_id: None, server: String::new(), others: vec![Other { name: "Anna".into(), avatar: Some("https://cdn/a.png".into()) }] }
    }

    #[test]
    fn what_discord_reports_becomes_a_channel() {
        let data = json!({"id": "42", "type": 2, "name": "General", "guild_id": "7", "voice_states": [
            {"nick": "Me on this server", "user": {"id": "100", "username": "me"}},
            {"nick": "Anni", "user": {"id": "200", "username": "anna", "global_name": "Anna", "avatar": "abc"}},
            {"nick": null, "user": {"id": "300", "username": "tom", "global_name": "Tom", "avatar": null}},
            {"user": {"id": "400", "username": "lea"}},
            {"user": {}}]});
        let channel = read_channel(&data, Some("100")).unwrap();
        assert_eq!((channel.place, channel.id.as_str(), channel.guild_id.as_deref()), (Place::Server, "42", Some("7")));
        let names: Vec<&str> = channel.others.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Anni", "Tom", "lea", "?"], "nickname, then display name, then user name – without the user");
        assert_eq!(channel.others[0].avatar.as_deref(), Some("https://cdn.discordapp.com/avatars/200/abc.png?size=256"));
        assert_eq!(channel.others[1].avatar, None);
        assert_eq!(read_channel(&json!({"type": 1, "id": "1"}), None).unwrap().place, Place::Call);
        assert_eq!(read_channel(&json!({"type": 3, "id": "1", "name": "Friends"}), None).unwrap().place, Place::Group);
        assert_eq!(read_channel(&Value::Null, Some("100")), None, "in no call");
    }

    #[test]
    fn the_activity_follows_the_settings() {
        english();
        let s = Settings::default();
        assert_eq!(activity(None, &s, 5), None);
        assert_eq!(activity(Some(&call()), &Settings { call_status: false, ..s.clone() }, 5), None);

        // The defaults: no names, no place; the number of people only from three on or on a server; the duration.
        assert_eq!(activity(Some(&call()), &s, 5).unwrap(), json!({"type": 0, "status_display_type": 2, "details": "In a call", "timestamps": {"start": 5}}));
        assert_eq!(activity(Some(&server(&["Anna", "Tom"])), &s, 5).unwrap()["state"], "3 people");
        assert_eq!(activity(Some(&server(&[])), &s, 5).unwrap()["state"], "alone");
        assert_eq!(activity(Some(&server(&["Anna"])), &s, 5).unwrap()["details"], "In a voice chat");

        // Only calls, only servers.
        assert_eq!(activity(Some(&server(&["Anna"])), &Settings { show_for: ShowFor::Calls, ..s.clone() }, 5), None);
        assert_eq!(activity(Some(&call()), &Settings { show_for: ShowFor::Servers, ..s.clone() }, 5), None);
        assert!(activity(Some(&call()), &Settings { show_for: ShowFor::Calls, ..s.clone() }, 5).is_some());

        // Everything switched on.
        let all = Settings { names: true, place: true, duration: false, kind: 3, beside_name: 0, text_call: "On the phone".into(), ..s.clone() };
        assert_eq!(
            activity(Some(&call()), &all, 5).unwrap(),
            json!({"type": 3, "status_display_type": 0, "details": "On the phone with Anna", "assets": {"large_image": "https://cdn/a.png", "large_text": "Anna"}})
        );
        let act = activity(Some(&server(&["Anna", "Tom", "Lea", "Ben"])), &all, 5).unwrap();
        assert_eq!((&act["details"], &act["state"]), (&json!("In a voice chat with Anna, Tom, Lea and 1 more"), &json!("My server · General · 5 people")));
        assert!(act.get("assets").is_none(), "a picture only for a call with one person");
        assert_eq!(activity(Some(&server(&["Anna", "Tom"])), &all, 5).unwrap()["details"], "In a voice chat with Anna and Tom");
        let group = Channel { place: Place::Group, name: "Friends".into(), ..server(&["Anna", "Tom"]) };
        assert_eq!(activity(Some(&group), &all, 5).unwrap()["state"], "Friends · 3 people");

        // A text too short for Discord gives way to the standard text; numbers Discord does not know to the first.
        let odd = Settings { text_call: " x ".into(), kind: 9, beside_name: 1, ..s.clone() };
        let act = activity(Some(&call()), &odd, 5).unwrap();
        assert_eq!((&act["details"], &act["type"], &act["status_display_type"]), (&json!("In a call"), &json!(0), &json!(0)));
        // Discord takes at most 128 characters.
        let long = Settings { text_call: "x".repeat(300), ..s };
        let details = activity(Some(&call()), &long, 5).unwrap()["details"].as_str().unwrap().to_string();
        assert_eq!((details.chars().count(), details.ends_with('…')), (128, true));
    }

    #[test]
    fn what_the_page_says() {
        english();
        assert_eq!(channel_text(None), "not in a call or voice chat");
        assert_eq!(channel_text(Some(&call())), "Private call with Anna");
        assert_eq!(channel_text(Some(&server(&[]))), "Voice chat My server · General – with nobody else");
        let s = Settings { names: true, ..Default::default() };
        let text = preview(&s, Some("Phone call"));
        assert!(text.starts_with("Call with Anna:\n      Playing Phone call\n      In a call with Anna\n      12:34 elapsed\n      (with Anna's profile picture)\n      next to your name: Playing In a call with Anna"), "{text}");
        assert!(text.contains("Voice chat on a server:\n      Playing Phone call\n      In a voice chat with Anna, Tom and Lea\n      4 people"), "{text}");
        let calls_only = preview(&Settings { show_for: ShowFor::Calls, call_status: false, ..Default::default() }, None);
        assert!(calls_only.contains("Playing ‹your app's name›") && calls_only.ends_with("Voice chat on a server:  not shown"), "{calls_only}");
    }
}
