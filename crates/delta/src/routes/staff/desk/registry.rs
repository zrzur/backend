//! The desk's command registry: every `/desk` command the server offers,
//! with its area, options, the permission that shows it, whether it changes
//! anything, and the handler that runs it as the presser's own
//! [`StaffContext`].
//!
//! The server is the source of the command tree: `GET /staff/desk/commands`
//! gives the bot exactly what to register, and a press counts only on a
//! command registered exactly like that (see [`registered_as_offered`]).
//! Handlers call the console's own core functions, so every permission
//! check, step-up and audit record is the console's.

use futures::future::BoxFuture;
use revolt_config::FeaturesStaffDesk;
use revolt_database::{
    record_platform_audit, AuditOutcome, Database, Interaction, StaffPermission, STAFF_SESSION_IDLE_MS,
};
use revolt_models::v0::{
    ApplicationCommandOption, ApplicationCommandType, CommandOptionChoice, CommandOptionType,
    CommandOptionValue, InteractionDataOption, InteractionOptionValue, Localizations,
};
use revolt_result::Result;
use serde::Serialize;

use super::super::{platform::stats_core, StaffContext};
use super::view::DeskView;
use super::COMMAND;

/// The areas `[features.staff_desk].areas` can switch on
pub const AREAS: &[&str] = &[
    "overview",
    "lookup",
    "moderation",
    "support",
    "review",
    "servers",
    "billing",
    "settings",
    "staff",
    "audit",
];

/// A text in the three languages of the product (Arabic: Modern Standard)
#[derive(Debug, Clone, Copy)]
pub struct Text {
    pub en: &'static str,
    pub ar: &'static str,
    pub de: &'static str,
}

impl Text {
    fn localizations(&self) -> Option<Localizations> {
        Some(
            [("ar", self.ar), ("de", self.de)]
                .into_iter()
                .map(|(lang, text)| (lang.to_string(), text.to_string()))
                .collect(),
        )
    }
}

/// A fixed choice of a `String` option
#[derive(Debug, Clone, Copy)]
pub struct DeskChoice {
    pub value: &'static str,
    pub name: Text,
}

/// A parameter of a sub-command
#[derive(Debug, Clone, Copy)]
pub struct DeskOption {
    pub name: &'static str,
    pub kind: CommandOptionType,
    pub required: bool,
    pub description: Text,
    pub choices: &'static [DeskChoice],
    pub max_length: Option<u32>,
}

/// What a handler gets besides the presser: the chosen sub-command's
/// options, read only from the stored interaction (or, on confirmation,
/// from the stored pending action), and the enabled areas
#[derive(Debug, Clone, Default)]
pub struct DeskInput {
    pub options: Vec<InteractionDataOption>,
    pub areas: Vec<String>,
}

impl DeskInput {
    /// The value of option `name`, only if it has the expected kind
    fn value(&self, name: &str, kind: CommandOptionType) -> Option<&InteractionOptionValue> {
        self.options
            .iter()
            .find(|o| o.name == name && o.kind == kind)
            .and_then(|o| o.value.as_ref())
    }

    fn string_of(&self, name: &str, kind: CommandOptionType) -> Option<String> {
        match self.value(name, kind) {
            Some(InteractionOptionValue::String(value)) => Some(value.trim().to_string()),
            _ => None,
        }
    }

    /// A `String` option, trimmed
    pub fn text(&self, name: &str) -> Option<String> {
        self.string_of(name, CommandOptionType::String)
    }

    /// A `User` option: the user's id, as the server resolved it
    pub fn user(&self, name: &str) -> Option<String> {
        self.string_of(name, CommandOptionType::User)
    }

    /// An `Integer` option
    #[allow(dead_code)]
    pub fn integer(&self, name: &str) -> Option<i64> {
        match self.value(name, CommandOptionType::Integer) {
            Some(InteractionOptionValue::Integer(value)) => Some(*value),
            _ => None,
        }
    }

    /// A `Boolean` option
    #[allow(dead_code)]
    pub fn boolean(&self, name: &str) -> Option<bool> {
        match self.value(name, CommandOptionType::Boolean) {
            Some(InteractionOptionValue::Boolean(value)) => Some(*value),
            _ => None,
        }
    }
}

