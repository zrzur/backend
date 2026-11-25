//! Desk reviews (area `review`, command `/desk-review`): Discover
//! requests, bot verification, designs, plugins, creators, the word list,
//! badges and the official mark. Every change makes a confirmation card at
//! `/run` and runs on confirmation through the console's own core, with
//! its rules and records. A fresh code wherever the console steps up, and
//! for removals from Discover, taking a design down, plugin revocations
//! and removals, revoking a creator and every change of the official mark.
//!
//! The console's design file proxy is not offered: it needs the console's
//! own session headers, so a bot could never open its links. The design
//! review card shows the design's name, kind and creator instead.

use futures::future::BoxFuture;
use revolt_database::{
    AuditOutcome, BadgeGrantRule, BotVerificationStatus, Database, DiscoverRequestType, PluginRisk,
    StaffPermission, User, is_seeded_official, record_platform_audit,
};
use revolt_models::v0::{
    CommandOptionType, DataDiscoverDecision, DataPlatformWord, GatewayIntents,
    InteractionDataOption, InteractionOptionValue,
};
use revolt_result::{Result, create_error};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::super::badges::{DataBadgeChange, grant_core, revoke_core};
use super::super::bots::{
    DataUnverify, DataVerificationDecision, decide_verification_core, unverify_core,
};
use super::super::creators::{DataCreatorChange, change_core};
use super::super::discover::decide_core;
use super::super::official::{
    DataOfficialProposal, DataOfficialRefusal, confirm_core, propose_core, refuse_core, status_of,
};
use super::super::review::{
    DataStaffDesignDecision, DataStaffPluginDecision, design_decide_core, plugin_decide_core,
};
use super::super::words::{DataAddWord, DataWordCheck, add_core, check_core, remove_core};
use super::super::{DataReason, StaffContext};
use super::lookup::{Show, fields};
use super::registry::{
    DeskChoice, DeskCommand, DeskGroup, DeskInput, DeskOption, Lang, Prepare, Prepared, Text,
};
use super::view::DeskView;
use crate::util::on_heap::on_heap;

const fn t(en: &'static str, ar: &'static str, de: &'static str) -> Text {
    Text { en, ar, de }
}

const fn show(key: &'static str, label: Text) -> Show {
    Show { key, label }
}

/// The name of the stored design version
const VERSION: &str = "_version";

mod w {
    use super::{Text, t};

    pub const DONE: Text = t("Done", "تم", "Erledigt");
    pub const ID: Text = t("ID", "المعرّف", "ID");
    pub const STATUS: Text = t("Status", "الحالة", "Status");
    pub const NAME: Text = t("Name", "الاسم", "Name");
    pub const USER: Text = t("User", "المستخدم", "Benutzer");
    pub const BADGE: Text = t("Badge", "الشارة", "Abzeichen");
    pub const CATEGORY: Text = t("Category", "الفئة", "Kategorie");
    pub const MATCH: Text = t("The list says", "تقول القائمة", "Die Liste sagt");
    pub const NO_MATCH: Text = t("nothing", "لا شيء", "nichts");

    pub const T_DISCOVER: Text = t(
        "Decide a Discover request",
        "البت في طلب استكشاف",
        "Discover-Anfrage entscheiden",
    );
    pub const T_VERIFY: Text = t(
        "Decide a bot verification",
        "البت في توثيق بوت",
        "Bot-Verifizierung entscheiden",
    );
    pub const T_UNVERIFY: Text = t(
        "Take a bot's mark away",
        "سحب علامة توثيق بوت",
        "Bot-Markierung entfernen",
    );
    pub const T_DESIGN: Text = t("Review a design", "مراجعة تصميم", "Design prüfen");
    pub const T_PLUGIN: Text = t("Review a plugin", "مراجعة إضافة", "Plugin prüfen");
    pub const T_CREATOR: Text = t("Change a creator", "تغيير حالة مبدع", "Creator ändern");
    pub const T_WORD_ADD: Text = t(
        "Add to the word list",
        "إضافة إلى قائمة الكلمات",
        "Zur Wortliste hinzufügen",
    );
    pub const T_WORD_REMOVE: Text = t(
        "Remove from the word list",
        "حذف من قائمة الكلمات",
        "Aus der Wortliste entfernen",
    );
    pub const T_WORD_CHECK: Text = t("Word list check", "فحص قائمة الكلمات", "Wortlistenprüfung");
    pub const T_BADGE_GIVE: Text = t("Give a badge", "منح شارة", "Abzeichen vergeben");
    pub const T_BADGE_REMOVE: Text = t("Take a badge away", "سحب شارة", "Abzeichen entziehen");
    pub const T_PROPOSE: Text = t(
        "Propose the official mark",
        "اقتراح العلامة الرسمية",
        "Offizielle Markierung vorschlagen",
    );
    pub const T_CONFIRM: Text = t(
        "Confirm an official mark proposal",
        "تأكيد اقتراح العلامة الرسمية",
        "Vorschlag bestätigen",
    );
    pub const T_REFUSE: Text = t(
        "Refuse an official mark proposal",
        "رفض اقتراح العلامة الرسمية",
        "Vorschlag ablehnen",
    );

