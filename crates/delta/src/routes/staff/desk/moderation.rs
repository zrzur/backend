//! Desk moderation (area `moderation`): every command changes something,
//! so each one only makes a confirmation card at `/run`; the change runs on
//! its confirmation, through the console's own core (`*_core`), with the
//! console's checks, refusals and audit records.
//!
//! The card names exactly who or what is affected and what happens, in the
//! command's language. Values the server read for the card (a case's
//! version) are stored with the pending action: if the case changed since,
//! the console's version check refuses the confirmation.

use futures::future::BoxFuture;
use revolt_database::{Database, StaffPermission, User};
use revolt_models::v0::{
    CommandOptionType, DataSafetyAction, DataSafetyAppealDecision, DataSafetyCaseEdit,
    DataSafetyCaseStatus, DataSafetyNote, DataSafetyRevoke, InteractionDataOption,
    InteractionOptionValue, SafetyActionKind,
};
use revolt_result::{Result, create_error};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::super::bots::{DataDisableBot, disable_core};
use super::super::platform::revoke_user_sessions_core;
use super::super::safety::{
    DataUserAction, decide_core, edit_core, note_core, revoke_core, set_status_core, take_core,
    user_action_core,
};
use super::super::{DataReason, StaffContext};
use super::lookup::{Show, fields};
use super::registry::{
    DeskChoice, DeskCommand, DeskGroup, DeskInput, DeskOption, Lang, Prepare, Prepared, Text,
};
use super::view::DeskView;

const fn t(en: &'static str, ar: &'static str, de: &'static str) -> Text {
    Text { en, ar, de }
}

const fn show(key: &'static str, label: Text) -> Show {
    Show { key, label }
}

/// The name of the stored case version
const VERSION: &str = "_version";

// ---- words ----

mod w {
    use super::{Text, t};

    pub const DONE: Text = t("Done", "تم", "Erledigt");
    pub const ID: Text = t("ID", "المعرّف", "ID");
    pub const KIND: Text = t("Kind", "النوع", "Art");
    pub const STATUS: Text = t("Status", "الحالة", "Status");
    pub const USER: Text = t("User", "المستخدم", "Benutzer");
    pub const CREATED: Text = t("Created", "أُنشئ", "Erstellt");
    pub const ENDS: Text = t("Ends", "ينتهي", "Endet");
    pub const REFERENCE: Text = t("Reference", "المرجع", "Referenz");
    pub const PRIORITY: Text = t("Priority", "الأولوية", "Priorität");
    pub const ASSIGNEE: Text = t("Assigned to", "مُسند إلى", "Zugewiesen an");
    pub const HOLD: Text = t("Legal hold", "حجز قانوني", "Rechtliche Sperre");
    pub const SESSIONS_ENDED: Text = t("Sessions ended", "الجلسات المُنهاة", "Beendete Sitzungen");

    pub const T_WARN: Text = t("Warn a user", "تحذير مستخدم", "Benutzer verwarnen");
    pub const T_SUSPEND: Text = t("Suspend a user", "إيقاف مستخدم", "Benutzer sperren");
    pub const T_BAN: Text = t("Ban a user", "حظر مستخدم", "Benutzer bannen");
    pub const T_SESSIONS: Text = t(
        "End all sessions",
        "إنهاء كل الجلسات",
        "Alle Sitzungen beenden",
    );
    pub const T_STATUS: Text = t(
        "Change a report's status",
        "تغيير حالة بلاغ",
        "Status einer Meldung ändern",
    );
    pub const T_NOTE: Text = t(
        "Add an internal note",
        "إضافة ملاحظة داخلية",
        "Interne Notiz hinzufügen",
    );
    pub const T_ASSIGN: Text = t("Assign a report", "إسناد بلاغ", "Meldung zuweisen");
    pub const T_PRIORITY: Text = t(
        "Set a report's priority",
        "تحديد أولوية بلاغ",
        "Priorität einer Meldung setzen",
    );
    pub const T_HOLD: Text = t("Legal hold", "الحجز القانوني", "Rechtliche Sperre");
    pub const T_ACTION: Text = t(
        "Take action on a report",
        "اتخاذ إجراء في بلاغ",
        "Maßnahme zu einer Meldung",
    );
    pub const T_REVOKE: Text = t("Revoke an action", "إلغاء إجراء", "Maßnahme aufheben");
    pub const T_APPEAL: Text = t(
        "Decide an appeal",
        "البت في طعن",
        "Über einen Einspruch entscheiden",
    );
    pub const T_BOT: Text = t("Disable a bot", "تعطيل بوت", "Bot deaktivieren");

