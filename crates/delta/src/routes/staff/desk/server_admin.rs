//! Desk servers (area `servers`, command `/desk-servers`): server marks,
//! handing a server over, deletions and account deletion requests. Every
//! change makes a confirmation card at `/run` and runs on confirmation
//! through the console's own core, with its gates and records.
//!
//! Deleting asks for more than a fresh code: the command takes
//! `confirm_name`, the target's exact current name, checked when the card
//! is made and again right before the deletion. The card lists what goes.

use futures::future::BoxFuture;
use revolt_database::{
    AuditOutcome, Database, DeletionInfo, StaffPermission, User, record_platform_audit,
    staff_deletion::{self, PlanIds},
    voice::VoiceClient,
};
use revolt_models::v0::{CommandOptionType, InteractionDataOption, InteractionOptionValue};
use revolt_result::{Result, create_error};
use serde_json::Value;

use super::super::StaffContext;
use super::super::bots::{DataDeleteBot, delete_core as delete_bot_core};
use super::super::deletion::{
    DataDeleteOwned, DataDeletionRequest, cancel_delete_request_core, cancel_outcome,
    delete_owned_core, delete_request_check, delete_request_core, deletions_core,
};
use super::super::servers::{DataServerAction, act_core};
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

const SERVERS: &str = "_servers";
/// The server's owner as the card showed it
const OWNER: &str = "_owner";
const BOTS: &str = "_bots";

mod w {
    use super::{Text, t};

    pub const DONE: Text = t("Done", "تم", "Erledigt");
    pub const ID: Text = t("ID", "المعرّف", "ID");
    pub const OWNER: Text = t("Owner", "المالك", "Eigentümer");
    pub const DELETED: Text = t("Deleted", "حُذف", "Gelöscht");
    pub const USER: Text = t("User", "المستخدم", "Benutzer");
    pub const AFTER: Text = t("Deleted after", "يُحذف بعد", "Gelöscht nach");
    pub const SERVERS_DELETED: Text =
        t("Servers deleted", "السيرفرات المحذوفة", "Gelöschte Server");
    pub const BOTS_DELETED: Text = t("Bots deleted", "البوتات المحذوفة", "Gelöschte Bots");
    pub const KEPT: Text = t("Kept (official)", "باقية (رسمية)", "Behalten (offiziell)");
    pub const FAILED: Text = t("Failed", "فشلت", "Fehlgeschlagen");
    pub const KIND_SERVER: Text = t("server", "سيرفر", "Server");
    pub const KIND_BOT: Text = t("bot", "بوت", "Bot");

    pub const T_MARK: Text = t(
        "Change a server's mark",
        "تغيير علامة سيرفر",
        "Server-Markierung ändern",
    );
    pub const T_TRANSFER: Text = t("Hand a server over", "نقل ملكية سيرفر", "Server übergeben");
    pub const T_DELETE_SERVER: Text = t("Delete a server", "حذف سيرفر", "Server löschen");
    pub const T_DELETE_BOT: Text = t("Delete a bot", "حذف بوت", "Bot löschen");
    pub const T_DELETE_OWNED: Text = t(
        "Delete everything an account owns",
        "حذف كل ما يملكه حساب",
        "Alles eines Kontos löschen",
    );
    pub const T_DELETE_REQUEST: Text = t("Delete an account", "حذف حساب", "Konto löschen");
    pub const T_DELETIONS: Text = t(
        "Accounts waiting for deletion",
        "حسابات بانتظار الحذف",
        "Konten vor der Löschung",
    );