    pub const DISCOVER: Text = t(
        "Decides the Discover request of {kind} {name} ({id}): {decision}.",
        "يبت في طلب الاستكشاف لـ{kind} {name} ({id}): {decision}.",
        "Entscheidet die Discover-Anfrage von {kind} {name} ({id}): {decision}.",
    );
    pub const FEATURED: Text = t("Featured: {on}", "مميّز: {on}", "Hervorgehoben: {on}");
    pub const OWNER_READS: Text = t(
        "The owner reads: {reason}",
        "يقرأ المالك: {reason}",
        "Die Besitzer lesen: {reason}",
    );
    pub const VERIFY: Text = t(
        "Decides the verification request {id} of the bot {bot} ({bot_id}), owned by {owner}: {decision}.",
        "يبت في طلب التوثيق {id} للبوت {bot} ({bot_id}) الذي يملكه {owner}: {decision}.",
        "Entscheidet die Verifizierungsanfrage {id} des Bots {bot} ({bot_id}) von {owner}: {decision}.",
    );
    pub const UNVERIFY: Text = t(
        "Takes the verified mark away from the bot {bot} ({id}), owned by {owner}; the owner is told.",
        "يسحب علامة التوثيق من البوت {bot} ({id}) الذي يملكه {owner}؛ ويُبلَّغ المالك.",
        "Entfernt die Verifizierung des Bots {bot} ({id}) von {owner}; die Besitzer werden informiert.",
    );
    pub const DESIGN: Text = t(
        "Decides the design {name} ({kind}, {id}) by {creator}: {decision}.",
        "يبت في التصميم {name} ({kind}، {id}) من {creator}: {decision}.",
        "Entscheidet das Design {name} ({kind}, {id}) von {creator}: {decision}.",
    );
    pub const CREATOR_READS: Text = t(
        "The creator reads: {reason}",
        "يقرأ المبدع: {reason}",
        "Die Creator lesen: {reason}",
    );
    pub const PLUGIN_RELEASE: Text = t(
        "Decides the release {version} ({id}) of the plugin {name} by {publisher}, risk {risk}: {decision}.",
        "يبت في النسخة {version} ({id}) من الإضافة {name} للناشر {publisher}، والخطورة {risk}: {decision}.",
        "Entscheidet die Version {version} ({id}) des Plugins {name} von {publisher}, Risiko {risk}: {decision}.",
    );
    pub const PLUGIN_PLUGIN: Text = t(
        "Decides the plugin {name} ({id}), version {version}, by {publisher}: {decision}.",
        "يبت في الإضافة {name} ({id})، النسخة {version}، للناشر {publisher}: {decision}.",
        "Entscheidet das Plugin {name} ({id}), Version {version}, von {publisher}: {decision}.",
    );
    pub const PLUGIN_PUBLISHER: Text = t(
        "Decides the publisher {publisher}: {decision}.",
        "يبت في الناشر {publisher}: {decision}.",
        "Entscheidet über die Herausgeber {publisher}: {decision}.",
    );
    pub const RISK_LOW: Text = t("low", "منخفضة", "niedrig");
    pub const RISK_MEDIUM: Text = t("medium", "متوسطة", "mittel");
    pub const RISK_HIGH: Text = t("high", "عالية", "hoch");
    pub const INTENTS: Text = t(
        "It asks for the privileged intents {intents}; approving grants them.",
        "يطلب صلاحيات البوابة المميّزة: {intents}؛ والقبول يمنحها.",
        "Er fragt nach den privilegierten Intents {intents}; die Annahme gewährt sie.",
    );
    pub const MESSAGE_CONTENT: Text = t("message content", "محتوى الرسائل", "Nachrichteninhalte");
    pub const SERVER_MEMBERS: Text = t("server members", "أعضاء السيرفرات", "Servermitglieder");
    pub const PRESENCE: Text = t("presence", "حالة الاتصال", "Online-Status");
    pub const DECORATION: Text = t(
        "avatar decoration",
        "زخرفة الصورة الرمزية",
        "Avatar-Dekoration",
    );
    pub const NAMEPLATE: Text = t("nameplate", "لوحة الاسم", "Namensschild");
    pub const EFFECT: Text = t("profile effect", "تأثير الملف الشخصي", "Profileffekt");
    pub const FRAME: Text = t("profile frame", "إطار الملف الشخصي", "Profilrahmen");
    /// Zrzur Desk's own rule, stricter than the console
    pub const ABOUT_YOURSELF: &str = "a decision about yourself";
    pub const REASON: Text = t("Reason: {reason}", "السبب: {reason}", "Grund: {reason}");
    pub const HIGH_RISK: Text = t(
        "A high-risk release is approved knowingly.",
        "تُعتمد نسخة عالية الخطورة عن علم.",
        "Eine Version mit hohem Risiko wird bewusst freigegeben.",
    );
    pub const VERIFY_PUBLISHER: Text = t(
        "The publisher is marked verified too.",
        "ويُوثَّق الناشر أيضًا.",
        "Die Herausgeber werden auch verifiziert.",
    );
    pub const CREATOR: Text = t(
        "Changes the creator {user}: {action}; they are told the reason: {reason}",
        "يغيّر حالة المبدع {user}: {action}؛ ويُبلَّغ بالسبب: {reason}",
        "Ändert die Creator-Person {user}: {action}; sie erfährt den Grund: {reason}",
    );
    pub const WORD_ADD: Text = t(
        "Adds \"{pattern}\" to the word list as {kind} ({category}).",
        "يضيف «{pattern}» إلى قائمة الكلمات بصفة {kind} ({category}).",
        "Fügt „{pattern}“ der Wortliste als {kind} hinzu ({category}).",
    );
    pub const BLOCKED: Text = t("blocked", "محظورة", "gesperrt");
    pub const ALLOWED: Text = t("allowed", "مسموحة", "erlaubt");
    pub const WORD_REMOVE: Text = t(
        "Removes \"{pattern}\" ({id}) from the word list.",
        "يحذف «{pattern}» ({id}) من قائمة الكلمات.",
        "Entfernt „{pattern}“ ({id}) aus der Wortliste.",
    );
    pub const BADGE_GIVE: Text = t(
        "Gives {user} the badge {badge}; they are told.",
        "يمنح {user} شارة {badge}؛ ويُبلَّغ بذلك.",
        "Gibt {user} das Abzeichen {badge}; die Person wird informiert.",
    );
    pub const BADGE_UNTIL: Text = t("Until {time}.", "حتى {time}.", "Bis {time}.");
    pub const SECOND_PERSON: Text = t(
        "This change waits for a second staff member to approve it.",
        "ينتظر هذا التغيير موافقة عضو آخر في الإدارة.",
        "Diese Änderung wartet auf die Zustimmung eines zweiten Teammitglieds.",
    );
    pub const BADGE_REMOVE: Text = t(
        "Takes the badge {badge} away from {user}; they are told.",
        "يسحب شارة {badge} من {user}؛ ويُبلَّغ بذلك.",
        "Entzieht {user} das Abzeichen {badge}; die Person wird informiert.",
    );
    pub const PROPOSE_GIVE: Text = t(
        "Proposes giving {user} the official mark. Nothing changes until another staff member confirms within 7 days.",
        "يقترح منح {user} العلامة الرسمية. لا يتغير شيء حتى يؤكد عضو آخر في الإدارة خلال 7 أيام.",
        "Schlägt vor, {user} die offizielle Markierung zu geben. Nichts ändert sich, bis eine andere Person aus dem Team binnen 7 Tagen bestätigt.",
    );
    pub const PROPOSE_REMOVE: Text = t(
        "Proposes removing the official mark from {user}. Nothing changes until another staff member confirms within 7 days.",
        "يقترح سحب العلامة الرسمية من {user}. لا يتغير شيء حتى يؤكد عضو آخر في الإدارة خلال 7 أيام.",
        "Schlägt vor, {user} die offizielle Markierung zu entziehen. Nichts ändert sich, bis eine andere Person aus dem Team binnen 7 Tagen bestätigt.",
    );
    pub const CONFIRM_GIVE: Text = t(
        "Confirms proposal {id} by {by}: {user} gets the official mark now.",
        "يؤكد الاقتراح {id} من {by}: يحصل {user} على العلامة الرسمية الآن.",
        "Bestätigt Vorschlag {id} von {by}: {user} erhält jetzt die offizielle Markierung.",
    );
    pub const CONFIRM_REMOVE: Text = t(
        "Confirms proposal {id} by {by}: {user} loses the official mark now.",
        "يؤكد الاقتراح {id} من {by}: يفقد {user} العلامة الرسمية الآن.",
        "Bestätigt Vorschlag {id} von {by}: {user} verliert jetzt die offizielle Markierung.",
    );
    pub const REFUSE: Text = t(
        "Refuses proposal {id} by {by} about {user}; nothing changes.",
        "يرفض الاقتراح {id} من {by} بشأن {user}؛ ولا يتغير شيء.",
        "Lehnt Vorschlag {id} von {by} zu {user} ab; nichts ändert sich.",
    );
    pub const WITHDRAW: Text = t(
        "Withdraws your own proposal {id} about {user}; nothing changes.",
        "يسحب اقتراحك {id} بشأن {user}؛ ولا يتغير شيء.",
        "Zieht deinen Vorschlag {id} zu {user} zurück; nichts ändert sich.",
    );
}

