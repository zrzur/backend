//! Desk administration: billing (`/desk-billing`), settings
//! (`/desk-settings`) and the staff team (`/desk-staff`). Every command
//! here changes something and needs a fresh code, whatever the console
//! asks: the card says exactly what happens (money to the cent, who loses
//! what) and the confirmation runs the console's own core with its checks
//! and records.
//!
//! Not offered: the legacy `/community/permissions` (platform roles for
//! designs, badges and the beta). Its rules hang on the user's own
//! `privileged` flag and design permissions, not on staff roles, so it
//! does not map onto a staff session; see the report.

use std::collections::HashMap;

use futures::future::BoxFuture;
use revolt_database::{
    AuditOutcome, Database, StaffPermission, StaffRole, User, WebhookEventStatus,
    check_staff_change, record_platform_audit,
};
use revolt_models::v0::{
    self, CommandOptionType, DataBetaFeature, DataBetaMember, InteractionDataOption,
    InteractionOptionValue, OrderStatus,
};
use revolt_result::{Result, create_error};
use serde_json::Value;

use super::super::beta::{member_core, save_core};
use super::super::billing::{
    deliver_core, grant_core, reconcile_now_core, refund_core, retry_webhook_core,
};
use super::super::policies::{DataPolicyChange, DataWithdrawPolicy, publish_core, withdraw_core};
use super::super::security::{DataLock, lock_core};
use super::super::{
    DataReason, DataSetMember, StaffContext, disable_member_core, revoke_member_sessions_core,
    set_member_core,
};
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

/// The staff grant version the card read
const VERSION: &str = "_version";
/// The refund amount the card showed (all that was left, when none was
/// typed): the confirmation refunds exactly that
const AMOUNT: &str = "_amount";

mod w {
    use super::{Text, t};

    pub const DONE: Text = t("Done", "تم", "Erledigt");
    pub const ID: Text = t("ID", "المعرّف", "ID");
    pub const STATUS: Text = t("Status", "الحالة", "Status");
    pub const USER: Text = t("User", "المستخدم", "Benutzer");
    pub const ROLES: Text = t("Roles", "الأدوار", "Rollen");
    pub const ISSUES: Text = t("Open issues", "مشكلات مفتوحة", "Offene Probleme");
    pub const SESSIONS: Text = t("Sessions ended", "جلسات أُنهيت", "Beendete Sitzungen");

    pub const T_REFUND: Text = t(
        "Refund an order",
        "استرداد مبلغ طلب",
        "Bestellung erstatten",
    );
    pub const T_DELIVER: Text = t(
        "Deliver an order again",
        "تسليم طلب من جديد",
        "Bestellung erneut liefern",
    );
    pub const T_RETRY: Text = t(
        "Retry a payment event",
        "إعادة معالجة حدث دفع",
        "Zahlungsereignis erneut verarbeiten",
    );
    pub const T_RECONCILE: Text = t("Reconcile now", "مطابقة الآن", "Jetzt abgleichen");
    pub const T_GRANT: Text = t("Give Feather", "منح ريشة", "Feather schenken");
    pub const T_FEATURE: Text = t(
        "Switch a beta feature",
        "تبديل ميزة تجريبية",
        "Beta-Funktion schalten",
    );
    pub const T_MEMBER: Text = t(
        "Change a beta tester",
        "تغيير مختبِر تجريبي",
        "Beta-Tester ändern",
    );
    pub const T_PUBLISH: Text = t(
        "Publish a policy update",
        "نشر تحديث للسياسات",
        "Richtlinien-Änderung veröffentlichen",
    );
    pub const T_WITHDRAW: Text = t(
        "Withdraw a policy update",
        "سحب تحديث للسياسات",
        "Richtlinien-Änderung zurückziehen",
    );
    pub const T_ROLES: Text = t(
        "Change staff roles",
        "تغيير أدوار الإدارة",
        "Team-Rollen ändern",
    );
    pub const T_DISABLE: Text = t(
        "Disable a staff member",
        "تعطيل عضو في الإدارة",
        "Teammitglied deaktivieren",
    );
    pub const T_SESSIONS: Text = t(
        "End staff sessions",
        "إنهاء جلسات الإدارة",
        "Team-Sitzungen beenden",
    );
    pub const T_LOCK: Text = t(
        "Lock a staff member out",
        "إقفال حساب عضو في الإدارة",
        "Teammitglied aussperren",
    );