    pub const MARK: Text = t(
        "{action} the server {name} ({id}) of {owner}; the owner is told.",
        "{action} السيرفر {name} ({id}) التابع لـ {owner}؛ ويُبلَّغ المالك.",
        "{action}: Server {name} ({id}) von {owner}; die Besitzer werden informiert.",
    );
    pub const TRANSFER: Text = t(
        "Hands the server {name} ({id}) from {owner} to {to}; both are told.",
        "ينقل ملكية السيرفر {name} ({id}) من {owner} إلى {to}؛ ويُبلَّغ الاثنان.",
        "Übergibt den Server {name} ({id}) von {owner} an {to}; beide werden informiert.",
    );
    pub const DELETE_SERVER: Text = t(
        "Deletes the server {name} ({id}) of {owner}: {channels} channels and {members} members go with it.",
        "يحذف السيرفر {name} ({id}) التابع لـ {owner}: وتُحذف معه {channels} قناة و{members} عضو.",
        "Löscht den Server {name} ({id}) von {owner}: {channels} Kanäle und {members} Mitglieder gehen mit.",
    );
    pub const DELETE_BOT: Text = t(
        "Deletes the bot {name} ({id}) of {owner}: it leaves its {servers} servers, its commands go and its token stops working.",
        "يحذف البوت {name} ({id}) التابع لـ {owner}: يغادر {servers} سيرفر، وتُحذف أوامره ويتوقف رمزه.",
        "Löscht den Bot {name} ({id}) von {owner}: er verlässt seine {servers} Server, seine Befehle gehen und sein Token funktioniert nicht mehr.",
    );
    pub const DELETE_OWNED: Text = t(
        "Deletes everything the banned account {user} owns:",
        "يحذف كل ما يملكه الحساب المحظور {user}:",
        "Löscht alles, was das gesperrte Konto {user} besitzt:",
    );
    pub const OWNED_SERVER: Text = t(
        "Server {name} ({id}), {members} members",
        "سيرفر {name} ({id})، {members} عضو",
        "Server {name} ({id}), {members} Mitglieder",
    );
    pub const OWNED_BOT: Text = t(
        "Bot {name} ({id})",
        "بوت {name} ({id})",
        "Bot {name} ({id})",
    );
    pub const OWNED_KEPT: Text = t(
        "Kept, official: {kind} {name} ({id})",
        "باقٍ لأنه رسمي: {kind} {name} ({id})",
        "Bleibt, offiziell: {kind} {name} ({id})",
    );
    pub const OWNED_NONE: Text = t(
        "It owns nothing to delete.",
        "لا يملك شيئًا لحذفه.",
        "Es besitzt nichts zum Löschen.",
    );
    pub const DELETE_REQUEST: Text = t(
        "Schedules the deletion of the account {user}: it is disabled and signed out now, and after {days} days it is deleted for good with everything it owns:",
        "يجدول حذف الحساب {user}: يُعطَّل ويُسجَّل خروجه الآن، وبعد {days} يومًا يُحذف نهائيًا مع كل ما يملكه:",
        "Plant die Löschung des Kontos {user}: es wird jetzt deaktiviert und abgemeldet und nach {days} Tagen endgültig gelöscht, mit allem, was ihm gehört:",
    );
    pub const GROUND: Text = t("Ground: {ground}", "الأساس: {ground}", "Grundlage: {ground}");
    pub const FACTS: Text = t(
        "Facts the person reads: {facts}",
        "الوقائع التي يقرؤها صاحب الحساب: {facts}",
        "Fakten, die die Person liest: {facts}",
    );
    pub const REQUEST_TOLD: Text = t(
        "The person gets a statement of reasons by message and e-mail: that a person decided, the ground and these facts, the date, how to object or appeal (an Owner then cancels with user delete-cancel) and how to ask for a copy of their data. With no way to tell them, nothing is scheduled.",
        "يتلقى صاحب الحساب بيان أسباب برسالة وبالبريد الإلكتروني: أن إنسانًا قرّر، والأساس وهذه الوقائع، والتاريخ، وكيف يعترض أو يطعن (فيلغي المالك الحذف بالأمر user delete-cancel)، وكيف يطلب نسخة من بياناته. وإن تعذّر إبلاغه فلا يُجدول شيء.",
        "Die Person erhält eine Begründung per Nachricht und E-Mail: dass ein Mensch entschieden hat, die Grundlage und diese Fakten, das Datum, wie sie widersprechen oder Einspruch einlegen kann (ein Owner sagt dann mit user delete-cancel ab) und wie sie eine Kopie ihrer Daten anfordert. Ist sie nicht erreichbar, wird nichts geplant.",
    );
    pub const T_DELETE_CANCEL: Text = t(
        "Cancel an account deletion",
        "إلغاء حذف حساب",
        "Kontolöschung absagen",
    );
    pub const DELETE_CANCEL: Text = t(
        "Cancels the scheduled deletion of {user}; the person is told.",
        "يلغي الحذف المجدول للحساب {user}؛ ويُبلَّغ صاحبه.",
        "Sagt die geplante Löschung von {user} ab; die Person wird informiert.",
    );
    pub const CANCEL_ENABLED: Text = t(
        "The account is enabled again, as it was before the request.",
        "يُفعَّل الحساب من جديد كما كان قبل الطلب.",
        "Das Konto wird wieder aktiviert, wie vor der Anfrage.",
    );
    pub const CANCEL_STAYS: Text = t(
        "The account stays disabled: it was disabled before the request, or it is suspended or banned.",
        "يبقى الحساب معطّلًا: فقد كان معطّلًا قبل الطلب، أو هو موقوف أو محظور.",
        "Das Konto bleibt deaktiviert: es war vor der Anfrage deaktiviert, oder es ist gesperrt oder gebannt.",
    );
    pub const REASON: Text = t("Reason: {reason}", "السبب: {reason}", "Grund: {reason}");
    pub const OWNER_READS: Text = t(
        "The owner reads: {reason}",
        "يقرأ المالك: {reason}",
        "Die Besitzer lesen: {reason}",
    );
    pub const WRONG_NAME: &str = "the typed name is not the target's exact name";
    pub const NAME_CHANGED: &str = "the target's name changed since the card";
    pub const OWNER_CHANGED: &str = "the server's owner changed since the card";
    pub const PLAN_CHANGED: &str = "what the account owns changed since the card";
    pub const OFFICIAL_TRANSFER: &str = "transfer of an official server";
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

fn stored_text(name: &str, value: String) -> InteractionDataOption {
    InteractionDataOption {
        name: name.to_string(),
        kind: CommandOptionType::String,
        value: Some(InteractionOptionValue::String(value)),
        options: None,
    }
}

async fn voice() -> VoiceClient {
    VoiceClient::from_revolt_config().await
}

/// A refusal the console records: the same Denied record, `NotPrivileged`
async fn denied(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    kind: &str,
    id: &str,
    permission: StaffPermission,
    summary: &str,
) -> Result<()> {
    let mut record = staff.record(action, Some(permission));
    record.target_kind = Some(kind.to_string());
    record.target_id = Some(id.to_string());
    record.summary = Some(summary.to_string());
    record_platform_audit(db, record, AuditOutcome::Denied).await?;
    Err(create_error!(NotPrivileged))
}

/// The Owner role, as the console's deletion gate asks first; refused with
/// the console's own Denied record
async fn owner_only(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    kind: &str,
    id: &str,
    permission: StaffPermission,
) -> Result<()> {
    if staff.grant.is_owner(staff.now) {
        return Ok(());
    }
    denied(
        db,
        staff,
        action,
        kind,
        id,
        permission,
        "owner role required",
    )
    .await
}

/// The typed name, exactly the target's current one
fn typed(input: &DeskInput, name: &str) -> Result<()> {
    if same_name(input, name) {
        Ok(())
    } else {
        Err(create_error!(FailedValidation {
            error: w::WRONG_NAME.to_string()
        }))
    }
}

/// The typed confirmation, compared to the character (not trimmed)
fn same_name(input: &DeskInput, name: &str) -> bool {
    input.raw_text("confirm_name").as_deref() == Some(name)
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

const SERVER_DONE: &[Show] = &[
    show("/id", w::ID),
    show("/owner", w::OWNER),
    show("/deleted", w::DELETED),
];

// ---- server marks and hand-over ----

async fn mark_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "server")?;
    let server = db.fetch_server(&id).await?;
    let action = required(input, "action")?;
    let reason = required(input, "reason")?;
    Ok(prepared(
        "server",
        &id,
        w::T_MARK,
        lang,
        vec![
            w::MARK.fill(
                lang,
                &[
                    ("action", &named(&MARK_ACTION, &action, lang)),
                    ("name", &server.name),
                    ("id", &id),
                    ("owner", &person(db, &server.owner).await),
                ],
            ),
            w::OWNER_READS.fill(lang, &[("reason", &reason)]),
        ],
    ))
}