    pub const WARN: Text = t(
        "Warns {user} ({id}) for {category}.",
        "يحذّر {user} ({id}) بسبب {category}.",
        "Verwarnt {user} ({id}) wegen {category}.",
    );
    pub const SUSPEND: Text = t(
        "Suspends {user} ({id}) for {days} days for {category}; they cannot sign in until then.",
        "يوقف {user} ({id}) مدة {days} يومًا بسبب {category}؛ لا يستطيع تسجيل الدخول حتى ذلك الحين.",
        "Sperrt {user} ({id}) für {days} Tage wegen {category}; bis dahin keine Anmeldung möglich.",
    );
    pub const BAN: Text = t(
        "Bans {user} ({id}) for {category}; the account cannot be used until the ban is revoked.",
        "يحظر {user} ({id}) بسبب {category}؛ لا يمكن استخدام الحساب حتى يُلغى الحظر.",
        "Bannt {user} ({id}) wegen {category}; das Konto ist bis zur Aufhebung des Banns nicht nutzbar.",
    );
    pub const THEY_SEE: Text = t(
        "They see this reason: {reason}",
        "يرى السبب التالي: {reason}",
        "Sichtbare Begründung: {reason}",
    );
    pub const INTERNAL: Text = t(
        "Internal note: {note}",
        "ملاحظة داخلية: {note}",
        "Interne Notiz: {note}",
    );
    pub const APPEALABLE: Text = t(
        "A staff report is opened; they can appeal.",
        "يُفتح بلاغ من الإدارة؛ ويمكنه الطعن.",
        "Eine Team-Meldung wird eröffnet; Einspruch ist möglich.",
    );
    pub const SESSIONS: Text = t(
        "Signs {user} ({id}) out of all {count} sessions and ends their staff sessions.",
        "يسجّل خروج {user} ({id}) من جلساته كلها ({count}) وينهي جلسات الإدارة الخاصة به.",
        "Meldet {user} ({id}) aus allen {count} Sitzungen ab und beendet die Team-Sitzungen.",
    );
    pub const REASON: Text = t("Reason: {reason}", "السبب: {reason}", "Grund: {reason}");
    pub const STATUS_TO: Text = t(
        "Sets report {reference} ({id}) from {from} to {to}.",
        "يغيّر حالة البلاغ {reference} ({id}) من {from} إلى {to}.",
        "Setzt Meldung {reference} ({id}) von {from} auf {to}.",
    );
    pub const PUBLIC_NOTE: Text = t(
        "The people concerned see: {note}",
        "يرى المعنيون: {note}",
        "Die Beteiligten sehen: {note}",
    );
    pub const NOTE: Text = t(
        "Adds to report {reference} ({id}) the note, seen by staff only: {note}",
        "يضيف إلى البلاغ {reference} ({id}) ملاحظة لا يراها إلا فريق الإدارة: {note}",
        "Fügt Meldung {reference} ({id}) eine nur für das Team sichtbare Notiz hinzu: {note}",
    );
    pub const ASSIGN: Text = t(
        "Assigns report {reference} ({id}) to {user}.",
        "يُسند البلاغ {reference} ({id}) إلى {user}.",
        "Weist Meldung {reference} ({id}) {user} zu.",
    );
    pub const UNASSIGN: Text = t(
        "Removes the assignee of report {reference} ({id}).",
        "يزيل الإسناد عن البلاغ {reference} ({id}).",
        "Entfernt die Zuweisung von Meldung {reference} ({id}).",
    );
    pub const PRIORITY_TO: Text = t(
        "Sets the priority of report {reference} ({id}) from {from} to {to}.",
        "يغيّر أولوية البلاغ {reference} ({id}) من {from} إلى {to}.",
        "Setzt die Priorität von Meldung {reference} ({id}) von {from} auf {to}.",
    );
    pub const HOLD_ON: Text = t(
        "Puts report {reference} ({id}) on legal hold: it cannot be deleted.",
        "يضع البلاغ {reference} ({id}) تحت الحجز القانوني: لا يمكن حذفه.",
        "Stellt Meldung {reference} ({id}) unter rechtliche Sperre: sie kann nicht gelöscht werden.",
    );
    pub const HOLD_OFF: Text = t(
        "Lifts the legal hold on report {reference} ({id}).",
        "يرفع الحجز القانوني عن البلاغ {reference} ({id}).",
        "Hebt die rechtliche Sperre von Meldung {reference} ({id}) auf.",
    );
    pub const ACTION: Text = t(
        "Takes the action {kind} on report {reference} ({id}); it applies to {subject}.",
        "يتخذ الإجراء {kind} في البلاغ {reference} ({id})؛ ويقع على {subject}.",
        "Ergreift die Maßnahme {kind} zu Meldung {reference} ({id}); sie trifft {subject}.",
    );
    pub const FOR_DAYS: Text = t("For {days} days.", "لمدة {days} يومًا.", "Für {days} Tage.");
    pub const REVOKE: Text = t(
        "Revokes the action {kind} ({id}) on {subject}; its effect is reversed.",
        "يلغي الإجراء {kind} ({id}) على {subject}؛ ويُعكس أثره.",
        "Hebt die Maßnahme {kind} ({id}) gegen {subject} auf; ihre Wirkung wird zurückgenommen.",
    );
    pub const OVERTURN: Text = t(
        "Overturns appeal {id} by {user}: the action is reversed.",
        "يقبل طعن {user} رقم {id}: يُلغى الإجراء.",
        "Gibt dem Einspruch {id} von {user} statt: die Maßnahme wird aufgehoben.",
    );
    pub const UPHOLD: Text = t(
        "Upholds the action under appeal {id} by {user}: it stays in place.",
        "يرفض طعن {user} رقم {id}: يبقى الإجراء قائمًا.",
        "Weist den Einspruch {id} von {user} ab: die Maßnahme bleibt.",
    );
    pub const THEY_READ: Text = t(
        "They read this note: {note}",
        "يقرأ الملاحظة التالية: {note}",
        "Diese Notiz wird gelesen: {note}",
    );
    pub const BOT: Text = t(
        "Disables the bot {bot} ({id}) of {owner}: it cannot sign in and is unlisted.",
        "يعطّل البوت {bot} ({id}) التابع لـ {owner}: لا يستطيع تسجيل الدخول ويُزال من القوائم.",
        "Deaktiviert den Bot {bot} ({id}) von {owner}: er kann sich nicht anmelden und wird ausgeblendet.",
    );
    pub const NOBODY: Text = t("nobody", "لا أحد", "niemanden");
    pub const APPEALED: Text = t(
        "The appealed action: {kind} ({action}) on {subject}.",
        "الإجراء المطعون فيه: {kind} ({action}) على {subject}.",
        "Die angefochtene Maßnahme: {kind} ({action}) gegen {subject}.",
    );
    pub const OTHER_SUBJECT: &str = "the report now concerns another account";
}

// ---- helpers ----

fn handle(user: &User) -> String {
    revolt_models::v0::user_handle(&user.username, &user.discriminator)
}

/// An enum value from its name (the option's fixed choice)
fn parse<T: DeserializeOwned>(name: &str) -> Result<T> {
    serde_json::from_value(Value::String(name.to_string()))
        .map_err(|_| create_error!(InvalidOperation))
}

