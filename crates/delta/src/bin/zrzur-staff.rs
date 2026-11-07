//! Platform staff from the command line. Run on the server host, with the
//! same configuration (and database access) as the API.
//!
//!   zrzur-staff list
//!   zrzur-staff grant-owner <user-id> <reason...>
//!   zrzur-staff break-glass <user-id> <reason...>   (Owner for 24 hours)
//!   zrzur-staff revoke <user-id> <reason...>        (removes every role)
//!   zrzur-staff verify-audit
//!   zrzur-staff system-bot <owner-user-id> <name> <reason...>
//!       creates a bot on the allowlist `SYSTEM_BOTS`, marked; prints the
//!       token once
//!   zrzur-staff official-bot <bot-id> <reason...>        (gives the mark)
//!   zrzur-staff official-bot-off <bot-id> <reason...>    (removes the mark)
//!
//! The official bot mark changes only here.
//!
//! This is the only way to create or remove an Owner. There is no HTTP
//! path, no default account and no password. The target account must have
//! TOTP switched on. Every change is written to the platform audit log.

use revolt_database::{
    create_system_bot, is_system_bot_name, record_platform_audit, set_official_bot, staff_now,
    verify_audit_chain, AuditOutcome, AuditRecord, Database, DatabaseInfo, StaffGrant, StaffRole,
    Totp, STAFF_ACTOR_CLI, STAFF_BREAK_GLASS_MS,
};

fn usage() -> ! {
    eprintln!(
        "usage: zrzur-staff list | grant-owner <user-id> <reason> | break-glass <user-id> <reason> | revoke <user-id> <reason> | verify-audit | system-bot <owner-user-id> <name> <reason> | official-bot <bot-id> <reason> | official-bot-off <bot-id> <reason>"
    );
    std::process::exit(2)
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("error: {message}");
    std::process::exit(1)
}

/// Write the record before the change: fail closed, like `set_roles`
async fn record(db: &Database, action: &str, target: &str, reason: String, summary: String) {
    record_platform_audit(
        db,
        AuditRecord {
            actor_id: STAFF_ACTOR_CLI.to_string(),
            action: action.to_string(),
            target_kind: Some("user".into()),
            target_id: Some(target.to_string()),
            reason: Some(reason),
            summary: Some(summary),
            ..Default::default()
        },
        AuditOutcome::Success,
    )
    .await
    .unwrap_or_else(|e| fail(format!("audit log write failed, nothing changed: {e:?}")));
}

async fn set_roles(
    db: &Database,
    user_id: &str,
    roles: Vec<StaffRole>,
    expires_at: Option<i64>,
    action: &str,
    reason: String,
) {
    let user = db
        .fetch_user(user_id)
        .await
        .unwrap_or_else(|_| fail("no such user"));
    if user.bot.is_some() {
        fail("bots can't be staff");
    }
    if !roles.is_empty() {
        let account = db
            .fetch_account(user_id)
            .await
            .unwrap_or_else(|_| fail("no account for this user"));
        if !matches!(account.mfa.totp_token, Totp::Enabled { .. }) {
            fail("the account must switch on TOTP (two-factor) first");
        }
    }
    let current = db
        .fetch_staff_grant(user_id)
        .await
        .unwrap_or_else(|e| fail(format!("{e:?}")));
    let before = current
        .as_ref()
        .map(|g| g.roles.iter().map(|r| r.name()).collect::<Vec<_>>().join(","))
        .unwrap_or_else(|| "none".into());
    let grant = StaffGrant {
        id: user_id.to_string(),
        roles: roles.clone(),
        disabled: false,
        expires_at,
        last_totp_step: current.as_ref().map(|g| g.last_totp_step).unwrap_or(0),
        version: current.as_ref().map(|g| g.version + 1).unwrap_or(1),
        updated_at: staff_now(),
        updated_by: STAFF_ACTOR_CLI.to_string(),
    };
    // fail closed: the record comes first
    let after = roles.iter().map(|r| r.name()).collect::<Vec<_>>().join(",");
    record_platform_audit(
        db,
        AuditRecord {
            actor_id: STAFF_ACTOR_CLI.to_string(),
            action: action.to_string(),
            target_kind: Some("user".into()),
            target_id: Some(user_id.to_string()),
            reason: Some(reason),
            summary: Some(format!(
                "roles {before} -> {}{}",
                if after.is_empty() { "none" } else { &after },
                expires_at
                    .map(|e| format!(" (until {e})"))
                    .unwrap_or_default()
            )),
            ..Default::default()
        },
        AuditOutcome::Success,
    )
    .await
    .unwrap_or_else(|e| fail(format!("audit log write failed, nothing changed: {e:?}")));
    if !db
        .save_staff_grant(&grant, current.map(|g| g.version))
        .await
        .unwrap_or_else(|e| fail(format!("{e:?}")))
    {
        fail("the grant changed meanwhile; run the command again");
    }
    let ended = db
        .revoke_staff_sessions_for_user(user_id, staff_now())
        .await
        .unwrap_or(0);
    println!(
        "{user_id} ({}): roles {}; {ended} staff session(s) ended",
        user.username,
        if after.is_empty() { "none" } else { &after }
    );
}