/// What a changing command will do, in plain words, before it is confirmed
#[derive(Debug, Clone, Default)]
pub struct Prepared {
    pub target_kind: Option<String>,
    pub target: Option<String>,
    pub title: String,
    /// The exact consequences, one per line
    pub lines: Vec<String>,
}

pub type Handler =
    for<'a> fn(&'a Database, &'a StaffContext, &'a DeskInput) -> BoxFuture<'a, Result<DeskView>>;
pub type Prepare =
    for<'a> fn(&'a Database, &'a StaffContext, &'a DeskInput) -> BoxFuture<'a, Result<Prepared>>;

/// One `/desk` command
pub struct DeskCommand {
    /// The area that offers it; `None`: always offered (`me`)
    pub area: Option<&'static str>,
    /// `/desk <group> <name>`, or `/desk <name>` without a group
    pub group: Option<&'static str>,
    pub name: &'static str,
    pub description: Text,
    pub options: &'static [DeskOption],
    /// Shown to (and usable by) holders of this permission; `None`: anyone
    /// with a desk session
    pub permission: Option<StaffPermission>,
    /// Changes something: `/run` only stores a pending action, the change
    /// happens on its confirmation
    pub changes: bool,
    /// Cannot be undone: the confirmation always needs a step-up
    pub irreversible: bool,
    /// For changing commands: what the confirmation card says
    pub prepare: Option<Prepare>,
    pub handler: Handler,
}

impl DeskCommand {
    /// `group.name`, or `name`: the action key of audit records and
    /// pending actions
    pub fn key(&self) -> String {
        match self.group {
            Some(group) => format!("{group}.{}", self.name),
            None => self.name.to_string(),
        }
    }

    /// Offered while its area is on
    pub fn offered(&self, settings: &FeaturesStaffDesk) -> bool {
        self.area
            .is_none_or(|area| settings.areas.iter().any(|on| on == area))
    }

    /// The presser may see and use it
    pub fn visible_to(&self, staff: &StaffContext) -> bool {
        self.permission.is_none_or(|permission| staff.has(permission))
    }
}

/// A sub-command group's own description
pub struct DeskGroup {
    pub name: &'static str,
    pub description: Text,
}

/// The sign-in, step-up and sign-out sub-commands: in the tree, served by
/// their own routes (the bot opens a form for `login` and `stepup`)
const SESSION_SUBS: &[(&str, Text)] = &[
    (
        "login",
        Text {
            en: "Sign in with your 6-digit code",
            ar: "تسجيل الدخول برمز من 6 أرقام",
            de: "Mit deinem 6-stelligen Code anmelden",
        },
    ),
    (
        "logout",
        Text {
            en: "Sign out of the desk",
            ar: "تسجيل الخروج من المكتب",
            de: "Vom Desk abmelden",
        },
    ),
    (
        "stepup",
        Text {
            en: "Confirm it is you for a sensitive action",
            ar: "تأكيد هويتك لإجراء حساس",
            de: "Für eine heikle Aktion bestätigen, dass du es bist",
        },
    ),
];

const DESK_DESCRIPTION: Text = Text {
    en: "Zrzur Desk: sign in and see the platform",
    ar: "مكتب زرزور: سجّل الدخول وتابع المنصة",
    de: "Zrzur Desk: anmelden und die Plattform ansehen",
};

static ENTRIES: &[DeskCommand] = &[
    DeskCommand {
        area: None,
        group: None,
        name: "me",
        description: Text {
            en: "Show your roles and session",
            ar: "عرض أدوارك وجلستك",
            de: "Deine Rollen und Sitzung zeigen",
        },
        options: &[],
        permission: None,
        changes: false,
        irreversible: false,
        prepare: None,
        handler: me,
    },
    DeskCommand {
        area: Some("overview"),
        group: None,
        name: "stats",
        description: Text {
            en: "Show the platform overview",
            ar: "عرض نظرة عامة على المنصة",
            de: "Die Übersicht der Plattform zeigen",
        },
        options: &[],
        permission: Some(StaffPermission::DashboardRead),
        changes: false,
        irreversible: false,
        prepare: None,
        handler: stats,
    },
];