    pub const REFUND: Text = t(
        "Refunds {amount} of order {order} ({product}) paid by {user}. Paid {total}, refunded so far {refunded}, left to refund {left}.",
        "يردّ {amount} من الطلب {order} ({product}) الذي دفعه {user}. المدفوع {total}، والمردود حتى الآن {refunded}، والمتبقي للرد {left}.",
        "Erstattet {amount} der Bestellung {order} ({product}), bezahlt von {user}. Bezahlt {total}, bisher erstattet {refunded}, noch erstattbar {left}.",
    );
    pub const REFUND_FULL: Text = t(
        "This refunds all that is left: what was bought is taken back.",
        "هذا يردّ كل المتبقي: ويُسحب ما اشتُري.",
        "Das erstattet den ganzen Rest: das Gekaufte wird entzogen.",
    );
    pub const REFUND_REVOKE: Text = t(
        "What was bought is taken back.",
        "ويُسحب ما اشتُري.",
        "Das Gekaufte wird entzogen.",
    );
    pub const REFUND_KEEP: Text = t(
        "What was bought stays.",
        "ويبقى ما اشتُري.",
        "Das Gekaufte bleibt.",
    );
    pub const PERSON_TOLD: Text = t(
        "The person is told.",
        "ويُبلَّغ صاحب الحساب.",
        "Die Person wird informiert.",
    );
    pub const DELIVER: Text = t(
        "Delivers order {order} ({product}, {total}, {status}) of {user} again. Delivery is keyed by the order, so nothing is given twice.",
        "يسلّم الطلب {order} ({product}، {total}، {status}) الخاص بـ {user} من جديد. التسليم مربوط بالطلب، فلا يُعطى شيء مرتين.",
        "Liefert Bestellung {order} ({product}, {total}, {status}) von {user} erneut. Die Lieferung hängt an der Bestellung, nichts wird doppelt gegeben.",
    );
    pub const RETRY: Text = t(
        "Processes the failed {provider} event {event} ({kind}, about {object}) again; it failed {attempts} times.",
        "يعيد معالجة حدث {provider} الفاشل {event} ({kind}، بشأن {object})؛ وقد فشل {attempts} مرات.",
        "Verarbeitet das fehlgeschlagene {provider}-Ereignis {event} ({kind}, zu {object}) erneut; es ist {attempts}-mal gescheitert.",
    );
    pub const RECONCILE: Text = t(
        "Compares orders and subscriptions with the payment provider and delivers what failed. It never creates a payment or a refund. Open issues are listed after.",
        "يطابق الطلبات والاشتراكات مع مزوّد الدفع ويسلّم ما فشل تسليمه. لا ينشئ أي دفعة أو استرداد. وتُعرض المشكلات المفتوحة بعد ذلك.",
        "Gleicht Bestellungen und Abos mit dem Zahlungsanbieter ab und liefert Fehlgeschlagenes. Es entsteht nie eine Zahlung oder Erstattung. Offene Probleme werden danach gezeigt.",
    );
    pub const GRANT: Text = t(
        "Gives {user} Feather for {days} without payment; they are told.",
        "يمنح {user} ريشة دون دفع، وعدد الأيام ({days})؛ ويُبلَّغ بذلك.",
        "Gibt {user} Feather für {days} ohne Zahlung; die Person wird informiert.",
    );
    pub const FEATURE_ON: Text = t(
        "Switches the beta feature {name} ({id}) on for {who}.",
        "يشغّل الميزة التجريبية {name} ({id}) لـ{who}.",
        "Schaltet die Beta-Funktion {name} ({id}) für {who} ein.",
    );
    pub const FEATURE_OFF: Text = t(
        "Switches the beta feature {name} ({id}) off for everyone.",
        "يوقف الميزة التجريبية {name} ({id}) للجميع.",
        "Schaltet die Beta-Funktion {name} ({id}) für alle aus.",
    );
    pub const ALL_TESTERS: Text = t("every tester", "كل المختبِرين", "alle Tester");
    pub const CHOSEN_TESTERS: Text = t(
        "the testers given it",
        "المختبِرين الممنوحين إياها",
        "die Tester, denen sie gegeben wurde",
    );
    pub const MEMBER: Text = t(
        "{action}: {user}; they are told.",
        "{action}: {user}؛ ويُبلَّغ بذلك.",
        "{action}: {user}; die Person wird informiert.",
    );
    pub const MEMBER_FEATURES: Text = t(
        "Features: {features}",
        "الميزات: {features}",
        "Funktionen: {features}",
    );
    pub const PUBLISH: Text = t(
        "Publishes a policy update to every user, effective {when}; each is asked to read and acknowledge it. Link: {url}",
        "ينشر تحديثًا للسياسات لكل المستخدمين، يسري في {when}؛ ويُطلب من كل مستخدم قراءته والإقرار به. الرابط: {url}",
        "Veröffentlicht eine Richtlinien-Änderung für alle, gültig ab {when}; alle werden gebeten, sie zu lesen und zu bestätigen. Link: {url}",
    );
    pub const PUBLISH_HOST: Text = t(
        "Host: zrzur.com (a legal page). Who sees it: every user, at their next connection.",
        "المضيف: zrzur.com (صفحة قانونية). من يراه: كل المستخدمين عند اتصالهم التالي.",
        "Host: zrzur.com (eine Rechtsseite). Wer es sieht: alle Nutzerinnen und Nutzer bei der nächsten Verbindung.",
    );
    pub const PUBLISH_NOW: Text = t(
        "It takes effect now (0 days).",
        "يسري الآن (0 يوم).",
        "Es gilt sofort (0 Tage).",
    );
    pub const PUBLISH_VERSION: Text = t(
        "A change to the Terms or the Privacy Policy also needs the version and date on the legal page.",
        "تغيير شروط الخدمة أو سياسة الخصوصية يحتاج أيضًا إلى تحديث الإصدار والتاريخ في الصفحة القانونية.",
        "Eine Änderung der Nutzungsbedingungen oder der Datenschutzerklärung braucht auch Version und Datum auf der Rechtsseite.",
    );
    pub const WITHDRAW: Text = t(
        "Withdraws the policy update {id} ({url}): people who have not acknowledged it stop seeing it. It cannot be undone.",
        "يسحب تحديث السياسات {id} ({url}): ومن لم يُقرّ به لن يراه بعد الآن. ولا يمكن التراجع عن ذلك.",
        "Zieht die Richtlinien-Änderung {id} ({url}) zurück: wer sie nicht bestätigt hat, sieht sie nicht mehr. Nicht rückgängig zu machen.",
    );
    pub const ROLES_CHANGE: Text = t(
        "Changes the staff roles of {user} from {from} to {to}; their open staff sessions end.",
        "يغيّر أدوار {user} في الإدارة من {from} إلى {to}؛ وتنتهي جلساته المفتوحة في الإدارة.",
        "Ändert die Team-Rollen von {user} von {from} zu {to}; die offenen Team-Sitzungen enden.",
    );
    pub const NONE: Text = t(
        "none (no access)",
        "لا شيء (دون صلاحية)",
        "keine (kein Zugang)",
    );
    pub const END_REMOVED: Text = t(
        "Their access end date ({when}) is removed: access no longer ends by itself.",
        "يُزال تاريخ انتهاء صلاحيته ({when}): فلا تنتهي الصلاحية من تلقاء نفسها بعد الآن.",
        "Das Enddatum des Zugangs ({when}) wird entfernt: der Zugang endet nicht mehr von selbst.",
    );
    pub const DISABLED_GRANT: &str = "this staff access is disabled: switch it on again in the console first, then change the roles";
    pub const DISABLE: Text = t(
        "Disables staff access of {user} ({roles}): roles stay on record, their staff sessions end now.",
        "يعطّل صلاحية {user} في الإدارة ({roles}): تبقى الأدوار مسجّلة، وتنتهي جلساته في الإدارة الآن.",
        "Deaktiviert den Team-Zugang von {user} ({roles}): die Rollen bleiben gespeichert, die Team-Sitzungen enden jetzt.",
    );
    pub const SESSIONS_END: Text = t(
        "Ends every open staff session of {user} (console and Zrzur Desk); their roles stay.",
        "ينهي كل جلسات {user} المفتوحة في الإدارة (لوحة التحكم ومكتب زرزور)؛ وتبقى أدواره.",
        "Beendet alle offenen Team-Sitzungen von {user} (Konsole und Zrzur Desk); die Rollen bleiben.",
    );
    pub const LOCK: Text = t(
        "Locks {user} out: every staff session ends for a session that looks stolen; they sign in again with their authenticator. Roles stay.",
        "يقفل {user}: تنتهي كل جلساته في الإدارة لجلسة تبدو مسروقة؛ ويسجّل الدخول من جديد بتطبيق المصادقة. وتبقى الأدوار.",
        "Sperrt {user} aus: alle Team-Sitzungen enden wegen einer verdächtigen Sitzung; Anmeldung erneut mit dem Authenticator. Die Rollen bleiben.",
    );
    pub const YOURSELF: Text = t(
        "This is you: your own Zrzur Desk session ends too.",
        "هذا أنت: تنتهي جلستك في مكتب زرزور أيضًا.",
        "Das bist du: deine eigene Zrzur-Desk-Sitzung endet auch.",
    );
    pub const REASON: Text = t("Reason: {reason}", "السبب: {reason}", "Grund: {reason}");