// ---- helpers ----

fn handle(user: &User) -> String {
    revolt_models::v0::user_handle(&user.username, &user.discriminator)
}

async fn person(db: &Database, id: &str) -> String {
    match db.fetch_user(id).await {
        Ok(user) => super::registry::person_line(&user),
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

fn parse<T: DeserializeOwned>(name: &str) -> Result<T> {
    serde_json::from_value(Value::String(name.to_string()))
        .map_err(|_| create_error!(InvalidOperation))
}

fn json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn named(option: &DeskOption, value: &str, lang: Lang) -> String {
    option
        .choices
        .iter()
        .find(|choice| choice.value == value)
        .map(|choice| choice.name.get(lang).to_string())
        .unwrap_or_else(|| value.to_string())
}

fn yes_no(on: bool, lang: Lang) -> &'static str {
    if on {
        t("yes", "نعم", "ja").get(lang)
    } else {
        t("no", "لا", "nein").get(lang)
    }
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

const STATUS_DONE: &[Show] = &[
    show("@id", w::ID),
    show("/status", w::STATUS),
    show("/name", w::NAME),
];
const BADGE_DONE: &[Show] = &[show("/badge", w::BADGE), show("/status", w::STATUS)];
const OFFICIAL_DONE: &[Show] = &[
    show("/id", w::ID),
    show("/username", w::USER),
    show("/status", w::STATUS),
];
const WORD_DONE: &[Show] = &[
    show("@id", w::ID),
    show("/pattern", w::NAME),
    show("/category", w::CATEGORY),
];
const CREATOR_DONE: &[Show] = &[show("/user/username", w::USER), show("/creator", w::STATUS)];

// ---- Discover ----

/// Zrzur Desk's own rule (the console has none): nobody decides about
/// their own server's or bot's listing
async fn own_listing(db: &Database, staff: &StaffContext, kind: &str, id: &str) -> bool {
    let owner = match kind {
        "server" => db.fetch_server(id).await.map(|s| s.owner).ok(),
        "bot" => db.fetch_bot(id).await.map(|b| b.owner).ok(),
        _ => None,
    };
    owner.as_deref() == Some(staff.user.id.as_str())
}

async fn discover_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let kind = required(input, "kind")?;
    let id = required(input, "id")?;
    let decision = required(input, "decision")?;
    let request_type = match kind.as_str() {
        "server" => DiscoverRequestType::Server,
        "bot" => DiscoverRequestType::Bot,
        _ => return Err(create_error!(InvalidOperation)),
    };
    // the console core's own checks, before a card is made
    if matches!(decision.as_str(), "Deny" | "Remove")
        && optional(input, "reason").is_none_or(|r| r.chars().count() < 3)
    {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::length("reason", 3, 512),
        ]));
    }
    db.fetch_discover_request_by_item_id(request_type, &id)
        .await?;
    if own_listing(db, staff, &kind, &id).await {
        return Err(create_error!(NotPrivileged));
    }
    let name = match kind.as_str() {
        "server" => db
            .fetch_server(&id)
            .await
            .map(|s| s.name)
            .unwrap_or_default(),
        _ => db
            .fetch_user(&id)
            .await
            .map(|u| handle(&u))
            .unwrap_or_default(),
    };
    let mut lines = vec![w::DISCOVER.fill(
        lang,
        &[
            ("kind", &named(&KIND, &kind, lang)),
            ("name", &name),
            ("id", &id),
            ("decision", &named(&DECISION, &decision, lang)),
        ],
    )];
    // featured goes with an approval only
    if let Some(featured) = input.boolean("featured").filter(|_| decision == "Approve") {
        lines.push(w::FEATURED.fill(lang, &[("on", yes_no(featured, lang))]));
    }
    if let Some(reason) = optional(input, "reason") {
        lines.push(w::OWNER_READS.fill(lang, &[("reason", &reason)]));
    }
    let mut prepared = prepared(&kind, &id, w::T_DISCOVER, lang, lines);
    // taking a listing off Discover needs a fresh code
    prepared.irreversible = decision == "Remove";
    Ok(prepared)
}

async fn discover_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let decision = required(input, "decision")?;
    let data = DataDiscoverDecision {
        featured: input.boolean("featured").filter(|_| decision == "Approve"),
        decision: Some(parse(&decision)?),
        reason: optional(input, "reason"),
    };
    let id = required(input, "id")?;
    let kind = required(input, "kind")?;
    on_heap(|| decide_core(db, staff, &kind, &id, data)).await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::ID.get(input.lang), id))
}
async fn discover_self(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let (kind, id) = (required(input, "kind")?, required(input, "id")?);
    Ok(own_listing(db, staff, &kind, &id)
        .await
        .then_some(w::ABOUT_YOURSELF))
}

/// Zrzur Desk's own rule: no change about yourself (creator, badges)
async fn user_self(
    _: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    Ok(
        (input.user("user").as_deref() == Some(staff.user.id.as_str()))
            .then_some(w::ABOUT_YOURSELF),
    )
}

