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
    AuditOutcome, Database, Interaction, STAFF_SESSION_IDLE_MS, StaffPermission,
    record_platform_audit,
};
use revolt_models::v0::{
    ApplicationCommandOption, ApplicationCommandType, CommandOptionChoice, CommandOptionType,
    CommandOptionValue, InteractionDataOption, InteractionOptionValue, Localizations,
};
use revolt_result::Result;
use serde::Serialize;

use super::super::{StaffContext, platform::stats_core};
use super::COMMAND;
use super::view::DeskView;

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

/// The language of an answer, from the command's `locale`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Ar,
    De,
}

impl Lang {
    /// `ar...` and `de...` locales, else English
    pub fn from_locale(locale: Option<&str>) -> Lang {
        let primary = locale
            .unwrap_or_default()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match primary.as_str() {
            "ar" => Lang::Ar,
            "de" => Lang::De,
            _ => Lang::En,
        }
    }
}

impl Text {
    /// The text in `lang`
    pub fn get(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.en,
            Lang::Ar => self.ar,
            Lang::De => self.de,
        }
    }

    /// The text in `lang` with `{name}` placeholders filled in, in one pass:
    /// a value that itself contains `{other}` stays as it is
    pub fn fill(&self, lang: Lang, values: &[(&str, &str)]) -> String {
        let template = self.get(lang);
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close) => {
                    let name = &after[..close];
                    match values.iter().find(|(key, _)| *key == name) {
                        Some((_, value)) => out.push_str(value),
                        None => out.push_str(&rest[open..open + close + 2]),
                    }
                    rest = &after[close + 1..];
                }
                None => {
                    out.push_str(&rest[open..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

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
    /// The language for the texts the server writes itself (titles, field
    /// names, consequences); values (names, ids) stay as they are
    pub lang: Lang,
    /// The pending action being confirmed (none for a reading command)
    pub pending: Option<String>,
    /// The page of a list, from 1 (a `zd:page:` press asks for another)
    pub page: u32,
    /// The `/desk` command interaction this answer belongs to (page
    /// buttons name it)
    pub origin: String,
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
    /// Values the server read when the card was made (a case's version),
    /// stored with the options and given to the handler on confirmation
    pub stored: Vec<InteractionDataOption>,
    /// This card cannot be undone (a suspension among case actions), even
    /// if the command can be
    pub irreversible: bool,
}

/// A command's handler. It runs as the presser and must call the
/// console's own core, and every core must call `StaffContext::require`
/// (with its permission and step-up rules) before any write: the desk's
/// own checks are an extra layer, never the only one.
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
    /// Exists only while `features.plugins_enabled` is on (the console's
    /// plugin routes answer `PluginsDisabled` otherwise)
    pub plugins: bool,
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

    /// The top-level command it is under (`desk`, `desk-mod`, ...)
    pub fn command(&self) -> &'static str {
        command_of(self.area)
    }

    /// Offered while its area is on (and, for a plugin command, while
    /// plugins are on: see [`plugins_on`] and the tree)
    pub fn offered(&self, settings: &FeaturesStaffDesk) -> bool {
        self.area
            .is_none_or(|area| settings.areas.iter().any(|on| on == area))
    }

    /// The presser may see and use it
    pub fn visible_to(&self, staff: &StaffContext) -> bool {
        self.permission
            .is_none_or(|permission| staff.has(permission))
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
        plugins: false,
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
        plugins: false,
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
    ENTRIES
        .iter()
        .chain(super::lookup::ENTRIES.iter())
        .chain(super::moderation::ENTRIES.iter())
        .chain(test_commands().0.iter())
}

fn groups() -> impl Iterator<Item = &'static DeskGroup> {
    GROUPS
        .iter()
        .chain(super::lookup::GROUPS.iter())
        .chain(super::moderation::GROUPS.iter())
        .chain(test_commands().1.iter())
}

/// Whether plugins are switched on (`features.plugins_enabled`)
pub async fn plugins_on() -> bool {
    revolt_config::config().await.features.plugins_enabled
}