async fn mark_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataServerAction {
        action: required(input, "action")?,
        reason: required(input, "reason")?,
        to: None,
    };
    let result = act_core(db, &voice().await, staff, &required(input, "server")?, data).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&result),
        SERVER_DONE,
        input.lang,
    ))
}
boxed!(server_mark_card, mark_card, Prepared);
boxed!(server_mark, mark_run, DeskView);

async fn transfer_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    // the console's rule: the Owner role only
    if !staff.grant.is_owner(staff.now) {
        return Err(create_error!(NotPrivileged));
    }
    let id = required(input, "server")?;
    let server = db.fetch_server(&id).await?;
    if staff_deletion::is_official_server(&server) {
        denied(
            db,
            staff,
            "staff.server.action",
            "server",
            &id,
            StaffPermission::ServersRestrict,
            w::OFFICIAL_TRANSFER,
        )
        .await?;
    }
    let to = required_user(input, "user")?;
    if to == server.owner {
        return Err(create_error!(InvalidOperation));
    }
    if db.fetch_user(&to).await?.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    // only someone already in the server
    db.fetch_member(&server.id, &to).await?;
    let reason = required(input, "reason")?;
    let mut card = prepared(
        "server",
        &id,
        w::T_TRANSFER,
        lang,
        vec![
            w::TRANSFER.fill(
                lang,
                &[
                    ("name", &server.name),
                    ("id", &id),
                    ("owner", &person(db, &server.owner).await),
                    ("to", &person(db, &to).await),
                ],
            ),
            w::REASON.fill(lang, &[("reason", &reason)]),
        ],
    );
    card.stored = vec![stored_text(OWNER, server.owner.clone())];
    Ok(card)
}

