//! The buttons of the desk bot's rooms: what each press stands for.
//!
//! A press is `zd:act:<action>:<id>`, maybe from a form. Only the actions
//! listed here exist. Each stands for one desk command: the id is the only
//! value taken from the button (it fills one named option), the form gives
//! only the fields the action lists, and some options are fixed by the
//! action itself. Every value is then checked against the command's own
//! option: its kind, its choices, its length and bounds. Anything else is
//! refused. What runs is the desk command, with every check of `/run`: the
//! presser's grant and session, the command's permission, and for a
//! changing command its confirmation card first.

use revolt_models::v0::{CommandOptionType, InteractionDataOption, InteractionOptionValue};

use super::registry::{self, DeskCommand};

/// One kind of press
#[derive(Debug, Clone, Copy)]
pub struct RoomAction {
    pub action: &'static str,
    pub command: &'static str,
    pub group: Option<&'static str>,
    pub name: &'static str,
    /// The option the id of the button fills (none: the id must be `-`)
    pub id: Option<&'static str>,
    /// Options the action fixes
    pub fixed: &'static [(&'static str, &'static str)],
    /// Options the form may fill
    pub form: &'static [&'static str],
    /// Of those, the ones the form must fill (beyond the command's own)
    pub need: &'static [&'static str],
}

const fn act(
    action: &'static str,
    command: &'static str,
    group: Option<&'static str>,
    name: &'static str,
    id: Option<&'static str>,
) -> RoomAction {
    RoomAction {
        action,
        command,
        group,
        name,
        id,
        fixed: &[],
        form: &[],
        need: &[],
    }
}

const REVIEW: &str = "desk-review";
const SUPPORT: &str = "desk-support";
const MOD: &str = "desk-mod";
const SERVERS: &str = "desk-servers";
const DESK: &str = "desk";