fn required(input: &DeskInput, name: &str) -> Result<String> {
    input
        .text(name)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| create_error!(InvalidOperation))
}

fn required_user(input: &DeskInput, name: &str) -> Result<String> {
    input
        .user(name)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| create_error!(InvalidOperation))
}

/// The case's version, stored with the card
fn version_option(version: i64) -> InteractionDataOption {
    InteractionDataOption {
        name: VERSION.to_string(),
        kind: CommandOptionType::Integer,
        value: Some(InteractionOptionValue::Integer(version)),
        options: None,
    }
}

fn version(input: &DeskInput) -> Result<i64> {
    input
        .integer(VERSION)
        .ok_or_else(|| create_error!(InvalidOperation))
}

fn json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn done(value: Value, shows: &[Show], lang: Lang) -> DeskView {
    fields(DeskView::new(w::DONE.get(lang)), &value, shows, lang)
}

const ACTION_DONE: &[Show] = &[
    show("/kind", w::KIND),
    show("@id", w::ID),
    show("/subject_user_id", w::USER),
    show("/created_at", w::CREATED),
    show("/expires_at", w::ENDS),
];

const CASE_DONE: &[Show] = &[
    show("/reference", w::REFERENCE),
    show("@id", w::ID),
    show("/status", w::STATUS),
    show("/priority", w::PRIORITY),
    show("/assignee_id", w::ASSIGNEE),
    show("/legal_hold", w::HOLD),
];

const APPEAL_DONE: &[Show] = &[
    show("@id", w::ID),
    show("/status", w::STATUS),
    show("/user_id", w::USER),
];

macro_rules! boxed {
    ($name:ident, $inner:ident, $out:ty) => {
        fn $name<'a>(
            db: &'a Database,
            staff: &'a StaffContext,
            input: &'a DeskInput,
        ) -> BoxFuture<'a, Result<$out>> {
            Box::pin($inner(db, staff, input))
        }
    };
}

fn prepared(
    target_kind: &str,
    target: &str,
    title: Text,
    lang: Lang,
    lines: Vec<String>,
) -> Prepared {
    Prepared {
        target_kind: Some(target_kind.to_string()),
        target: Some(target.to_string()),
        title: title.get(lang).to_string(),
        lines,
        ..Default::default()
    }
}

/// A fixed choice's name in `lang`; the raw value when it has none
fn named(option: &DeskOption, value: &str, lang: Lang) -> String {
    option
        .choices
        .iter()
        .find(|choice| choice.value == value)
        .map(|choice| choice.name.get(lang).to_string())
        .unwrap_or_else(|| value.to_string())
}

/// The console's refusals that cost nothing, before a card is made: not
/// yourself, not a bot, not staff or a privileged account
async fn refuse_protected(db: &Database, staff: &StaffContext, user: &User) -> Result<()> {
    if user.id == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let staff_member = db
        .fetch_staff_grant(&user.id)
        .await?
        .is_some_and(|grant| grant.is_active(staff.now));
    if user.privileged || staff_member {
        return Err(create_error!(NotPrivileged));
    }
    Ok(())
}

// ---- users ----

/// The lines of a warn, suspend or ban card
async fn user_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
    kind: SafetyActionKind,
) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    let user = db.fetch_user(&id).await?;
    refuse_protected(db, staff, &user).await?;
    let category = named(&CATEGORY, &required(input, "category")?, lang);
    let reason = required(input, "reason")?;
    let (title, text) = match kind {
        SafetyActionKind::Warning => (w::T_WARN, w::WARN),
        SafetyActionKind::SuspendAccount => (w::T_SUSPEND, w::SUSPEND),
        _ => (w::T_BAN, w::BAN),
    };
    let days = input.integer("days").unwrap_or_default().to_string();
    let mut lines = vec![
        text.fill(
            lang,
            &[
                ("user", &handle(&user)),
                ("id", &user.id),
                ("category", &category),
                ("days", &days),
            ],
        ),
        w::THEY_SEE.fill(lang, &[("reason", &reason)]),
    ];
    if let Some(note) = input.text("note").filter(|n| !n.is_empty()) {
        lines.push(w::INTERNAL.fill(lang, &[("note", &note)]));
    }
    lines.push(w::APPEALABLE.get(lang).to_string());
    Ok(prepared("user", &user.id, title, lang, lines))
}

async fn user_act(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
    kind: SafetyActionKind,
) -> Result<DeskView> {
    let id = required_user(input, "user")?;
    let data = DataUserAction {
        kind,
        category: parse(&required(input, "category")?)?,
        public_reason: required(input, "reason")?,
        internal_note: input.text("note").filter(|n| !n.is_empty()),
        duration_days: match kind {
            SafetyActionKind::SuspendAccount => {
                input.integer("days").and_then(|d| u32::try_from(d).ok())
            }
            _ => None,
        },
    };
    let action = user_action_core(db, staff, &id, data).await?;
    Ok(done(json(&action), ACTION_DONE, input.lang))
}

async fn warn_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    user_card(db, staff, input, SafetyActionKind::Warning).await
}
async fn warn_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    user_act(db, staff, input, SafetyActionKind::Warning).await
}
async fn suspend_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    user_card(db, staff, input, SafetyActionKind::SuspendAccount).await
}
async fn suspend_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    user_act(db, staff, input, SafetyActionKind::SuspendAccount).await
}
async fn ban_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    user_card(db, staff, input, SafetyActionKind::BanAccount).await
}
async fn ban_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    user_act(db, staff, input, SafetyActionKind::BanAccount).await
}
boxed!(user_warn_card, warn_card, Prepared);
boxed!(user_warn, warn_run, DeskView);
boxed!(user_suspend_card, suspend_card, Prepared);
boxed!(user_suspend, suspend_run, DeskView);
boxed!(user_ban_card, ban_card, Prepared);
boxed!(user_ban, ban_run, DeskView);