/// Areas with a top-level command of their own. The platform allows a
/// command 25 options and 8000 characters of names, descriptions and
/// choices; one command for every area would not fit. The reading areas
/// stay in `/desk`.
const AREA_COMMANDS: &[(&str, &str, Text)] = &[(
    "moderation",
    "desk-mod",
    Text {
        en: "Zrzur Desk: moderation",
        ar: "مكتب زرزور: الإشراف",
        de: "Zrzur Desk: Moderation",
    },
)];

/// The top-level command an area's commands are under
pub fn command_of(area: Option<&str>) -> &'static str {
    AREA_COMMANDS
        .iter()
        .find(|(a, _, _)| Some(*a) == area)
        .map(|(_, command, _)| *command)
        .unwrap_or(COMMAND)
}

/// Whether `name` is one of the desk's top-level commands
pub fn is_desk_command(name: &str) -> bool {
    name == COMMAND || AREA_COMMANDS.iter().any(|(_, command, _)| *command == name)
}

/// The command `/<command> [group] name`
pub fn find(command: &str, group: Option<&str>, name: &str) -> Option<&'static DeskCommand> {
    entries().find(|e| e.command() == command && e.group == group && e.name == name)
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

fn sub_command(
    name: &str,
    description: Text,
    options: Vec<ApplicationCommandOption>,
) -> ApplicationCommandOption {
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
    sub_command(
        entry.name,
        entry.description,
        entry.options.iter().map(parameter).collect(),
    )
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
        existing
            .options
            .get_or_insert_with(Vec::new)
            .push(entry_option(entry));
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

/// The commands for the enabled areas. `/desk` always: the commands
/// without an area (`me`), the session sub-commands, then each enabled
/// reading area's commands in registry order. Then one command per
/// enabled area that has its own (`/desk-mod`), when it offers anything.
pub fn desk_tree(settings: &FeaturesStaffDesk, plugins_on: bool) -> Vec<DeskCommandTree> {
    let shown = |e: &&DeskCommand| plugins_on || !e.plugins;
    let mut options = Vec::new();
    for entry in entries().filter(|e| e.area.is_none()) {
        place(&mut options, entry);
    }
    for (name, description) in SESSION_SUBS {
        options.push(sub_command(name, *description, vec![]));
    }
    for entry in entries()
        .filter(|e| e.area.is_some() && e.command() == COMMAND && e.offered(settings))
        .filter(shown)
    {
        place(&mut options, entry);
    }
    let mut trees = vec![DeskCommandTree {
        kind: ApplicationCommandType::ChatInput,
        name: COMMAND.to_string(),
        description: DESK_DESCRIPTION.en.to_string(),
        description_localizations: DESK_DESCRIPTION.localizations(),
        options,
    }];
    for (_, command, description) in AREA_COMMANDS {
        let mut options = Vec::new();
        for entry in entries()
            .filter(|e| e.command() == *command && e.offered(settings))
            .filter(shown)
        {
            place(&mut options, entry);
        }
        if !options.is_empty() {
            trees.push(DeskCommandTree {
                kind: ApplicationCommandType::ChatInput,
                name: command.to_string(),
                description: description.en.to_string(),
                description_localizations: description.localizations(),
                options,
            });
        }
    }
    trees
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
pub async fn registered_as_offered(
    db: &Database,
    settings: &FeaturesStaffDesk,
    interaction: &Interaction,
) -> bool {
    let Some(id) = interaction.command_id.as_deref() else {
        return false;
    };
    let Ok(command) = db.fetch_application_command(id).await else {
        return false;
    };
    let Some(offered) = desk_tree(settings, plugins_on().await)
        .into_iter()
        .find(|tree| Some(tree.name.as_str()) == interaction.command_name.as_deref())
    else {
        return false;
    };
    command.application_id == settings.bot
        && interaction.application_id == settings.bot
        && command.kind == ApplicationCommandType::ChatInput
        && command.name == offered.name
        && interaction.command_version.as_deref() == Some(command.version.as_str())
        && command
            .name_localizations
            .as_ref()
            .is_none_or(|l| l.is_empty())
        && command.default_member_permissions.is_none()
        && !command.nsfw
        && command.description == offered.description
        && command.description_localizations.filter(|l| !l.is_empty())
            == offered.description_localizations
        && normalised(&command.options) == normalised(&offered.options)
}

/// The group, sub-command and its options the person chose, from the
/// stored interaction: exactly one sub-command, at the top or in one group
pub fn chosen(
    interaction: &Interaction,
) -> Option<(Option<&str>, &str, Vec<InteractionDataOption>)> {
    let [top] = interaction.command_options.as_slice() else {
        return None;
    };
    match top.kind {
        CommandOptionType::SubCommand => Some((
            None,
            top.name.as_str(),
            top.options.clone().unwrap_or_default(),
        )),
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

/// Runs a reading command for the presser: the command's own permission
/// first (the console's check and Denied record), then its handler. A
/// changing command never runs here, only through a confirmed pending
/// action.
pub async fn dispatch(
    db: &Database,
    staff: &StaffContext,
    entry: &DeskCommand,
    input: &DeskInput,
) -> Result<DeskView> {
    if entry.changes {
        return Err(revolt_result::create_error!(InvalidOperation));
    }
    // the plugin switch answers first, like the console's plugin guard
    if entry.plugins && !plugins_on().await {
        return Err(revolt_result::create_error!(PluginsDisabled));
    }
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

// ---- texts the server writes itself ----

/// The desk's own words, in the three languages (Arabic: Modern Standard)
pub mod texts {
    use super::Text;

    pub const DESK: Text = Text {
        en: "Zrzur Desk",
        ar: "مكتب زرزور",
        de: "Zrzur Desk",
    };
    pub const USER: Text = Text {
        en: "User",
        ar: "المستخدم",
        de: "Benutzer",
    };
    pub const ROLES: Text = Text {
        en: "Roles",
        ar: "الأدوار",
        de: "Rollen",
    };
    pub const PERMISSIONS: Text = Text {
        en: "Permissions",
        ar: "الصلاحيات",
        de: "Berechtigungen",
    };
    pub const SESSION_ENDS: Text = Text {
        en: "Session ends",
        ar: "تنتهي الجلسة",
        de: "Sitzung endet",
    };
    pub const IDLE: Text = Text {
        en: "Idle timeout",
        ar: "مهلة الخمول",
        de: "Zeitlimit bei Inaktivität",
    };
    pub const MINUTES: Text = Text {
        en: "{n} min",
        ar: "{n} دقيقة",
        de: "{n} Min.",
    };
    pub const STEP_UP: Text = Text {
        en: "Step-up",
        ar: "التحقق الإضافي",
        de: "Zusätzliche Bestätigung",
    };
    pub const UNTIL: Text = Text {
        en: "until {time}",
        ar: "حتى {time}",
        de: "bis {time}",
    };
    pub const NOT_ACTIVE: Text = Text {
        en: "not active",
        ar: "غير مفعّل",
        de: "nicht aktiv",
    };
    pub const ACCESS_ENDS: Text = Text {
        en: "Access ends",
        ar: "ينتهي الوصول",
        de: "Zugang endet",
    };
    pub const AREAS: Text = Text {
        en: "Areas",
        ar: "المجالات",
        de: "Bereiche",
    };
    pub const NONE: Text = Text {
        en: "none",
        ar: "لا شيء",
        de: "keine",
    };
    pub const OVERVIEW: Text = Text {
        en: "Platform overview",
        ar: "نظرة عامة على المنصة",
        de: "Übersicht der Plattform",
    };
    pub const USERS: Text = Text {
        en: "Users",
        ar: "المستخدمون",
        de: "Benutzer",
    };
    pub const ONLINE: Text = Text {
        en: "Online now",
        ar: "المتصلون الآن",
        de: "Jetzt online",
    };
    pub const SERVERS: Text = Text {
        en: "Servers",
        ar: "السيرفرات",
        de: "Server",
    };
    pub const CHANNELS: Text = Text {
        en: "Channels",
        ar: "القنوات",
        de: "Kanäle",
    };
    pub const MESSAGES_HOUR: Text = Text {
        en: "Messages, last hour",
        ar: "الرسائل في الساعة الأخيرة",
        de: "Nachrichten, letzte Stunde",
    };
    pub const MESSAGES_DAY: Text = Text {
        en: "Messages, last 24 hours",
        ar: "الرسائل في آخر 24 ساعة",
        de: "Nachrichten, letzte 24 Stunden",
    };
    pub const OPEN_REPORTS: Text = Text {
        en: "Open reports",
        ar: "البلاغات المفتوحة",
        de: "Offene Meldungen",
    };
    pub const WINDOW: Text = Text {
        en: "{total} (+{day} in 24 hours, +{week} in 7 days, +{month} in 30 days)",
        ar: "{total} (+{day} خلال 24 ساعة، +{week} خلال 7 أيام، +{month} خلال 30 يومًا)",
        de: "{total} (+{day} in 24 Stunden, +{week} in 7 Tagen, +{month} in 30 Tagen)",
    };
    pub const AS_OF: Text = Text {
        en: "As of {time}",
        ar: "بتاريخ {time}",
        de: "Stand: {time}",
    };
    /// The confirmation button
    pub const CONFIRM: Text = Text {
        en: "Confirm",
        ar: "تأكيد",
        de: "Bestätigen",
    };
    pub const IRREVERSIBLE: Text = Text {
        en: "This cannot be undone. You will be asked for a fresh code.",
        ar: "لا يمكن التراجع عن هذا الإجراء. سيُطلب منك رمز جديد.",
        de: "Das lässt sich nicht rückgängig machen. Du wirst nach einem neuen Code gefragt.",
    };
    pub const WITHIN: Text = Text {
        en: "Press Confirm within 10 minutes.",
        ar: "اضغط «تأكيد» خلال 10 دقائق.",
        de: "Drücke innerhalb von 10 Minuten auf „Bestätigen“.",
    };
}

// ---- handlers ----

/// `/desk me`: who the presser is on the desk, their session, and the
/// areas and commands they may use. Their own data only.
fn me<'a>(
    db: &'a Database,
    staff: &'a StaffContext,
    input: &'a DeskInput,
) -> BoxFuture<'a, Result<DeskView>> {
    Box::pin(async move {
        let mut record = staff.record("staff.desk.me", None);
        record.summary = Some("desk me".to_string());
        record_platform_audit(db, record, AuditOutcome::Success).await?;

        let lang = input.lang;
        let areas = usable_areas(&input.areas, staff);
        let roles: Vec<&str> = staff.grant.roles.iter().map(|r| r.name()).collect();
        let minutes = (STAFF_SESSION_IDLE_MS / 60_000).to_string();
        let mut view = DeskView::new(texts::DESK.get(lang))
            .field(
                texts::USER.get(lang),
                format!("{}#{}", staff.user.username, staff.user.discriminator),
            )
            .field(texts::ROLES.get(lang), roles.join(", "))
            .field(
                texts::PERMISSIONS.get(lang),
                staff.permissions.len().to_string(),
            )
            .field(
                texts::SESSION_ENDS.get(lang),
                when(staff.session.expires_at),
            )
            .field(
                texts::IDLE.get(lang),
                texts::MINUTES.fill(lang, &[("n", &minutes)]),
            )
            .field(
                texts::STEP_UP.get(lang),
                staff
                    .session
                    .step_up_until
                    .filter(|until| *until > staff.now)
                    .map(|until| texts::UNTIL.fill(lang, &[("time", &when(until))]))
                    .unwrap_or_else(|| texts::NOT_ACTIVE.get(lang).to_string()),
            );
        if let Some(end) = staff.grant.expires_at {
            view = view.field(texts::ACCESS_ENDS.get(lang), when(end));
        }
        view = view.field(
            texts::AREAS.get(lang),
            if areas.is_empty() {
                texts::NONE.get(lang).to_string()
            } else {
                areas.join(", ")
            },
        );
        for entry in entries() {
            let usable = entry
                .area
                .is_some_and(|area| areas.iter().any(|a| a == area))
                && entry.visible_to(staff);
            if usable {
                view = view.line(format!(
                    "/{} {}",
                    entry.command(),
                    entry.key().replace('.', " ")
                ));
            }
        }
        Ok(view)
    })
}

/// `/desk stats`: the console's platform statistics (`stats_core`)
fn stats<'a>(
    db: &'a Database,
    staff: &'a StaffContext,
    input: &'a DeskInput,
) -> BoxFuture<'a, Result<DeskView>> {
    Box::pin(async move {
        let lang = input.lang;
        let stats = stats_core(db, staff).await?;
        let window = |w: &super::super::platform::CountWindow| {
            texts::WINDOW.fill(
                lang,
                &[
                    ("total", &w.total.to_string()),
                    ("day", &w.last_24h.to_string()),
                    ("week", &w.last_7d.to_string()),
                    ("month", &w.last_30d.to_string()),
                ],
            )
        };
        let mut view = DeskView::new(texts::OVERVIEW.get(lang))
            .field(texts::USERS.get(lang), window(&stats.users))
            .field(
                texts::ONLINE.get(lang),
                stats
                    .online_users
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            )
            .field(texts::SERVERS.get(lang), window(&stats.servers))
            .field(texts::CHANNELS.get(lang), stats.channels_total.to_string())
            .field(
                texts::MESSAGES_HOUR.get(lang),
                stats.messages_last_hour.to_string(),
            )
            .field(
                texts::MESSAGES_DAY.get(lang),
                stats.messages_last_24h.to_string(),
            );
        if let Some(open) = stats.open_reports {
            view = view.field(texts::OPEN_REPORTS.get(lang), open.to_string());
        }
        Ok(view.line(texts::AS_OF.fill(lang, &[("time", &when(stats.generated_at))])))
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
            plugins: false,
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
            plugins: false,
            prepare: Some(prepare),
            handler: guarded,
        },
        // reading, behind a permission some roles lack (Support)
        DeskCommand {
            area: Some("overview"),
            group: Some("test"),
            name: "peek",
            description: Text {
                en: "A test reading for billing staff",
                ar: "قراءة تجريبية لفريق الفوترة",
                de: "Eine Testabfrage für das Abrechnungsteam",
            },
            options: &[],
            permission: Some(StaffPermission::BillingRead),
            changes: false,
            irreversible: false,
            plugins: false,
            prepare: None,
            handler: peek,
        },
    ];

    fn peek<'a>(
        _: &'a Database,
        _: &'a StaffContext,
        input: &'a DeskInput,
    ) -> BoxFuture<'a, Result<DeskView>> {
        Box::pin(async move { Ok(DeskView::new(TITLE.get(input.lang))) })
    }

    /// How many times a test change ran (each test is its own process)
    static RUNS: std::sync::Mutex<Option<std::collections::HashMap<String, usize>>> =
        std::sync::Mutex::new(None);

    /// How many times the test change ran for this pending action
    pub fn runs(pending: &str) -> usize {
        RUNS.lock()
            .unwrap()
            .as_ref()
            .and_then(|all| all.get(pending).copied())
            .unwrap_or(0)
    }

    const TITLE: Text = Text {
        en: "Test change",
        ar: "تغيير تجريبي",
        de: "Teständerung",
    };
    const WRITES: Text = Text {
        en: "Writes the note \"{note}\" about {user}.",
        ar: "يكتب الملاحظة «{note}» عن {user}.",
        de: "Schreibt die Notiz „{note}“ über {user}.",
    };
    const DONE: Text = Text {
        en: "Test change done",
        ar: "تم التغيير التجريبي",
        de: "Teständerung erledigt",
    };
    const NOTE: Text = Text {
        en: "Note",
        ar: "الملاحظة",
        de: "Notiz",
    };

    fn prepare<'a>(
        _: &'a Database,
        _: &'a StaffContext,
        input: &'a DeskInput,
    ) -> BoxFuture<'a, Result<Prepared>> {
        Box::pin(async move {
            let user = input.user("user").unwrap_or_default();
            let note = input.text("note").unwrap_or_default();
            Ok(Prepared {
                target_kind: Some("user".to_string()),
                target: Some(user.clone()),
                title: TITLE.get(input.lang).to_string(),
                lines: vec![WRITES.fill(input.lang, &[("note", &note), ("user", &user)])],
                ..Default::default()
            })
        })
    }

    fn done<'a>(
        _: &'a Database,
        _: &'a StaffContext,
        input: &'a DeskInput,
    ) -> BoxFuture<'a, Result<DeskView>> {
        Box::pin(async move {
            let note = input.text("note").unwrap_or_default();
            // a core that fails after the record was written
            if note == "fail" {
                return Err(revolt_result::create_error!(NotFound));
            }
            let key = input.pending.clone().unwrap_or_default();
            *RUNS
                .lock()
                .unwrap()
                .get_or_insert_with(Default::default)
                .entry(key)
                .or_insert(0) += 1;
            Ok(DeskView::new(DONE.get(input.lang))
                .field(
                    texts::USER.get(input.lang),
                    input.user("user").unwrap_or_default(),
                )
                .field(NOTE.get(input.lang), note))
        })
    }

    fn guarded<'a>(
        db: &'a Database,
        staff: &'a StaffContext,
        input: &'a DeskInput,
    ) -> BoxFuture<'a, Result<DeskView>> {
        Box::pin(async move {
            staff
                .require_step_up(db, StaffPermission::DashboardRead)
                .await?;
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
            assert!(
                entry.area.is_none_or(|a| AREAS.contains(&a)),
                "{}",
                entry.key()
            );
            assert_eq!(entry.changes, entry.prepare.is_some(), "{}", entry.key());
            assert!(!entry.irreversible || entry.changes, "{}", entry.key());
            // a change always names the permission the console needs for it
            assert!(
                !entry.changes || entry.permission.is_some(),
                "{}",
                entry.key()
            );
            assert!(
                SESSION_SUBS
                    .iter()
                    .all(|(name, _)| *name != entry.name || entry.group.is_some())
            );
            if let Some(group) = entry.group {
                assert!(
                    groups().any(|g| g.name == group),
                    "group {group} has no description"
                );
            }
        }
    }

    #[test]
    fn desk_lang_from_locale() {
        assert_eq!(Lang::from_locale(Some("ar-SY")), Lang::Ar);
        assert_eq!(Lang::from_locale(Some("de_AT")), Lang::De);
        assert_eq!(Lang::from_locale(Some("fr")), Lang::En);
        assert_eq!(Lang::from_locale(None), Lang::En);
        assert_eq!(texts::UNTIL.fill(Lang::De, &[("time", "x")]), "bis x");
        // one pass: a value holding a placeholder stays as it is
        assert_eq!(
            texts::UNTIL.fill(Lang::En, &[("time", "{time}")]),
            "until {time}"
        );
        assert_eq!(
            texts::WINDOW
                .fill(Lang::En, &[("total", "{day}"), ("day", "1")])
                .starts_with("{day} (+1 in 24 hours"),
            true
        );
        // an unknown or unclosed placeholder is kept
        let text = Text {
            en: "a {x} b {",
            ar: "",
            de: "",
        };
        assert_eq!(text.fill(Lang::En, &[]), "a {x} b {");
    }

    /// The tree passes the server's own command validation
    #[test]
    fn desk_tree_is_a_valid_command() {
        for tree in desk_tree(&all_on(), true) {
            let data: DataApplicationCommand =
                serde_json::from_value(serde_json::to_value(&tree).unwrap()).unwrap();
            data.validate_command()
                .unwrap_or_else(|e| panic!("{}: {e}", tree.name));
        }
    }
}