#[rocket::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else { usage() };
    let reason = || {
        let reason = args[2..].join(" ");
        if reason.trim().len() < 3 {
            fail("give a reason (at least 3 characters)");
        }
        reason
    };
    let db = DatabaseInfo::Auto
        .connect()
        .await
        .unwrap_or_else(|e| fail(format!("database: {e}")));

    match (command.as_str(), args.get(1)) {
        ("list", _) => {
            let now = staff_now();
            for grant in db.fetch_staff_grants().await.unwrap_or_default() {
                let roles: Vec<_> = grant.roles.iter().map(|r| r.name()).collect();
                println!(
                    "{}  roles={}  active={}  disabled={}  expires_at={}",
                    grant.id,
                    roles.join(","),
                    grant.is_active(now),
                    grant.disabled,
                    grant
                        .expires_at
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "-".into())
                );
            }
        }
        ("grant-owner", Some(user)) => {
            set_roles(&db, user, vec![StaffRole::Owner], None, "staff.cli.grant_owner", reason())
                .await
        }
        ("break-glass", Some(user)) => {
            set_roles(
                &db,
                user,
                vec![StaffRole::Owner],
                Some(staff_now() + STAFF_BREAK_GLASS_MS),
                "staff.cli.break_glass",
                reason(),
            )
            .await
        }
        ("revoke", Some(user)) => {
            set_roles(&db, user, vec![], None, "staff.cli.revoke", reason()).await
        }
        ("system-bot", Some(owner)) => {
            let Some(name) = args.get(2) else { usage() };
            if !is_system_bot_name(name) {
                fail("this name is not on the allowlist of system bots");
            }
            let why = args[3..].join(" ");
            if why.trim().len() < 3 {
                fail("give a reason (at least 3 characters)");
            }
            let owner = db.fetch_user(owner).await.unwrap_or_else(|_| fail("no such user"));
            record(&db, "staff.cli.system_bot", &owner.id, why, name.clone()).await;
            let (bot, user) = create_system_bot(&db, name, &owner)
                .await
                .unwrap_or_else(|e| fail(format!("{e:?}")));
            println!("{} ({}#{}) token: {}", user.id, user.username, user.discriminator, bot.token);
        }
        ("official-bot", Some(user)) | ("official-bot-off", Some(user)) => {
            let mark = command == "official-bot";
            record(&db, "staff.cli.official_bot", user, reason(), format!("mark {mark}")).await;
            let user = set_official_bot(&db, user, mark)
                .await
                .unwrap_or_else(|e| fail(format!("{e:?}")));
            println!("{} ({}): official bot {mark}", user.id, user.username);
        }
        ("verify-audit", _) => {
            let mut from = 1;
            let mut before = None;
            let mut checked = 0;
            loop {
                let page = db
                    .fetch_platform_audit_range(from, 1000)
                    .await
                    .unwrap_or_else(|e| fail(format!("{e:?}")));
                if page.is_empty() {
                    break;
                }
                if let Err(seq) = verify_audit_chain(before.as_ref(), &page) {
                    fail(format!("audit chain broken at seq {seq} ({checked} records fine before it)"));
                }
                checked += page.len();
                from = page.last().map(|e| e.seq + 1).unwrap_or(from);
                before = page.into_iter().last();
            }
            println!("audit chain intact: {checked} records");
        }
        _ => usage(),
    }
}
