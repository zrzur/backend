//! Desk lookups (areas `lookup` and `audit`): read-only commands, each the
//! console's own core function (`*_core`), so every permission check,
//! personal-data rule and audited read is the console's. Lists page (8 per
//! page) with `zd:page:` buttons that `/staff/desk/run` understands.
//!
//! The words the server writes (titles, field names, flags) come in the
//! command's language; values (names, ids, numbers, statuses) are data and
//! stay as they are.

use futures::future::BoxFuture;
use revolt_database::{Database, StaffPermission};
use revolt_models::v0::CommandOptionType;
use revolt_result::{create_error, Result};
use serde::Serialize;
use serde_json::Value;

use super::super::{badges, bots, platform, review, safety, support};
use super::super::StaffContext;
use super::registry::{when, DeskChoice, DeskCommand, DeskGroup, DeskInput, DeskOption, Lang, Text};
use super::view::{DeskButton, DeskButtonStyle, DeskView};

/// Rows on one page of a list
pub const PAGE_SIZE: usize = 8;
/// Most rows a list fetches (the console's own page size)
const FETCH: usize = 50;
/// Custom id prefix of a page button: `zd:page:<command interaction>:<page>`
pub const PAGE_PREFIX: &str = "zd:page:";

const fn t(en: &'static str, ar: &'static str, de: &'static str) -> Text {
    Text { en, ar, de }
}

/// A value to show: a JSON pointer into the core's answer and its label.
/// `@id` is `_id` or `id`; `@handle` is `username#discriminator`.
pub struct Show {
    pub key: &'static str,
    pub label: Text,
}

const fn show(key: &'static str, label: Text) -> Show {
    Show { key, label }
}

// ---- words ----