async fn sessions_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    let user = db.fetch_user(&id).await?;
    // the console's refusals: no bots; an Owner's sessions only by an Owner
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let owner = db
        .fetch_staff_grant(&user.id)
        .await?
        .is_some_and(|grant| grant.is_owner(staff.now));
    if owner && !staff.grant.is_owner(staff.now) {
        return Err(create_error!(NotPrivileged));
    }
    let count = db
        .fetch_sessions(&user.id)
        .await
        .map(|s| s.len())
        .unwrap_or(0)
        .to_string();
    let reason = required(input, "reason")?;
    Ok(prepared(
        "user",
        &user.id,
        w::T_SESSIONS,
        lang,
        vec![
            w::SESSIONS.fill(
                lang,
                &[
                    ("user", &handle(&user)),
                    ("id", &user.id),
                    ("count", &count),
                ],
            ),
            w::REASON.fill(lang, &[("reason", &reason)]),
        ],
    ))
}

async fn sessions_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required_user(input, "user")?;
    let reason = required(input, "reason")?;
    let count = revoke_user_sessions_core(db, staff, &id, DataReason { reason }).await?;
    Ok(DeskView::new(w::DONE.get(input.lang))
        .field(w::SESSIONS_ENDED.get(input.lang), count.to_string()))
}
boxed!(user_sessions_card, sessions_card, Prepared);
boxed!(user_sessions, sessions_run, DeskView);

// ---- cases ----

/// The case, with the card's id line values and its version stored
async fn case_of(db: &Database, input: &DeskInput) -> Result<(String, Value)> {
    let id = required(input, "case")?;
    let case = db.fetch_safety_case(&id).await?;
    Ok((id, json(&case)))
}

fn case_prepared(id: &str, case: &Value, title: Text, lang: Lang, lines: Vec<String>) -> Prepared {
    let mut prepared = prepared("safety_case", id, title, lang, lines);
    prepared.stored = vec![version_option(case["version"].as_i64().unwrap_or_default())];
    prepared
}

fn reference(case: &Value) -> String {
    case["reference"].as_str().unwrap_or_default().to_string()
}

async fn status_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, case) = case_of(db, input).await?;
    let to = required(input, "status")?;
    let from = case["status"].as_str().unwrap_or_default().to_string();
    let mut lines = vec![w::STATUS_TO.fill(
        lang,
        &[
            ("reference", &reference(&case)),
            ("id", &id),
            ("from", &named(&STATUS, &from, lang)),
            ("to", &named(&STATUS, &to, lang)),
        ],
    )];
    if let Some(note) = input.text("public_note").filter(|n| !n.is_empty()) {
        lines.push(w::PUBLIC_NOTE.fill(lang, &[("note", &note)]));
    }
    if let Some(note) = input.text("note").filter(|n| !n.is_empty()) {
        lines.push(w::INTERNAL.fill(lang, &[("note", &note)]));
    }
    let mut prepared = case_prepared(&id, &case, w::T_STATUS, lang, lines);
    // the console's senior decisions (escalated cases, reopening decided
    // ones) need a step-up: asked for before anything is recorded
    let senior = match (
        parse::<revolt_models::v0::SafetyStatus>(&from),
        parse::<revolt_models::v0::SafetyStatus>(&to),
    ) {
        (Ok(from), Ok(to)) => crate::routes::safety::review::senior_transition(from, to),
        _ => false,
    };
    prepared.irreversible = senior;
    Ok(prepared)
}

async fn status_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "case")?;
    let data = DataSafetyCaseStatus {
        status: parse(&required(input, "status")?)?,
        public_note: input.text("public_note").filter(|n| !n.is_empty()),
        note: input.text("note").filter(|n| !n.is_empty()),
        version: version(input)?,
    };
    let case = set_status_core(db, staff, &id, data).await?;
    Ok(done(json(&case), CASE_DONE, input.lang))
}
boxed!(case_status_card, status_card, Prepared);
boxed!(case_status, status_run, DeskView);

async fn note_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, case) = case_of(db, input).await?;
    let text = required(input, "text")?;
    let line = w::NOTE.fill(
        lang,
        &[
            ("reference", &reference(&case)),
            ("id", &id),
            ("note", &text),
        ],
    );
    Ok(case_prepared(&id, &case, w::T_NOTE, lang, vec![line]))
}

async fn note_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "case")?;
    let case = note_core(
        db,
        staff,
        &id,
        DataSafetyNote {
            text: required(input, "text")?,
        },
    )
    .await?;
    Ok(done(json(&case), CASE_DONE, input.lang))
}
boxed!(case_note_card, note_card, Prepared);
boxed!(case_note, note_run, DeskView);

fn edit(version: i64) -> DataSafetyCaseEdit {
    DataSafetyCaseEdit {
        assignee_id: None,
        priority: None,
        duplicate_of: None,
        legal_hold: None,
        version,
    }
}

async fn assign_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, case) = case_of(db, input).await?;
    let line = match input.user("user").filter(|u| !u.is_empty()) {
        Some(user) => {
            let user = db.fetch_user(&user).await?;
            w::ASSIGN.fill(
                lang,
                &[
                    ("reference", &reference(&case)),
                    ("id", &id),
                    ("user", &handle(&user)),
                ],
            )
        }
        None => w::UNASSIGN.fill(lang, &[("reference", &reference(&case)), ("id", &id)]),
    };
    Ok(case_prepared(&id, &case, w::T_ASSIGN, lang, vec![line]))
}

async fn assign_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "case")?;
    let data = DataSafetyCaseEdit {
        // empty: nobody, as in the console
        assignee_id: Some(input.user("user").unwrap_or_default()),
        ..edit(version(input)?)
    };
    let case = edit_core(db, staff, &id, data).await?;
    Ok(done(json(&case), CASE_DONE, input.lang))
}
boxed!(case_assign_card, assign_card, Prepared);
boxed!(case_assign, assign_run, DeskView);