boxed!(discover_decide_card, discover_card, Prepared);
boxed!(discover_decide_verify, discover_self, Option<&'static str>);
boxed!(about_yourself_verify, user_self, Option<&'static str>);
boxed!(discover_decide, discover_run, DeskView);

// ---- bot verification ----

async fn bot_names(db: &Database, bot_id: &str) -> Result<(String, String)> {
    let bot = db.fetch_bot(bot_id).await?;
    let name = db
        .fetch_user(&bot.id)
        .await
        .map(|u| handle(&u))
        .unwrap_or_default();
    Ok((name, person(db, &bot.owner).await))
}

async fn verify_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "request")?;
    let request = db.fetch_bot_verification(&id).await?;
    let decision = required(input, "decision")?;
    // the console core's own checks, before a card is made
    if request.status != BotVerificationStatus::Pending {
        return Err(create_error!(InvalidOperation));
    }
    if decision != "approve" && optional(input, "note").is_none() {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("note", "required"),
        ]));
    }
    let bot = db.fetch_bot(&request.bot_id).await?;
    // nobody verifies their own bot
    if bot.owner == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let (name, owner) = bot_names(db, &bot.id).await?;
    let mut lines = vec![w::VERIFY.fill(
        lang,
        &[
            ("id", &id),
            ("bot", &name),
            ("bot_id", &bot.id),
            ("owner", &owner),
            ("decision", &named(&VERIFY_DECISION, &decision, lang)),
        ],
    )];
    let privileged = request.intents as u64 & GatewayIntents::PRIVILEGED;
    if privileged != 0 {
        lines.push(w::INTENTS.fill(lang, &[("intents", &intent_names(privileged, lang))]));
    }
    if let Some(note) = optional(input, "note") {
        lines.push(w::OWNER_READS.fill(lang, &[("reason", &note)]));
    }
    let mut prepared = prepared("bot", &bot.id, w::T_VERIFY, lang, lines);
    // granting privileged intents needs a fresh code
    prepared.irreversible = decision == "approve" && privileged != 0;
    Ok(prepared)
}

/// The privileged intents among `bits`, by name in `lang`
fn intent_names(bits: u64, lang: Lang) -> String {
    [
        (GatewayIntents::MESSAGE_CONTENT, w::MESSAGE_CONTENT),
        (GatewayIntents::SERVER_MEMBERS, w::SERVER_MEMBERS),
        (GatewayIntents::PRESENCE, w::PRESENCE),
    ]
    .into_iter()
    .filter(|(bit, _)| bits & bit != 0)
    .map(|(_, name)| name.get(lang))
    .collect::<Vec<_>>()
    .join(", ")
}

async fn verify_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataVerificationDecision {
        approve: required(input, "decision")? == "approve",
        note: optional(input, "note"),
    };
    let request = required(input, "request")?;
    let view = on_heap(|| decide_verification_core(db, staff, &request, data)).await?;
    Ok(done(json(&view), STATUS_DONE, input.lang))
}
boxed!(verification_decide_card, verify_card, Prepared);
boxed!(verification_decide, verify_run, DeskView);

async fn unverify_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "bot")?;
    let (name, owner) = bot_names(db, &id).await?;
    let reason = required(input, "reason")?;
    Ok(prepared(
        "bot",
        &id,
        w::T_UNVERIFY,
        lang,
        vec![
            w::UNVERIFY.fill(lang, &[("bot", &name), ("id", &id), ("owner", &owner)]),
            w::OWNER_READS.fill(lang, &[("reason", &reason)]),
        ],
    ))
}

async fn unverify_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "bot")?;
    let data = DataUnverify {
        reason: required(input, "reason")?,
    };
    on_heap(|| unverify_core(db, staff, &id, data)).await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::ID.get(input.lang), id))
}
boxed!(bot_unverify_card, unverify_card, Prepared);
boxed!(bot_unverify, unverify_run, DeskView);

// ---- designs and plugins ----

async fn design_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "design")?;
    let item = db.fetch_community_item(&id).await?;
    // nobody decides on their own work
    if item.creator_id == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let action = required(input, "action")?;
    let kind = match json(&item.kind).as_str().unwrap_or_default() {
        "Decoration" => w::DECORATION.get(lang).to_string(),
        "Nameplate" => w::NAMEPLATE.get(lang).to_string(),
        "Effect" => w::EFFECT.get(lang).to_string(),
        "Frame" => w::FRAME.get(lang).to_string(),
        other => other.to_string(),
    };
    let mut lines = vec![w::DESIGN.fill(
        lang,
        &[
            ("name", &item.name),
            ("kind", &kind),
            ("id", &id),
            ("creator", &person(db, &item.creator_id).await),
            ("decision", &named(&DESIGN_ACTION, &action, lang)),
        ],
    )];
    if let Some(reason) = optional(input, "reason") {
        lines.push(w::CREATOR_READS.fill(lang, &[("reason", &reason)]));
    }
    let mut prepared = prepared("community_item", &id, w::T_DESIGN, lang, lines);
    // the design as read: changed since, the console refuses
    prepared.stored = vec![InteractionDataOption {
        name: VERSION.to_string(),
        kind: CommandOptionType::Integer,
        value: Some(InteractionOptionValue::Integer(
            json(&item)["version"].as_i64().unwrap_or_default(),
        )),
        options: None,
    }];
    Ok(prepared)
}

async fn design_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataStaffDesignDecision {
        action: required(input, "action")?,
        reason: optional(input, "reason"),
        version: input
            .integer(VERSION)
            .ok_or_else(|| create_error!(InvalidOperation))?,
    };
    let design = required(input, "design")?;
    let item = on_heap(|| design_decide_core(db, staff, &design, data)).await?;
    Ok(done(json(&item), STATUS_DONE, input.lang))
}
boxed!(design_review_card, design_card, Prepared);
boxed!(design_review, design_run, DeskView);

async fn plugin_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "id")?;
    let action = required(input, "action")?;
    let decision = named(&PLUGIN_ACTION, &action, lang);
    let target_kind = plugin_target(&action)?;
    // what the core acts on, read now: an unknown id is refused here
    let line = match target_kind {
        "plugin_release" => {
            let release = db.fetch_plugin_release(&id).await?;
            let plugin = db.fetch_plugin(&release.plugin_id).await?;
            let risk = match release.risk {
                PluginRisk::Low => w::RISK_LOW,
                PluginRisk::Medium => w::RISK_MEDIUM,
                PluginRisk::High => w::RISK_HIGH,
            };
            w::PLUGIN_RELEASE.fill(
                lang,
                &[
                    ("version", &release.version),
                    ("id", &id),
                    ("name", &plugin.name),
                    ("publisher", &person(db, &plugin.publisher_id).await),
                    ("risk", risk.get(lang)),
                    ("decision", &decision),
                ],
            )
        }
        "plugin" => {
            let plugin = db.fetch_plugin(&id).await?;
            w::PLUGIN_PLUGIN.fill(
                lang,
                &[
                    ("name", &plugin.name),
                    ("id", &id),
                    ("version", plugin.latest_version.as_deref().unwrap_or("-")),
                    ("publisher", &person(db, &plugin.publisher_id).await),
                    ("decision", &decision),
                ],
            )
        }
        _ => {
            let user = db.fetch_user(&id).await?;
            w::PLUGIN_PUBLISHER.fill(
                lang,
                &[
                    ("publisher", &super::registry::person_line(&user)),
                    ("decision", &decision),
                ],
            )
        }
    };
    let mut lines = vec![line];
    if let Some(reason) = optional(input, "reason") {
        lines.push(w::REASON.fill(lang, &[("reason", &reason)]));
    }
    if input.boolean("acknowledge_high_risk") == Some(true) {
        lines.push(w::HIGH_RISK.get(lang).to_string());
    }
    if input.boolean("verify_publisher") == Some(true) {
        lines.push(w::VERIFY_PUBLISHER.get(lang).to_string());
    }
    Ok(prepared(target_kind, &id, w::T_PLUGIN, lang, lines))
}