mod w {
    use super::{t, Text};
    pub const YES: Text = t("yes", "نعم", "ja");
    pub const NO: Text = t("no", "لا", "nein");
    pub const NOTHING: Text = t("Nothing found.", "لم يُعثر على شيء.", "Nichts gefunden.");
    pub const PREVIOUS: Text = t("Previous", "السابق", "Zurück");
    pub const NEXT: Text = t("Next", "التالي", "Weiter");
    pub const MORE: Text = t(
        "Showing the newest {n}; narrow the search for more.",
        "تُعرض أحدث {n} نتيجة؛ ضيّق البحث لرؤية المزيد.",
        "Die neuesten {n} werden gezeigt; grenze die Suche ein für mehr.",
    );
    pub const ID: Text = t("ID", "المعرّف", "ID");
    pub const USER: Text = t("User", "المستخدم", "Benutzer");
    pub const NAME: Text = t("Name", "الاسم", "Name");
    pub const DISPLAY_NAME: Text = t("Display name", "الاسم الظاهر", "Anzeigename");
    pub const EMAIL: Text = t("Email", "البريد الإلكتروني", "E-Mail");
    pub const CREATED: Text = t("Created", "أُنشئ", "Erstellt");
    pub const UPDATED: Text = t("Updated", "حُدّث", "Aktualisiert");
    pub const CLOSED_AT: Text = t("Closed", "أُغلق", "Geschlossen");
    pub const ONLINE: Text = t("Online", "متصل", "Online");
    pub const SERVERS: Text = t("Servers", "السيرفرات", "Server");
    pub const SESSIONS: Text = t("Sessions", "الجلسات", "Sitzungen");
    pub const VERIFIED: Text = t("verified", "موثّق", "verifiziert");
    pub const MFA: Text = t("Two-factor", "التحقق بخطوتين", "Zwei-Faktor");
    pub const DISABLED: Text = t("disabled", "معطّل", "deaktiviert");
    pub const DELETION: Text = t("deletion pending", "الحذف قيد الانتظار", "Löschung ausstehend");
    pub const REPORTS: Text = t("Reports", "البلاغات", "Meldungen");
    pub const SUSPENDED_UNTIL: Text = t("Suspended until", "موقوف حتى", "Gesperrt bis");
    pub const BOT: Text = t("bot", "بوت", "Bot");
    pub const SUSPENDED: Text = t("suspended", "موقوف", "gesperrt");
    pub const BANNED: Text = t("banned", "محظور", "gebannt");
    pub const DELETED: Text = t("deleted", "محذوف", "gelöscht");
    pub const OFFICIAL: Text = t("official", "رسمي", "offiziell");
    pub const OWNER: Text = t("Owner", "المالك", "Eigentümer");
    pub const OWNER_ID: Text = t("Owner ID", "معرّف المالك", "Eigentümer-ID");
    pub const MEMBERS: Text = t("Members", "الأعضاء", "Mitglieder");
    pub const CHANNELS: Text = t("Channels", "القنوات", "Kanäle");
    pub const ROLES: Text = t("Roles", "الأدوار", "Rollen");
    pub const NSFW: Text = t("18+", "+18", "18+");
    pub const FLAGS: Text = t("Flags", "العلامات", "Kennzeichen");
    pub const DESCRIPTION: Text = t("Description", "الوصف", "Beschreibung");
    pub const PUBLIC: Text = t("public", "عام", "öffentlich");
    pub const DISCOVERABLE: Text = t("discoverable", "قابل للاكتشاف", "auffindbar");
    pub const INTERACTIONS_URL: Text = t("Interactions URL", "رابط التفاعلات", "Interaktions-URL");
    pub const TERMS: Text = t("Terms", "الشروط", "Bedingungen");
    pub const PRIVACY: Text = t("Privacy policy", "سياسة الخصوصية", "Datenschutzerklärung");
    pub const VERIFICATIONS: Text = t("Verification requests", "طلبات التوثيق", "Verifizierungsanfragen");
    pub const STATUS: Text = t("Status", "الحالة", "Status");
    pub const CATEGORY: Text = t("Category", "الفئة", "Kategorie");
    pub const SUBCATEGORY: Text = t("Subcategory", "الفئة الفرعية", "Unterkategorie");
    pub const PRIORITY: Text = t("Priority", "الأولوية", "Priorität");
    pub const REFERENCE: Text = t("Reference", "المرجع", "Referenz");
    pub const ROUTE: Text = t("Route", "المسار", "Weg");
    pub const TARGET: Text = t("Target", "الهدف", "Ziel");
    pub const ASSIGNEE: Text = t("Assigned to", "مُسند إلى", "Zugewiesen an");
    pub const REPORTERS: Text = t("Reporters", "المبلّغون", "Meldende");
    pub const EVIDENCE: Text = t("Evidence", "الأدلة", "Beweise");
    pub const ACTIONS: Text = t("Actions", "الإجراءات", "Maßnahmen");
    pub const APPEALS: Text = t("Appeals", "الطعون", "Einsprüche");
    pub const RELATED: Text = t("Related cases", "حالات مرتبطة", "Verwandte Fälle");
    pub const SUBJECT: Text = t("Subject", "الموضوع", "Betreff");
    pub const MESSAGES: Text = t("Messages", "الرسائل", "Nachrichten");
    pub const WAITING: Text = t("waiting", "بانتظار الرد", "wartet");
    pub const KIND: Text = t("Kind", "النوع", "Art");
    pub const TITLE: Text = t("Title", "العنوان", "Titel");
    pub const PLATFORM: Text = t("Platform", "المنصة", "Plattform");
    pub const VERSION: Text = t("App version", "إصدار التطبيق", "App-Version");
    pub const AUTHOR: Text = t("Reported by", "أبلغ عنه", "Gemeldet von");
    pub const DUPLICATES: Text = t("Possible duplicates", "تكرارات محتملة", "Mögliche Duplikate");
    pub const OK: Text = t("Chain intact", "السلسلة سليمة", "Kette intakt");
    pub const CHECKED: Text = t("Records checked", "السجلات المفحوصة", "Geprüfte Einträge");
    pub const FROM: Text = t("From record", "من السجل", "Ab Eintrag");
    pub const FIRST_BAD: Text = t("First broken record", "أول سجل تالف", "Erster defekter Eintrag");
    pub const DENIED_DAY: Text = t("Denied, 24 hours", "مرفوض، 24 ساعة", "Abgelehnt, 24 Stunden");
    pub const FAILED_DAY: Text = t("Failed, 24 hours", "فاشل، 24 ساعة", "Fehlgeschlagen, 24 Stunden");
    pub const SIGN_INS_DAY: Text = t("Failed sign-ins, 24 hours", "تسجيلات دخول فاشلة، 24 ساعة", "Fehlgeschlagene Anmeldungen, 24 Stunden");
    pub const STEP_UPS_DAY: Text = t("Failed step-ups, 24 hours", "تحققات إضافية فاشلة، 24 ساعة", "Fehlgeschlagene Bestätigungen, 24 Stunden");
    pub const REVOKED_DAY: Text = t("Sessions ended, 24 hours", "جلسات مُنهاة، 24 ساعة", "Beendete Sitzungen, 24 Stunden");
    pub const DENIED_WEEK: Text = t("Denied, 7 days", "مرفوض، 7 أيام", "Abgelehnt, 7 Tage");
    pub const FAILED_WEEK: Text = t("Failed, 7 days", "فاشل، 7 أيام", "Fehlgeschlagen, 7 Tage");
    pub const SCANNED: Text = t("Records scanned", "السجلات المفحوصة", "Durchsuchte Einträge");
    pub const API_VERSION: Text = t("API version", "إصدار الواجهة البرمجية", "API-Version");
    pub const STARTED: Text = t("Started", "بدأ التشغيل", "Gestartet");
    pub const DATABASE_OK: Text = t("Database", "قاعدة البيانات", "Datenbank");
    pub const DATABASE_MS: Text = t("Database, ms", "قاعدة البيانات، ملّي ثانية", "Datenbank, ms");
    pub const PRESENCE_OK: Text = t("Presence store", "مخزن الحضور", "Präsenzspeicher");
    pub const PRESENCE_MS: Text = t("Presence store, ms", "مخزن الحضور، ملّي ثانية", "Präsenzspeicher, ms");
    pub const ONLINE_USERS: Text = t("Online users", "المستخدمون المتصلون", "Nutzer online");
    pub const SINCE: Text = t("Since", "منذ", "Seit");
    pub const REQUESTS: Text = t("Requests", "الطلبات", "Anfragen");
    pub const CLIENT_ERRORS: Text = t("Client errors", "أخطاء العميل", "Client-Fehler");
    pub const SERVER_ERRORS: Text = t("Server errors", "أخطاء الخادم", "Server-Fehler");
    pub const ACTIVE: Text = t("active", "نشط", "aktiv");
    pub const OPEN_SESSIONS: Text = t("Open sessions", "الجلسات المفتوحة", "Offene Sitzungen");
    pub const PENDING: Text = t("Pending", "قيد الانتظار", "Ausstehend");
    pub const REVISIONS: Text = t("Revisions", "التعديلات", "Überarbeitungen");
    pub const REPORTED: Text = t("Reported", "مُبلّغ عنها", "Gemeldet");
    pub const ALLOW: Text = t("allowed", "مسموح", "erlaubt");
    pub const LIVE: Text = t("live", "ظاهر الآن", "aktiv");
    pub const MODE: Text = t("Mode", "الوضع", "Modus");
    pub const PROVIDER: Text = t("Provider", "مزوّد الدفع", "Anbieter");
    pub const WEBHOOKS_FAILED: Text = t("Webhooks failed", "إشعارات فاشلة", "Fehlgeschlagene Webhooks");
    pub const WEBHOOKS_PENDING: Text = t("Webhooks pending", "إشعارات قيد الانتظار", "Ausstehende Webhooks");
    pub const OPEN_ISSUES: Text = t("Open issues", "مشكلات مفتوحة", "Offene Probleme");
    pub const PASSED: Text = t("passed", "ناجح", "bestanden");
    pub const NOT_PASSED: Text = t("not passed", "غير ناجح", "nicht bestanden");
    pub const COUNT: Text = t("Count", "العدد", "Anzahl");
    pub const HELD: Text = t("held", "محمولة", "gehalten");