pub const ROOM_ACTIONS: &[RoomAction] = &[
    // bot verification
    RoomAction { fixed: &[("decision", "approve")], form: &["note"], ..act("verify-approve", REVIEW, Some("verification"), "decide", Some("request")) },
    RoomAction { fixed: &[("decision", "deny")], form: &["note"], need: &["note"], ..act("verify-deny", REVIEW, Some("verification"), "decide", Some("request")) },
    RoomAction { form: &["reason"], need: &["reason"], ..act("bot-unverify", REVIEW, Some("bot"), "unverify", Some("bot")) },
    // designs
    RoomAction { fixed: &[("action", "approve")], form: &["reason"], ..act("design-approve", REVIEW, Some("design"), "review", Some("design")) },
    RoomAction { fixed: &[("action", "reject")], form: &["reason"], need: &["reason"], ..act("design-reject", REVIEW, Some("design"), "review", Some("design")) },
    RoomAction { fixed: &[("action", "suspend")], form: &["reason"], need: &["reason"], ..act("design-suspend", REVIEW, Some("design"), "review", Some("design")) },
    // Discover
    RoomAction { fixed: &[("kind", "server"), ("decision", "Approve")], form: &["reason"], ..act("discover-approve-server", REVIEW, Some("discover"), "decide", Some("id")) },
    RoomAction { fixed: &[("kind", "server"), ("decision", "Deny")], form: &["reason"], need: &["reason"], ..act("discover-deny-server", REVIEW, Some("discover"), "decide", Some("id")) },
    RoomAction { fixed: &[("kind", "bot"), ("decision", "Approve")], form: &["reason"], ..act("discover-approve-bot", REVIEW, Some("discover"), "decide", Some("id")) },
    RoomAction { fixed: &[("kind", "bot"), ("decision", "Deny")], form: &["reason"], need: &["reason"], ..act("discover-deny-bot", REVIEW, Some("discover"), "decide", Some("id")) },
    // support tickets
    act("ticket-view", DESK, Some("ticket"), "view", Some("ticket")),
    RoomAction { fixed: &[("close", "false")], form: &["text"], need: &["text"], ..act("ticket-reply", SUPPORT, Some("ticket"), "reply", Some("ticket")) },
    RoomAction { fixed: &[("close", "true")], form: &["text"], need: &["text"], ..act("ticket-reply-close", SUPPORT, Some("ticket"), "reply", Some("ticket")) },
    RoomAction { form: &["text"], need: &["text"], ..act("ticket-note", SUPPORT, Some("ticket"), "note", Some("ticket")) },
    act("ticket-close", SUPPORT, Some("ticket"), "close", Some("ticket")),
    // reports
    act("case-view", DESK, Some("case"), "view", Some("case")),
    RoomAction { fixed: &[("status", "UnderReview")], ..act("case-review", MOD, Some("case"), "status", Some("case")) },
    RoomAction { fixed: &[("status", "Escalated")], form: &["note"], ..act("case-escalate", MOD, Some("case"), "status", Some("case")) },
    RoomAction { fixed: &[("status", "NoViolation")], form: &["public_note", "note"], ..act("case-dismiss", MOD, Some("case"), "status", Some("case")) },
    RoomAction { form: &["kind", "reason", "days", "note"], ..act("case-act", MOD, Some("case"), "action", Some("case")) },
    // appeals
    RoomAction { fixed: &[("decision", "overturn")], form: &["note"], ..act("appeal-overturn", MOD, Some("appeal"), "decide", Some("appeal")) },
    RoomAction { fixed: &[("decision", "uphold")], form: &["note"], ..act("appeal-uphold", MOD, Some("appeal"), "decide", Some("appeal")) },
    // bugs and feedback
    act("bug-view", DESK, Some("bug"), "view", Some("bug")),
    RoomAction { fixed: &[("action", "confirm")], form: &["note", "severity"], ..act("bug-confirm", SUPPORT, Some("bug"), "review", Some("bug")) },
    RoomAction { fixed: &[("action", "need_info")], form: &["note"], need: &["note"], ..act("bug-need-info", SUPPORT, Some("bug"), "review", Some("bug")) },
    RoomAction { fixed: &[("action", "fix")], form: &["note"], ..act("bug-fixed", SUPPORT, Some("bug"), "review", Some("bug")) },
    RoomAction { fixed: &[("action", "not_a_bug")], form: &["note"], ..act("bug-not", SUPPORT, Some("bug"), "review", Some("bug")) },
    // people
    RoomAction { form: &["query"], need: &["query"], ..act("user-find", DESK, Some("user"), "find", None) },
    act("user-view", DESK, Some("user"), "view", Some("user")),
    act("user-badges", DESK, Some("user"), "badges", Some("user")),
    act("user-history", DESK, Some("user"), "history", Some("user")),
    RoomAction { form: &["category", "reason", "note"], ..act("user-warn", MOD, Some("user"), "warn", Some("user")) },
    RoomAction { form: &["category", "days", "reason", "note"], ..act("user-suspend", MOD, Some("user"), "suspend", Some("user")) },
    RoomAction { form: &["category", "reason", "note"], ..act("user-ban", MOD, Some("user"), "ban", Some("user")) },
    RoomAction { form: &["reason"], ..act("user-sessions", MOD, Some("user"), "sessions-end", Some("user")) },
    RoomAction { form: &["badge", "reason", "days"], ..act("badge-give", REVIEW, Some("badge"), "give", Some("user")) },
    RoomAction { form: &["badge", "reason"], ..act("badge-remove", REVIEW, Some("badge"), "remove", Some("user")) },
    // servers
    RoomAction { form: &["query"], need: &["query"], ..act("server-find", DESK, Some("server"), "find", None) },
    act("server-view", DESK, Some("server"), "view", Some("server")),
    RoomAction { form: &["action", "reason"], ..act("server-mark", SERVERS, Some("server"), "mark", Some("server")) },
    RoomAction { form: &["confirm_name", "reason"], ..act("server-delete", SERVERS, Some("server"), "delete", Some("server")) },
    // bots
    RoomAction { form: &["query"], need: &["query"], ..act("bot-find", DESK, Some("bot"), "find", None) },
    act("bot-view", DESK, Some("bot"), "view", Some("bot")),
    RoomAction { form: &["category", "reason", "note"], ..act("bot-disable", MOD, Some("bot"), "disable", Some("bot")) },
    RoomAction { form: &["confirm_name", "reason"], ..act("bot-delete", SERVERS, Some("bot"), "delete", Some("bot")) },
    // the platform
    act("stats", DESK, None, "stats", None),
];

