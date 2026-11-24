//! Desk support (area `support`, command `/desk-support`): tickets,
//! notices, account change requests, bug reviews and the volunteer team.
//! Changes make a confirmation card at `/run` and run on confirmation
//! through the console's own core; the team's reading commands answer at
//! once. Free text comes only from the stored command options, the core
//! applies the console's length limits, and the card shows the text as
//! written (escaped).

use futures::future::BoxFuture;
use revolt_database::{AccountChanges, Database, StaffPermission, User};
use revolt_models::v0::{
    CommandOptionType, DataBugReview, InteractionDataOption, InteractionOptionValue,
};
use revolt_result::{Result, create_error};
use serde_json::Value;

use super::super::StaffContext;
use super::super::change_requests::{
    DataChangeRequest, DataSecurityChange, cancel_core, create_core, security_core,
};
use super::super::review::bug_review_core;
use super::super::support::{
    DataNotice, DataStaffReply, DataTicketChange, DataWithdrawNotice, change_core, reply_core,
    send_core, withdraw_core,
};
use super::super::team::{
    DataAway, DataCounting, DataNote, DataRemoveVolunteer, away_core, confirm_core, counting_core,
    detail_core, identity_core, list_core, note_core, remove_core,
};
use super::lookup::{Show, fields, paged};
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

/// The name of the stored `updated_at` of a bug report
const UPDATED: &str = "_updated_at";

mod w {
    use super::{Text, t};

    pub const DONE: Text = t("Done", "تم", "Erledigt");
    pub const ID: Text = t("ID", "المعرّف", "ID");
    pub const STATUS: Text = t("Status", "الحالة", "Status");
    pub const SUBJECT: Text = t("Subject", "الموضوع", "Betreff");
    pub const ASSIGNEE: Text = t("Assigned to", "مُسند إلى", "Zugewiesen an");
    pub const MESSAGES: Text = t("Messages", "الرسائل", "Nachrichten");
    pub const TONE: Text = t("Tone", "النبرة", "Ton");
    pub const USER: Text = t("User", "المستخدم", "Benutzer");
    pub const LIVE: Text = t("Shown", "ظاهر", "Sichtbar");
    pub const ENDS: Text = t("Ends", "ينتهي", "Endet");
    pub const TITLE: Text = t("Title", "العنوان", "Titel");
    pub const NAME: Text = t("Name", "الاسم", "Name");
    pub const ROLE: Text = t("Role", "الدور", "Rolle");
    pub const SERVER: Text = t("Server", "السيرفر", "Server");
    pub const JOINED: Text = t("Joined", "انضم", "Beigetreten");
    pub const TRIAL_ENDS: Text = t("Trial ends", "تنتهي التجربة", "Probezeit endet");
    pub const AWAY_UNTIL: Text = t("Away until", "غائب حتى", "Abwesend bis");
    pub const THIS_MONTH: Text = t(
        "Contributions this month",
        "المساهمات هذا الشهر",
        "Beiträge diesen Monat",
    );
    pub const MINIMUM: Text = t(
        "Monthly minimum",
        "الحد الأدنى الشهري",
        "Monatliches Minimum",
    );
    pub const IDENTITY: Text = t("Identity on file", "هوية مسجلة", "Identität hinterlegt");
    pub const COUNTING_STOPPED: Text = t("Counting stopped", "العدّ متوقف", "Zählung gestoppt");
    pub const NOTE: Text = t("Staff note", "ملاحظة الإدارة", "Team-Notiz");
    pub const AGE: Text = t("Age", "العمر", "Alter");
    pub const COUNTRY: Text = t("Country", "البلد", "Land");
    pub const AGE_NOTE: Text = t("Age needs a look", "العمر يحتاج مراجعة", "Alter prüfen");

    pub const T_REPLY: Text = t("Answer a ticket", "الرد على تذكرة", "Ticket beantworten");
    pub const T_NOTE: Text = t(
        "Staff note on a ticket",
        "ملاحظة إدارية على تذكرة",
        "Team-Notiz zum Ticket",
    );
    pub const T_ASSIGN: Text = t("Assign a ticket", "إسناد تذكرة", "Ticket zuweisen");
    pub const T_CLOSE: Text = t("Close a ticket", "إغلاق تذكرة", "Ticket schließen");
    pub const T_REOPEN: Text = t("Reopen a ticket", "إعادة فتح تذكرة", "Ticket wieder öffnen");
    pub const T_NOTICE: Text = t("Send a notice", "إرسال إشعار", "Hinweis senden");
    pub const T_WITHDRAW: Text = t("Withdraw a notice", "سحب إشعار", "Hinweis zurückziehen");
    pub const T_CHANGE: Text = t(
        "Ask for a profile change",
        "طلب تعديل الملف الشخصي",
        "Profiländerung anfragen",
    );
    pub const T_CANCEL: Text = t(
        "Withdraw a change request",
        "سحب طلب تعديل",
        "Änderungsanfrage zurückziehen",
    );
    pub const T_SECURITY: Text = t(
        "Ask for a security change",
        "طلب تغيير أمني",
        "Sicherheitsänderung anfragen",
    );
    pub const T_BUG: Text = t(
        "Review a bug report",
        "مراجعة بلاغ خطأ",
        "Fehlerbericht prüfen",
    );
    pub const T_TEAM: Text = t("Volunteer team", "فريق المتطوعين", "Freiwilligenteam");
    pub const T_PERSON: Text = t("Volunteer", "متطوع", "Freiwillige Person");
    pub const T_IDENTITY: Text = t("Identity", "الهوية", "Identität");
    pub const T_CONFIRM: Text = t(
        "Confirm a volunteer",
        "تثبيت متطوع",
        "Freiwillige Person bestätigen",
    );
    pub const T_COUNTING: Text = t("Activity counting", "عدّ النشاط", "Aktivitätszählung");
    pub const T_AWAY: Text = t("Set away", "تسجيل غياب", "Abwesenheit setzen");
    pub const T_TEAM_NOTE: Text = t(
        "Staff note on a volunteer",
        "ملاحظة إدارية على متطوع",
        "Team-Notiz zur Person",
    );
    pub const T_REMOVE: Text = t(
        "Remove a volunteer",
        "إنهاء عضوية متطوع",
        "Freiwillige Person entfernen",
    );