/// What a plugin action is about, as the console core records it: a
/// release, a plugin or a publisher
fn plugin_target(action: &str) -> Result<&'static str> {
    Ok(match action {
        "approve" | "reject" | "revoke" => "plugin_release",
        "suspend" | "reinstate" | "remove" => "plugin",
        "verify" | "suspend_publisher" | "reinstate_publisher" => "user",
        _ => return Err(create_error!(InvalidOperation)),
    })
}

async fn plugin_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataStaffPluginDecision {
        action: required(input, "action")?,
        reason: optional(input, "reason"),
        verify_publisher: input.boolean("verify_publisher") == Some(true),
        acknowledge_high_risk: input.boolean("acknowledge_high_risk") == Some(true),
    };
    let id = required(input, "id")?;
    on_heap(|| plugin_decide_core(db, staff, &id, data)).await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::ID.get(input.lang), id))
}
boxed!(plugin_review_card, plugin_card, Prepared);
boxed!(plugin_review, plugin_run, DeskView);

// ---- creators and words ----

async fn creator_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    // Zrzur Desk's own rule: not about yourself
    if user == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let action = required(input, "action")?;
    let reason = required(input, "reason")?;
    let mut prepared = prepared(
        "user",
        &user,
        w::T_CREATOR,
        lang,
        vec![w::CREATOR.fill(
            lang,
            &[
                ("user", &person(db, &user).await),
                ("action", &named(&CREATOR_ACTION, &action, lang)),
                ("reason", &reason),
            ],
        )],
    );
    // revoking or suspending a creator needs a fresh code
    prepared.irreversible = matches!(action.as_str(), "revoke" | "suspend");
    Ok(prepared)
}

async fn creator_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataCreatorChange {
        action: required(input, "action")?,
        reason: required(input, "reason")?,
    };
    let user = required_user(input, "user")?;
    let record = on_heap(|| change_core(db, staff, &user, data)).await?;
    Ok(done(json(&record), CREATOR_DONE, input.lang))
}
boxed!(creator_change_card, creator_card, Prepared);
boxed!(creator_change, creator_run, DeskView);

async fn word_add_card(_: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let pattern = required(input, "pattern")?;
    let allow = input.boolean("allow") == Some(true);
    let category = required(input, "category")?;
    let kind = if allow { w::ALLOWED } else { w::BLOCKED };
    Ok(prepared(
        "platform_word",
        &pattern,
        w::T_WORD_ADD,
        lang,
        vec![
            w::WORD_ADD.fill(
                lang,
                &[
                    ("pattern", &pattern),
                    ("kind", kind.get(lang)),
                    ("category", &named(&WORD_CATEGORY, &category, lang)),
                ],
            ),
            w::REASON.fill(lang, &[("reason", &required(input, "reason")?)]),
        ],
    ))
}

async fn word_add_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataAddWord {
        word: DataPlatformWord {
            pattern: required(input, "pattern")?,
            category: parse(&required(input, "category")?)?,
            allow: input.boolean("allow") == Some(true),
            note: optional(input, "note"),
        },
        reason: required(input, "reason")?,
    };
    let word = add_core(db, staff, data).await?;
    Ok(done(json(&word), WORD_DONE, input.lang))
}
boxed!(words_add_card, word_add_card, Prepared);
boxed!(words_add, word_add_run, DeskView);

async fn word_remove_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "word")?;
    let pattern = db
        .fetch_platform_words()
        .await?
        .into_iter()
        .find(|word| word.id == id)
        .map(|word| word.pattern)
        .ok_or_else(|| create_error!(NotFound))?;
    Ok(prepared(
        "platform_word",
        &id,
        w::T_WORD_REMOVE,
        lang,
        vec![
            w::WORD_REMOVE.fill(lang, &[("pattern", &pattern), ("id", &id)]),
            w::REASON.fill(lang, &[("reason", &required(input, "reason")?)]),
        ],
    ))
}

async fn word_remove_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let id = required(input, "word")?;
    remove_core(
        db,
        staff,
        &id,
        DataReason {
            reason: required(input, "reason")?,
        },
    )
    .await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::ID.get(input.lang), id))
}
boxed!(words_remove_card, word_remove_card, Prepared);
boxed!(words_remove, word_remove_run, DeskView);

async fn word_check_view(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let lang = input.lang;
    let check = check_core(
        db,
        staff,
        DataWordCheck {
            text: required(input, "text")?,
        },
    )
    .await?;
    let said = match json(&check)["category"].as_str() {
        Some(category) => named(&WORD_CATEGORY, category, lang),
        None => w::NO_MATCH.get(lang).to_string(),
    };
    Ok(DeskView::new(w::T_WORD_CHECK.get(lang)).field(w::MATCH.get(lang), said))
}
boxed!(words_check, word_check_view, DeskView);

// ---- badges ----