/// A plain id: letters and digits only
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// One value as the command's option takes it, or `None` when it doesn't fit
fn value_for(entry: &DeskCommand, name: &str, given: &str) -> Option<InteractionDataOption> {
    let option = entry.options.iter().find(|o| o.name == name)?;
    if !option.choices.is_empty() && !option.choices.iter().any(|c| c.value == given) {
        return None;
    }
    if option.max_length.is_some_and(|max| given.chars().count() > max as usize) {
        return None;
    }
    let value = match option.kind {
        // people are found by id or name#tag later, as with a typed command
        CommandOptionType::String | CommandOptionType::User => InteractionOptionValue::String(given.to_string()),
        CommandOptionType::Integer => {
            let n: i64 = given.parse().ok()?;
            if option.min_value.is_some_and(|min| (n as f64) < min)
                || option.max_value.is_some_and(|max| (n as f64) > max)
            {
                return None;
            }
            InteractionOptionValue::Integer(n)
        }
        CommandOptionType::Boolean => InteractionOptionValue::Boolean(match given {
            "true" => true,
            "false" => false,
            _ => return None,
        }),
        _ => return None,
    };
    // a person stays a `String` here: `resolve_people` finds them and
    // makes the option a `User`, as for a typed command
    let kind = match &option.kind {
        CommandOptionType::User => CommandOptionType::String,
        other => other.clone(),
    };
    Some(InteractionDataOption {
        name: name.to_string(),
        kind,
        value: Some(value),
        options: None,
    })
}