    pub const FEATHER: Text = t("Feather", "ريشة", "Feather");
    pub const FEATHER_GIFT: Text = t("Feather gift", "ريشة هدية", "Feather-Geschenk");
}

// ---- helpers ----

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

fn reason_line(input: &DeskInput) -> Result<String> {
    Ok(w::REASON.fill(input.lang, &[("reason", &required(input, "reason")?)]))
}

/// Minor units as money: `12.34 EUR`
fn money(minor: u32, currency: &str) -> String {
    format!(
        "{}.{:02} {}",
        minor / 100,
        minor % 100,
        currency.to_uppercase()
    )
}

fn product(order: &revolt_database::BillingOrder, lang: Lang) -> String {
    match json(&order.product)["type"].as_str().unwrap_or_default() {
        "Plus" => w::FEATHER.get(lang).to_string(),
        "PlusGift" => w::FEATHER_GIFT.get(lang).to_string(),
        other => other.to_string(),
    }
}

/// A refusal the console records: the same Denied record, `NotPrivileged`
async fn denied(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    permission: StaffPermission,
    id: &str,
    summary: Option<String>,
) -> Result<()> {
    let mut record = staff.record(action, Some(permission));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(id.to_string());
    record.summary = summary;
    record_platform_audit(db, record, AuditOutcome::Denied).await?;
    Err(create_error!(NotPrivileged))
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

const ORDER_DONE: &[Show] = &[
    show("@id", w::ID),
    show("/status", w::STATUS),
    show("/user_id", w::USER),
];

// ---- billing ----

/// The order a refund or delivery is about, and what is left to refund
async fn refund_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "order")?;
    let order = db.fetch_order(&id).await?;
    // the billing core's own checks, before a card is made
    if !matches!(
        order.status,
        OrderStatus::Paid | OrderStatus::PartiallyRefunded
    ) {
        return Err(create_error!(InvalidOperation));
    }
    let left = order.amount.saturating_sub(order.amount_refunded);
    let amount = match input.integer("amount") {
        Some(value) if value <= 0 || value > u32::MAX as i64 => {
            return Err(create_error!(FailedValidation {
                error: "amount".to_string()
            }));
        }
        Some(value) => value as u32,
        None => left,
    };
    if amount == 0 || amount > left {
        return Err(create_error!(FailedValidation {
            error: "amount".to_string()
        }));
    }
    let cur = &order.currency;
    let mut lines = vec![w::REFUND.fill(
        lang,
        &[
            ("amount", &money(amount, cur)),
            ("order", &order.id),
            ("product", &product(&order, lang)),
            ("user", &person(db, &order.user_id).await),
            ("total", &money(order.amount, cur)),
            ("refunded", &money(order.amount_refunded, cur)),
            ("left", &money(left, cur)),
        ],
    )];
    let full = amount == left;
    lines.push(
        if full {
            w::REFUND_FULL
        } else if input.boolean("revoke") == Some(true) {
            w::REFUND_REVOKE
        } else {
            w::REFUND_KEEP
        }
        .get(lang)
        .to_string(),
    );
    lines.push(w::PERSON_TOLD.get(lang).to_string());
    lines.push(reason_line(input)?);
    let mut card = prepared("order", &id, w::T_REFUND, lang, lines);
    card.stored = vec![InteractionDataOption {
        name: AMOUNT.to_string(),
        kind: CommandOptionType::Integer,
        value: Some(InteractionOptionValue::Integer(amount as i64)),
        options: None,
    }];
    Ok(card)
}