    pub const REPLY: Text = t(
        "Sends {user} this answer on ticket \"{subject}\" ({id}), in their private messages from Zrzur's official account: {text}",
        "يرسل إلى {user} هذا الرد على التذكرة «{subject}» ({id}) في رسائله الخاصة من حساب زرزور الرسمي: {text}",
        "Schickt {user} diese Antwort auf das Ticket „{subject}“ ({id}) als private Nachricht vom offiziellen Zrzur-Konto: {text}",
    );
    pub const AND_CLOSE: Text = t(
        "The ticket is then closed.",
        "ثم تُغلق التذكرة.",
        "Danach wird das Ticket geschlossen.",
    );
    pub const STAFF_NOTE: Text = t(
        "Adds to ticket \"{subject}\" ({id}) a note seen by staff only: {text}",
        "يضيف إلى التذكرة «{subject}» ({id}) ملاحظة لا يراها إلا فريق الإدارة: {text}",
        "Fügt dem Ticket „{subject}“ ({id}) eine nur für das Team sichtbare Notiz hinzu: {text}",
    );
    pub const ASSIGN: Text = t(
        "Assigns ticket \"{subject}\" ({id}) to {user}.",
        "يُسند التذكرة «{subject}» ({id}) إلى {user}.",
        "Weist das Ticket „{subject}“ ({id}) {user} zu.",
    );
    pub const CLOSE: Text = t(
        "Closes ticket \"{subject}\" ({id}) of {user}.",
        "يغلق تذكرة {user} «{subject}» ({id}).",
        "Schließt das Ticket „{subject}“ ({id}) von {user}.",
    );
    pub const REOPEN: Text = t(
        "Reopens ticket \"{subject}\" ({id}) of {user}.",
        "يعيد فتح تذكرة {user} «{subject}» ({id}).",
        "Öffnet das Ticket „{subject}“ ({id}) von {user} wieder.",
    );
    pub const NOTICE_TO: Text = t(
        "Sends {user} this notice in their private messages from Zrzur's official account ({tone}):",
        "يرسل إلى {user} هذا الإشعار في رسائله الخاصة من حساب زرزور الرسمي ({tone}):",
        "Schickt {user} diesen Hinweis als private Nachricht vom offiziellen Zrzur-Konto ({tone}):",
    );
    pub const NOTICE_ALL: Text = t(
        "Shows this notice to everyone on Zrzur ({tone}):",
        "يعرض هذا الإشعار على كل مستخدمي زرزور ({tone}):",
        "Zeigt allen auf Zrzur diesen Hinweis ({tone}):",
    );
    pub const IN: Text = t(
        "{lang}: {title} — {body}",
        "{lang}: {title} — {body}",
        "{lang}: {title} — {body}",
    );
    pub const LINK: Text = t("Link: {link}", "الرابط: {link}", "Link: {link}");
    pub const FOR_DAYS: Text = t(
        "Shown for {days} days.",
        "يظهر مدة {days} يومًا.",
        "Sichtbar für {days} Tage.",
    );
    pub const BY_EMAIL: Text = t(
        "Also sent by email.",
        "يُرسل أيضًا بالبريد الإلكتروني.",
        "Wird auch per E-Mail geschickt.",
    );
    pub const WITHDRAW: Text = t(
        "Withdraws notice {id} ({who}); it stops showing in the app. An email already sent stays sent.",
        "يسحب الإشعار {id} ({who})؛ فيتوقف ظهوره في التطبيق. أما البريد المُرسل فيبقى.",
        "Zieht den Hinweis {id} ({who}) zurück; er wird in der App nicht mehr gezeigt. Eine schon gesendete E-Mail bleibt.",
    );
    pub const EVERYONE: Text = t("everyone", "الجميع", "alle");
    pub const REASON: Text = t("Reason: {reason}", "السبب: {reason}", "Grund: {reason}");
    pub const THEY_SEE: Text = t(
        "They see this reason: {reason}",
        "يرى السبب التالي: {reason}",
        "Sichtbare Begründung: {reason}",
    );
    pub const CHANGE: Text = t(
        "Asks {user} to agree to these profile changes; nothing changes unless they accept:",
        "يطلب من {user} الموافقة على تعديلات الملف الشخصي التالية؛ ولا يتغير شيء ما لم يوافق:",
        "Bittet {user}, diesen Profiländerungen zuzustimmen; ohne Zustimmung ändert sich nichts:",
    );
    pub const SET: Text = t("{field}: {value}", "{field}: {value}", "{field}: {value}");
    pub const CLEAR: Text = t("Clear: {field}", "مسح: {field}", "Leeren: {field}");
    pub const CANCEL: Text = t(
        "Withdraws change request {id} sent to {user}.",
        "يسحب طلب التعديل {id} المرسل إلى {user}.",
        "Zieht die Änderungsanfrage {id} an {user} zurück.",
    );
    pub const SECURITY_EMAIL: Text = t(
        "Emails {user} a link to change their sign-in email to {email}; nothing changes unless they confirm within 24 hours.",
        "يرسل إلى {user} رابطًا بالبريد لتغيير بريد تسجيل الدخول إلى {email}؛ ولا يتغير شيء ما لم يؤكد خلال 24 ساعة.",
        "Schickt {user} einen Link, um die Anmelde-E-Mail in {email} zu ändern; ohne Bestätigung binnen 24 Stunden ändert sich nichts.",
    );
    pub const SECURITY_TOTP: Text = t(
        "Emails {user} a link to turn off two-factor authentication; nothing changes unless they confirm within 24 hours.",
        "يرسل إلى {user} رابطًا بالبريد لإيقاف التحقق بخطوتين؛ ولا يتغير شيء ما لم يؤكد خلال 24 ساعة.",
        "Schickt {user} einen Link, um die Zwei-Faktor-Authentifizierung auszuschalten; ohne Bestätigung binnen 24 Stunden ändert sich nichts.",
    );
    pub const BUG: Text = t(
        "Reviews bug report \"{title}\" ({id}) by {user}: {action}.",
        "يراجع بلاغ الخطأ «{title}» ({id}) من {user}: {action}.",
        "Prüft den Fehlerbericht „{title}“ ({id}) von {user}: {action}.",
    );
    pub const HUNTER: Text = t(
        "Confirming or fixing gives the reporter the Bug Hunter badge.",
        "التأكيد أو الإصلاح يمنح المُبلّغ شارة صائد الأخطاء.",
        "Bestätigen oder Beheben gibt der meldenden Person das Bug-Hunter-Abzeichen.",
    );
    pub const SEVERITY: Text = t(
        "Severity: {severity}",
        "الخطورة: {severity}",
        "Schweregrad: {severity}",
    );
    pub const DUPLICATE_OF: Text = t("Duplicate of: {id}", "مكرر لـ: {id}", "Duplikat von: {id}");
    pub const NO_END: Text = t("No end date.", "بلا تاريخ انتهاء.", "Ohne Enddatum.");
    pub const TICKET_CHANGED: &str = "the ticket changed since the card";
    pub const NOT_SEEN: &str = "desk: a report this person may not see";
    pub const NOTE_TO: Text = t(
        "Note they read: {note}",
        "ملاحظة يقرؤها: {note}",
        "Notiz für die Person: {note}",
    );
    pub const CONFIRM: Text = t(
        "Confirms {user} ({id}) as a volunteer: the trial ends now.",
        "يثبّت {user} ({id}) متطوعًا: تنتهي فترة التجربة الآن.",
        "Bestätigt {user} ({id}) als freiwillige Person: die Probezeit endet jetzt.",
    );
    pub const COUNTING_OFF: Text = t(
        "Stops counting the activity of {user} ({id}) after their objection, in every current membership.",
        "يوقف عدّ نشاط {user} ({id}) بعد اعتراضه، في كل عضوياته الحالية.",
        "Stoppt nach dem Widerspruch die Aktivitätszählung von {user} ({id}) in allen aktuellen Mitgliedschaften.",
    );
    pub const COUNTING_ON: Text = t(
        "Starts counting the activity of {user} ({id}) again, from tomorrow.",
        "يستأنف عدّ نشاط {user} ({id}) ابتداءً من الغد.",
        "Zählt die Aktivität von {user} ({id}) ab morgen wieder.",
    );
    pub const AWAY: Text = t(
        "Sets {user} ({id}) away for {days} days; they are told until when.",
        "يسجّل غياب {user} ({id}) مدة {days} يومًا؛ ويُبلّغ بموعد العودة.",
        "Setzt {user} ({id}) für {days} Tage abwesend; die Person erfährt bis wann.",
    );
    pub const BACK: Text = t(
        "Brings {user} ({id}) back from being away; they are told.",
        "يعيد {user} ({id}) من الغياب؛ ويُبلّغ بذلك.",
        "Holt {user} ({id}) aus der Abwesenheit zurück; die Person erfährt es.",
    );
    pub const TEAM_NOTE: Text = t(
        "Sets the staff note on {user} ({id}) to: {text}",
        "يجعل ملاحظة الإدارة على {user} ({id}): {text}",
        "Setzt die Team-Notiz zu {user} ({id}) auf: {text}",
    );
    pub const TEAM_NOTE_CLEAR: Text = t(
        "Clears the staff note on {user} ({id}).",
        "يمسح ملاحظة الإدارة على {user} ({id}).",
        "Löscht die Team-Notiz zu {user} ({id}).",
    );
    pub const REMOVE: Text = t(
        "Ends the membership of {user} ({id}); they get the official message with this reason: {reason}",
        "ينهي عضوية {user} ({id})؛ ويصله الإشعار الرسمي مع هذا السبب: {reason}",
        "Beendet die Mitgliedschaft von {user} ({id}); die Person erhält die offizielle Nachricht mit diesem Grund: {reason}",
    );
}