    // titles
    pub const T_USERS: Text = t("Users", "المستخدمون", "Benutzer");
    pub const T_USER: Text = t("User", "المستخدم", "Benutzer");
    pub const T_BADGES: Text = t("Badges", "الشارات", "Abzeichen");
    pub const T_HISTORY: Text = t("Account history", "سجل الحساب", "Kontoverlauf");
    pub const T_SERVERS: Text = t("Servers", "السيرفرات", "Server");
    pub const T_SERVER: Text = t("Server", "السيرفر", "Server");
    pub const T_BOTS: Text = t("Bots", "البوتات", "Bots");
    pub const T_BOT: Text = t("Bot", "البوت", "Bot");
    pub const T_CASES: Text = t("Reports", "البلاغات", "Meldungen");
    pub const T_CASE: Text = t("Report", "البلاغ", "Meldung");
    pub const T_APPEALS: Text = t("Appeals", "الطعون", "Einsprüche");
    pub const T_TICKETS: Text = t("Support tickets", "تذاكر الدعم", "Support-Tickets");
    pub const T_TICKET: Text = t("Support ticket", "تذكرة الدعم", "Support-Ticket");
    pub const T_BUGS: Text = t("Bug reports", "بلاغات الأخطاء", "Fehlerberichte");
    pub const T_BUG: Text = t("Bug report", "بلاغ الخطأ", "Fehlerbericht");
    pub const T_AUDIT: Text = t("Audit log", "سجل التدقيق", "Prüfprotokoll");
    pub const T_VERIFY: Text = t("Audit chain check", "فحص سلسلة التدقيق", "Prüfung der Protokollkette");
    pub const T_SECURITY: Text = t("Security summary", "ملخص الأمان", "Sicherheitsübersicht");
    pub const T_HEALTH: Text = t("System health", "حالة النظام", "Systemzustand");
    pub const T_METRICS: Text = t("API metrics", "مقاييس الواجهة البرمجية", "API-Kennzahlen");
    pub const T_STAFF: Text = t("Staff", "فريق الإدارة", "Team");
    pub const T_CREATORS: Text = t("Creators", "المبدعون", "Creator");
    pub const T_DESIGNS: Text = t("Design review", "مراجعة التصاميم", "Design-Prüfung");
    pub const T_PLUGINS: Text = t("Plugin review", "مراجعة الإضافات", "Plugin-Prüfung");
    pub const T_DISCOVER: Text = t("Discover requests", "طلبات الاستكشاف", "Discover-Anfragen");
    pub const T_VERIFICATIONS: Text = t("Bot verification requests", "طلبات توثيق البوتات", "Bot-Verifizierungsanfragen");
    pub const T_WORDS: Text = t("Word list", "قائمة الكلمات", "Wortliste");
    pub const T_NOTICES: Text = t("Notices", "الإشعارات", "Hinweise");
    pub const T_BILLING: Text = t("Billing summary", "ملخص الفوترة", "Abrechnungsübersicht");
}