/// The badge's name in `lang`, or its id
fn badge_name(id: &str, lang: Lang) -> String {
    let names = crate::util::official::badge_names(id);
    let code = match lang {
        Lang::Ar => "ar",
        Lang::De => "de",
        Lang::En => "en",
    };
    names
        .get(code)
        .or_else(|| names.get("en"))
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

/// Beta's single badge gate, asked at card time as the confirmation will
/// ask it (Zrzur Desk's path, with the fresh code the card asks for):
/// refused here with a Denied record in the gate's own form; gives whether
/// the change needs the Owner (Privileged or Historical rule) and whether
/// it waits for a second staff member
async fn badge_gate(
    db: &Database,
    staff: &StaffContext,
    user: &str,
    badge: &str,
    give: bool,
) -> Result<(bool, bool)> {
    use revolt_database::{BadgeChange, BadgeDecision, BadgeMutation, BadgePath};
    let change = if give {
        BadgeChange::Grant
    } else {
        BadgeChange::Revoke
    };
    let mut actor = super::super::badges::actor_on(staff, BadgePath::Desk);
    // the confirmation needs a fresh code: the card checks the rest
    actor.step_up = true;
    let mutation = BadgeMutation {
        user_id: user.to_string(),
        badge: badge.to_string(),
        change,
        reason: String::new(),
        expires_at: None,
        source_ref: None,
    };
    match revolt_database::authorize_badge_mutation(db, &actor, &mutation).await? {
        Ok(authorized) => Ok((
            matches!(
                authorized.definition.rule,
                BadgeGrantRule::Privileged | BadgeGrantRule::Historical
            ),
            authorized.decision == BadgeDecision::NeedsSecondPerson,
        )),
        Err(denial) => {
            let word = if give { "grant" } else { "revoke" };
            let mut record = staff.record(
                &format!("staff.desk.badge.{word}"),
                Some(StaffPermission::BadgesManage),
            );
            record.target_kind = Some("user".to_string());
            record.target_id = Some(user.to_string());
            // the catalogue's id, never the typed text
            let known = revolt_database::badge_definition(badge)
                .map(|d| d.id)
                .unwrap_or("unknown");
            record.summary = Some(format!("{known} denied:{} via desk", denial.code()));
            record_platform_audit(db, record, AuditOutcome::Denied).await?;
            Err(denial.error())
        }
    }
}

async fn badge_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
    give: bool,
) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    let badge = required(input, "badge")?;
    // Zrzur Desk's own rule: not about yourself
    if user == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let (privileged, second_person) = badge_gate(db, staff, &user, &badge, give).await?;
    let who = person(db, &user).await;
    let (title, text) = if give {
        (w::T_BADGE_GIVE, w::BADGE_GIVE)
    } else {
        (w::T_BADGE_REMOVE, w::BADGE_REMOVE)
    };
    let mut lines = vec![text.fill(
        lang,
        &[("user", &who), ("badge", &badge_name(&badge, lang))],
    )];
    if give {
        if let Some(days) = input.integer("days") {
            let until = staff.now + days * 24 * 60 * 60 * 1000;
            lines.push(w::BADGE_UNTIL.fill(lang, &[("time", &super::registry::when(until))]));
        }
    }
    if second_person {
        lines.push(w::SECOND_PERSON.get(lang).to_string());
    }
    lines.push(w::REASON.fill(lang, &[("reason", &required(input, "reason")?)]));
    let mut prepared = prepared("user", &user, title, lang, lines);
    // a privileged badge, or taking any badge away: a fresh code
    prepared.irreversible = privileged || !give;
    // the end of a timed badge, as the card says it
    if let Some(days) = input.integer("days").filter(|_| give) {
        prepared.stored = vec![InteractionDataOption {
            name: "_expires_at".to_string(),
            kind: CommandOptionType::Integer,
            value: Some(InteractionOptionValue::Integer(
                staff.now + days * 24 * 60 * 60 * 1000,
            )),
            options: None,
        }];
    }
    Ok(prepared)
}

async fn give_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    badge_card(db, staff, input, true).await
}
async fn remove_badge_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Prepared> {
    badge_card(db, staff, input, false).await
}

async fn give_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataBadgeChange {
        badge: required(input, "badge")?,
        reason: required(input, "reason")?,
        expires_at: input.integer("_expires_at"),
    };
    let user = required_user(input, "user")?;
    let grant = on_heap(|| grant_core(db, staff, &user, data)).await?;
    Ok(done(json(&grant), BADGE_DONE, input.lang))
}
async fn remove_badge_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let data = DataBadgeChange {
        badge: required(input, "badge")?,
        reason: required(input, "reason")?,
        expires_at: None,
    };
    let user = required_user(input, "user")?;
    let grant = on_heap(|| revoke_core(db, staff, &user, data)).await?;
    Ok(done(json(&grant), BADGE_DONE, input.lang))
}
boxed!(badge_give_card, give_card, Prepared);
boxed!(badge_give, give_run, DeskView);
boxed!(badge_remove_card, remove_badge_card, Prepared);
boxed!(badge_remove, remove_badge_run, DeskView);

// ---- the official mark ----

async fn propose_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    let give = required(input, "action")? == "give";
    // the console core's own checks, before a card is made
    let subject = db.fetch_user(&user).await?;
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if user == staff.user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if subject.official == give {
        return Err(create_error!(NoEffect));
    }
    if !give && is_seeded_official(&user) {
        return Err(create_error!(InvalidOperation));
    }
    let open = db.fetch_official_mark_requests().await?;
    if open
        .iter()
        .any(|r| r.user_id == user && status_of(r, staff.now) == "Pending")
    {
        return Err(create_error!(InvalidOperation));
    }
    let text = if give {
        w::PROPOSE_GIVE
    } else {
        w::PROPOSE_REMOVE
    };
    Ok(prepared(
        "user",
        &user,
        w::T_PROPOSE,
        lang,
        vec![
            text.fill(lang, &[("user", &person(db, &user).await)]),
            w::REASON.fill(lang, &[("reason", &required(input, "reason")?)]),
        ],
    ))
}

async fn propose_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataOfficialProposal {
        grant: required(input, "action")? == "give",
        reason: required(input, "reason")?,
    };
    let user = required_user(input, "user")?;
    let view = on_heap(|| propose_core(db, staff, &user, data)).await?;
    Ok(done(json(&view), OFFICIAL_DONE, input.lang))
}
boxed!(official_propose_card, propose_card, Prepared);
boxed!(official_propose, propose_run, DeskView);

async fn confirm_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "request")?;
    let request = db.fetch_official_mark_request(&id).await?;
    // the two-person rule: never your own proposal, never about you;
    // refused with the console's Denied record
    if request.requested_by == staff.user.id || request.user_id == staff.user.id {
        let mut record = staff.record(
            "staff.official.confirm",
            Some(StaffPermission::OfficialManage),
        );
        record.target_kind = Some("user".to_string());
        record.target_id = Some(request.user_id.clone());
        record.reason = Some(request.reason.clone());
        record.summary = Some(format!(
            "{} {} proposed by {}",
            if request.grant { "give" } else { "remove" },
            request.id,
            request.requested_by
        ));
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    if status_of(&request, staff.now) != "Pending" {
        return Err(create_error!(InvalidOperation));
    }
    let text = if request.grant {
        w::CONFIRM_GIVE
    } else {
        w::CONFIRM_REMOVE
    };
    let line = text.fill(
        lang,
        &[
            ("id", &id),
            ("by", &person(db, &request.requested_by).await),
            ("user", &person(db, &request.user_id).await),
        ],
    );
    Ok(prepared(
        "user",
        &request.user_id,
        w::T_CONFIRM,
        lang,
        vec![line],
    ))
}

async fn confirm_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let request = required(input, "request")?;
    let view = on_heap(|| confirm_core(db, staff, &request)).await?;
    Ok(done(json(&view), OFFICIAL_DONE, input.lang))
}
boxed!(official_confirm_card, confirm_card, Prepared);
boxed!(official_confirm, confirm_run, DeskView);