/// Right before the hand-over: the owner is still the one on the card
async fn transfer_check(
    db: &Database,
    _: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let server = db.fetch_server(&required(input, "server")?).await?;
    Ok((input.text(OWNER).as_deref() != Some(server.owner.as_str())).then_some(w::OWNER_CHANGED))
}
boxed!(server_transfer_verify, transfer_check, Option<&'static str>);

async fn transfer_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataServerAction {
        action: "transfer".to_string(),
        reason: required(input, "reason")?,
        to: Some(required_user(input, "user")?),
    };
    let result = act_core(db, &voice().await, staff, &required(input, "server")?, data).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&result),
        SERVER_DONE,
        input.lang,
    ))
}
boxed!(server_transfer_card, transfer_card, Prepared);
boxed!(server_transfer, transfer_run, DeskView);

// ---- deletions ----

fn permanent(mut prepared: Prepared) -> Prepared {
    prepared.permanent = true;
    prepared
}

async fn delete_server_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "server")?;
    owner_only(
        db,
        staff,
        "staff.server.delete",
        "server",
        &id,
        StaffPermission::ServersRestrict,
    )
    .await?;
    let server = db.fetch_server(&id).await?;
    if staff_deletion::is_official_server(&server) {
        denied(
            db,
            staff,
            "staff.server.delete",
            "server",
            &id,
            StaffPermission::ServersRestrict,
            "official",
        )
        .await?;
    }
    typed(input, &server.name)?;
    let members = db
        .fetch_member_count(&server.id)
        .await
        .unwrap_or(0)
        .to_string();
    let channels = server.channels.len().to_string();
    let reason = required(input, "reason")?;
    let mut card = permanent(prepared(
        "server",
        &id,
        w::T_DELETE_SERVER,
        lang,
        vec![
            w::DELETE_SERVER.fill(
                lang,
                &[
                    ("name", &server.name),
                    ("id", &id),
                    ("owner", &person(db, &server.owner).await),
                    ("channels", &channels),
                    ("members", &members),
                ],
            ),
            w::OWNER_READS.fill(lang, &[("reason", &reason)]),
        ],
    ));
    card.stored = vec![stored_text(OWNER, server.owner.clone())];
    Ok(card)
}

async fn delete_server_check(
    db: &Database,
    _: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let server = db.fetch_server(&required(input, "server")?).await?;
    if !same_name(input, &server.name) {
        return Ok(Some(w::NAME_CHANGED));
    }
    Ok((input.text(OWNER).as_deref() != Some(server.owner.as_str())).then_some(w::OWNER_CHANGED))
}

async fn delete_server_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let data = DataServerAction {
        action: "delete".to_string(),
        reason: required(input, "reason")?,
        to: None,
    };
    let result = act_core(db, &voice().await, staff, &required(input, "server")?, data).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&result),
        SERVER_DONE,
        input.lang,
    ))
}
boxed!(server_delete_card, delete_server_card, Prepared);
boxed!(
    server_delete_verify,
    delete_server_check,
    Option<&'static str>
);
boxed!(server_delete, delete_server_run, DeskView);

async fn bot_name(db: &Database, id: &str) -> Result<(revolt_database::Bot, Option<User>, String)> {
    let bot = db.fetch_bot(id).await?;
    let user = db.fetch_user(&bot.id).await.ok();
    let name = user
        .as_ref()
        .map(|u| u.username.clone())
        .unwrap_or_default();
    Ok((bot, user, name))
}