static GROUPS: &[DeskGroup] = &[];

/// Commands that exist only in tests (none in a build)
#[cfg(test)]
fn test_commands() -> (&'static [DeskCommand], &'static [DeskGroup]) {
    (test_entries::ENTRIES, test_entries::GROUPS)
}

#[cfg(not(test))]
fn test_commands() -> (&'static [DeskCommand], &'static [DeskGroup]) {
    (&[], &[])
}

/// Every registered command, in tree order
pub fn entries() -> impl Iterator<Item = &'static DeskCommand> {
    ENTRIES.iter().chain(test_commands().0.iter())
}

fn groups() -> impl Iterator<Item = &'static DeskGroup> {
    GROUPS.iter().chain(test_commands().1.iter())
}

/// The command `/desk [group] name`
pub fn find(group: Option<&str>, name: &str) -> Option<&'static DeskCommand> {
    entries().find(|e| e.group == group && e.name == name)
}

/// The command with this action key
pub fn find_key(key: &str) -> Option<&'static DeskCommand> {
    entries().find(|e| e.key() == key)
}

/// The enabled areas this person may use (at least one command of the area
/// is visible to them), in settings order, each once
pub fn usable_areas(enabled: &[String], staff: &StaffContext) -> Vec<String> {
    let mut areas: Vec<String> = Vec::new();
    for area in enabled {
        let usable = entries().any(|e| e.area == Some(area.as_str()) && e.visible_to(staff));
        if usable && !areas.contains(area) {
            areas.push(area.clone());
        }
    }
    areas
}

/// The `/desk` command as the bot registers it (the SDK's command
/// definition: `type`, `name`, `description`, `description_localizations`,
/// `options`)
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct DeskCommandTree {
    #[serde(rename = "type")]
    pub kind: ApplicationCommandType,
    pub name: String,
    pub description: String,
    pub description_localizations: Option<Localizations>,
    pub options: Vec<ApplicationCommandOption>,
}

fn sub_command(name: &str, description: Text, options: Vec<ApplicationCommandOption>) -> ApplicationCommandOption {
    ApplicationCommandOption {
        kind: CommandOptionType::SubCommand,
        name: name.to_string(),
        name_localizations: None,
        description: description.en.to_string(),
        description_localizations: description.localizations(),
        required: false,
        choices: None,
        options: (!options.is_empty()).then_some(options),
        channel_types: None,
        min_value: None,
        max_value: None,
        min_length: None,
        max_length: None,
        autocomplete: false,
    }
}

fn parameter(option: &DeskOption) -> ApplicationCommandOption {
    let choices: Vec<CommandOptionChoice> = option
        .choices
        .iter()
        .map(|choice| CommandOptionChoice {
            name: choice.name.en.to_string(),
            name_localizations: choice.name.localizations(),
            value: CommandOptionValue::String(choice.value.to_string()),
        })
        .collect();
    ApplicationCommandOption {
        kind: option.kind,
        name: option.name.to_string(),
        name_localizations: None,
        description: option.description.en.to_string(),
        description_localizations: option.description.localizations(),
        required: option.required,
        choices: (!choices.is_empty()).then_some(choices),
        options: None,
        channel_types: None,
        min_value: None,
        max_value: None,
        min_length: None,
        max_length: option.max_length,
        autocomplete: false,
    }
}

fn entry_option(entry: &DeskCommand) -> ApplicationCommandOption {
    sub_command(entry.name, entry.description, entry.options.iter().map(parameter).collect())
}

/// Adds a command to the tree: at the top, or in its group (made at its
/// first command)
fn place(options: &mut Vec<ApplicationCommandOption>, entry: &DeskCommand) {
    let Some(group) = entry.group else {
        options.push(entry_option(entry));
        return;
    };
    if let Some(existing) = options
        .iter_mut()
        .find(|o| o.kind == CommandOptionType::SubCommandGroup && o.name == group)
    {
        existing.options.get_or_insert_with(Vec::new).push(entry_option(entry));
        return;
    }
    let description = groups()
        .find(|g| g.name == group)
        .map(|g| g.description)
        .unwrap_or(Text {
            en: group,
            ar: group,
            de: group,
        });
    let mut made = sub_command(group, description, vec![entry_option(entry)]);
    made.kind = CommandOptionType::SubCommandGroup;
    options.push(made);
}