// ---- rendering ----

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// The value at `key` (see [`Show`])
pub fn at<'v>(value: &'v Value, key: &str) -> Option<&'v Value> {
    match key {
        "@id" => value.get("_id").or_else(|| value.get("id")),
        _ => value.pointer(key),
    }
    .filter(|v| !v.is_null())
}

/// A value as text, or `None` when there is nothing to show
pub fn text_of(value: &Value, key: &str, lang: Lang) -> Option<String> {
    if key == "@handle" {
        let name = value.get("username").and_then(Value::as_str)?;
        return Some(match value.get("discriminator").and_then(Value::as_str) {
            Some(tag) => format!("{name}#{tag}"),
            None => name.to_string(),
        });
    }
    let v = at(value, key)?;
    match v {
        Value::Bool(b) => Some(if *b { w::YES } else { w::NO }.get(lang).to_string()),
        Value::Number(n) => {
            let time = key.ends_with("_at") || key.ends_with("since") || key.ends_with("until");
            match n.as_i64() {
                Some(ms) if time && ms > 100_000_000_000 => Some(when(ms)),
                _ => Some(n.to_string()),
            }
        }
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(items) if items.iter().all(|i| !i.is_object() && !i.is_array()) => {
            let parts: Vec<String> = items
                .iter()
                .map(|i| i.as_str().map(str::to_string).unwrap_or_else(|| i.to_string()))
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        Value::Array(items) => Some(items.len().to_string()),
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .filter(|(_, v)| !v.is_object() && !v.is_array() && !v.is_null())
                .map(|(k, v)| format!("{k}: {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        Value::Null => None,
    }
}

/// `label: value` fields for the shows that have a value
pub fn fields(view: DeskView, value: &Value, shows: &[Show], lang: Lang) -> DeskView {
    shows.iter().fold(view, |view, s| match text_of(value, s.key, lang) {
        Some(text) => view.field(s.label.get(lang), text),
        None => view,
    })
}

/// One row of a list: the values joined with ` · `; a true flag shows its
/// word, a false one nothing
pub fn line(item: &Value, shows: &[Show], lang: Lang) -> String {
    shows
        .iter()
        .filter_map(|s| match at(item, s.key) {
            Some(Value::Bool(true)) => Some(s.label.get(lang).to_string()),
            Some(Value::Bool(false)) => None,
            _ => text_of(item, s.key, lang),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A list, one page of it, with page buttons that come back to `/run`
pub fn paged(mut view: DeskView, items: &[Value], shows: &[Show], input: &DeskInput) -> DeskView {
    let lang = input.lang;
    let pages = items.len().div_ceil(PAGE_SIZE).max(1);
    let page = (input.page.max(1) as usize).min(pages);
    if items.is_empty() {
        return view.line(w::NOTHING.get(lang));
    }
    for item in items.iter().skip((page - 1) * PAGE_SIZE).take(PAGE_SIZE) {
        view = view.line(line(item, shows, lang));
    }
    if items.len() >= FETCH {
        view = view.line(w::MORE.fill(lang, &[("n", &FETCH.to_string())]));
    }
    if pages > 1 {
        view.page = Some(page as u32);
        view.pages = Some(pages as u32);
        let button = |label: Text, to: usize| DeskButton {
            label: label.get(lang).to_string(),
            action_id: format!("{PAGE_PREFIX}{}:{to}", input.origin),
            style: DeskButtonStyle::Secondary,
        };
        if page > 1 {
            view.buttons.push(button(w::PREVIOUS, page - 1));
        }
        if page < pages {
            view.buttons.push(button(w::NEXT, page + 1));
        }
    }
    view
}

fn items(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        _ => vec![],
    }
}

/// A required id option, or `InvalidOperation`
fn required(input: &DeskInput, name: &str, kind: CommandOptionType) -> Result<String> {
    let value = match kind {
        CommandOptionType::User => input.user(name),
        _ => input.text(name),
    };
    value
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .ok_or_else(|| create_error!(InvalidOperation))
}

fn query(input: &DeskInput) -> Option<String> {
    input.text("query").filter(|q| !q.is_empty())
}

macro_rules! handler {
    ($name:ident, $inner:ident) => {
        fn $name<'a>(db: &'a Database, staff: &'a StaffContext, input: &'a DeskInput) -> BoxFuture<'a, Result<DeskView>> {
            Box::pin($inner(db, staff, input))
        }
    };
}

// ---- users ----

pub const USER_ROW: &[Show] = &[
    show("@handle", w::USER),
    show("/display_name", w::DISPLAY_NAME),
    show("@id", w::ID),
    show("/email", w::EMAIL),
    show("/bot", w::BOT),
    show("/suspended", w::SUSPENDED),
    show("/banned", w::BANNED),
    show("/deleted", w::DELETED),
    show("/official", w::OFFICIAL),
];

pub const USER_VIEW: &[Show] = &[
    show("@handle", w::USER),
    show("/display_name", w::DISPLAY_NAME),
    show("@id", w::ID),
    show("/email", w::EMAIL),
    show("/created_at", w::CREATED),
    show("/online", w::ONLINE),
    show("/servers", w::SERVERS),
    show("/sessions", w::SESSIONS),
    show("/account/verified", w::VERIFIED),
    show("/account/mfa_enabled", w::MFA),
    show("/account/disabled", w::DISABLED),
    show("/account/deletion_pending", w::DELETION),
    show("/reports_about", w::REPORTS),
    show("/suspended_until", w::SUSPENDED_UNTIL),
    show("/bot", w::BOT),
    show("/suspended", w::SUSPENDED),
    show("/banned", w::BANNED),
    show("/deleted", w::DELETED),
    show("/official", w::OFFICIAL),
];

async fn user_find_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let rows = platform::users_core(db, staff, query(input).as_deref(), None, Some(FETCH)).await?;
    Ok(paged(DeskView::new(w::T_USERS.get(input.lang)), &items(json(&rows)), USER_ROW, input))
}
handler!(user_find, user_find_view);

async fn user_view_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "user", CommandOptionType::User)?;
    let user = platform::user_core(db, staff, &id).await?;
    Ok(fields(DeskView::new(w::T_USER.get(input.lang)), &json(&user), USER_VIEW, input.lang))
}
handler!(user_view, user_view_view);