async fn delete_bot_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "bot")?;
    owner_only(
        db,
        staff,
        "staff.bot.delete",
        "bot",
        &id,
        StaffPermission::ServersRestrict,
    )
    .await?;
    let (bot, _, name) = bot_name(db, &id).await?;
    // the bot's Official flag is the one source of the mark
    if staff_deletion::is_official_bot(&bot) {
        denied(
            db,
            staff,
            "staff.bot.delete",
            "bot",
            &id,
            StaffPermission::ServersRestrict,
            "official",
        )
        .await?;
    }
    typed(input, &name)?;
    let servers = db
        .fetch_server_count(&bot.id)
        .await
        .unwrap_or(0)
        .to_string();
    let reason = required(input, "reason")?;
    Ok(permanent(prepared(
        "bot",
        &id,
        w::T_DELETE_BOT,
        lang,
        vec![
            w::DELETE_BOT.fill(
                lang,
                &[
                    ("name", &name),
                    ("id", &id),
                    ("owner", &person(db, &bot.owner).await),
                    ("servers", &servers),
                ],
            ),
            w::OWNER_READS.fill(lang, &[("reason", &reason)]),
        ],
    )))
}

async fn delete_bot_check(
    db: &Database,
    _: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let (_, _, name) = bot_name(db, &required(input, "bot")?).await?;
    Ok((!same_name(input, &name)).then_some(w::NAME_CHANGED))
}

async fn delete_bot_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let data = DataDeleteBot {
        reason: required(input, "reason")?,
    };
    let deleted =
        delete_bot_core(db, &voice().await, staff, &required(input, "bot")?, data).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&deleted),
        &[show("/id", w::ID), show("/deleted", w::DELETED)],
        input.lang,
    ))
}
boxed!(bot_delete_card, delete_bot_card, Prepared);
boxed!(bot_delete_verify, delete_bot_check, Option<&'static str>);
boxed!(bot_delete, delete_bot_run, DeskView);

fn ids_text(ids: &std::collections::BTreeSet<String>) -> String {
    ids.iter().cloned().collect::<Vec<_>>().join(",")
}

fn ids_of(input: &DeskInput, name: &str) -> std::collections::BTreeSet<String> {
    input
        .text(name)
        .unwrap_or_default()
        .split(',')
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

async fn delete_owned_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    owner_only(
        db,
        staff,
        "staff.user.delete_owned",
        "user",
        &id,
        StaffPermission::ServersRestrict,
    )
    .await?;
    let user = db.fetch_user(&id).await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    // only for an account staff already banned, as in the console
    if !staff_deletion::is_banned(&user) {
        return Err(create_error!(InvalidOperation));
    }
    typed(input, &handle(&user))?;
    let plan = staff_deletion::plan_delete_owned(db, &user).await?;
    let mut lines = vec![w::DELETE_OWNED.fill(
        lang,
        &[("user", &format!("{} ({})", handle(&user), user.id))],
    )];
    owned_lines(&plan, lang, &mut lines);
    for kept in &plan.skipped {
        let kind = if kept.kind == "bot" {
            w::KIND_BOT
        } else {
            w::KIND_SERVER
        };
        lines.push(w::OWNED_KEPT.fill(
            lang,
            &[
                ("kind", kind.get(lang)),
                ("name", &kept.name),
                ("id", &kept.id),
            ],
        ));
    }
    lines.push(w::REASON.fill(lang, &[("reason", &required(input, "reason")?)]));
    let ids = plan.ids();
    let mut prepared = permanent(prepared("user", &id, w::T_DELETE_OWNED, lang, lines));
    // exactly what the card listed: anything else is refused at the press
    prepared.stored = vec![
        stored_text(SERVERS, ids_text(&ids.servers)),
        stored_text(BOTS, ids_text(&ids.bots)),
    ];
    Ok(prepared)
}

async fn delete_owned_check(
    db: &Database,
    _: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let user = db.fetch_user(&required_user(input, "user")?).await?;
    Ok((!same_name(input, &handle(&user))).then_some(w::NAME_CHANGED))
}

async fn delete_owned_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let expected = PlanIds {
        servers: ids_of(input, SERVERS),
        bots: ids_of(input, BOTS),
    };
    let data = DataDeleteOwned {
        reason: required(input, "reason")?,
    };
    let result = delete_owned_core(
        db,
        &voice().await,
        staff,
        &required_user(input, "user")?,
        data,
        Some(&expected),
    )
    .await?;
    let lang = input.lang;
    let result = json(&result);
    let count = |key: &str| {
        result[key]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0)
            .to_string()
    };
    Ok(DeskView::new(w::DONE.get(lang))
        .field(w::SERVERS_DELETED.get(lang), count("deleted_servers"))
        .field(w::BOTS_DELETED.get(lang), count("deleted_bots"))
        .field(w::KEPT.get(lang), count("skipped"))
        .field(w::FAILED.get(lang), count("failed")))
}
boxed!(user_delete_owned_card, delete_owned_card, Prepared);
boxed!(
    user_delete_owned_verify,
    delete_owned_check,
    Option<&'static str>
);
boxed!(user_delete_owned, delete_owned_run, DeskView);