async fn refund_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = v0::DataRefundOrder {
        // the amount on the card, never "whatever is left" at the press
        amount: Some(
            input
                .integer(AMOUNT)
                .ok_or_else(|| create_error!(InvalidOperation))? as u32,
        ),
        reason: required(input, "reason")?,
        revoke: input.boolean("revoke"),
    };
    let order = required(input, "order")?;
    let order = on_heap(|| refund_core(db, staff, &order, data)).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&order),
        ORDER_DONE,
        input.lang,
    ))
}
boxed!(billing_refund_card, refund_card, Prepared);
boxed!(billing_refund, refund_run, DeskView);

async fn deliver_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "order")?;
    let order = db.fetch_order(&id).await?;
    let status = json(&order.status).as_str().unwrap_or_default().to_string();
    Ok(prepared(
        "order",
        &id,
        w::T_DELIVER,
        lang,
        vec![w::DELIVER.fill(
            lang,
            &[
                ("order", &order.id),
                ("product", &product(&order, lang)),
                ("total", &money(order.amount, &order.currency)),
                ("status", &status),
                ("user", &person(db, &order.user_id).await),
            ],
        )],
    ))
}

async fn deliver_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let order = required(input, "order")?;
    let order = on_heap(|| deliver_core(db, staff, &order)).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&order),
        ORDER_DONE,
        input.lang,
    ))
}
boxed!(billing_deliver_card, deliver_card, Prepared);
boxed!(billing_deliver, deliver_run, DeskView);

async fn retry_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "event")?;
    let event = db
        .fetch_webhook_event(&id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    // only a failed event is retried, as in the console
    if event.status != WebhookEventStatus::Failed {
        return Err(create_error!(InvalidOperation));
    }
    Ok(prepared(
        "webhook",
        &id,
        w::T_RETRY,
        lang,
        vec![w::RETRY.fill(
            lang,
            &[
                ("provider", &event.provider),
                ("event", &event.id),
                ("kind", &event.event_type),
                ("object", &event.object_id),
                ("attempts", &event.attempts.to_string()),
            ],
        )],
    ))
}

async fn retry_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let event = required(input, "event")?;
    let event = on_heap(|| retry_webhook_core(db, staff, &event)).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&event),
        &[show("@id", w::ID), show("/status", w::STATUS)],
        input.lang,
    ))
}
boxed!(webhook_retry_card, retry_card, Prepared);
boxed!(webhook_retry, retry_run, DeskView);

async fn reconcile_card(_: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    Ok(Prepared {
        title: w::T_RECONCILE.get(input.lang).to_string(),
        lines: vec![w::RECONCILE.get(input.lang).to_string()],
        ..Default::default()
    })
}

async fn reconcile_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let issues = on_heap(|| reconcile_now_core(db, staff)).await?;
    Ok(DeskView::new(w::DONE.get(input.lang))
        .field(w::ISSUES.get(input.lang), issues.len().to_string()))
}
boxed!(billing_reconcile_card, reconcile_card, Prepared);
boxed!(billing_reconcile, reconcile_run, DeskView);

async fn grant_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    let subject = db.fetch_user(&user).await?;
    // the console core's own checks, before a card is made
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if subject.id == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let days = input
        .integer("days")
        .filter(|d| (1..=366).contains(d))
        .ok_or_else(|| {
            create_error!(FailedValidation {
                error: "days".to_string()
            })
        })?;
    Ok(prepared(
        "user",
        &user,
        w::T_GRANT,
        lang,
        vec![
            w::GRANT.fill(
                lang,
                &[
                    ("user", &super::registry::person_line(&subject)),
                    (
                        "days",
                        &super::registry::counted(
                            lang,
                            &days.to_string(),
                            ("day", "Tag"),
                            ("days", "Tage"),
                        ),
                    ),
                ],
            ),
            reason_line(input)?,
        ],
    ))
}

async fn grant_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let user = required_user(input, "user")?;
    let data = v0::DataGrantPlus {
        user: user.clone(),
        days: input.integer("days").unwrap_or_default() as u32,
        reason: required(input, "reason")?,
    };
    on_heap(|| grant_core(db, staff, data)).await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::USER.get(input.lang), user))
}
boxed!(billing_grant_card, grant_card, Prepared);
boxed!(billing_grant, grant_run, DeskView);

// ---- settings: beta ----

async fn feature_of(db: &Database, id: &str) -> Result<v0::BetaFeature> {
    crate::routes::badges::beta::features_list(db)
        .await?
        .into_iter()
        .find(|f| f.id == id)
        .ok_or_else(|| create_error!(NotFound))
}