/// The `/desk` tree for the enabled areas: the commands without an area
/// (`me`), the session sub-commands, then each enabled area's commands in
/// registry order
pub fn desk_tree(settings: &FeaturesStaffDesk) -> DeskCommandTree {
    let mut options = Vec::new();
    for entry in entries().filter(|e| e.area.is_none()) {
        place(&mut options, entry);
    }
    for (name, description) in SESSION_SUBS {
        options.push(sub_command(name, *description, vec![]));
    }
    for entry in entries().filter(|e| e.area.is_some() && e.offered(settings)) {
        place(&mut options, entry);
    }
    DeskCommandTree {
        kind: ApplicationCommandType::ChatInput,
        name: COMMAND.to_string(),
        description: DESK_DESCRIPTION.en.to_string(),
        description_localizations: DESK_DESCRIPTION.localizations(),
        options,
    }
}

/// Empty lists and maps count as absent, so a bot that sends `[]` or `{}`
/// for "none" still matches
fn normalised(options: &[ApplicationCommandOption]) -> Vec<ApplicationCommandOption> {
    options
        .iter()
        .map(|o| {
            let mut o = o.clone();
            o.name_localizations = o.name_localizations.filter(|l| !l.is_empty());
            o.description_localizations = o.description_localizations.filter(|l| !l.is_empty());
            o.choices = o.choices.filter(|c| !c.is_empty()).map(|choices| {
                choices
                    .into_iter()
                    .map(|mut c| {
                        c.name_localizations = c.name_localizations.filter(|l| !l.is_empty());
                        c
                    })
                    .collect()
            });
            o.options = o
                .options
                .filter(|inner| !inner.is_empty())
                .map(|inner| normalised(&inner));
            o.channel_types = o.channel_types.filter(|c| !c.is_empty());
            o
        })
        .collect()
}

/// The command the person used is registered exactly as the server offers
/// it now: the configured bot's `desk`, the version the person saw (a bot
/// could show a relabelled command, take the press, then put the honest one
/// back), and the same sub-commands, groups, options, kinds, required
/// flags, choices and descriptions as [`desk_tree`] for the enabled areas.
pub async fn registered_as_offered(db: &Database, settings: &FeaturesStaffDesk, interaction: &Interaction) -> bool {
    let Some(id) = interaction.command_id.as_deref() else {
        return false;
    };
    let Ok(command) = db.fetch_application_command(id).await else {
        return false;
    };
    let offered = desk_tree(settings);
    command.application_id == settings.bot
        && interaction.application_id == settings.bot
        && command.kind == ApplicationCommandType::ChatInput
        && command.name == offered.name
        && interaction.command_version.as_deref() == Some(command.version.as_str())
        && command.name_localizations.as_ref().is_none_or(|l| l.is_empty())
        && command.default_member_permissions.is_none()
        && !command.nsfw
        && command.description == offered.description
        && command.description_localizations.filter(|l| !l.is_empty()) == offered.description_localizations
        && normalised(&command.options) == normalised(&offered.options)
}

/// The group, sub-command and its options the person chose, from the
/// stored interaction: exactly one sub-command, at the top or in one group
pub fn chosen(interaction: &Interaction) -> Option<(Option<&str>, &str, Vec<InteractionDataOption>)> {
    let [top] = interaction.command_options.as_slice() else {
        return None;
    };
    match top.kind {
        CommandOptionType::SubCommand => Some((None, top.name.as_str(), top.options.clone().unwrap_or_default())),
        CommandOptionType::SubCommandGroup => match top.options.as_deref() {
            Some([sub]) if sub.kind == CommandOptionType::SubCommand => Some((
                Some(top.name.as_str()),
                sub.name.as_str(),
                sub.options.clone().unwrap_or_default(),
            )),
            _ => None,
        },
        _ => None,
    }
}