// ---- account deletion requests ----

async fn delete_request_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    // the console core's own refusals, with its records
    let (user, plan) = delete_request_check(db, staff, &id).await?;
    let account = db.fetch_account(&user.id).await?;
    if matches!(
        account.deletion,
        Some(DeletionInfo::Scheduled { .. } | DeletionInfo::Deleted)
    ) {
        return Err(create_error!(NoEffect));
    }
    typed(input, &handle(&user))?;
    let days = revolt_database::ACCOUNT_DELETION_GRACE_DAYS.to_string();
    let mut lines = vec![w::DELETE_REQUEST.fill(
        lang,
        &[
            ("user", &format!("{} ({})", handle(&user), user.id)),
            ("days", &days),
        ],
    )];
    owned_lines(&plan, lang, &mut lines);
    let category = category_of(input)?;
    let code = match lang {
        Lang::Ar => "ar",
        Lang::De => "de",
        Lang::En => "en",
    };
    lines.push(w::GROUND.fill(
        lang,
        &[("ground", &crate::util::official::rule_name(code, category))],
    ));
    let facts = super::super::deletion::facts(&required(input, "reason")?)?;
    lines.push(w::FACTS.fill(lang, &[("facts", &facts)]));
    lines.push(w::REQUEST_TOLD.get(lang).to_string());
    let ids = plan.ids();
    let mut card = permanent(prepared("user", &id, w::T_DELETE_REQUEST, lang, lines));
    card.stored = vec![
        stored_text(SERVERS, ids_text(&ids.servers)),
        stored_text(BOTS, ids_text(&ids.bots)),
    ];
    Ok(card)
}

/// The ground chosen on the command
fn category_of(input: &DeskInput) -> Result<revolt_models::v0::SafetyCategory> {
    serde_json::from_value(Value::String(required(input, "category")?))
        .map_err(|_| create_error!(InvalidOperation))
}

/// The servers (with members) and bots an account owns, one per line
fn owned_lines(plan: &staff_deletion::DeleteOwnedPlan, lang: Lang, lines: &mut Vec<String>) {
    for planned in &plan.servers {
        lines.push(w::OWNED_SERVER.fill(
            lang,
            &[
                ("name", &planned.server.name),
                ("id", &planned.server.id),
                ("members", &planned.members.to_string()),
            ],
        ));
    }
    for planned in &plan.bots {
        lines.push(w::OWNED_BOT.fill(lang, &[("name", &planned.name), ("id", &planned.bot.id)]));
    }
    if plan.servers.is_empty() && plan.bots.is_empty() {
        lines.push(w::OWNED_NONE.get(lang).to_string());
    }
}

fn stored_plan(input: &DeskInput) -> PlanIds {
    PlanIds {
        servers: ids_of(input, SERVERS),
        bots: ids_of(input, BOTS),
    }
}

/// Right before the request: the same name, and the same servers and bots
async fn delete_request_recheck(
    db: &Database,
    _: &StaffContext,
    input: &DeskInput,
) -> Result<Option<&'static str>> {
    let user = db.fetch_user(&required_user(input, "user")?).await?;
    if !same_name(input, &handle(&user)) {
        return Ok(Some(w::NAME_CHANGED));
    }
    let plan = staff_deletion::plan_delete_owned(db, &user).await?;
    Ok((plan.ids() != stored_plan(input)).then_some(w::PLAN_CHANGED))
}

async fn delete_request_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let data = DataDeletionRequest {
        reason: required(input, "reason")?,
        category: Some(category_of(input)?),
    };
    let expected = stored_plan(input);
    let scheduled = delete_request_core(
        db,
        staff,
        &required_user(input, "user")?,
        data,
        Some(&expected),
    )
    .await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&scheduled),
        DELETION_ROW,
        input.lang,
    ))
}
boxed!(user_delete_request_card, delete_request_card, Prepared);
boxed!(
    user_delete_request_verify,
    delete_request_recheck,
    Option<&'static str>
);
boxed!(user_delete_request, delete_request_run, DeskView);