async fn feature_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "feature")?;
    let feature = feature_of(db, &id).await?;
    let on = required(input, "state")? == "on";
    if feature.enabled == on {
        return Err(create_error!(NoEffect));
    }
    let line = if on {
        let who = if feature.all_testers {
            w::ALL_TESTERS
        } else {
            w::CHOSEN_TESTERS
        };
        w::FEATURE_ON.fill(
            lang,
            &[("name", &feature.name), ("id", &id), ("who", who.get(lang))],
        )
    } else {
        w::FEATURE_OFF.fill(lang, &[("name", &feature.name), ("id", &id)])
    };
    Ok(prepared(
        "beta_feature",
        &id,
        w::T_FEATURE,
        lang,
        vec![line],
    ))
}

async fn feature_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let feature = feature_of(db, &required(input, "feature")?).await?;
    // only the switch changes; the rest stays as it is
    let data = DataBetaFeature {
        id: feature.id,
        name: feature.name,
        description: feature.description,
        enabled: required(input, "state")? == "on",
        all_testers: feature.all_testers,
    };
    let saved = save_core(db, staff, data).await?;
    Ok(DeskView::new(w::DONE.get(input.lang))
        .field(w::ID.get(input.lang), saved.id)
        .field(w::STATUS.get(input.lang), saved.enabled.to_string()))
}
boxed!(beta_feature_card, feature_card, Prepared);
boxed!(beta_feature, feature_run, DeskView);

fn feature_list(input: &DeskInput) -> Option<Vec<String>> {
    optional(input, "features").map(|list| {
        list.split(',')
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect()
    })
}

async fn member_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let user = required_user(input, "user")?;
    let action = required(input, "action")?;
    if !BETA_ACTION.choices.iter().any(|c| c.value == action) {
        return Err(create_error!(InvalidOperation));
    }
    let mut lines = vec![w::MEMBER.fill(
        lang,
        &[
            ("action", &named(&BETA_ACTION, &action, lang)),
            ("user", &person(db, &user).await),
        ],
    )];
    if let Some(features) = feature_list(input) {
        lines.push(w::MEMBER_FEATURES.fill(lang, &[("features", &features.join(", "))]));
    }
    if let Some(note) = optional(input, "note") {
        lines.push(w::REASON.fill(lang, &[("reason", &note)]));
    }
    Ok(prepared("user", &user, w::T_MEMBER, lang, lines))
}

async fn member_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let data = DataBetaMember {
        user: required_user(input, "user")?,
        action: required(input, "action")?,
        features: feature_list(input),
        note: optional(input, "note"),
    };
    let record = on_heap(|| member_core(db, staff, data)).await?;
    Ok(fields(
        DeskView::new(w::DONE.get(input.lang)),
        &json(&record),
        &[show("/user/username", w::USER), show("/status", w::STATUS)],
        input.lang,
    ))
}
boxed!(beta_member_card, member_card, Prepared);
boxed!(beta_member, member_run, DeskView);

// ---- settings: policy updates ----

fn policy_data(input: &DeskInput) -> Result<DataPolicyChange> {
    let mut messages = HashMap::new();
    for language in ["ar", "en", "de"] {
        messages.insert(
            language.to_string(),
            required(input, &format!("message_{language}"))?,
        );
    }
    let days = input.integer("days").unwrap_or(0);
    Ok(DataPolicyChange {
        messages,
        url: required(input, "url")?,
        effective_in_days: days.clamp(0, u32::MAX as i64) as u32,
    })
}

async fn publish_card(_: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let data = policy_data(input)?;
    // the console core's own checks, before a card is made
    for text in data.messages.values() {
        if !(3..=300).contains(&text.chars().count()) {
            return Err(create_error!(FailedValidation {
                error: "the message is needed in ar, en and de, 3 to 300 characters each"
                    .to_string()
            }));
        }
    }
    let url = data.url.trim();
    if !super::super::policies::legal_page(url) {
        return Err(create_error!(FailedValidation {
            error: "the link must be one of the legal pages on https://zrzur.com".to_string()
        }));
    }
    if data.effective_in_days > 90 {
        return Err(create_error!(FailedValidation {
            error: "effective in 0 to 90 days".to_string()
        }));
    }
    let when = super::registry::when(staff.now + data.effective_in_days as i64 * 86_400_000);
    let mut lines = vec![w::PUBLISH.fill(lang, &[("when", &when), ("url", url)])];
    lines.push(w::PUBLISH_HOST.get(lang).to_string());
    if data.effective_in_days == 0 {
        lines.push(w::PUBLISH_NOW.get(lang).to_string());
    }
    lines.push(w::PUBLISH_VERSION.get(lang).to_string());
    for language in ["ar", "en", "de"] {
        lines.push(format!("{language}: {}", data.messages[language]));
    }
    Ok(prepared("policy_change", "new", w::T_PUBLISH, lang, lines))
}

async fn publish_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let change = publish_core(db, staff, policy_data(input)?).await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::ID.get(input.lang), change.id))
}
boxed!(policy_publish_card, publish_card, Prepared);
boxed!(policy_publish, publish_run, DeskView);

async fn withdraw_card(db: &Database, _: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required(input, "policy")?;
    let change = db
        .fetch_policy_changes()
        .await?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| create_error!(NotFound))?;
    Ok(prepared(
        "policy_change",
        &id,
        w::T_WITHDRAW,
        lang,
        vec![
            w::WITHDRAW.fill(lang, &[("id", &id), ("url", &change.url)]),
            reason_line(input)?,
        ],
    ))
}

async fn withdraw_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required(input, "policy")?;
    withdraw_core(
        db,
        staff,
        &id,
        DataWithdrawPolicy {
            reason: required(input, "reason")?,
        },
    )
    .await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::ID.get(input.lang), id))
}
boxed!(policy_withdraw_card, withdraw_card, Prepared);
boxed!(policy_withdraw, withdraw_run, DeskView);