// ---- helpers ----

fn handle(user: &User) -> String {
    revolt_models::v0::user_handle(&user.username, &user.discriminator)
}

async fn person(db: &Database, id: &str) -> String {
    match db.fetch_user(id).await {
        Ok(user) => format!("{} ({})", handle(&user), user.id),
        Err(_) => id.to_string(),
    }
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

fn optional(input: &DeskInput, name: &str) -> Option<String> {
    input.text(name).filter(|v| !v.is_empty())
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

/// Not about yourself, not about a bot: refused before a card is made
async fn about_a_person(db: &Database, staff: &StaffContext, id: &str) -> Result<()> {
    if id == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if db.fetch_user(id).await?.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    Ok(())
}

/// The name of a stored `updated_at`
const TICKET_UPDATED: &str = "_ticket_updated_at";

fn json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn done(value: Value, shows: &[Show], lang: Lang) -> DeskView {
    fields(DeskView::new(w::DONE.get(lang)), &value, shows, lang)
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

const TICKET_DONE: &[Show] = &[
    show("/subject", w::SUBJECT),
    show("@id", w::ID),
    show("/status", w::STATUS),
    show("/assignee", w::ASSIGNEE),
    show("/message_count", w::MESSAGES),
];

const NOTICE_DONE: &[Show] = &[
    show("@id", w::ID),
    show("/tone", w::TONE),
    show("/user", w::USER),
    show("/live", w::LIVE),
    show("/expires_at", w::ENDS),
];

const REQUEST_DONE: &[Show] = &[
    show("@id", w::ID),
    show("/status", w::STATUS),
    show("/expires_at", w::ENDS),
];

const BUG_DONE: &[Show] = &[
    show("/report/title", w::TITLE),
    show("/report/_id", w::ID),
    show("/report/status", w::STATUS),
];

pub const TEAM_ROW: &[Show] = &[
    show("/account_name", w::NAME),
    show("/role_name", w::ROLE),
    show("/status", w::STATUS),
    show("/member_id", w::ID),
];

pub const TEAM_VIEW: &[Show] = &[
    show("/row/account_name", w::NAME),
    show("/row/member_id", w::ID),
    show("/row/role_name", w::ROLE),
    show("/row/server_name", w::SERVER),
    show("/row/status", w::STATUS),
    show("/row/joined_at", w::JOINED),
    show("/row/trial_ends_at", w::TRIAL_ENDS),
    show("/row/away_until", w::AWAY_UNTIL),
    show("/row/month_contributions", w::THIS_MONTH),
    show("/monthly_minimum", w::MINIMUM),
    show("/row/has_identity", w::IDENTITY),
    show("/counting_stopped", w::COUNTING_STOPPED),
    show("/notes", w::NOTE),
];

pub const IDENTITY_VIEW: &[Show] = &[
    show("/name", w::NAME),
    show("/age", w::AGE),
    show("/country", w::COUNTRY),
    show("/age_note", w::AGE_NOTE),
];

// ---- tickets ----

async fn ticket_of(db: &Database, input: &DeskInput) -> Result<(String, String, String)> {
    let id = required(input, "ticket")?;
    let ticket = db.fetch_support_ticket(&id).await?;
    let user = person(db, &ticket.user_id).await;
    Ok((id, ticket.subject.clone(), user))
}

/// A ticket card: the ticket as read, stored; nobody answers their own
async fn ticket_prepared(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
    title: Text,
    lines: Vec<String>,
    own_refused: bool,
) -> Result<Prepared> {
    let id = required(input, "ticket")?;
    let ticket = db.fetch_support_ticket(&id).await?;
    if own_refused && ticket.user_id == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let mut prepared = prepared("support_ticket", &id, title, input.lang, lines);
    prepared.stored = vec![InteractionDataOption {
        name: TICKET_UPDATED.to_string(),
        kind: CommandOptionType::Integer,
        value: Some(InteractionOptionValue::Integer(ticket.updated_at)),
        options: None,
    }];
    Ok(prepared)
}

/// Right before the change: the ticket is still as the card showed it
async fn ticket_check(
    db: &Database,
    _: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let ticket = db.fetch_support_ticket(&required(input, "ticket")?).await?;
    Ok((input.integer(TICKET_UPDATED) != Some(ticket.updated_at)).then_some(w::TICKET_CHANGED))
}
boxed!(ticket_verify, ticket_check, Option<&'static str>);

async fn reply_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, subject, user) = ticket_of(db, input).await?;
    let text = required(input, "text")?;
    let mut lines = vec![w::REPLY.fill(
        lang,
        &[
            ("user", &user),
            ("subject", &subject),
            ("id", &id),
            ("text", &text),
        ],
    )];
    if input.boolean("close") == Some(true) {
        lines.push(w::AND_CLOSE.get(lang).to_string());
    }
    ticket_prepared(db, staff, input, w::T_REPLY, lines, true).await
}

async fn reply_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "ticket")?;
    let data = DataStaffReply {
        body: required(input, "text")?,
        internal: false,
        close: input.boolean("close") == Some(true),
    };
    let ticket = reply_core(db, staff, &id, data).await?;
    Ok(done(json(&ticket), TICKET_DONE, input.lang))
}
boxed!(ticket_reply_card, reply_card, Prepared);
boxed!(ticket_reply, reply_run, DeskView);