async fn delete_cancel_card(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    owner_only(
        db,
        staff,
        "staff.user.delete_request_cancel",
        "user",
        &id,
        StaffPermission::UsersDeleteRequest,
    )
    .await?;
    let user = db.fetch_user(&id).await?;
    let account = db.fetch_account(&user.id).await?;
    if !matches!(account.deletion, Some(DeletionInfo::Scheduled { .. })) {
        return Err(create_error!(NoEffect));
    }
    // a person's own deletion is theirs
    let Some(stays_disabled) = cancel_outcome(&account, &user) else {
        return Err(create_error!(InvalidOperation));
    };
    Ok(prepared(
        "user",
        &id,
        w::T_DELETE_CANCEL,
        lang,
        vec![
            w::DELETE_CANCEL.fill(
                lang,
                &[("user", &format!("{} ({})", handle(&user), user.id))],
            ),
            if stays_disabled {
                w::CANCEL_STAYS
            } else {
                w::CANCEL_ENABLED
            }
            .get(lang)
            .to_string(),
            w::REASON.fill(lang, &[("reason", &required(input, "reason")?)]),
        ],
    ))
}

async fn delete_cancel_run(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let data = DataDeletionRequest {
        reason: required(input, "reason")?,
        category: None,
    };
    let cancelled =
        cancel_delete_request_core(db, staff, &required_user(input, "user")?, data).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&cancelled),
        &[show("/username", w::USER), show("/user_id", w::ID)],
        input.lang,
    ))
}
boxed!(user_delete_cancel_card, delete_cancel_card, Prepared);
boxed!(user_delete_cancel, delete_cancel_run, DeskView);

pub const DELETION_ROW: &[Show] = &[
    show("/username", w::USER),
    show("/user_id", w::ID),
    show("/after", w::AFTER),
];

async fn deletions_view(
    db: &Database,
    staff: &StaffContext,
    input: &DeskInput,
) -> Result<DeskView> {
    let list = deletions_core(db, staff).await?;
    let items = json(&list).as_array().cloned().unwrap_or_default();
    Ok(paged(
        DeskView::new(w::T_DELETIONS.get(input.lang)),
        &items,
        DELETION_ROW,
        input,
        Some(100),
    ))
}
boxed!(deletion_list, deletions_view, DeskView);

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

const SERVER: DeskOption = text(
    "server",
    true,
    64,
    t("Server ID", "معرّف السيرفر", "Server-ID"),
);
const USER: DeskOption = opt(
    "user",
    CommandOptionType::User,
    true,
    t("The user", "المستخدم", "Der Benutzer"),
);
const REASON: DeskOption = text("reason", true, 300, t("Reason", "السبب", "Grund"));
const CONFIRM_NAME: DeskOption = text(
    "confirm_name",
    true,
    100,
    t(
        "Type the exact name to confirm",
        "اكتب الاسم الدقيق للتأكيد",
        "Den genauen Namen zur Bestätigung eingeben",
    ),
);
const CONFIRM_HANDLE: DeskOption = text(
    "confirm_name",
    true,
    100,
    t(
        "Type name#tag exactly to confirm",
        "اكتب الاسم#الرمز بدقة للتأكيد",
        "Name#Tag genau zur Bestätigung eingeben",
    ),
);

/// The ground of a staff deletion: the rules of the Terms and Community
/// Guidelines it can rest on (the safety categories)
const GROUND: DeskOption = DeskOption {
    choices: &[
        choice("Harassment", "Harassment", "التحرش", "Belästigung"),
        choice(
            "HateOrThreats",
            "Hate or threats",
            "الكراهية أو التهديد",
            "Hass oder Drohungen",
        ),
        choice("Spam", "Spam", "الرسائل المزعجة", "Spam"),
        choice("ScamFraud", "Scam or fraud", "الاحتيال", "Betrug"),
        choice(
            "Impersonation",
            "Impersonation",
            "انتحال الشخصية",
            "Identitätsmissbrauch",
        ),
        choice("Malware", "Malware", "البرمجيات الخبيثة", "Schadsoftware"),
        choice(
            "IllegalContent",
            "Illegal content",
            "محتوى غير قانوني",
            "Illegale Inhalte",
        ),
        choice("Underage", "Underage", "دون السن", "Minderjährig"),
        choice("Other", "Other rule", "قاعدة أخرى", "Andere Regel"),
    ],
    ..opt(
        "category",
        CommandOptionType::String,
        true,
        t(
            "Ground (rule broken)",
            "الأساس (القاعدة المخالَفة)",
            "Grundlage (verletzte Regel)",
        ),
    )
};
const FACTS: DeskOption = text(
    "reason",
    true,
    300,
    t(
        "Facts the person reads (no names of others)",
        "الوقائع التي يقرؤها (دون أسماء آخرين)",
        "Fakten für die Person (keine fremden Namen)",
    ),
);