/// Runs a command for the presser: the command's own permission first (the
/// console's check and Denied record), then its handler
pub async fn dispatch(db: &Database, staff: &StaffContext, entry: &DeskCommand, input: &DeskInput) -> Result<DeskView> {
    if let Some(permission) = entry.permission {
        staff.require_protective(db, permission).await?;
    }
    (entry.handler)(db, staff, input).await
}

/// A time (milliseconds since the epoch) as `YYYY-MM-DD HH:MM UTC`
pub fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "-".to_string())
}

// ---- handlers ----

/// `/desk me`: who the presser is on the desk, their session, and the
/// areas and commands they may use. Their own data only.
fn me<'a>(db: &'a Database, staff: &'a StaffContext, input: &'a DeskInput) -> BoxFuture<'a, Result<DeskView>> {
    Box::pin(async move {
        let mut record = staff.record("staff.desk.me", None);
        record.summary = Some("desk me".to_string());
        record_platform_audit(db, record, AuditOutcome::Success).await?;

        let areas = usable_areas(&input.areas, staff);
        let roles: Vec<&str> = staff.grant.roles.iter().map(|r| r.name()).collect();
        let mut view = DeskView::new("Zrzur Desk")
            .field("User", format!("{}#{}", staff.user.username, staff.user.discriminator))
            .field("Roles", roles.join(", "))
            .field("Permissions", staff.permissions.len().to_string())
            .field("Session ends", when(staff.session.expires_at))
            .field("Idle timeout", format!("{} min", STAFF_SESSION_IDLE_MS / 60_000))
            .field(
                "Step-up",
                staff
                    .session
                    .step_up_until
                    .filter(|until| *until > staff.now)
                    .map(|until| format!("until {}", when(until)))
                    .unwrap_or_else(|| "not active".to_string()),
            );
        if let Some(end) = staff.grant.expires_at {
            view = view.field("Access ends", when(end));
        }
        view = view.field(
            "Areas",
            if areas.is_empty() {
                "none".to_string()
            } else {
                areas.join(", ")
            },
        );
        for entry in entries() {
            let usable = entry.area.is_some_and(|area| areas.iter().any(|a| a == area)) && entry.visible_to(staff);
            if usable {
                view = view.line(format!("/{COMMAND} {}", entry.key().replace('.', " ")));
            }
        }
        Ok(view)
    })
}

/// `/desk stats`: the console's platform statistics (`stats_core`)
fn stats<'a>(db: &'a Database, staff: &'a StaffContext, _: &'a DeskInput) -> BoxFuture<'a, Result<DeskView>> {
    Box::pin(async move {
        let stats = stats_core(db, staff).await?;
        let window = |w: &super::super::platform::CountWindow| {
            format!("{} (+{} 24h, +{} 7d, +{} 30d)", w.total, w.last_24h, w.last_7d, w.last_30d)
        };
        let mut view = DeskView::new("Platform overview")
            .field("Users", window(&stats.users))
            .field(
                "Online now",
                stats.online_users.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string()),
            )
            .field("Servers", window(&stats.servers))
            .field("Channels", stats.channels_total.to_string())
            .field("Messages, last hour", stats.messages_last_hour.to_string())
            .field("Messages, last 24h", stats.messages_last_24h.to_string());
        if let Some(open) = stats.open_reports {
            view = view.field("Open reports", open.to_string());
        }
        Ok(view.line(format!("As of {}", when(stats.generated_at))))
    })
}

#[cfg(test)]
pub(crate) mod test_entries {
    //! Commands that exist only in tests, to drive the pending-action path
    //! before real changing commands land.
    use super::*;

    pub static GROUPS: &[DeskGroup] = &[DeskGroup {
        name: "test",
        description: Text {
            en: "Test commands",
            ar: "أوامر تجريبية",
            de: "Testbefehle",
        },
    }];