pub const BADGE_ROW: &[Show] = &[
    show("/badge", w::T_BADGES),
    show("/source", w::KIND),
    show("/granted_at", w::CREATED),
    show("/held", w::HELD),
];

async fn user_badges_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "user", CommandOptionType::User)?;
    let record = json(&badges::user_badges_core(db, staff, &id).await?);
    let view = fields(DeskView::new(w::T_BADGES.get(input.lang)), &record["user"], &[show("@handle", w::USER), show("@id", w::ID)], input.lang);
    Ok(paged(view, &items(record["grants"].clone()), BADGE_ROW, input))
}
handler!(user_badges, user_badges_view);

pub const ACTION_ROW: &[Show] = &[
    show("/kind", w::KIND),
    show("/category", w::CATEGORY),
    show("/created_at", w::CREATED),
    show("@id", w::ID),
];

async fn user_history_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "user", CommandOptionType::User)?;
    let history = json(&safety::user_history_core(db, staff, &id).await?);
    let view = fields(
        DeskView::new(w::T_HISTORY.get(input.lang)),
        &history,
        &[show("/actions", w::ACTIONS), show("/appeals", w::APPEALS), show("/reported", w::REPORTS)],
        input.lang,
    );
    Ok(paged(view, &items(history["actions"].clone()), ACTION_ROW, input))
}
handler!(user_history, user_history_view);

// ---- servers and bots ----

pub const SERVER_ROW: &[Show] = &[
    show("/name", w::NAME),
    show("@id", w::ID),
    show("/owner_username", w::OWNER),
    show("/nsfw", w::NSFW),
];

pub const SERVER_VIEW: &[Show] = &[
    show("/name", w::NAME),
    show("@id", w::ID),
    show("/owner_username", w::OWNER),
    show("/owner_id", w::OWNER_ID),
    show("/created_at", w::CREATED),
    show("/members", w::MEMBERS),
    show("/channels", w::CHANNELS),
    show("/roles", w::ROLES),
    show("/reports_about", w::REPORTS),
    show("/flags", w::FLAGS),
    show("/nsfw", w::NSFW),
    show("/description", w::DESCRIPTION),
];