const MARK_ACTION: DeskOption = DeskOption {
    choices: &[
        choice(
            "unverify",
            "Take the verified mark away",
            "سحب علامة التوثيق",
            "Verifizierung entfernen",
        ),
        choice("mark_nsfw", "Mark 18+", "تعليم +18", "Als 18+ markieren"),
        choice("unmark_nsfw", "Take 18+ away", "إزالة +18", "18+ entfernen"),
    ],
    ..opt(
        "action",
        CommandOptionType::String,
        true,
        t("Change", "التغيير", "Änderung"),
    )
};

const fn entry(
    group: Option<&'static str>,
    name: &'static str,
    description: Text,
    options: &'static [DeskOption],
    permission: StaffPermission,
    prepare: Option<Prepare>,
    handler: super::registry::Handler,
) -> DeskCommand {
    DeskCommand {
        area: Some("servers"),
        group,
        name,
        description,
        options,
        permission: Some(permission),
        changes: prepare.is_some(),
        // every change here: the console steps up for all of them
        irreversible: prepare.is_some(),
        plugins: false,
        prepare,
        handler,
        verify: None,
    }
}

use StaffPermission as P;

pub static GROUPS: &[DeskGroup] = &[DeskGroup {
    name: "deletion",
    description: t("Account deletions", "حذف الحسابات", "Kontolöschungen"),
}];

pub static ENTRIES: &[DeskCommand] = &[
    entry(
        Some("server"),
        "mark",
        t(
            "Verified or 18+ mark",
            "علامة التوثيق أو +18",
            "Verifiziert oder 18+",
        ),
        &[SERVER, MARK_ACTION, REASON],
        P::ServersRestrict,
        Some(server_mark_card),
        server_mark,
    ),
    entry(
        Some("server"),
        "transfer",
        t(
            "Hand over (Owner)",
            "نقل الملكية (المالك)",
            "Übergeben (Owner)",
        ),
        &[SERVER, USER, REASON],
        P::ServersRestrict,
        Some(server_transfer_card),
        server_transfer,
    )
    .checked(server_transfer_verify),
    DeskCommand {
        verify: Some(server_delete_verify),
        ..entry(
            Some("server"),
            "delete",
            t("Delete (Owner)", "حذف (المالك)", "Löschen (Owner)"),
            &[SERVER, CONFIRM_NAME, REASON],
            P::ServersRestrict,
            Some(server_delete_card),
            server_delete,
        )
    },
    DeskCommand {
        verify: Some(bot_delete_verify),
        ..entry(
            Some("bot"),
            "delete",
            t(
                "Delete a bot (Owner)",
                "حذف بوت (المالك)",
                "Bot löschen (Owner)",
            ),
            &[
                text("bot", true, 64, t("Bot ID", "معرّف البوت", "Bot-ID")),
                CONFIRM_NAME,
                REASON,
            ],
            P::ServersRestrict,
            Some(bot_delete_card),
            bot_delete,
        )
    },
    DeskCommand {
        verify: Some(user_delete_owned_verify),
        ..entry(
            Some("user"),
            "delete-owned",
            t(
                "Delete all it owns (Owner)",
                "حذف كل ما يملك (المالك)",
                "Alles löschen (Owner)",
            ),
            &[USER, CONFIRM_HANDLE, REASON],
            P::ServersRestrict,
            Some(user_delete_owned_card),
            user_delete_owned,
        )
    },
    DeskCommand {
        verify: Some(user_delete_request_verify),
        ..entry(
            Some("user"),
            "delete-request",
            t(
                "Delete the account (Owner)",
                "حذف الحساب (المالك)",
                "Konto löschen (Owner)",
            ),
            &[USER, CONFIRM_HANDLE, GROUND, FACTS],
            P::UsersDeleteRequest,
            Some(user_delete_request_card),
            user_delete_request,
        )
    },
    entry(
        Some("user"),
        "delete-cancel",
        t(
            "Cancel a deletion (Owner)",
            "إلغاء الحذف (المالك)",
            "Löschung absagen (Owner)",
        ),
        &[USER, REASON],
        P::UsersDeleteRequest,
        Some(user_delete_cancel_card),
        user_delete_cancel,
    ),
    // who is leaving, and when: personal data
    entry(
        Some("deletion"),
        "list",
        t("Waiting for deletion", "بانتظار الحذف", "Vor der Löschung"),
        &[],
        P::UsersPiiRead,
        None,
        deletion_list,
    ),
];