async fn refuse_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "request")?;
    let request = db.fetch_official_mark_request(&id).await?;
    let user = person(db, &request.user_id).await;
    let line = if request.requested_by == staff.user.id {
        w::WITHDRAW.fill(lang, &[("id", &id), ("user", &user)])
    } else {
        w::REFUSE.fill(
            lang,
            &[
                ("id", &id),
                ("by", &person(db, &request.requested_by).await),
                ("user", &user),
            ],
        )
    };
    Ok(prepared(
        "user",
        &request.user_id,
        w::T_REFUSE,
        lang,
        vec![
            line,
            w::REASON.fill(lang, &[("reason", &required(input, "reason")?)]),
        ],
    ))
}

async fn refuse_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let view = refuse_core(
        db,
        staff,
        &required(input, "request")?,
        DataOfficialRefusal {
            reason: required(input, "reason")?,
        },
    )
    .await?;
    Ok(done(json(&view), OFFICIAL_DONE, input.lang))
}
boxed!(official_refuse_card, refuse_card, Prepared);
boxed!(official_refuse, refuse_run, DeskView);

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
    t(
        "User ID or name#tag",
        "معرّف المستخدم أو الاسم#الرمز",
        "Benutzer-ID oder Name#Tag",
    ),
);
const REASON: DeskOption = text("reason", true, 300, t("Reason", "السبب", "Grund"));
const REASON_OPTIONAL: DeskOption = text(
    "reason",
    false,
    300,
    t(
        "Reason (needed to refuse)",
        "السبب (لازم للرفض)",
        "Grund (zum Ablehnen nötig)",
    ),
);

const KIND: DeskOption = DeskOption {
    choices: &[
        choice("server", "Server", "سيرفر", "Server"),
        choice("bot", "Bot", "بوت", "Bot"),
    ],
    ..opt(
        "kind",
        CommandOptionType::String,
        true,
        t("What is listed", "ما المُدرج", "Was gelistet ist"),
    )
};
const DECISION: DeskOption = DeskOption {
    choices: &[
        choice("Approve", "Approve", "قبول", "Annehmen"),
        choice("Deny", "Deny", "رفض", "Ablehnen"),
        choice(
            "Remove",
            "Remove from Discover",
            "إزالة من الاستكشاف",
            "Aus Discover entfernen",
        ),
        choice("Review", "Under review", "قيد المراجعة", "In Prüfung"),
    ],
    ..opt(
        "decision",
        CommandOptionType::String,
        true,
        t("Decision", "القرار", "Entscheidung"),
    )
};
const VERIFY_DECISION: DeskOption = DeskOption {
    choices: &[
        choice("approve", "Approve", "قبول", "Annehmen"),
        choice("deny", "Deny", "رفض", "Ablehnen"),
    ],
    ..opt(
        "decision",
        CommandOptionType::String,
        true,
        t("Decision", "القرار", "Entscheidung"),
    )
};
const DESIGN_ACTION: DeskOption = DeskOption {
    choices: &[
        choice("approve", "Approve", "قبول", "Annehmen"),
        choice("reject", "Reject", "رفض", "Ablehnen"),
        choice("suspend", "Take down", "إزالة", "Sperren"),
    ],
    ..opt(
        "action",
        CommandOptionType::String,
        true,
        t("Decision", "القرار", "Entscheidung"),
    )
};
const PLUGIN_ACTION: DeskOption = DeskOption {
    choices: &[
        choice(
            "approve",
            "Approve release",
            "قبول النسخة",
            "Version freigeben",
        ),
        choice("reject", "Reject release", "رفض النسخة", "Version ablehnen"),
        choice(
            "revoke",
            "Revoke release",
            "سحب النسخة",
            "Version zurückziehen",
        ),
        choice(
            "suspend",
            "Suspend plugin",
            "إيقاف الإضافة",
            "Plugin sperren",
        ),
        choice(
            "reinstate",
            "Reinstate plugin",
            "إعادة الإضافة",
            "Plugin wiederherstellen",
        ),
        choice("remove", "Remove plugin", "حذف الإضافة", "Plugin entfernen"),
        choice(
            "verify",
            "Verify publisher",
            "توثيق الناشر",
            "Herausgeber verifizieren",
        ),
        choice(
            "suspend_publisher",
            "Suspend publisher",
            "إيقاف الناشر",
            "Herausgeber sperren",
        ),
        choice(
            "reinstate_publisher",
            "Reinstate publisher",
            "إعادة الناشر",
            "Herausgeber wiederherstellen",
        ),
    ],
    ..opt(
        "action",
        CommandOptionType::String,
        true,
        t("Decision", "القرار", "Entscheidung"),
    )
};
const CREATOR_ACTION: DeskOption = DeskOption {
    choices: &[
        choice(
            "grant",
            "Make a creator",
            "منح صفة المبدع",
            "Zum Creator machen",
        ),
        choice("suspend", "Suspend", "إيقاف", "Sperren"),
        choice("revoke", "Revoke", "سحب", "Entziehen"),
        choice("reinstate", "Reinstate", "إعادة", "Wiederherstellen"),
    ],
    ..opt(
        "action",
        CommandOptionType::String,
        true,
        t("Change", "التغيير", "Änderung"),
    )
};
const WORD_CATEGORY: DeskOption = DeskOption {
    choices: &[
        choice("Harassment", "Harassment", "تحرش", "Belästigung"),
        choice(
            "HateOrThreats",
            "Hate or threats",
            "كراهية أو تهديد",
            "Hass oder Drohungen",
        ),
        choice(
            "SexualContent",
            "Sexual content",
            "محتوى جنسي",
            "Sexuelle Inhalte",
        ),
        choice("Spam", "Spam", "رسائل مزعجة", "Spam"),
        choice("Other", "Other", "أخرى", "Sonstiges"),
    ],
    ..opt(
        "category",
        CommandOptionType::String,
        true,
        t("Category", "الفئة", "Kategorie"),
    )
};
const OFFICIAL_ACTION: DeskOption = DeskOption {
    choices: &[
        choice("give", "Give the mark", "منح العلامة", "Markierung geben"),
        choice(
            "remove",
            "Remove the mark",
            "سحب العلامة",
            "Markierung entziehen",
        ),
    ],
    ..opt(
        "action",
        CommandOptionType::String,
        true,
        t("Change", "التغيير", "Änderung"),
    )
};