async fn priority_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, case) = case_of(db, input).await?;
    let to = required(input, "priority")?;
    let from = case["priority"].as_str().unwrap_or_default().to_string();
    let line = w::PRIORITY_TO.fill(
        lang,
        &[
            ("reference", &reference(&case)),
            ("id", &id),
            ("from", &named(&PRIORITY, &from, lang)),
            ("to", &named(&PRIORITY, &to, lang)),
        ],
    );
    Ok(case_prepared(&id, &case, w::T_PRIORITY, lang, vec![line]))
}

async fn priority_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "case")?;
    let data = DataSafetyCaseEdit {
        priority: Some(parse(&required(input, "priority")?)?),
        ..edit(version(input)?)
    };
    let case = edit_core(db, staff, &id, data).await?;
    Ok(done(json(&case), CASE_DONE, input.lang))
}
boxed!(case_priority_card, priority_card, Prepared);
boxed!(case_priority, priority_run, DeskView);

async fn hold_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, case) = case_of(db, input).await?;
    let on = input
        .boolean("on")
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let text = if on { w::HOLD_ON } else { w::HOLD_OFF };
    let line = text.fill(lang, &[("reference", &reference(&case)), ("id", &id)]);
    Ok(case_prepared(&id, &case, w::T_HOLD, lang, vec![line]))
}

async fn hold_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "case")?;
    let data = DataSafetyCaseEdit {
        legal_hold: Some(
            input
                .boolean("on")
                .ok_or_else(|| create_error!(InvalidOperation))?,
        ),
        ..edit(version(input)?)
    };
    let case = edit_core(db, staff, &id, data).await?;
    Ok(done(json(&case), CASE_DONE, input.lang))
}
boxed!(case_hold_card, hold_card, Prepared);
boxed!(case_hold, hold_run, DeskView);

fn affects_account(kind: SafetyActionKind) -> bool {
    matches!(
        kind,
        SafetyActionKind::SuspendAccount | SafetyActionKind::BanAccount
    )
}

/// The name of the stored account an action would concern
const SUBJECT: &str = "_subject";

/// The account an action on the case would concern, as `take_as` will
/// resolve it (read only)
async fn subject_of(db: &Database, id: &str) -> Result<Option<String>> {
    let case = db.fetch_safety_case(id).await?;
    Ok(crate::routes::safety::actions::resolved_owner(db, &case).await)
}

async fn action_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, case) = case_of(db, input).await?;
    let kind_name = required(input, "kind")?;
    let kind: SafetyActionKind = parse(&kind_name)?;
    let reason = required(input, "reason")?;
    // a suspension says for how long, like the console asks
    if kind == SafetyActionKind::SuspendAccount && input.integer("days").is_none() {
        return Err(create_error!(FailedValidation {
            error: "a suspension needs days".to_string()
        }));
    }
    // the account the action will really concern, resolved as `take_as`
    // does: a public report names nobody yet, an old one may name a bot
    let owner = subject_of(db, &id).await?;
    let subject = match &owner {
        Some(owner) => match db.fetch_user(owner).await {
            Ok(user) => format!("{} ({owner})", handle(&user)),
            Err(_) => owner.to_string(),
        },
        None => w::NOBODY.get(lang).to_string(),
    };
    let mut lines = vec![w::ACTION.fill(
        lang,
        &[
            ("kind", &named(&KIND, &kind_name, lang)),
            ("reference", &reference(&case)),
            ("id", &id),
            ("subject", &subject),
        ],
    )];
    if let Some(days) = input.integer("days") {
        lines.push(w::FOR_DAYS.fill(lang, &[("days", &days.to_string())]));
    }
    lines.push(w::THEY_SEE.fill(lang, &[("reason", &reason)]));
    if let Some(note) = input.text("note").filter(|n| !n.is_empty()) {
        lines.push(w::INTERNAL.fill(lang, &[("note", &note)]));
    }
    let mut prepared = case_prepared(&id, &case, w::T_ACTION, lang, lines);
    prepared.stored.push(InteractionDataOption {
        name: SUBJECT.to_string(),
        kind: CommandOptionType::String,
        value: Some(InteractionOptionValue::String(owner.unwrap_or_default())),
        options: None,
    });
    // a fresh code for: an account's suspension or ban, a removed message
    // (gone for good), disabling a bot (as `bot disable`), and anything on
    // an escalated case (a senior decision)
    let escalated = case["status"].as_str() == Some("Escalated");
    prepared.irreversible = affects_account(kind)
        || matches!(
            kind,
            SafetyActionKind::RemoveMessage | SafetyActionKind::DisableBot
        )
        || escalated;
    prepared.permanent = kind == SafetyActionKind::RemoveMessage;
    Ok(prepared)
}

/// Right before the action: the report still concerns the account the card
/// named
async fn action_check(
    db: &Database,
    _: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let id = required(input, "case")?;
    let stored = input.text(SUBJECT).unwrap_or_default();
    let now = subject_of(db, &id).await?.unwrap_or_default();
    Ok((stored != now).then_some(w::OTHER_SUBJECT))
}
boxed!(case_action_verify, action_check, Option<&'static str>);

async fn action_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "case")?;
    let data = DataSafetyAction {
        kind: parse(&required(input, "kind")?)?,
        category: None,
        public_reason: required(input, "reason")?,
        internal_note: input.text("note").filter(|n| !n.is_empty()),
        duration_days: input.integer("days").and_then(|d| u32::try_from(d).ok()),
        activity_value: None,
        reporter_note: None,
        version: version(input)?,
    };
    let action = take_core(db, staff, &id, data).await?;
    Ok(done(json(&action), ACTION_DONE, input.lang))
}
boxed!(case_action_card, action_card, Prepared);
boxed!(case_action, action_run, DeskView);