async fn ticket_note_card_fn(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Prepared> {
    let lang = input.lang;
    let (id, subject, _) = ticket_of(db, input).await?;
    let text = required(input, "text")?;
    let line = w::STAFF_NOTE.fill(lang, &[("subject", &subject), ("id", &id), ("text", &text)]);
    ticket_prepared(db, staff, input, w::T_NOTE, vec![line], true).await
}

async fn ticket_note_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let id = required(input, "ticket")?;
    let data = DataStaffReply {
        body: required(input, "text")?,
        internal: true,
        close: false,
    };
    let ticket = reply_core(db, staff, &id, data).await?;
    Ok(done(json(&ticket), TICKET_DONE, input.lang))
}
boxed!(ticket_note_card, ticket_note_card_fn, Prepared);
boxed!(ticket_note, ticket_note_run, DeskView);

async fn assign_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, subject, _) = ticket_of(db, input).await?;
    // nobody named: yourself, as in the console
    let to = input
        .user("user")
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| staff.user.id.clone());
    let line = w::ASSIGN.fill(
        lang,
        &[
            ("subject", &subject),
            ("id", &id),
            ("user", &person(db, &to).await),
        ],
    );
    ticket_prepared(db, staff, input, w::T_ASSIGN, vec![line], false).await
}

async fn ticket_change(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
    action: &str,
    to: Option<String>,
) -> Result<DeskView> {
    let id = required(input, "ticket")?;
    let data = DataTicketChange {
        action: action.to_string(),
        to,
    };
    let ticket = change_core(db, staff, &id, data).await?;
    Ok(done(json(&ticket), TICKET_DONE, input.lang))
}

async fn assign_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    ticket_change(
        db,
        staff,
        input,
        "assign",
        input.user("user").filter(|u| !u.is_empty()),
    )
    .await
}
boxed!(ticket_assign_card, assign_card, Prepared);
boxed!(ticket_assign, assign_run, DeskView);

async fn close_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, subject, user) = ticket_of(db, input).await?;
    let line = w::CLOSE.fill(lang, &[("subject", &subject), ("id", &id), ("user", &user)]);
    ticket_prepared(db, staff, input, w::T_CLOSE, vec![line], false).await
}
async fn close_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    ticket_change(db, staff, input, "close", None).await
}
boxed!(ticket_close_card, close_card, Prepared);
boxed!(ticket_close, close_run, DeskView);

async fn reopen_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, subject, user) = ticket_of(db, input).await?;
    let line = w::REOPEN.fill(lang, &[("subject", &subject), ("id", &id), ("user", &user)]);
    ticket_prepared(db, staff, input, w::T_REOPEN, vec![line], false).await
}
async fn reopen_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    ticket_change(db, staff, input, "reopen", None).await
}
boxed!(ticket_reopen_card, reopen_card, Prepared);
boxed!(ticket_reopen, reopen_run, DeskView);

// ---- notices ----

const LANGS: [&str; 3] = ["en", "ar", "de"];

/// The notice as asked: titles and texts by language
fn notice_data(input: &DeskInput, user: Option<String>) -> Result<DataNotice> {
    let mut title = std::collections::HashMap::new();
    let mut body = std::collections::HashMap::new();
    for lang in LANGS {
        title.insert(lang.to_string(), required(input, &format!("title_{lang}"))?);
        body.insert(lang.to_string(), required(input, &format!("text_{lang}"))?);
    }
    Ok(DataNotice {
        user,
        title,
        body,
        link: optional(input, "link"),
        tone: required(input, "tone")?,
        expires_in_days: input.integer("days").and_then(|d| u32::try_from(d).ok()),
        email: input.boolean("email") == Some(true),
    })
}

fn notice_lines(lang: Lang, head: String, data: &DataNotice) -> Vec<String> {
    let mut lines = vec![head];
    for code in LANGS {
        lines.push(w::IN.fill(
            lang,
            &[
                ("lang", code),
                ("title", &data.title[code]),
                ("body", &data.body[code]),
            ],
        ));
    }
    if let Some(link) = &data.link {
        lines.push(w::LINK.fill(lang, &[("link", link)]));
    }
    match data.expires_in_days {
        Some(days) => lines.push(w::FOR_DAYS.fill(lang, &[("days", &days.to_string())])),
        None => lines.push(w::NO_END.get(lang).to_string()),
    }
    if data.email {
        lines.push(w::BY_EMAIL.get(lang).to_string());
    }
    lines
}

async fn send_card(db: &Database, _staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    if db.fetch_user(&user).await?.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let data = notice_data(input, Some(user.clone()))?;
    let head = w::NOTICE_TO.fill(
        lang,
        &[
            ("user", &person(db, &user).await),
            ("tone", &named(&TONE, &data.tone, lang)),
        ],
    );
    Ok(prepared(
        "user",
        &user,
        w::T_NOTICE,
        lang,
        notice_lines(lang, head, &data),
    ))
}

async fn send_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let user = required_user(input, "user")?;
    let notice = send_core(db, staff, notice_data(input, Some(user))?).await?;
    Ok(done(json(&notice), NOTICE_DONE, input.lang))
}
boxed!(notice_send_card, send_card, Prepared);
boxed!(notice_send, send_run, DeskView);

async fn everyone_card(_: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let data = notice_data(input, None)?;
    let head = w::NOTICE_ALL.fill(lang, &[("tone", &named(&TONE, &data.tone, lang))]);
    Ok(Prepared {
        target_kind: Some("platform".to_string()),
        target: None,
        title: w::T_NOTICE.get(lang).to_string(),
        lines: notice_lines(lang, head, &data),
        ..Default::default()
    })
}

async fn everyone_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let notice = send_core(db, staff, notice_data(input, None)?).await?;
    Ok(done(json(&notice), NOTICE_DONE, input.lang))
}
boxed!(notice_everyone_card, everyone_card, Prepared);
boxed!(notice_everyone, everyone_run, DeskView);

async fn withdraw_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "notice")?;
    let notice = db.fetch_platform_notice(&id).await?;
    let who = match &notice.user_id {
        Some(user) => person(db, user).await,
        None => {
            // a notice to everyone is the settings team's, as in the console
            staff
                .require_protective(db, StaffPermission::SettingsManage)
                .await?;
            w::EVERYONE.get(lang).to_string()
        }
    };
    let reason = required(input, "reason")?;
    Ok(prepared(
        "notice",
        &id,
        w::T_WITHDRAW,
        lang,
        vec![
            w::WITHDRAW.fill(lang, &[("id", &id), ("who", &who)]),
            w::REASON.fill(lang, &[("reason", &reason)]),
        ],
    ))
}

