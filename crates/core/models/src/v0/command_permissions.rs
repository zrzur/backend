//! Per-server command permissions: who may use an application's commands,
//! and where.
//!
//! A server's managers can add rules for the whole application or for one
//! command. A rule targets a role, a member or a channel and either allows
//! or denies. Two special ids stand for "everyone" and "every channel": the
//! server's own id, with the type `Role` or `Channel`.
//!
//! How a member's use of a command in a channel is decided
//! ([`command_access`]):
//!
//! 1. **Channel.** The command's own rules are read first, then the
//!    application's. At each level a rule for this channel wins over the
//!    "every channel" rule. The first rule found decides: deny stops the
//!    command here, allow lets it through. No channel rule means every
//!    channel.
//! 2. **Server owner.** Always passes the member and role rules (so nobody
//!    can lock the owner out of their own server's commands).
//! 3. **Member and roles.** Again the command's rules come first, then the
//!    application's; the first level with a rule that matches decides. At a
//!    level: a rule for the member wins; otherwise the member's role rules
//!    count, and **a deny on any of their roles beats an allow on another**;
//!    only when no role matches does the "everyone" rule count.
//! 4. **Default.** With no matching member or role rule, the command's
//!    `default_member_permissions` decide, as before. A matching allow
//!    replaces that check.

/// Most rules in one scope (the whole application, or one command)
pub const COMMAND_PERMISSIONS_MAX: usize = 100;

auto_derived!(
    /// What a command permission rule targets
    #[derive(Copy, Hash)]
    pub enum CommandPermissionType {
        /// A role; the server id means everyone
        Role,
        /// One member
        Member,
        /// A channel; the server id means every channel
        Channel,
    }

    /// One rule
    pub struct CommandPermission {
        /// Role, user or channel id (or the server id, see the type)
        pub id: String,
        /// What the id is
        #[cfg_attr(feature = "serde", serde(rename = "type"))]
        pub kind: CommandPermissionType,
        /// `true` allows, `false` denies
        pub permission: bool,
    }

    /// Rules of one application in one server, for the whole application
    /// (no `command_id`) or for one of its commands
    pub struct CommandPermissions {
        pub server_id: String,
        pub application_id: String,
        /// The command these rules are for; absent for the whole application
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub command_id: Option<String>,
        pub permissions: Vec<CommandPermission>,
    }

    /// New rules for a scope; an empty list removes them all
    pub struct DataEditCommandPermissions {
        pub permissions: Vec<CommandPermission>,
    }
);

impl DataEditCommandPermissions {
    /// At most [`COMMAND_PERMISSIONS_MAX`] rules, each id once per type
    pub fn validate_rules(&self) -> Result<(), String> {
        if self.permissions.len() > COMMAND_PERMISSIONS_MAX {
            return Err(format!(
                "at most {COMMAND_PERMISSIONS_MAX} rules per command"
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for rule in &self.permissions {
            if rule.id.is_empty() || rule.id.len() > 64 {
                return Err("every rule needs an id of 1-64 characters".to_string());
            }
            if !seen.insert((rule.kind, rule.id.as_str())) {
                return Err(format!("{} is listed twice", rule.id));
            }
        }
        Ok(())
    }
}

/// Who is asking, and where
pub struct CommandAccessQuery<'a> {
    pub server_id: &'a str,
    pub user_id: &'a str,
    /// The member's roles, without "everyone"
    pub roles: &'a [String],
    /// The channel; `None` skips the channel rules (listing for a server)
    pub channel_id: Option<&'a str>,
    pub is_owner: bool,
    /// Whether the member has the command's `default_member_permissions`
    pub has_default_permissions: bool,
}

/// The decision for one command
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAccess {
    Allowed,
    /// A rule denies it in this channel
    DeniedInChannel,
    /// A member or role rule denies it
    Denied,
    /// No rule matched and the member lacks `default_member_permissions`
    MissingPermissions,
}

impl CommandAccess {
    pub fn allowed(self) -> bool {
        self == CommandAccess::Allowed
    }
}

fn find<'a>(
    rules: &'a [CommandPermission],
    kind: CommandPermissionType,
    id: &str,
) -> Option<&'a CommandPermission> {
    rules.iter().find(|rule| rule.kind == kind && rule.id == id)
}