// ---- actions, appeals, bots ----

async fn revoke_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "action")?;
    let action = db.fetch_safety_action(&id).await?;
    let reason = required(input, "reason")?;
    let subject = match &action.subject_user_id {
        Some(user_id) => match db.fetch_user(user_id).await {
            Ok(user) => format!("{} ({user_id})", handle(&user)),
            Err(_) => user_id.clone(),
        },
        None => w::NOBODY.get(lang).to_string(),
    };
    let kind = json(&action.kind).as_str().unwrap_or_default().to_string();
    let mut prepared = prepared(
        "safety_action",
        &id,
        w::T_REVOKE,
        lang,
        vec![
            w::REVOKE.fill(
                lang,
                &[
                    ("kind", &named(&KIND, &kind, lang)),
                    ("id", &id),
                    ("subject", &subject),
                ],
            ),
            w::REASON.fill(lang, &[("reason", &reason)]),
        ],
    );
    // reversing an account action: the console asks for a step-up too
    prepared.irreversible = affects_account(action.kind);
    Ok(prepared)
}

async fn revoke_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "action")?;
    let action = revoke_core(
        db,
        staff,
        &id,
        DataSafetyRevoke {
            reason: required(input, "reason")?,
        },
    )
    .await?;
    Ok(done(json(&action), ACTION_DONE, input.lang))
}
boxed!(action_revoke_card, revoke_card, Prepared);
boxed!(action_revoke, revoke_run, DeskView);

async fn appeal_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "appeal")?;
    let appeal = db.fetch_safety_appeal(&id).await?;
    let user = match db.fetch_user(&appeal.user_id).await {
        Ok(user) => format!("{} ({})", handle(&user), user.id),
        Err(_) => appeal.user_id.clone(),
    };
    let overturn = required(input, "decision")? == "overturn";
    let note = required(input, "note")?;
    let text = if overturn { w::OVERTURN } else { w::UPHOLD };
    let action = db.fetch_safety_action(&appeal.action_id).await?;
    let kind = json(&action.kind).as_str().unwrap_or_default().to_string();
    let subject = match &action.subject_user_id {
        Some(subject) => match db.fetch_user(subject).await {
            Ok(person) => format!("{} ({subject})", handle(&person)),
            Err(_) => subject.clone(),
        },
        None => w::NOBODY.get(lang).to_string(),
    };
    Ok(prepared(
        "safety_appeal",
        &id,
        w::T_APPEAL,
        lang,
        vec![
            text.fill(lang, &[("id", &id), ("user", &user)]),
            w::APPEALED.fill(
                lang,
                &[
                    ("kind", &named(&KIND, &kind, lang)),
                    ("action", &action.id),
                    ("subject", &subject),
                ],
            ),
            w::THEY_READ.fill(lang, &[("note", &note)]),
        ],
    ))
}

async fn appeal_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "appeal")?;
    let data = DataSafetyAppealDecision {
        overturn: required(input, "decision")? == "overturn",
        note: required(input, "note")?,
    };
    let appeal = decide_core(db, staff, &id, data).await?;
    Ok(done(json(&appeal), APPEAL_DONE, input.lang))
}
boxed!(appeal_decide_card, appeal_card, Prepared);
boxed!(appeal_decide, appeal_run, DeskView);

async fn bot_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "bot")?;
    let bot = db.fetch_bot(&id).await?;
    // the console's refusal: not your own bot, not a staff member's
    let staff_owner = db
        .fetch_staff_grant(&bot.owner)
        .await?
        .is_some_and(|grant| grant.is_active(staff.now));
    if bot.owner == staff.user.id || staff_owner {
        return Err(create_error!(NotPrivileged));
    }
    let bot_user = db.fetch_user(&bot.id).await?;
    let owner = match db.fetch_user(&bot.owner).await {
        Ok(user) => format!("{} ({})", handle(&user), user.id),
        Err(_) => bot.owner.clone(),
    };
    let reason = required(input, "reason")?;
    let mut lines = vec![
        w::BOT.fill(
            lang,
            &[
                ("bot", &handle(&bot_user)),
                ("id", &bot.id),
                ("owner", &owner),
            ],
        ),
        w::THEY_SEE.fill(lang, &[("reason", &reason)]),
    ];
    if let Some(note) = input.text("note").filter(|n| !n.is_empty()) {
        lines.push(w::INTERNAL.fill(lang, &[("note", &note)]));
    }
    lines.push(w::APPEALABLE.get(lang).to_string());
    Ok(prepared("bot", &bot.id, w::T_BOT, lang, lines))
}

async fn bot_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "bot")?;
    let data = DataDisableBot {
        category: parse(&required(input, "category")?)?,
        public_reason: required(input, "reason")?,
        internal_note: input.text("note").filter(|n| !n.is_empty()),
    };
    let action = disable_core(db, staff, &id, data).await?;
    Ok(done(json(&action), ACTION_DONE, input.lang))
}
boxed!(bot_disable_card, bot_card, Prepared);
boxed!(bot_disable, bot_run, DeskView);

// ---- the registry entries ----

const fn opt(
    name: &'static str,
    kind: CommandOptionType,
    required: bool,
    description: Text,
) -> DeskOption {
    DeskOption {
        name,
        kind,
        required,
        description,
        choices: &[],
        max_length: None,
        min_value: None,
        max_value: None,
    }
}

const fn choice(
    value: &'static str,
    en: &'static str,
    ar: &'static str,
    de: &'static str,
) -> DeskChoice {
    DeskChoice {
        value,
        name: t(en, ar, de),
    }
}