async fn withdraw_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "notice")?;
    withdraw_core(
        db,
        staff,
        &id,
        DataWithdrawNotice {
            reason: required(input, "reason")?,
        },
    )
    .await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::ID.get(input.lang), id))
}
boxed!(notice_withdraw_card, withdraw_card, Prepared);
boxed!(notice_withdraw, withdraw_run, DeskView);

// ---- account change requests ----

const CHANGE_FIELDS: [(&str, Text); 4] = [
    (
        "display_name",
        t("Display name", "الاسم الظاهر", "Anzeigename"),
    ),
    ("pronouns", t("Pronouns", "الضمائر", "Pronomen")),
    ("status_text", t("Status", "الحالة", "Status")),
    ("bio", t("About me", "نبذة", "Über mich")),
];

fn changes(input: &DeskInput) -> AccountChanges {
    AccountChanges {
        display_name: optional(input, "display_name"),
        pronouns: optional(input, "pronouns"),
        status_text: optional(input, "status_text"),
        bio: optional(input, "bio"),
        remove: optional(input, "clear").into_iter().collect(),
        ..Default::default()
    }
}

async fn change_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    about_a_person(db, staff, &user).await?;
    let reason = required(input, "reason")?;
    let mut lines = vec![w::CHANGE.fill(lang, &[("user", &person(db, &user).await)])];
    for (name, label) in CHANGE_FIELDS {
        if let Some(value) = optional(input, name) {
            lines.push(w::SET.fill(lang, &[("field", label.get(lang)), ("value", &value)]));
        }
    }
    if let Some(field) = optional(input, "clear") {
        lines.push(w::CLEAR.fill(lang, &[("field", &named(&CLEAR, &field, lang))]));
    }
    lines.push(w::THEY_SEE.fill(lang, &[("reason", &reason)]));
    Ok(prepared("user", &user, w::T_CHANGE, lang, lines))
}

async fn change_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let user = required_user(input, "user")?;
    let data = DataChangeRequest {
        changes: changes(input),
        reason: required(input, "reason")?,
    };
    let request = create_core(db, staff, &user, data).await?;
    Ok(done(json(&request), REQUEST_DONE, input.lang))
}
boxed!(change_create_card, change_card, Prepared);
boxed!(change_create, change_run, DeskView);

async fn cancel_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "request")?;
    let request = db.fetch_account_change_request(&id).await?;
    let line = w::CANCEL.fill(
        lang,
        &[("id", &id), ("user", &person(db, &request.user_id).await)],
    );
    Ok(prepared(
        "account_change_request",
        &id,
        w::T_CANCEL,
        lang,
        vec![line],
    ))
}

async fn cancel_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let request = cancel_core(db, staff, &required(input, "request")?).await?;
    Ok(done(json(&request), REQUEST_DONE, input.lang))
}
boxed!(change_cancel_card, cancel_card, Prepared);
boxed!(change_cancel, cancel_run, DeskView);

async fn security_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    about_a_person(db, staff, &user).await?;
    let who = person(db, &user).await;
    let line = match required(input, "kind")?.as_str() {
        "email" => w::SECURITY_EMAIL.fill(
            lang,
            &[("user", &who), ("email", &required(input, "new_email")?)],
        ),
        _ => w::SECURITY_TOTP.fill(lang, &[("user", &who)]),
    };
    let reason = required(input, "reason")?;
    Ok(prepared(
        "user",
        &user,
        w::T_SECURITY,
        lang,
        vec![line, w::THEY_SEE.fill(lang, &[("reason", &reason)])],
    ))
}

async fn security_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let user = required_user(input, "user")?;
    let data = DataSecurityChange {
        kind: required(input, "kind")?,
        new_email: optional(input, "new_email"),
        reason: required(input, "reason")?,
    };
    let request = security_core(db, staff, &user, data).await?;
    Ok(done(json(&request), REQUEST_DONE, input.lang))
}
boxed!(security_request_card, security_card, Prepared);
boxed!(security_request, security_run, DeskView);

// ---- bug reviews ----

async fn bug_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "bug")?;
    let report = db.fetch_bug_report(&id).await?;
    // a report this person may not see is "not found", as in the console,
    // before anything about it is shown
    if !super::super::review::sees(staff, &report) {
        let mut record = staff.record("staff.bug.review", Some(StaffPermission::SupportManage));
        record.target_kind = Some("bug_report".to_string());
        record.target_id = Some(id.clone());
        record.summary = Some(w::NOT_SEEN.to_string());
        revolt_database::record_platform_audit(db, record, revolt_database::AuditOutcome::Denied)
            .await?;
        return Err(create_error!(NotFound));
    }
    let action = required(input, "action")?;
    let mut lines = vec![w::BUG.fill(
        lang,
        &[
            ("title", &report.title),
            ("id", &id),
            ("user", &person(db, &report.author_id).await),
            ("action", &named(&BUG_ACTION, &action, lang)),
        ],
    )];
    if matches!(action.as_str(), "confirm" | "fix") {
        lines.push(w::HUNTER.get(lang).to_string());
    }
    if let Some(severity) = optional(input, "severity") {
        lines.push(w::SEVERITY.fill(lang, &[("severity", &named(&SEVERITY, &severity, lang))]));
    }
    if let Some(duplicate) = optional(input, "duplicate_of") {
        lines.push(w::DUPLICATE_OF.fill(lang, &[("id", &duplicate)]));
    }
    if let Some(note) = optional(input, "note") {
        lines.push(w::NOTE_TO.fill(lang, &[("note", &note)]));
    }
    let mut prepared = prepared("bug_report", &id, w::T_BUG, lang, lines);
    // the report as read: changed since, the console refuses
    prepared.stored = vec![InteractionDataOption {
        name: UPDATED.to_string(),
        kind: CommandOptionType::Integer,
        value: Some(InteractionOptionValue::Integer(report.updated_at)),
        options: None,
    }];
    // security reports need SecurityRespond, which needs a step-up
    prepared.irreversible = crate::routes::badges::bugs::is_security(&report);
    Ok(prepared)
}

async fn bug_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "bug")?;
    let data = DataBugReview {
        action: required(input, "action")?,
        note: optional(input, "note"),
        severity: optional(input, "severity"),
        duplicate_of: optional(input, "duplicate_of"),
        updated_at: input
            .integer(UPDATED)
            .ok_or_else(|| create_error!(InvalidOperation))?,
    };
    let reviewed = bug_review_core(db, staff, &id, data).await?;
    Ok(done(json(&reviewed), BUG_DONE, input.lang))
}
boxed!(bug_review_card, bug_card, Prepared);
boxed!(bug_review, bug_run, DeskView);

// ---- the volunteer team ----