    const OPTIONS: &[DeskOption] = &[
        DeskOption {
            name: "user",
            kind: CommandOptionType::User,
            required: true,
            description: Text {
                en: "Who it is about",
                ar: "من يخصّه الأمر",
                de: "Um wen es geht",
            },
            choices: &[],
            max_length: None,
        },
        DeskOption {
            name: "note",
            kind: CommandOptionType::String,
            required: true,
            description: Text {
                en: "A note",
                ar: "ملاحظة",
                de: "Eine Notiz",
            },
            choices: &[],
            max_length: Some(200),
        },
    ];

    pub static ENTRIES: &[DeskCommand] = &[
        // irreversible: the confirmation always needs a step-up
        DeskCommand {
            area: Some("overview"),
            group: Some("test"),
            name: "change",
            description: Text {
                en: "A test change",
                ar: "تغيير تجريبي",
                de: "Eine Teständerung",
            },
            options: OPTIONS,
            permission: Some(StaffPermission::DashboardRead),
            changes: true,
            irreversible: true,
            prepare: Some(prepare),
            handler: done,
        },
        // reversible, but its core asks for a step-up itself
        DeskCommand {
            area: Some("overview"),
            group: Some("test"),
            name: "soft",
            description: Text {
                en: "A test change its core guards",
                ar: "تغيير تجريبي تحرسه دالته",
                de: "Eine Teständerung mit eigener Prüfung",
            },
            options: OPTIONS,
            permission: Some(StaffPermission::DashboardRead),
            changes: true,
            irreversible: false,
            prepare: Some(prepare),
            handler: guarded,
        },
    ];

    fn prepare<'a>(_: &'a Database, _: &'a StaffContext, input: &'a DeskInput) -> BoxFuture<'a, Result<Prepared>> {
        Box::pin(async move {
            let user = input.user("user").unwrap_or_default();
            let note = input.text("note").unwrap_or_default();
            Ok(Prepared {
                target_kind: Some("user".to_string()),
                target: Some(user.clone()),
                title: "Test change".to_string(),
                lines: vec![format!("Writes the note \"{note}\" about {user}.")],
            })
        })
    }

    fn done<'a>(_: &'a Database, _: &'a StaffContext, input: &'a DeskInput) -> BoxFuture<'a, Result<DeskView>> {
        Box::pin(async move {
            Ok(DeskView::new("Test change done")
                .field("User", input.user("user").unwrap_or_default())
                .field("Note", input.text("note").unwrap_or_default()))
        })
    }

    fn guarded<'a>(db: &'a Database, staff: &'a StaffContext, input: &'a DeskInput) -> BoxFuture<'a, Result<DeskView>> {
        Box::pin(async move {
            staff.require_step_up(db, StaffPermission::DashboardRead).await?;
            done(db, staff, input).await
        })
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use revolt_models::v0::DataApplicationCommand;

    fn all_on() -> FeaturesStaffDesk {
        FeaturesStaffDesk {
            enabled: true,
            bot: "bot".to_string(),
            places: vec![],
            allow_dms: true,
            areas: AREAS.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// Every entry is well formed: unique keys, known areas, a changing
    /// command has a confirmation text, an irreversible one a permission
    #[test]
    fn desk_registry_is_consistent() {
        let mut keys = Vec::new();
        for entry in entries() {
            assert!(!keys.contains(&entry.key()), "duplicate {}", entry.key());
            keys.push(entry.key());
            assert!(entry.area.is_none_or(|a| AREAS.contains(&a)), "{}", entry.key());
            assert_eq!(entry.changes, entry.prepare.is_some(), "{}", entry.key());
            assert!(!entry.irreversible || (entry.changes && entry.permission.is_some()), "{}", entry.key());
            assert!(SESSION_SUBS.iter().all(|(name, _)| *name != entry.name || entry.group.is_some()));
            if let Some(group) = entry.group {
                assert!(groups().any(|g| g.name == group), "group {group} has no description");
            }
        }
    }

    /// The tree passes the server's own command validation
    #[test]
    fn desk_tree_is_a_valid_command() {
        let tree = desk_tree(&all_on());
        let data: DataApplicationCommand =
            serde_json::from_value(serde_json::to_value(&tree).unwrap()).unwrap();
        data.validate_command().unwrap();
    }
}