// ---- the staff team ----

fn role_names(roles: &[StaffRole]) -> String {
    roles
        .iter()
        .map(|r| r.name())
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_roles(input: &DeskInput) -> Result<Vec<StaffRole>> {
    let mut roles = input
        .text("roles")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "none")
        .map(|name| {
            StaffRole::from_name(name).ok_or_else(|| {
                create_error!(FailedValidation {
                    error: format!("unknown role {name}")
                })
            })
        })
        .collect::<Result<Vec<_>>>()?;
    roles.sort();
    roles.dedup();
    Ok(roles)
}

async fn roles_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    let target = db.fetch_user(&id).await?;
    if target.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let new_roles = parse_roles(input)?;
    let current = db.fetch_staff_grant(&id).await?;
    let current_roles = current
        .as_ref()
        .map(|g| g.roles.clone())
        .unwrap_or_default();
    // setting roles switches a disabled grant on again: never as a side
    // effect from the desk
    if current.as_ref().is_some_and(|g| g.disabled) {
        return Err(create_error!(FailedValidation {
            error: w::DISABLED_GRANT.to_string()
        }));
    }
    // the console's rules, with its Denied record: not yourself, never the
    // Owner role, only roles whose permissions you hold
    if let Err(refusal) = check_staff_change(
        &staff.user.id,
        &staff.permissions,
        &id,
        &current_roles,
        &new_roles,
    ) {
        let names = |roles: &[StaffRole]| {
            if roles.is_empty() {
                "none".to_string()
            } else {
                roles.iter().map(|r| r.name()).collect::<Vec<_>>().join(",")
            }
        };
        let summary = format!(
            "roles {} -> {} ({refusal:?})",
            names(&current_roles),
            names(&new_roles)
        );
        denied(
            db,
            staff,
            "staff.grant.update",
            StaffPermission::StaffManage,
            &id,
            Some(summary),
        )
        .await?;
    }
    let shown = |roles: &[StaffRole]| {
        if roles.is_empty() {
            w::NONE.get(lang).to_string()
        } else {
            role_names(roles)
        }
    };
    let mut card = prepared(
        "user",
        &id,
        w::T_ROLES,
        lang,
        vec![
            w::ROLES_CHANGE.fill(
                lang,
                &[
                    ("user", &super::registry::person_line(&target)),
                    ("from", &shown(&current_roles)),
                    ("to", &shown(&new_roles)),
                ],
            ),
            reason_line(input)?,
        ],
    );
    // the console's change also removes an end date
    if let Some(end) = current.as_ref().and_then(|g| g.expires_at) {
        let line = w::END_REMOVED.fill(lang, &[("when", &super::registry::when(end))]);
        let last = card.lines.len() - 1;
        card.lines.insert(last, line);
    }
    // the grant as read: changed since, the console refuses
    card.stored = vec![InteractionDataOption {
        name: VERSION.to_string(),
        kind: CommandOptionType::Integer,
        value: Some(InteractionOptionValue::Integer(
            current.map(|g| g.version).unwrap_or(-1),
        )),
        options: None,
    }];
    Ok(card)
}

async fn roles_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let version = input
        .integer(VERSION)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let data = DataSetMember {
        roles: parse_roles(input)?
            .iter()
            .map(|r| r.name().to_string())
            .collect(),
        reason: required(input, "reason")?,
        version: (version >= 0).then_some(version),
    };
    let user = required_user(input, "user")?;
    let member = on_heap(|| set_member_core(db, staff, &user, data)).await?;
    Ok(DeskView::new(w::DONE.get(input.lang))
        .field(w::USER.get(input.lang), member.user_id)
        .field(w::ROLES.get(input.lang), role_names(&member.roles)))
}
boxed!(staff_roles_card, roles_card, Prepared);
boxed!(staff_roles, roles_run, DeskView);

async fn disable_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    let grant = db
        .fetch_staff_grant(&id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    // not yourself, never an Owner: the console's Denied record
    if id == staff.user.id || grant.roles.contains(&StaffRole::Owner) {
        denied(
            db,
            staff,
            "staff.grant.disable",
            StaffPermission::StaffManage,
            &id,
            None,
        )
        .await?;
    }
    if grant.disabled {
        return Err(create_error!(NoEffect));
    }
    Ok(prepared(
        "user",
        &id,
        w::T_DISABLE,
        lang,
        vec![
            w::DISABLE.fill(
                lang,
                &[
                    ("user", &person(db, &id).await),
                    ("roles", &role_names(&grant.roles)),
                ],
            ),
            reason_line(input)?,
        ],
    ))
}

async fn disable_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let id = required_user(input, "user")?;
    let data = DataReason {
        reason: required(input, "reason")?,
    };
    on_heap(|| disable_member_core(db, staff, &id, data)).await?;
    Ok(DeskView::new(w::DONE.get(input.lang)).field(w::USER.get(input.lang), id))
}
boxed!(staff_disable_card, disable_card, Prepared);
boxed!(staff_disable, disable_run, DeskView);

/// Only an Owner acts on an Owner's sessions: the console's Denied record
async fn owner_target(
    db: &Database,
    staff: &StaffContext,
    action: &str,
    permission: StaffPermission,
    id: &str,
) -> Result<()> {
    let target_is_owner = db
        .fetch_staff_grant(id)
        .await?
        .is_some_and(|g| g.is_owner(staff.now));
    if target_is_owner && !staff.grant.is_owner(staff.now) {
        denied(
            db,
            staff,
            action,
            permission,
            id,
            Some("owner target".to_string()),
        )
        .await?;
    }
    Ok(())
}