async fn team_list_view(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let rows = list_core(
        db,
        staff,
        input.text("role").as_deref(),
        input.text("status").as_deref(),
        None,
        None,
    )
    .await?;
    Ok(paged(
        DeskView::new(w::T_TEAM.get(input.lang)),
        &json(&rows).as_array().cloned().unwrap_or_default(),
        TEAM_ROW,
        input,
        Some(100),
    ))
}
boxed!(team_list, team_list_view, DeskView);

async fn team_view_view(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let person = detail_core(db, staff, &required(input, "member")?).await?;
    Ok(fields(
        DeskView::new(w::T_PERSON.get(input.lang)),
        &json(&person),
        TEAM_VIEW,
        input.lang,
    ))
}
boxed!(team_view, team_view_view, DeskView);

async fn team_identity_view(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    // the console's rules: TeamManage, a fresh step-up, and every look logged
    let identity = identity_core(db, staff, &required(input, "member")?, "Zrzur Desk").await?;
    Ok(fields(
        DeskView::new(w::T_IDENTITY.get(input.lang)),
        &json(&identity),
        IDENTITY_VIEW,
        input.lang,
    ))
}
boxed!(team_identity, team_identity_view, DeskView);

/// The volunteer's name and member id for a card
async fn member_of(db: &Database, input: &DeskInput) -> Result<(String, String)> {
    let id = required(input, "member")?;
    let member = db.fetch_team_member(&id).await?;
    Ok((id, person(db, &member.user_id).await))
}

async fn confirm_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, user) = member_of(db, input).await?;
    Ok(prepared(
        "team_member",
        &id,
        w::T_CONFIRM,
        lang,
        vec![w::CONFIRM.fill(lang, &[("user", &user), ("id", &id)])],
    ))
}
async fn confirm_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let person = confirm_core(db, staff, &required(input, "member")?).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&person),
        TEAM_VIEW,
        input.lang,
    ))
}
boxed!(team_confirm_card, confirm_card, Prepared);
boxed!(team_confirm, confirm_run, DeskView);

async fn counting_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, user) = member_of(db, input).await?;
    let on = input
        .boolean("on")
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let text = if on { w::COUNTING_ON } else { w::COUNTING_OFF };
    Ok(prepared(
        "team_member",
        &id,
        w::T_COUNTING,
        lang,
        vec![text.fill(lang, &[("user", &user), ("id", &id)])],
    ))
}
async fn counting_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let on = input
        .boolean("on")
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let person = counting_core(db, staff, &required(input, "member")?, DataCounting { on }).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&person),
        TEAM_VIEW,
        input.lang,
    ))
}
boxed!(team_counting_card, counting_card, Prepared);
boxed!(team_counting, counting_run, DeskView);

async fn away_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, user) = member_of(db, input).await?;
    let days = input
        .integer("days")
        .ok_or_else(|| create_error!(InvalidOperation))?;
    // the volunteers' own longest stay, as the console checks it
    let max = crate::routes::team_desk::staff_desk(&staff.user.id)
        .await?
        .volunteers
        .away_max_days
        .max(0);
    if !(0..=max).contains(&days) {
        return Err(create_error!(FailedValidation {
            error: format!("days: 0 to {max}")
        }));
    }
    let line = if days == 0 {
        w::BACK.fill(lang, &[("user", &user), ("id", &id)])
    } else {
        w::AWAY.fill(
            lang,
            &[("user", &user), ("id", &id), ("days", &days.to_string())],
        )
    };
    Ok(prepared("team_member", &id, w::T_AWAY, lang, vec![line]))
}
async fn away_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let days = input
        .integer("days")
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let person = away_core(db, staff, &required(input, "member")?, DataAway { days }).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&person),
        TEAM_VIEW,
        input.lang,
    ))
}
boxed!(team_away_card, away_card, Prepared);
boxed!(team_away, away_run, DeskView);

async fn team_note_card_fn(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, user) = member_of(db, input).await?;
    let line = match optional(input, "text") {
        Some(text) => w::TEAM_NOTE.fill(lang, &[("user", &user), ("id", &id), ("text", &text)]),
        None => w::TEAM_NOTE_CLEAR.fill(lang, &[("user", &user), ("id", &id)]),
    };
    Ok(prepared(
        "team_member",
        &id,
        w::T_TEAM_NOTE,
        lang,
        vec![line],
    ))
}
async fn team_note_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    // nothing written: the note is cleared, as in the console
    let text = optional(input, "text").unwrap_or_default();
    let person = note_core(db, staff, &required(input, "member")?, DataNote { text }).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&person),
        TEAM_VIEW,
        input.lang,
    ))
}
boxed!(team_note_card, team_note_card_fn, Prepared);
boxed!(team_note, team_note_run, DeskView);

async fn remove_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let (id, user) = member_of(db, input).await?;
    let reason = required(input, "reason")?;
    let line = w::REMOVE.fill(lang, &[("user", &user), ("id", &id), ("reason", &reason)]);
    Ok(prepared("team_member", &id, w::T_REMOVE, lang, vec![line]))
}
async fn remove_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let reason = required(input, "reason")?;
    let person = remove_core(
        db,
        staff,
        &required(input, "member")?,
        DataRemoveVolunteer { reason },
    )
    .await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&person),
        TEAM_VIEW,
        input.lang,
    ))
}
boxed!(team_remove_card, remove_card, Prepared);
boxed!(team_remove, remove_run, DeskView);

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