async fn server_find_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let rows = platform::servers_core(db, staff, query(input).as_deref(), None, Some(FETCH)).await?;
    Ok(paged(DeskView::new(w::T_SERVERS.get(input.lang)), &items(json(&rows)), SERVER_ROW, input))
}
handler!(server_find, server_find_view);

async fn server_view_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "server", CommandOptionType::String)?;
    let server = platform::server_core(db, staff, &id).await?;
    Ok(fields(DeskView::new(w::T_SERVER.get(input.lang)), &json(&server), SERVER_VIEW, input.lang))
}
handler!(server_view, server_view_view);

pub const BOT_ROW: &[Show] = &[
    show("/username", w::NAME),
    show("@id", w::ID),
    show("/owner_username", w::OWNER),
    show("/public", w::PUBLIC),
    show("/verified", w::VERIFIED),
    show("/official", w::OFFICIAL),
    show("/suspended", w::SUSPENDED),
];

pub const BOT_VIEW: &[Show] = &[
    show("/username", w::NAME),
    show("@id", w::ID),
    show("/owner_username", w::OWNER),
    show("/owner_id", w::OWNER_ID),
    show("/servers", w::SERVERS),
    show("/public", w::PUBLIC),
    show("/discoverable", w::DISCOVERABLE),
    show("/verified", w::VERIFIED),
    show("/official", w::OFFICIAL),
    show("/suspended", w::SUSPENDED),
    show("/interactions_url", w::INTERACTIONS_URL),
    show("/terms_of_service_url", w::TERMS),
    show("/privacy_policy_url", w::PRIVACY),
    show("/verifications", w::VERIFICATIONS),
];

async fn bot_find_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let rows = bots::list_core(db, staff, query(input).as_deref(), None, Some(FETCH)).await?;
    Ok(paged(DeskView::new(w::T_BOTS.get(input.lang)), &items(json(&rows)), BOT_ROW, input))
}
handler!(bot_find, bot_find_view);

async fn bot_view_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "bot", CommandOptionType::String)?;
    let bot = bots::detail_core(db, staff, &id).await?;
    Ok(fields(DeskView::new(w::T_BOT.get(input.lang)), &json(&bot), BOT_VIEW, input.lang))
}
handler!(bot_view, bot_view_view);

// ---- cases and appeals ----

pub const CASE_ROW: &[Show] = &[
    show("/reference", w::REFERENCE),
    show("/status", w::STATUS),
    show("/category", w::CATEGORY),
    show("/priority", w::PRIORITY),
    show("/created_at", w::CREATED),
    show("@id", w::ID),
];

pub const CASE_VIEW: &[Show] = &[
    show("/case/reference", w::REFERENCE),
    show("/case/_id", w::ID),
    show("/case/status", w::STATUS),
    show("/case/category", w::CATEGORY),
    show("/case/subcategory", w::SUBCATEGORY),
    show("/case/priority", w::PRIORITY),
    show("/case/route", w::ROUTE),
    show("/case/target", w::TARGET),
    show("/case/assignee_id", w::ASSIGNEE),
    show("/case/created_at", w::CREATED),
    show("/case/updated_at", w::UPDATED),
    show("/distinct_reporters", w::REPORTERS),
    show("/evidence", w::EVIDENCE),
    show("/actions", w::ACTIONS),
    show("/appeals", w::APPEALS),
    show("/related", w::RELATED),
];

async fn case_list_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let status = input.text("status");
    let page = safety::cases_core(db, staff, status.as_deref(), None, None, None, None, None, None, None, Some(FETCH)).await?;
    let all = json(&page);
    Ok(paged(DeskView::new(w::T_CASES.get(input.lang)), &items(all["cases"].clone()), CASE_ROW, input))
}
handler!(case_list, case_list_view);

async fn case_view_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "case", CommandOptionType::String)?;
    // the console's audited read: opening a case is recorded
    let details = json(&safety::case_core(db, staff, &id).await?);
    let view = fields(DeskView::new(w::T_CASE.get(input.lang)), &details, CASE_VIEW, input.lang);
    Ok(match details.pointer("/case/description").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        Some(description) => view.line(description.to_string()),
        None => view,
    })
}
handler!(case_view, case_view_view);

pub const APPEAL_ROW: &[Show] = &[
    show("/status", w::STATUS),
    show("/user_id", w::USER),
    show("/created_at", w::CREATED),
    show("@id", w::ID),
];

async fn appeal_list_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let status = input.text("status");
    let list = safety::appeals_core(db, staff, status.as_deref(), None, Some(FETCH)).await?;
    Ok(paged(DeskView::new(w::T_APPEALS.get(input.lang)), &items(json(&list)), APPEAL_ROW, input))
}
handler!(appeal_list, appeal_list_view);

// ---- tickets and bugs ----