async fn sessions_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    owner_target(
        db,
        staff,
        "staff.sessions.revoke",
        StaffPermission::StaffManage,
        &id,
    )
    .await?;
    let mut lines = vec![w::SESSIONS_END.fill(lang, &[("user", &person(db, &id).await)])];
    if id == staff.user.id {
        lines.push(w::YOURSELF.get(lang).to_string());
    }
    lines.push(reason_line(input)?);
    Ok(prepared("user", &id, w::T_SESSIONS, lang, lines))
}

async fn sessions_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let ended = revoke_member_sessions_core(
        db,
        staff,
        &required_user(input, "user")?,
        DataReason {
            reason: required(input, "reason")?,
        },
    )
    .await?;
    Ok(
        DeskView::new(w::DONE.get(input.lang))
            .field(w::SESSIONS.get(input.lang), ended.to_string()),
    )
}
boxed!(staff_sessions_card, sessions_card, Prepared);
boxed!(staff_sessions, sessions_run, DeskView);

async fn lock_card(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<Prepared> {
    let lang = input.lang;
    let id = required_user(input, "user")?;
    // signing yourself out is the sign-out command
    if id == staff.user.id {
        return Err(create_error!(InvalidOperation));
    }
    if db.fetch_staff_grant(&id).await?.is_none() {
        return Err(create_error!(NotFound));
    }
    owner_target(
        db,
        staff,
        "staff.security.lock",
        StaffPermission::SecurityRespond,
        &id,
    )
    .await?;
    Ok(prepared(
        "user",
        &id,
        w::T_LOCK,
        lang,
        vec![
            w::LOCK.fill(lang, &[("user", &person(db, &id).await)]),
            reason_line(input)?,
        ],
    ))
}

async fn lock_run(db: &Database, staff: &StaffContext, input: &DeskInput) -> Result<DeskView> {
    let ended = lock_core(
        db,
        staff,
        &required_user(input, "user")?,
        DataLock {
            reason: required(input, "reason")?,
        },
    )
    .await?;
    Ok(
        DeskView::new(w::DONE.get(input.lang))
            .field(w::SESSIONS.get(input.lang), ended.to_string()),
    )
}
boxed!(security_lock_card, lock_card, Prepared);
boxed!(security_lock, lock_run, DeskView);

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

const fn integer(
    name: &'static str,
    required: bool,
    min: f64,
    max: f64,
    description: Text,
) -> DeskOption {
    DeskOption {
        min_value: Some(min),
        max_value: Some(max),
        ..opt(name, CommandOptionType::Integer, required, description)
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
    t(
        "User ID or name#tag",
        "معرّف المستخدم أو الاسم#الرمز",
        "Benutzer-ID oder Name#Tag",
    ),
);
const REASON: DeskOption = text("reason", true, 300, t("Reason", "السبب", "Grund"));
const ORDER: DeskOption = text(
    "order",
    true,
    200,
    t("Order ID", "معرّف الطلب", "Bestell-ID"),
);

const STATE: DeskOption = DeskOption {
    choices: &[
        choice("on", "On", "تشغيل", "Ein"),
        choice("off", "Off", "إيقاف", "Aus"),
    ],
    ..opt(
        "state",
        CommandOptionType::String,
        true,
        t("On or off", "تشغيل أو إيقاف", "Ein oder aus"),
    )
};

const BETA_ACTION: DeskOption = DeskOption {
    choices: &[
        choice("invite", "Invite", "دعوة", "Einladen"),
        choice("approve", "Approve", "قبول", "Annehmen"),
        choice("remove", "Remove", "إزالة", "Entfernen"),
        choice("complete", "Complete", "إتمام", "Abschließen"),
        choice(
            "features",
            "Set features",
            "تحديد الميزات",
            "Funktionen setzen",
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
    area: &'static str,
    group: &'static str,
    name: &'static str,
    description: Text,
    options: &'static [DeskOption],
    permission: StaffPermission,
    prepare: Prepare,
    handler: super::registry::Handler,
) -> DeskCommand {
    DeskCommand {
        area: Some(area),
        group: Some(group),
        name,
        description,
        options,
        permission: Some(permission),
        changes: true,
        // every change here needs a fresh code (the owner's rule)
        irreversible: true,
        plugins: false,
        prepare: Some(prepare),
        handler,
        verify: None,
    }
}

use StaffPermission as P;

pub static GROUPS: &[DeskGroup] = &[
    DeskGroup {
        name: "webhook",
        description: t("Payment events", "أحداث الدفع", "Zahlungsereignisse"),
    },
    DeskGroup {
        name: "beta",
        description: t("Beta program", "البرنامج التجريبي", "Beta-Programm"),
    },
    DeskGroup {
        name: "policy",
        description: t(
            "Policy updates",
            "تحديثات السياسات",
            "Richtlinien-Änderungen",
        ),
    },
];

pub static ENTRIES: &[DeskCommand] = &[
    // ---- billing ----
    entry(
        "billing",
        "billing",
        "refund",
        t(
            "Refund an order",
            "استرداد مبلغ طلب",
            "Bestellung erstatten",
        ),
        &[
            ORDER,
            REASON,
            integer(
                "amount",
                false,
                1.0,
                10_000_000.0,
                t(
                    "Cents (all that is left if empty)",
                    "بالسنت (كل المتبقي إن تُرك فارغًا)",
                    "Cent (alles Übrige, wenn leer)",
                ),
            ),
            opt(
                "revoke",
                CommandOptionType::Boolean,
                false,
                t("Take the purchase back", "سحب ما اشتُري", "Kauf entziehen"),
            ),
        ],
        P::BillingManage,
        billing_refund_card,
        billing_refund,
    ),
    entry(
        "billing",
        "billing",
        "deliver",
        t("Deliver again", "التسليم من جديد", "Erneut liefern"),
        &[ORDER],
        P::BillingManage,
        billing_deliver_card,
        billing_deliver,
    ),
    entry(
        "billing",
        "billing",
        "reconcile",
        t("Reconcile now", "مطابقة الآن", "Jetzt abgleichen"),
        &[],
        P::BillingManage,
        billing_reconcile_card,
        billing_reconcile,
    ),
    entry(
        "billing",
        "billing",
        "grant",
        t("Give Feather", "منح ريشة", "Feather schenken"),
        &[
            USER,
            integer("days", true, 1.0, 366.0, t("Days", "الأيام", "Tage")),
            REASON,
        ],
        P::BillingManage,
        billing_grant_card,
        billing_grant,
    ),
    entry(
        "billing",
        "webhook",
        "retry",
        t(
            "Retry a failed event",
            "إعادة حدث فاشل",
            "Fehlgeschlagenes Ereignis wiederholen",
        ),
        &[text(
            "event",
            true,
            200,
            t("Event ID", "معرّف الحدث", "Ereignis-ID"),
        )],
        P::BillingManage,
        webhook_retry_card,
        webhook_retry,
    ),
    // ---- settings ----
    entry(
        "settings",
        "beta",
        "feature",
        t("Switch a feature", "تبديل ميزة", "Funktion schalten"),
        &[
            text(
                "feature",
                true,
                48,
                t("Feature ID", "معرّف الميزة", "Funktions-ID"),
            ),
            STATE,
        ],
        P::SettingsManage,
        beta_feature_card,
        beta_feature,
    ),
    entry(
        "settings",
        "beta",
        "member",
        t("Change a tester", "تغيير مختبِر", "Tester ändern"),
        &[
            USER,
            BETA_ACTION,
            text(
                "features",
                false,
                500,
                t(
                    "Feature IDs, comma-separated",
                    "معرّفات الميزات مفصولة بفواصل",
                    "Funktions-IDs, durch Komma getrennt",
                ),
            ),
            text("note", false, 300, t("Note", "ملاحظة", "Notiz")),
        ],
        P::SettingsManage,
        beta_member_card,
        beta_member,
    ),
    entry(
        "settings",
        "policy",
        "publish",
        t("Publish an update", "نشر تحديث", "Änderung veröffentlichen"),
        &[
            text(
                "message_ar",
                true,
                300,
                t(
                    "Message in Arabic",
                    "الرسالة بالعربية",
                    "Nachricht auf Arabisch",
                ),
            ),
            text(
                "message_en",
                true,
                300,
                t(
                    "Message in English",
                    "الرسالة بالإنجليزية",
                    "Nachricht auf Englisch",
                ),
            ),
            text(
                "message_de",
                true,
                300,
                t(
                    "Message in German",
                    "الرسالة بالألمانية",
                    "Nachricht auf Deutsch",
                ),
            ),
            text(
                "url",
                true,
                300,
                t(
                    "Link to the full text (https)",
                    "رابط النص الكامل (https)",
                    "Link zum vollen Text (https)",
                ),
            ),
            integer(
                "days",
                false,
                0.0,
                90.0,
                t(
                    "Effective in days (0-90)",
                    "يسري بعد أيام (0-90)",
                    "Gültig in Tagen (0-90)",
                ),
            ),
        ],
        P::SettingsManage,
        policy_publish_card,
        policy_publish,
    ),
    entry(
        "settings",
        "policy",
        "withdraw",
        t("Withdraw an update", "سحب تحديث", "Änderung zurückziehen"),
        &[
            text(
                "policy",
                true,
                64,
                t("Update ID", "معرّف التحديث", "Änderungs-ID"),
            ),
            REASON,
        ],
        P::SettingsManage,
        policy_withdraw_card,
        policy_withdraw,
    ),
    // ---- the staff team ----
    entry(
        "staff",
        "staff",
        "roles",
        t("Set roles", "تحديد الأدوار", "Rollen setzen"),
        &[
            USER,
            text(
                "roles",
                true,
                200,
                t(
                    "Role names, comma-separated, or none",
                    "أسماء الأدوار مفصولة بفواصل، أو none",
                    "Rollennamen, durch Komma getrennt, oder none",
                ),
            ),
            REASON,
        ],
        P::StaffManage,
        staff_roles_card,
        staff_roles,
    ),
    entry(
        "staff",
        "staff",
        "disable",
        t("Disable access", "تعطيل الصلاحية", "Zugang deaktivieren"),
        &[USER, REASON],
        P::StaffManage,
        staff_disable_card,
        staff_disable,
    ),
    entry(
        "staff",
        "staff",
        "sessions-end",
        t("End sessions", "إنهاء الجلسات", "Sitzungen beenden"),
        &[USER, REASON],
        P::StaffManage,
        staff_sessions_card,
        staff_sessions,
    ),
    entry(
        "staff",
        "security",
        "lock",
        t("Lock a member out", "إقفال عضو", "Mitglied aussperren"),
        &[USER, REASON],
        P::SecurityRespond,
        security_lock_card,
        security_lock,
    ),
];