const USER: DeskOption = opt(
    "user",
    CommandOptionType::User,
    true,
    t("The user", "المستخدم", "Der Benutzer"),
);
const REASON: DeskOption = DeskOption {
    max_length: Some(500),
    ..opt(
        "reason",
        CommandOptionType::String,
        true,
        t("Reason they see", "السبب الظاهر له", "Sichtbarer Grund"),
    )
};
const STAFF_REASON: DeskOption = DeskOption {
    max_length: Some(500),
    ..opt(
        "reason",
        CommandOptionType::String,
        true,
        t("Reason", "السبب", "Grund"),
    )
};
const NOTE: DeskOption = DeskOption {
    max_length: Some(500),
    ..opt(
        "note",
        CommandOptionType::String,
        false,
        t("Internal note", "ملاحظة داخلية", "Interne Notiz"),
    )
};
const DAYS: DeskOption = DeskOption {
    min_value: Some(1.0),
    max_value: Some(365.0),
    ..opt(
        "days",
        CommandOptionType::Integer,
        true,
        t("Days", "الأيام", "Tage"),
    )
};
const DAYS_OPTIONAL: DeskOption = DeskOption {
    min_value: Some(1.0),
    max_value: Some(365.0),
    ..opt(
        "days",
        CommandOptionType::Integer,
        false,
        t("Days", "الأيام", "Tage"),
    )
};
const fn id(name: &'static str) -> DeskOption {
    DeskOption {
        max_length: Some(64),
        ..opt(
            name,
            CommandOptionType::String,
            true,
            t("ID", "المعرّف", "ID"),
        )
    }
}

const CATEGORY: DeskOption = DeskOption {
    choices: &[
        choice("Harassment", "Harassment", "تحرش", "Belästigung"),
        choice(
            "HateOrThreats",
            "Hate or threats",
            "كراهية أو تهديد",
            "Hass oder Drohungen",
        ),
        choice("Spam", "Spam", "رسائل مزعجة", "Spam"),
        choice("ScamFraud", "Scam or fraud", "احتيال", "Betrug"),
        choice(
            "Impersonation",
            "Impersonation",
            "انتحال شخصية",
            "Identitätsdiebstahl",
        ),
        choice(
            "MaliciousLinks",
            "Malicious links",
            "روابط ضارة",
            "Schädliche Links",
        ),
        choice("Malware", "Malware", "برمجيات خبيثة", "Schadsoftware"),
        choice(
            "PrivacyViolation",
            "Privacy violation",
            "انتهاك الخصوصية",
            "Datenschutzverstoß",
        ),
        choice(
            "PersonalInformation",
            "Personal information",
            "معلومات شخصية",
            "Persönliche Daten",
        ),
        choice(
            "SexualContent",
            "Sexual content",
            "محتوى جنسي",
            "Sexuelle Inhalte",
        ),
        choice("SelfHarm", "Self-harm", "إيذاء النفس", "Selbstverletzung"),
        choice(
            "IllegalContent",
            "Illegal content",
            "محتوى غير قانوني",
            "Illegale Inhalte",
        ),
        choice(
            "BotAbuse",
            "Bot abuse",
            "إساءة استخدام بوت",
            "Bot-Missbrauch",
        ),
        choice(
            "MisleadingApplication",
            "Misleading app",
            "تطبيق مضلل",
            "Irreführende App",
        ),
        choice(
            "UnauthorisedContent",
            "Unauthorised content",
            "محتوى غير مصرح به",
            "Unerlaubte Inhalte",
        ),
        choice(
            "IntellectualProperty",
            "Intellectual property",
            "ملكية فكرية",
            "Geistiges Eigentum",
        ),
        choice("Underage", "Underage", "دون السن", "Minderjährig"),
        choice("Other", "Other", "أخرى", "Sonstiges"),
    ],
    ..opt(
        "category",
        CommandOptionType::String,
        true,
        t("Category", "الفئة", "Kategorie"),
    )
};

const STATUS: DeskOption = DeskOption {
    choices: &[
        choice("Queued", "Queued", "في الطابور", "In Warteschlange"),
        choice("UnderReview", "Under review", "قيد المراجعة", "In Prüfung"),
        choice(
            "MoreInfoRequired",
            "More info needed",
            "مطلوب معلومات",
            "Infos nötig",
        ),
        choice("Escalated", "Escalated", "مُصعّد", "Eskaliert"),
        choice("NoViolation", "No violation", "لا مخالفة", "Kein Verstoß"),
        choice("Duplicate", "Duplicate", "مكرر", "Duplikat"),
        choice("Closed", "Closed", "مغلق", "Geschlossen"),
    ],
    ..opt(
        "status",
        CommandOptionType::String,
        true,
        t("New status", "الحالة الجديدة", "Neuer Status"),
    )
};

const PRIORITY: DeskOption = DeskOption {
    choices: &[
        choice("Low", "Low", "منخفضة", "Niedrig"),
        choice("Normal", "Normal", "عادية", "Normal"),
        choice("High", "High", "عالية", "Hoch"),
        choice("Urgent", "Urgent", "عاجلة", "Dringend"),
    ],
    ..opt(
        "priority",
        CommandOptionType::String,
        true,
        t("Priority", "الأولوية", "Priorität"),
    )
};

const KIND: DeskOption = DeskOption {
    choices: &[
        choice("Warning", "Warning", "تحذير", "Verwarnung"),
        choice(
            "RemoveMessage",
            "Remove message",
            "حذف الرسالة",
            "Nachricht entfernen",
        ),
        choice(
            "SuspendAccount",
            "Suspend account",
            "إيقاف الحساب",
            "Konto sperren",
        ),
        choice("BanAccount", "Ban account", "حظر الحساب", "Konto bannen"),
        choice(
            "DisableBot",
            "Disable bot",
            "تعطيل البوت",
            "Bot deaktivieren",
        ),
        choice(
            "RemoveDiscoverListing",
            "Remove listing",
            "إزالة الإدراج",
            "Eintrag entfernen",
        ),
        choice(
            "SuspendCommunityItem",
            "Suspend design",
            "إيقاف التصميم",
            "Design sperren",
        ),
    ],
    ..opt(
        "kind",
        CommandOptionType::String,
        true,
        t("Action", "الإجراء", "Maßnahme"),
    )
};