pub const TICKET_ROW: &[Show] = &[
    show("/subject", w::SUBJECT),
    show("/status", w::STATUS),
    show("/category", w::CATEGORY),
    show("/user", w::USER),
    show("/updated_at", w::UPDATED),
    show("/waiting", w::WAITING),
    show("@id", w::ID),
];

pub const TICKET_VIEW: &[Show] = &[
    show("/subject", w::SUBJECT),
    show("@id", w::ID),
    show("/status", w::STATUS),
    show("/category", w::CATEGORY),
    show("/user", w::USER),
    show("/user_id", w::ID),
    show("/assignee", w::ASSIGNEE),
    show("/created_at", w::CREATED),
    show("/updated_at", w::UPDATED),
    show("/closed_at", w::CLOSED_AT),
    show("/message_count", w::MESSAGES),
    show("/waiting", w::WAITING),
];

async fn ticket_list_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let status = input.text("status");
    let list = support::tickets_core(db, staff, status.as_deref(), None, None, None, None).await?;
    Ok(paged(DeskView::new(w::T_TICKETS.get(input.lang)), &items(json(&list)), TICKET_ROW, input))
}
handler!(ticket_list, ticket_list_view);

async fn ticket_view_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "ticket", CommandOptionType::String)?;
    let ticket = support::ticket_core(db, staff, &id).await?;
    Ok(fields(DeskView::new(w::T_TICKET.get(input.lang)), &json(&ticket), TICKET_VIEW, input.lang))
}
handler!(ticket_view, ticket_view_view);

pub const BUG_ROW: &[Show] = &[
    show("/report/title", w::TITLE),
    show("/report/status", w::STATUS),
    show("/report/kind", w::KIND),
    show("/report/_id", w::ID),
];

pub const BUG_VIEW: &[Show] = &[
    show("/report/title", w::TITLE),
    show("/report/_id", w::ID),
    show("/report/kind", w::KIND),
    show("/report/status", w::STATUS),
    show("/report/platform", w::PLATFORM),
    show("/report/app_version", w::VERSION),
    show("/author/username", w::AUTHOR),
    show("/possible_duplicates", w::DUPLICATES),
];

async fn bug_list_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let list = review::bugs_core(db, staff, input.boolean("security"), None).await?;
    Ok(paged(DeskView::new(w::T_BUGS.get(input.lang)), &items(json(&list)), BUG_ROW, input))
}
handler!(bug_list, bug_list_view);

async fn bug_view_view(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "bug", CommandOptionType::String)?;
    let bug = json(&review::bug_core(db, staff, &id).await?);
    let view = fields(DeskView::new(w::T_BUG.get(input.lang)), &bug, BUG_VIEW, input.lang);
    Ok(match bug.pointer("/report/description").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        Some(description) => view.line(description.to_string()),
        None => view,
    })
}
handler!(bug_view, bug_view_view);

// ---- the registry entries ----

const fn opt(name: &'static str, kind: CommandOptionType, required: bool, description: Text) -> DeskOption {
    DeskOption {
        name,
        kind,
        required,
        description,
        choices: &[],
        max_length: None,
    }
}

const QUERY: DeskOption = DeskOption {
    max_length: Some(64),
    ..opt("query", CommandOptionType::String, false, t("Name or ID", "الاسم أو المعرّف", "Name oder ID"))
};
const USER_OPT: DeskOption = opt("user", CommandOptionType::User, true, t("The user", "المستخدم", "Der Benutzer"));
const fn id_opt(name: &'static str) -> DeskOption {
    DeskOption {
        max_length: Some(64),
        ..opt(name, CommandOptionType::String, true, t("ID", "المعرّف", "ID"))
    }
}

const fn choice(value: &'static str, en: &'static str, ar: &'static str, de: &'static str) -> DeskChoice {
    DeskChoice { value, name: t(en, ar, de) }
}

const CASE_STATUS: DeskOption = DeskOption {
    choices: &[
        choice("Submitted", "Submitted", "مُرسل", "Eingereicht"),
        choice("Queued", "Queued", "في الطابور", "In Warteschlange"),
        choice("UnderReview", "Under review", "قيد المراجعة", "In Prüfung"),
        choice("MoreInfoRequired", "More info needed", "مطلوب معلومات", "Infos nötig"),
        choice("Escalated", "Escalated", "مُصعّد", "Eskaliert"),
        choice("ActionTaken", "Action taken", "اتُّخذ إجراء", "Maßnahme ergriffen"),
        choice("Closed", "Closed", "مغلق", "Geschlossen"),
    ],
    ..opt("status", CommandOptionType::String, false, t("Status", "الحالة", "Status"))
};