const fn entry(
    group: &'static str,
    name: &'static str,
    description: Text,
    options: &'static [DeskOption],
    permission: StaffPermission,
    irreversible: bool,
    prepare: Option<Prepare>,
    handler: super::registry::Handler,
) -> DeskCommand {
    DeskCommand {
        area: Some("review"),
        group: Some(group),
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
        name: "discover",
        description: t("Discover", "الاستكشاف", "Discover"),
    },
    DeskGroup {
        name: "verification",
        description: t("Bot verification", "توثيق البوتات", "Bot-Verifizierung"),
    },
    DeskGroup {
        name: "design",
        description: t("Designs", "التصاميم", "Designs"),
    },
    DeskGroup {
        name: "plugin",
        description: t("Plugins", "الإضافات", "Plugins"),
    },
    DeskGroup {
        name: "creator",
        description: t("Creators", "المبدعون", "Creator"),
    },
    DeskGroup {
        name: "badge",
        description: t("Badges", "الشارات", "Abzeichen"),
    },
    DeskGroup {
        name: "official",
        description: t("Official mark", "العلامة الرسمية", "Offizielle Markierung"),
    },
];

pub static ENTRIES: &[DeskCommand] = &[
    entry(
        "discover",
        "decide",
        t("Decide a request", "البت في طلب", "Anfrage entscheiden"),
        &[
            KIND,
            id("id"),
            DECISION,
            opt(
                "featured",
                CommandOptionType::Boolean,
                false,
                t("Featured", "مميّز", "Hervorgehoben"),
            ),
            text(
                "reason",
                false,
                300,
                t(
                    "Reason (deny, remove)",
                    "السبب (رفض، إزالة)",
                    "Grund (ablehnen, entfernen)",
                ),
            ),
        ],
        P::ServersRestrict,
        false,
        Some(discover_decide_card),
        discover_decide,
    )
    .checked(discover_decide_verify),
    entry(
        "verification",
        "decide",
        t(
            "Decide a verification",
            "البت في توثيق",
            "Verifizierung entscheiden",
        ),
        &[
            id("request"),
            VERIFY_DECISION,
            text(
                "note",
                false,
                300,
                t(
                    "Note for the owner (needed to deny)",
                    "ملاحظة للمالك (لازمة للرفض)",
                    "Notiz an die Besitzer (zum Ablehnen nötig)",
                ),
            ),
        ],
        P::ServersRestrict,
        false,
        Some(verification_decide_card),
        verification_decide,
    ),
    entry(
        "bot",
        "unverify",
        t(
            "Take a bot's mark away",
            "سحب توثيق بوت",
            "Verifizierung entfernen",
        ),
        &[id("bot"), REASON],
        P::ServersRestrict,
        true,
        Some(bot_unverify_card),
        bot_unverify,
    ),
    // the console asks for a step-up on every design decision
    entry(
        "design",
        "review",
        t("Decide a design", "البت في تصميم", "Design entscheiden"),
        &[id("design"), DESIGN_ACTION, REASON_OPTIONAL],
        P::ReportsResolve,
        true,
        Some(design_review_card),
        design_review,
    ),
    // the console asks for a step-up on every plugin decision; only while
    // plugins are on
    DeskCommand {
        plugins: true,
        ..entry(
            "plugin",
            "review",
            t("Decide a plugin", "البت في إضافة", "Plugin entscheiden"),
            &[
                id("id"),
                PLUGIN_ACTION,
                REASON_OPTIONAL,
                opt(
                    "verify_publisher",
                    CommandOptionType::Boolean,
                    false,
                    t(
                        "Verify the publisher too",
                        "توثيق الناشر أيضًا",
                        "Herausgeber auch verifizieren",
                    ),
                ),
                opt(
                    "acknowledge_high_risk",
                    CommandOptionType::Boolean,
                    false,
                    t(
                        "High risk understood",
                        "الخطورة العالية مفهومة",
                        "Hohes Risiko verstanden",
                    ),
                ),
            ],
            P::ReportsResolve,
            true,
            Some(plugin_review_card),
            plugin_review,
        )
    },
    entry(
        "creator",
        "change",
        t("Change a creator", "تغيير حالة مبدع", "Creator ändern"),
        &[USER, CREATOR_ACTION, REASON],
        P::CreatorsManage,
        false,
        Some(creator_change_card),
        creator_change,
    )
    .checked(about_yourself_verify),
    entry(
        "words",
        "add",
        t("Add a word", "إضافة كلمة", "Wort hinzufügen"),
        &[
            text(
                "pattern",
                true,
                100,
                t("Word or pattern", "الكلمة أو النمط", "Wort oder Muster"),
            ),
            WORD_CATEGORY,
            REASON,
            opt(
                "allow",
                CommandOptionType::Boolean,
                false,
                t("Allow it", "السماح بها", "Erlauben"),
            ),
            text("note", false, 300, t("Note", "ملاحظة", "Notiz")),
        ],
        P::ReportsResolve,
        false,
        Some(words_add_card),
        words_add,
    ),
    entry(
        "words",
        "remove",
        t("Remove a word", "حذف كلمة", "Wort entfernen"),
        &[id("word"), REASON],
        P::ReportsResolve,
        true,
        Some(words_remove_card),
        words_remove,
    ),
    entry(
        "words",
        "check",
        t("Check a text", "فحص نص", "Text prüfen"),
        &[text("text", true, 2000, t("Text", "النص", "Text"))],
        P::ReportsRead,
        false,
        None,
        words_check,
    ),
    entry(
        "badge",
        "give",
        t("Give a badge", "منح شارة", "Abzeichen vergeben"),
        &[
            USER,
            id("badge"),
            REASON,
            DeskOption {
                min_value: Some(1.0),
                max_value: Some(3650.0),
                ..opt(
                    "days",
                    CommandOptionType::Integer,
                    false,
                    t("For days", "لمدة أيام", "Für Tage"),
                )
            },
        ],
        P::BadgesManage,
        false,
        Some(badge_give_card),
        badge_give,
    )
    .checked(about_yourself_verify),
    entry(
        "badge",
        "remove",
        t("Take a badge away", "سحب شارة", "Abzeichen entziehen"),
        &[USER, id("badge"), REASON],
        P::BadgesManage,
        true,
        Some(badge_remove_card),
        badge_remove,
    )
    .checked(about_yourself_verify),
    // the console asks for a step-up on every change of the official mark
    entry(
        "official",
        "propose",
        t(
            "Propose the mark",
            "اقتراح العلامة",
            "Markierung vorschlagen",
        ),
        &[USER, OFFICIAL_ACTION, REASON],
        P::OfficialManage,
        true,
        Some(official_propose_card),
        official_propose,
    ),
    entry(
        "official",
        "confirm",
        t("Confirm a proposal", "تأكيد اقتراح", "Vorschlag bestätigen"),
        &[id("request")],
        P::OfficialManage,
        true,
        Some(official_confirm_card),
        official_confirm,
    ),
    entry(
        "official",
        "refuse",
        t("Refuse a proposal", "رفض اقتراح", "Vorschlag ablehnen"),
        &[id("request"), REASON],
        P::OfficialManage,
        true,
        Some(official_refuse_card),
        official_refuse,
    ),
];