const fn text(name: &'static str, required: bool, max: u32, description: Text) -> DeskOption {
    DeskOption {
        max_length: Some(max),
        ..opt(name, CommandOptionType::String, required, description)
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

const fn id(name: &'static str) -> DeskOption {
    text(name, true, 64, t("ID", "المعرّف", "ID"))
}

const USER: DeskOption = opt(
    "user",
    CommandOptionType::User,
    true,
    t("The user", "المستخدم", "Der Benutzer"),
);
const REASON: DeskOption = text("reason", true, 500, t("Reason", "السبب", "Grund"));
const ANSWER: DeskOption = text(
    "text",
    true,
    900,
    t(
        "Text (desk: up to 900)",
        "النص (في المكتب: حتى 900)",
        "Text (im Desk: bis 900)",
    ),
);

const TONE: DeskOption = DeskOption {
    choices: &[
        choice("info", "Information", "معلومة", "Information"),
        choice("warning", "Warning", "تحذير", "Warnung"),
        choice("important", "Important", "مهم", "Wichtig"),
    ],
    ..opt(
        "tone",
        CommandOptionType::String,
        true,
        t("Tone", "النبرة", "Ton"),
    )
};

const NOTICE_TEXT: [DeskOption; 6] = [
    text(
        "title_en",
        true,
        100,
        t("Title (English)", "العنوان (إنجليزي)", "Titel (Englisch)"),
    ),
    text(
        "title_ar",
        true,
        100,
        t("Title (Arabic)", "العنوان (عربي)", "Titel (Arabisch)"),
    ),
    text(
        "title_de",
        true,
        100,
        t("Title (German)", "العنوان (ألماني)", "Titel (Deutsch)"),
    ),
    text(
        "text_en",
        true,
        200,
        t("Text (English)", "النص (إنجليزي)", "Text (Englisch)"),
    ),
    text(
        "text_ar",
        true,
        200,
        t("Text (Arabic)", "النص (عربي)", "Text (Arabisch)"),
    ),
    text(
        "text_de",
        true,
        200,
        t("Text (German)", "النص (ألماني)", "Text (Deutsch)"),
    ),
];
const LINK: DeskOption = text(
    "link",
    false,
    300,
    t("https link", "رابط https", "https-Link"),
);
const DAYS: DeskOption = DeskOption {
    min_value: Some(1.0),
    max_value: Some(365.0),
    ..opt(
        "days",
        CommandOptionType::Integer,
        false,
        t(
            "Days shown (1-365)",
            "أيام الظهور (1-365)",
            "Tage sichtbar (1-365)",
        ),
    )
};

const CLEAR: DeskOption = DeskOption {
    choices: &[
        choice("Avatar", "Avatar", "الصورة الرمزية", "Avatar"),
        choice(
            "ProfileBackground",
            "Profile background",
            "خلفية الملف",
            "Profilhintergrund",
        ),
        choice("DisplayName", "Display name", "الاسم الظاهر", "Anzeigename"),
        choice("Pronouns", "Pronouns", "الضمائر", "Pronomen"),
        choice("StatusText", "Status", "الحالة", "Status"),
        choice("ProfileContent", "About me", "نبذة", "Über mich"),
    ],
    ..opt(
        "clear",
        CommandOptionType::String,
        false,
        t("Clear a field", "مسح حقل", "Feld leeren"),
    )
};

const BUG_ACTION: DeskOption = DeskOption {
    choices: &[
        choice("triage", "Triage", "فرز", "Sichten"),
        choice("need_info", "Needs info", "يحتاج معلومات", "Infos nötig"),
        choice("confirm", "Confirm", "تأكيد", "Bestätigen"),
        choice("fix", "Fixed", "تم الإصلاح", "Behoben"),
        choice("duplicate", "Duplicate", "مكرر", "Duplikat"),
        choice("not_a_bug", "Not a bug", "ليس خطأ", "Kein Fehler"),
        choice("reopen", "Reopen", "إعادة فتح", "Wieder öffnen"),
    ],
    ..opt(
        "action",
        CommandOptionType::String,
        true,
        t("Action", "الإجراء", "Aktion"),
    )
};

const SEVERITY: DeskOption = DeskOption {
    choices: &[
        choice("low", "Low", "منخفضة", "Niedrig"),
        choice("medium", "Medium", "متوسطة", "Mittel"),
        choice("high", "High", "عالية", "Hoch"),
        choice("critical", "Critical", "حرجة", "Kritisch"),
    ],
    ..opt(
        "severity",
        CommandOptionType::String,
        false,
        t("Severity", "الخطورة", "Schweregrad"),
    )
};

const TEAM_STATUS: DeskOption = DeskOption {
    choices: &[
        choice("trial", "Trial", "تجربة", "Probezeit"),
        choice("active", "Active", "نشط", "Aktiv"),
        choice("away", "Away", "غائب", "Abwesend"),
        choice("inactive", "Inactive", "غير نشط", "Inaktiv"),
        choice("left", "Left", "غادر", "Ausgetreten"),
    ],
    ..opt(
        "status",
        CommandOptionType::String,
        false,
        t("Status", "الحالة", "Status"),
    )
};

const fn entry(
    group: Option<&'static str>,
    name: &'static str,
    description: Text,
    options: &'static [DeskOption],
    permission: StaffPermission,
    irreversible: bool,
    prepare: Option<Prepare>,
    handler: super::registry::Handler,
) -> DeskCommand {
    DeskCommand {
        area: Some("support"),
        group,
        name,
        description,
        options,
        permission: Some(permission),
        changes: prepare.is_some(),
        irreversible,
        plugins: false,
        prepare,
        handler,
        verify: None,
    }
}

use StaffPermission as P;

pub static GROUPS: &[DeskGroup] = &[
    DeskGroup {
        name: "notice",
        description: t("Notices", "الإشعارات", "Hinweise"),
    },
    DeskGroup {
        name: "change-request",
        description: t(
            "Profile change requests",
            "طلبات تعديل الملف",
            "Profiländerungen",
        ),
    },
    DeskGroup {
        name: "team",
        description: t("Volunteer team", "فريق المتطوعين", "Freiwilligenteam"),
    },
];

pub static ENTRIES: &[DeskCommand] = &[
    DeskCommand {
        verify: Some(ticket_verify),
        ..entry(
            Some("ticket"),
            "reply",
            t("Answer a ticket", "الرد على تذكرة", "Beantworten"),
            &[
                id("ticket"),
                ANSWER,
                opt(
                    "close",
                    CommandOptionType::Boolean,
                    false,
                    t("Close it too", "وإغلاقها", "Auch schließen"),
                ),
            ],
            P::SupportManage,
            false,
            Some(ticket_reply_card),
            ticket_reply,
        )
    },
    DeskCommand {
        verify: Some(ticket_verify),
        ..entry(
            Some("ticket"),
            "note",
            t("Staff note", "ملاحظة إدارية", "Team-Notiz"),
            &[id("ticket"), ANSWER],
            P::SupportManage,
            false,
            Some(ticket_note_card),
            ticket_note,
        )
    },
    DeskCommand {
        verify: Some(ticket_verify),
        ..entry(
            Some("ticket"),
            "assign",
            t("Assign", "إسناد", "Zuweisen"),
            &[
                id("ticket"),
                opt(
                    "user",
                    CommandOptionType::User,
                    false,
                    t(
                        "Staff member (none: you)",
                        "عضو الإدارة (فارغ: أنت)",
                        "Teammitglied (leer: du)",
                    ),
                ),
            ],
            P::SupportManage,
            false,
            Some(ticket_assign_card),
            ticket_assign,
        )
    },
    DeskCommand {
        verify: Some(ticket_verify),
        ..entry(
            Some("ticket"),
            "close",
            t("Close", "إغلاق", "Schließen"),
            &[id("ticket")],
            P::SupportManage,
            false,
            Some(ticket_close_card),
            ticket_close,
        )
    },
    DeskCommand {
        verify: Some(ticket_verify),
        ..entry(
            Some("ticket"),
            "reopen",
            t("Reopen", "إعادة فتح", "Wieder öffnen"),
            &[id("ticket")],
            P::SupportManage,
            false,
            Some(ticket_reopen_card),
            ticket_reopen,
        )
    },
    // the console always asks for a step-up before a notice
    entry(
        Some("notice"),
        "send",
        t(
            "Notice to one person",
            "إشعار لشخص",
            "Hinweis an eine Person",
        ),
        &[
            USER,
            NOTICE_TEXT[0],
            NOTICE_TEXT[1],
            NOTICE_TEXT[2],
            NOTICE_TEXT[3],
            NOTICE_TEXT[4],
            NOTICE_TEXT[5],
            TONE,
            LINK,
            DAYS,
            opt(
                "email",
                CommandOptionType::Boolean,
                false,
                t("Also by email", "بالبريد أيضًا", "Auch per E-Mail"),
            ),
        ],
        P::SupportManage,
        true,
        Some(notice_send_card),
        notice_send,
    ),
    entry(
        Some("notice"),
        "everyone",
        t("Notice to everyone", "إشعار للجميع", "Hinweis an alle"),
        &[
            NOTICE_TEXT[0],
            NOTICE_TEXT[1],
            NOTICE_TEXT[2],
            NOTICE_TEXT[3],
            NOTICE_TEXT[4],
            NOTICE_TEXT[5],
            TONE,
            LINK,
            DAYS,
        ],
        P::SettingsManage,
        true,
        Some(notice_everyone_card),
        notice_everyone,
    ),
    entry(
        Some("notice"),
        "withdraw",
        t("Withdraw a notice", "سحب إشعار", "Zurückziehen"),
        &[id("notice"), REASON],
        P::SupportManage,
        true,
        Some(notice_withdraw_card),
        notice_withdraw,
    ),
    entry(
        Some("change-request"),
        "create",
        t("Ask for a profile change", "طلب تعديل", "Änderung anfragen"),
        &[
            USER,
            text(
                "reason",
                true,
                500,
                t("Reason they see", "السبب الظاهر له", "Sichtbarer Grund"),
            ),
            text(
                "display_name",
                false,
                32,
                t("New display name", "اسم ظاهر جديد", "Neuer Anzeigename"),
            ),
            text(
                "pronouns",
                false,
                32,
                t("New pronouns", "ضمائر جديدة", "Neue Pronomen"),
            ),
            text(
                "status_text",
                false,
                128,
                t("New status", "حالة جديدة", "Neuer Status"),
            ),
            text(
                "bio",
                false,
                900,
                t(
                    "New about me (desk: up to 900)",
                    "نبذة جديدة (في المكتب: حتى 900)",
                    "Neues Über mich (im Desk: bis 900)",
                ),
            ),
            CLEAR,
        ],
        P::SupportManage,
        false,
        Some(change_create_card),
        change_create,
    ),
    entry(
        Some("change-request"),
        "cancel",
        t("Withdraw a request", "سحب طلب", "Anfrage zurückziehen"),
        &[id("request")],
        P::SupportManage,
        false,
        Some(change_cancel_card),
        change_cancel,
    ),
    // the console asks for a step-up here
    entry(
        None,
        "security-request",
        t(
            "Email or two-factor change",
            "تغيير البريد أو التحقق",
            "E-Mail oder Zwei-Faktor",
        ),
        &[
            USER,
            DeskOption {
                choices: &[
                    choice(
                        "email",
                        "Change sign-in email",
                        "تغيير بريد الدخول",
                        "Anmelde-E-Mail ändern",
                    ),
                    choice(
                        "two_factor",
                        "Turn off two-factor",
                        "إيقاف التحقق بخطوتين",
                        "Zwei-Faktor ausschalten",
                    ),
                ],
                ..opt(
                    "kind",
                    CommandOptionType::String,
                    true,
                    t("What", "ماذا", "Was"),
                )
            },
            text(
                "reason",
                true,
                500,
                t("Reason they see", "السبب الظاهر له", "Sichtbarer Grund"),
            ),
            text(
                "new_email",
                false,
                128,
                t("New email", "البريد الجديد", "Neue E-Mail"),
            ),
        ],
        P::SupportManage,
        true,
        Some(security_request_card),
        security_request,
    ),
    entry(
        Some("bug"),
        "review",
        t(
            "Review a bug report",
            "مراجعة بلاغ خطأ",
            "Fehlerbericht prüfen",
        ),
        &[
            id("bug"),
            BUG_ACTION,
            text(
                "note",
                false,
                1000,
                t("Note they read", "ملاحظة يقرؤها", "Notiz für die Person"),
            ),
            SEVERITY,
            text(
                "duplicate_of",
                false,
                64,
                t("Duplicate of", "مكرر لـ", "Duplikat von"),
            ),
        ],
        P::SupportManage,
        false,
        Some(bug_review_card),
        bug_review,
    ),
    entry(
        Some("team"),
        "list",
        t("Volunteers", "المتطوعون", "Freiwillige"),
        &[
            text("role", false, 64, t("Role", "الدور", "Rolle")),
            TEAM_STATUS,
        ],
        P::TeamManage,
        false,
        None,
        team_list,
    ),
    entry(
        Some("team"),
        "view",
        t("One volunteer", "متطوع واحد", "Eine Person"),
        &[id("member")],
        P::TeamManage,
        false,
        None,
        team_view,
    ),
    entry(
        Some("team"),
        "identity",
        t("Show identity", "عرض الهوية", "Identität zeigen"),
        &[id("member")],
        P::TeamManage,
        false,
        None,
        team_identity,
    ),
    entry(
        Some("team"),
        "confirm",
        t(
            "Confirm after trial",
            "تثبيت بعد التجربة",
            "Nach Probezeit bestätigen",
        ),
        &[id("member")],
        P::TeamManage,
        false,
        Some(team_confirm_card),
        team_confirm,
    ),
    entry(
        Some("team"),
        "counting",
        t("Activity counting", "عدّ النشاط", "Aktivitätszählung"),
        &[
            id("member"),
            opt(
                "on",
                CommandOptionType::Boolean,
                true,
                t("Count", "العدّ", "Zählen"),
            ),
        ],
        P::TeamManage,
        false,
        Some(team_counting_card),
        team_counting,
    ),
    entry(
        Some("team"),
        "away",
        t("Set away", "تسجيل غياب", "Abwesend setzen"),
        &[
            id("member"),
            DeskOption {
                min_value: Some(0.0),
                ..opt(
                    "days",
                    CommandOptionType::Integer,
                    true,
                    t("Days (0: back)", "الأيام (0: عودة)", "Tage (0: zurück)"),
                )
            },
        ],
        P::TeamManage,
        false,
        Some(team_away_card),
        team_away,
    ),
    entry(
        Some("team"),
        "note",
        t("Staff note", "ملاحظة إدارية", "Team-Notiz"),
        &[
            id("member"),
            text(
                "text",
                false,
                900,
                t(
                    "Note (none: clear; desk: up to 900)",
                    "الملاحظة (فارغ: مسح؛ في المكتب: حتى 900)",
                    "Notiz (leer: löschen; im Desk: bis 900)",
                ),
            ),
        ],
        P::TeamManage,
        false,
        Some(team_note_card),
        team_note,
    ),
    // the console asks for a step-up here
    entry(
        Some("team"),
        "remove",
        t("Remove a volunteer", "إنهاء عضوية", "Entfernen"),
        &[
            id("member"),
            text(
                "reason",
                true,
                500,
                t("Reason (10-500)", "السبب (10-500)", "Grund (10-500)"),
            ),
        ],
        P::TeamManage,
        true,
        Some(team_remove_card),
        team_remove,
    ),
];