const APPEAL_STATUS: DeskOption = DeskOption {
    choices: &[
        choice("Pending", "Pending", "قيد الانتظار", "Ausstehend"),
        choice("Upheld", "Upheld", "مؤيَّد", "Bestätigt"),
        choice("Overturned", "Overturned", "مُلغى", "Aufgehoben"),
    ],
    ..opt("status", CommandOptionType::String, false, t("Status", "الحالة", "Status"))
};

const TICKET_STATUS: DeskOption = DeskOption {
    choices: &[
        choice("Open", "Open", "مفتوحة", "Offen"),
        choice("Answered", "Answered", "تمت الإجابة", "Beantwortet"),
        choice("Closed", "Closed", "مغلقة", "Geschlossen"),
    ],
    ..opt("status", CommandOptionType::String, false, t("Status", "الحالة", "Status"))
};

const fn read(
    area: &'static str,
    group: &'static str,
    name: &'static str,
    description: Text,
    options: &'static [DeskOption],
    permission: Option<StaffPermission>,
    handler: super::registry::Handler,
) -> DeskCommand {
    DeskCommand {
        area: Some(area),
        group: Some(group),
        name,
        description,
        options,
        permission,
        changes: false,
        irreversible: false,
        prepare: None,
        handler,
    }
}

use StaffPermission as P;

pub static GROUPS: &[DeskGroup] = &[
    DeskGroup { name: "user", description: t("Users", "المستخدمون", "Benutzer") },
    DeskGroup { name: "server", description: t("Servers", "السيرفرات", "Server") },
    DeskGroup { name: "bot", description: t("Bots", "البوتات", "Bots") },
    DeskGroup { name: "case", description: t("Reports", "البلاغات", "Meldungen") },
    DeskGroup { name: "appeal", description: t("Appeals", "الطعون", "Einsprüche") },
    DeskGroup { name: "ticket", description: t("Support tickets", "تذاكر الدعم", "Support-Tickets") },
    DeskGroup { name: "bug", description: t("Bug reports", "بلاغات الأخطاء", "Fehlerberichte") },
];

pub static ENTRIES: &[DeskCommand] = &[
    read("lookup", "user", "find", t("Find users", "البحث عن مستخدمين", "Benutzer suchen"), &[QUERY], Some(P::UsersRead), user_find),
    read("lookup", "user", "view", t("Show a user", "عرض مستخدم", "Benutzer zeigen"), &[USER_OPT], Some(P::UsersRead), user_view),
    read("lookup", "user", "badges", t("A user's badges", "شارات مستخدم", "Abzeichen"), &[USER_OPT], Some(P::UsersRead), user_badges),
    read("lookup", "user", "history", t("Safety history", "سجل السلامة", "Sicherheitsverlauf"), &[USER_OPT], Some(P::ReportsRead), user_history),
    read("lookup", "server", "find", t("Find servers", "البحث عن سيرفرات", "Server suchen"), &[QUERY], Some(P::ServersRead), server_find),
    read("lookup", "server", "view", t("Show a server", "عرض سيرفر", "Server zeigen"), &[id_opt("server")], Some(P::ServersRead), server_view),
    read("lookup", "bot", "find", t("Find bots", "البحث عن بوتات", "Bots suchen"), &[QUERY], Some(P::UsersRead), bot_find),
    read("lookup", "bot", "view", t("Show a bot", "عرض بوت", "Bot zeigen"), &[id_opt("bot")], Some(P::UsersRead), bot_view),
    read("lookup", "case", "list", t("Report queue", "طابور البلاغات", "Meldungen"), &[CASE_STATUS], Some(P::ReportsRead), case_list),
    read("lookup", "case", "view", t("Show a report", "عرض بلاغ", "Meldung zeigen"), &[id_opt("case")], Some(P::ReportsRead), case_view),
    read("lookup", "appeal", "list", t("Appeal queue", "طابور الطعون", "Einsprüche"), &[APPEAL_STATUS], Some(P::ReportsRead), appeal_list),
    read("lookup", "ticket", "list", t("Support tickets", "تذاكر الدعم", "Support-Tickets"), &[TICKET_STATUS], Some(P::SupportRead), ticket_list),
    read("lookup", "ticket", "view", t("Show a ticket", "عرض تذكرة", "Ticket zeigen"), &[id_opt("ticket")], Some(P::SupportRead), ticket_view),
    // bugs: Support or Security, decided by the console's core
    read(
        "lookup",
        "bug",
        "list",
        t("Bug reports", "بلاغات الأخطاء", "Fehlerberichte"),
        &[opt("security", CommandOptionType::Boolean, false, t("Security reports", "بلاغات أمنية", "Sicherheitsmeldungen"))],
        None,
        bug_list,
    ),
    read("lookup", "bug", "view", t("Show a bug report", "عرض بلاغ خطأ", "Fehlerbericht zeigen"), &[id_opt("bug")], None, bug_view),
];