/// What a press stands for: the desk command and its options. `form`
/// gives a field of the form (empty when absent).
pub fn room_command(
    custom_id: &str,
    form: &dyn Fn(&str) -> String,
) -> Option<(&'static DeskCommand, Vec<InteractionDataOption>)> {
    let rest = custom_id.strip_prefix(super::run::ACT_PREFIX)?;
    let (name, id) = rest.split_once(':')?;
    let action = ROOM_ACTIONS.iter().find(|a| a.action == name)?;
    let entry = registry::find(action.command, action.group, action.name)?;
    let mut options = Vec::new();
    match action.id {
        Some(option) => {
            if !plain_id(id) {
                return None;
            }
            options.push(value_for(entry, option, id)?);
        }
        None if id == "-" => {}
        None => return None,
    }
    for (option, value) in action.fixed {
        options.push(value_for(entry, option, value)?);
    }
    for field in action.form {
        let value = form(field);
        let value = value.trim();
        if value.is_empty() {
            if action.need.contains(field) {
                return None;
            }
            continue;
        }
        options.push(value_for(entry, field, value)?);
    }
    // every option the command needs is there
    for option in entry.options.iter().filter(|o| o.required) {
        if !options.iter().any(|o| o.name == option.name) {
            return None;
        }
    }
    Some((entry, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> String {
        String::new()
    }

    fn text(options: &[InteractionDataOption], name: &str) -> Option<String> {
        options.iter().find(|o| o.name == name).and_then(|o| match &o.value {
            Some(InteractionOptionValue::String(v)) => Some(v.clone()),
            _ => None,
        })
    }

    #[test]
    fn every_action_stands_for_a_real_desk_command_with_real_options() {
        for action in ROOM_ACTIONS {
            let entry = registry::find(action.command, action.group, action.name)
                .unwrap_or_else(|| panic!("{} has no desk command", action.action));
            let named = |name: &str| entry.options.iter().any(|o| o.name == name);
            if let Some(id) = action.id {
                assert!(named(id), "{}: no option {id}", action.action);
            }
            for (option, value) in action.fixed {
                let o = entry.options.iter().find(|o| o.name == *option).unwrap_or_else(|| panic!("{}: no option {option}", action.action));
                if !o.choices.is_empty() {
                    assert!(o.choices.iter().any(|c| c.value == *value), "{}: {option}={value}", action.action);
                }
            }
            for field in action.form.iter().chain(action.need) {
                assert!(named(field), "{}: no option {field}", action.action);
            }
        }
        let mut names: Vec<_> = ROOM_ACTIONS.iter().map(|a| a.action).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ROOM_ACTIONS.len(), "an action is listed twice");
    }

    #[test]
    fn a_press_takes_its_id_from_the_button_and_the_rest_from_the_form() {
        let (entry, options) = room_command("zd:act:verify-approve:01ABCDEF", &none).unwrap();
        assert_eq!(entry.key(), registry::find("desk-review", Some("verification"), "decide").unwrap().key());
        assert_eq!(text(&options, "request").as_deref(), Some("01ABCDEF"));
        assert_eq!(text(&options, "decision").as_deref(), Some("approve"));

        let form = |name: &str| match name {
            "category" => "Spam".to_string(),
            "reason" => "Sending the same link everywhere".to_string(),
            "decision" => "approve".to_string(),
            _ => String::new(),
        };
        let (_, options) = room_command("zd:act:user-ban:01ABCDEF", &form).unwrap();
        assert_eq!(text(&options, "user").as_deref(), Some("01ABCDEF"));
        assert_eq!(text(&options, "category").as_deref(), Some("Spam"));
        // a field the action doesn't list is never read
        assert!(options.iter().all(|o| o.name != "decision"));

        let (_, options) = room_command("zd:act:ticket-reply-close:01ABCDEF", &|n: &str| if n == "text" { "Fixed".into() } else { String::new() }).unwrap();
        assert!(options.iter().any(|o| o.name == "close" && matches!(o.value, Some(InteractionOptionValue::Boolean(true)))));

        let (_, options) = room_command("zd:act:user-find:-", &|n: &str| if n == "query" { "sami".into() } else { String::new() }).unwrap();
        assert_eq!(text(&options, "query").as_deref(), Some("sami"));
    }

    #[test]
    fn anything_else_is_refused() {
        // unknown actions, ids that aren't plain, a missing id
        assert!(room_command("zd:act:delete-everything:01ABCDEF", &none).is_none());
        assert!(room_command("zd:act:verify-approve:../x", &none).is_none());
        assert!(room_command("zd:act:verify-approve:", &none).is_none());
        assert!(room_command("zd:act:user-find:01ABCDEF", &none).is_none());
        assert!(room_command("zd:confirm:01ABCDEF", &none).is_none());
        // what the form must give, and the command's own required options
        assert!(room_command("zd:act:verify-deny:01ABCDEF", &none).is_none());
        assert!(room_command("zd:act:user-ban:01ABCDEF", &none).is_none());
        // a value outside the option's choices
        let bad = |name: &str| match name {
            "category" => "NotACategory".to_string(),
            "reason" => "x".to_string(),
            _ => String::new(),
        };
        assert!(room_command("zd:act:user-ban:01ABCDEF", &bad).is_none());
        // a number outside its bounds
        let days = |name: &str| match name {
            "badge" => "partner".to_string(),
            "reason" => "Our partner".to_string(),
            "days" => "999999".to_string(),
            _ => String::new(),
        };
        assert!(room_command("zd:act:badge-give:01ABCDEF", &days).is_none());
    }
}