const DECISION: DeskOption = DeskOption {
    choices: &[
        choice("overturn", "Overturn", "قبول الطعن", "Stattgeben"),
        choice("uphold", "Uphold", "رفض الطعن", "Abweisen"),
    ],
    ..opt(
        "decision",
        CommandOptionType::String,
        true,
        t("Decision", "القرار", "Entscheidung"),
    )
};

const fn change(
    group: &'static str,
    name: &'static str,
    description: Text,
    options: &'static [DeskOption],
    permission: StaffPermission,
    irreversible: bool,
    prepare: Prepare,
    handler: super::registry::Handler,
) -> DeskCommand {
    DeskCommand {
        area: Some("moderation"),
        group: Some(group),
        name,
        description,
        options,
        permission: Some(permission),
        changes: true,
        irreversible,
        plugins: false,
        prepare: Some(prepare),
        handler,
        verify: None,
    }
}

use StaffPermission as P;

pub static GROUPS: &[DeskGroup] = &[DeskGroup {
    name: "action",
    description: t("Actions taken", "الإجراءات المتخذة", "Ergriffene Maßnahmen"),
}];

pub static ENTRIES: &[DeskCommand] = &[
    change(
        "user",
        "warn",
        t("Warn a user", "تحذير مستخدم", "Verwarnen"),
        &[USER, CATEGORY, REASON, NOTE],
        P::ReportsResolve,
        false,
        user_warn_card,
        user_warn,
    ),
    change(
        "user",
        "suspend",
        t("Suspend a user", "إيقاف مستخدم", "Sperren"),
        &[USER, CATEGORY, DAYS, REASON, NOTE],
        P::UsersSuspend,
        true,
        user_suspend_card,
        user_suspend,
    ),
    change(
        "user",
        "ban",
        t("Ban a user", "حظر مستخدم", "Bannen"),
        &[USER, CATEGORY, REASON, NOTE],
        P::UsersSuspend,
        true,
        user_ban_card,
        user_ban,
    ),
    change(
        "user",
        "sessions-end",
        t("End all sessions", "إنهاء كل الجلسات", "Sitzungen beenden"),
        &[USER, STAFF_REASON],
        P::UsersSessionsRevoke,
        true,
        user_sessions_card,
        user_sessions,
    ),
    change(
        "case",
        "status",
        t("Change status", "تغيير الحالة", "Status ändern"),
        &[
            id("case"),
            STATUS,
            DeskOption {
                max_length: Some(500),
                ..opt(
                    "public_note",
                    CommandOptionType::String,
                    false,
                    t("Note they see", "ملاحظة ظاهرة", "Sichtbare Notiz"),
                )
            },
            NOTE,
        ],
        P::ReportsResolve,
        false,
        case_status_card,
        case_status,
    ),
    change(
        "case",
        "note",
        t("Add a note", "إضافة ملاحظة", "Notiz hinzufügen"),
        &[
            id("case"),
            DeskOption {
                max_length: Some(2000),
                ..opt(
                    "text",
                    CommandOptionType::String,
                    true,
                    t("Note", "الملاحظة", "Notiz"),
                )
            },
        ],
        P::ReportsResolve,
        false,
        case_note_card,
        case_note,
    ),
    change(
        "case",
        "assign",
        t("Assign", "إسناد", "Zuweisen"),
        &[
            id("case"),
            opt(
                "user",
                CommandOptionType::User,
                false,
                t(
                    "Staff member (none: nobody)",
                    "عضو الإدارة (فارغ: لا أحد)",
                    "Teammitglied (leer: niemand)",
                ),
            ),
        ],
        P::ReportsResolve,
        false,
        case_assign_card,
        case_assign,
    ),
    change(
        "case",
        "priority",
        t("Set priority", "تحديد الأولوية", "Priorität setzen"),
        &[id("case"), PRIORITY],
        P::ReportsResolve,
        false,
        case_priority_card,
        case_priority,
    ),
    change(
        "case",
        "hold",
        t("Legal hold", "الحجز القانوني", "Rechtliche Sperre"),
        &[
            id("case"),
            opt(
                "on",
                CommandOptionType::Boolean,
                true,
                t("On", "تفعيل", "An"),
            ),
        ],
        P::ReportsResolve,
        // a senior decision: a fresh step-up first
        true,
        case_hold_card,
        case_hold,
    ),
    DeskCommand {
        verify: Some(case_action_verify),
        ..change(
            "case",
            "action",
            t("Take action", "اتخاذ إجراء", "Maßnahme ergreifen"),
            &[id("case"), KIND, REASON, DAYS_OPTIONAL, NOTE],
            P::ReportsResolve,
            false,
            case_action_card,
            case_action,
        )
    },
    change(
        "action",
        "revoke",
        t("Revoke an action", "إلغاء إجراء", "Maßnahme aufheben"),
        &[id("action"), STAFF_REASON],
        P::ReportsResolve,
        false,
        action_revoke_card,
        action_revoke,
    ),
    change(
        "appeal",
        "decide",
        t("Decide an appeal", "البت في طعن", "Einspruch entscheiden"),
        &[
            id("appeal"),
            DECISION,
            DeskOption {
                max_length: Some(500),
                ..opt(
                    "note",
                    CommandOptionType::String,
                    true,
                    t("Note they read", "ملاحظة يقرؤها", "Notiz für die Person"),
                )
            },
        ],
        P::ReportsResolve,
        // a senior decision: a fresh step-up first
        true,
        appeal_decide_card,
        appeal_decide,
    ),
    change(
        "bot",
        "disable",
        t("Disable a bot", "تعطيل بوت", "Bot deaktivieren"),
        &[id("bot"), CATEGORY, REASON, NOTE],
        P::ReportsResolve,
        true,
        bot_disable_card,
        bot_disable,
    ),
];