/// Decide whether a member may use a command (rules in the module docs).
/// `command` holds the command's rules, `application` the whole app's.
pub fn command_access(
    command: &[CommandPermission],
    application: &[CommandPermission],
    query: &CommandAccessQuery<'_>,
) -> CommandAccess {
    let levels = [command, application];

    // 1. channel
    if let Some(channel_id) = query.channel_id {
        for rules in levels {
            let rule = find(rules, CommandPermissionType::Channel, channel_id)
                .or_else(|| find(rules, CommandPermissionType::Channel, query.server_id));
            if let Some(rule) = rule {
                if !rule.permission {
                    return CommandAccess::DeniedInChannel;
                }
                break;
            }
        }
    }

    // 2. owner
    if query.is_owner {
        return CommandAccess::Allowed;
    }

    // 3. member, then roles (deny wins), then everyone
    let decide = |allow: bool| {
        if allow {
            CommandAccess::Allowed
        } else {
            CommandAccess::Denied
        }
    };
    for rules in levels {
        if let Some(rule) = find(rules, CommandPermissionType::Member, query.user_id) {
            return decide(rule.permission);
        }
        let mut matched = rules.iter().filter(|rule| {
            rule.kind == CommandPermissionType::Role
                && rule.id != query.server_id
                && query.roles.contains(&rule.id)
        });
        if let Some(first) = matched.next() {
            let any_deny = !first.permission || matched.any(|rule| !rule.permission);
            return decide(!any_deny);
        }
        if let Some(rule) = find(rules, CommandPermissionType::Role, query.server_id) {
            return decide(rule.permission);
        }
    }

    // 4. default
    if query.has_default_permissions {
        CommandAccess::Allowed
    } else {
        CommandAccess::MissingPermissions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: CommandPermissionType, id: &str, permission: bool) -> CommandPermission {
        CommandPermission {
            id: id.to_string(),
            kind,
            permission,
        }
    }
    use CommandPermissionType::*;

    struct Who {
        roles: Vec<String>,
        channel: Option<&'static str>,
        owner: bool,
        default: bool,
    }

    fn who(roles: &[&str]) -> Who {
        Who {
            roles: roles.iter().map(|r| r.to_string()).collect(),
            channel: Some("general"),
            owner: false,
            default: true,
        }
    }

    fn check(command: &[CommandPermission], app: &[CommandPermission], who: &Who) -> CommandAccess {
        command_access(
            command,
            app,
            &CommandAccessQuery {
                server_id: "server",
                user_id: "alice",
                roles: &who.roles,
                channel_id: who.channel,
                is_owner: who.owner,
                has_default_permissions: who.default,
            },
        )
    }

    #[test]
    fn no_rules_falls_back_to_default_permissions() {
        assert_eq!(check(&[], &[], &who(&[])), CommandAccess::Allowed);
        let mut lacking = who(&[]);
        lacking.default = false;
        assert_eq!(check(&[], &[], &lacking), CommandAccess::MissingPermissions);
    }

    #[test]
    fn allow_rule_replaces_default_permissions() {
        let mut lacking = who(&["mods"]);
        lacking.default = false;
        assert_eq!(
            check(&[rule(Role, "mods", true)], &[], &lacking),
            CommandAccess::Allowed
        );
    }

    #[test]
    fn deny_on_one_role_beats_allow_on_another() {
        let rules = [rule(Role, "mods", true), rule(Role, "muted", false)];
        assert_eq!(check(&rules, &[], &who(&["mods", "muted"])), CommandAccess::Denied);
        // order of the rules does not matter
        let rules = [rule(Role, "muted", false), rule(Role, "mods", true)];
        assert_eq!(check(&rules, &[], &who(&["mods", "muted"])), CommandAccess::Denied);
        assert_eq!(check(&rules, &[], &who(&["mods"])), CommandAccess::Allowed);
    }

    #[test]
    fn member_rule_beats_role_rules() {
        let rules = [rule(Role, "muted", false), rule(Member, "alice", true)];
        assert_eq!(check(&rules, &[], &who(&["muted"])), CommandAccess::Allowed);
        let rules = [rule(Role, "mods", true), rule(Member, "alice", false)];
        assert_eq!(check(&rules, &[], &who(&["mods"])), CommandAccess::Denied);
    }

    #[test]
    fn specific_role_beats_everyone() {
        // "only moderators": deny everyone, allow mods
        let rules = [rule(Role, "server", false), rule(Role, "mods", true)];
        assert_eq!(check(&rules, &[], &who(&["mods"])), CommandAccess::Allowed);
        assert_eq!(check(&rules, &[], &who(&[])), CommandAccess::Denied);
    }

    #[test]
    fn command_rules_beat_application_rules() {
        let app = [rule(Role, "server", false)];
        let command = [rule(Role, "server", true)];
        assert_eq!(check(&command, &app, &who(&[])), CommandAccess::Allowed);
        assert_eq!(check(&[], &app, &who(&[])), CommandAccess::Denied);
        // a command deny beats an application allow for the same member
        let app = [rule(Member, "alice", true)];
        let command = [rule(Role, "server", false)];
        assert_eq!(check(&command, &app, &who(&[])), CommandAccess::Denied);
    }

    #[test]
    fn channel_rules() {
        // only in #bots
        let rules = [rule(Channel, "server", false), rule(Channel, "bots", true)];
        assert_eq!(check(&rules, &[], &who(&[])), CommandAccess::DeniedInChannel);
        let mut in_bots = who(&[]);
        in_bots.channel = Some("bots");
        assert_eq!(check(&rules, &[], &in_bots), CommandAccess::Allowed);
        // listing without a channel skips channel rules
        let mut listing = who(&[]);
        listing.channel = None;
        assert_eq!(check(&rules, &[], &listing), CommandAccess::Allowed);
        // command channel allow beats an application channel deny
        let app = [rule(Channel, "general", false)];
        let command = [rule(Channel, "general", true)];
        assert_eq!(check(&command, &app, &who(&[])), CommandAccess::Allowed);
    }

    #[test]
    fn owner_passes_role_rules_but_not_channel_rules() {
        let mut owner = who(&[]);
        owner.owner = true;
        owner.default = false;
        assert_eq!(
            check(&[rule(Member, "alice", false)], &[], &owner),
            CommandAccess::Allowed
        );
        assert_eq!(
            check(&[rule(Channel, "general", false)], &[], &owner),
            CommandAccess::DeniedInChannel
        );
    }

    #[test]
    fn validation() {
        let ok = DataEditCommandPermissions {
            permissions: vec![rule(Role, "a", true), rule(Member, "a", false)],
        };
        assert!(ok.validate_rules().is_ok());
        let twice = DataEditCommandPermissions {
            permissions: vec![rule(Role, "a", true), rule(Role, "a", false)],
        };
        assert!(twice.validate_rules().is_err());
        let empty_id = DataEditCommandPermissions {
            permissions: vec![rule(Role, "", true)],
        };
        assert!(empty_id.validate_rules().is_err());
        let too_many = DataEditCommandPermissions {
            permissions: (0..=COMMAND_PERMISSIONS_MAX)
                .map(|i| rule(Role, &i.to_string(), true))
                .collect(),
        };
        assert!(too_many.validate_rules().is_err());
    }
}
