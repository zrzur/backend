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
//!   zrzur-staff delete-owned <user-id> [--dry-run] <reason...>
//!       deletes every server and bot of an account staff banned, except
//!       official ones (the same code and audit records as the admin
//!       console's `POST /staff/users/<id>/delete-owned`); prints the plan
//!       and asks for the account's exact username#discriminator first.
//!       `--dry-run` prints the plan only. There is no way to skip the
//!       question.
//!
//!   zrzur-staff badge grant|revoke <user-id> <badge> --as <staff-user-id> <reason...>
//!       the same gate as the admin console (`mutate_badge`), with the
//!       rights of the `--as` staff member, after their TOTP code; sensitive
//!       changes wait for a second staff member in the console
//!   zrzur-staff appoint <user-id> staff|platform_moderator --as <owner-id>
//!       --second <staff-id> [--training-acknowledged] <reason...>
//!       the only way to give the Staff or Platform Moderator badge; two
//!       people, both TOTP codes
//!   zrzur-staff end-appointment <user-id> staff|platform_moderator --as <owner-id>
//!       --second <staff-id> <reason...>     (takes the badge away)
//!   zrzur-staff appointments    (read only: records, and holders without one)
//!
//! The official bot mark changes only here. Founder changes nowhere: it
//! follows `[api.users] founder_id` in Revolt.toml.
//!
//! This is the only way to create or remove an Owner. There is no HTTP
//! path, no default account and no password. The target account must have
//! TOTP switched on. Every change is written to the platform audit log.

use revolt_database::{
    appoint_staff, appointment_gaps, create_system_bot, end_staff_appointment, end_staff_badge_with_grant, is_system_bot_name,
    mutate_badge, record_platform_audit, set_official_bot, staff_now, verify_audit_chain,
    verify_staff_totp, AuditOutcome, AuditRecord, BadgeActor, BadgeChange, BadgeMutation,
    BadgeOutcome, BadgePath, Database, DatabaseInfo, StaffGrant, StaffRole, Totp, STAFF_ACTOR_CLI,
    STAFF_BREAK_GLASS_MS,
};

fn usage() -> ! {
    eprintln!(
        "usage: zrzur-staff list | grant-owner <user-id> <reason> | break-glass <user-id> <reason> | revoke <user-id> <reason> | verify-audit | system-bot <owner-user-id> <name> <reason> | official-bot <bot-id> <reason> | official-bot-off <bot-id> <reason> | delete-owned <user-id> [--dry-run] <reason> | badge grant|revoke <user-id> <badge> --as <staff-id> <reason> | appoint <user-id> staff|platform_moderator --as <owner-id> --second <staff-id> [--training-acknowledged] <reason> | end-appointment <user-id> staff|platform_moderator --as <owner-id> --second <staff-id> <reason> | appointments"
    );
    std::process::exit(2)
}

fn fail(message: impl std::fmt::Display) -> ! {
    terminal::restore();
    eprintln!("error: {message}");
    std::process::exit(1)
}

/// A TOTP code is typed without being shown: terminal echo is switched off
/// with `stty` (no extra crate) while it is read, and put back right after,
/// on every error and on Ctrl+C.
mod terminal {
    use std::io::IsTerminal;
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, Once};

    /// `stty -g` from before echo was switched off
    static SAVED: Mutex<Option<String>> = Mutex::new(None);
    static CTRL_C: Once = Once::new();

    fn stty(args: &[&str]) -> Option<String> {
        let output = Command::new("stty")
            .args(args)
            .stdin(Stdio::inherit())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Put the terminal back as it was (no-op when nothing was changed)
    pub fn restore() {
        let saved = SAVED.lock().map(|mut saved| saved.take()).unwrap_or(None);
        if let Some(saved) = saved {
            let _ = stty(&[&saved]);
        }
    }

    /// Switch echo off. `Err` when stdin is a terminal and echo could not be
    /// switched off: then the code is not asked for at all.
    fn hide() -> Result<(), &'static str> {
        if !std::io::stdin().is_terminal() {
            return Ok(());
        }
        CTRL_C.call_once(|| {
            rocket::tokio::spawn(async {
                if rocket::tokio::signal::ctrl_c().await.is_ok() {
                    restore();
                    eprintln!();
                    std::process::exit(130);
                }
            });
        });
        let saved = stty(&["-g"]).filter(|s| !s.is_empty()).ok_or("could not read the terminal settings")?;
        if let Ok(mut slot) = SAVED.lock() {
            *slot = Some(saved);
        }
        stty(&["-echo"]).map(|_| ()).ok_or_else(|| {
            restore();
            "could not hide what is typed"
        })
    }

    /// One line from stdin, typed without echo
    pub async fn read_hidden(prompt: &str) -> Result<String, &'static str> {
        eprint!("{prompt}");
        hide()?;
        let read = rocket::tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).map(|_| line)
        })
        .await;
        restore();
        eprintln!();
        match read {
            Ok(Ok(line)) => Ok(line),
            _ => Err("could not read the code"),
        }
    }
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
    let mut grant = StaffGrant {
        id: user_id.to_string(),
        roles: roles.clone(),
        disabled: false,
        expires_at,
        last_totp_step: current.as_ref().map(|g| g.last_totp_step).unwrap_or(0),
        version: current.as_ref().map(|g| g.version + 1).unwrap_or(1),
        updated_at: staff_now(),
        updated_by: STAFF_ACTOR_CLI.to_string(),
        active_since: None,
        activated_by: None,
    };
    grant.carry_activation(current.as_ref(), staff_now(), STAFF_ACTOR_CLI);
    let end_reason = reason.clone();
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
    // no access left: the Staff badge goes too (the appointment end)
    if roles.is_empty() {
        match end_staff_badge_with_grant(db, STAFF_ACTOR_CLI, user_id, &end_reason).await {
            Ok(true) => println!("{user_id}: Staff badge and appointment ended with the grant"),
            Ok(false) => {}
            Err(e) => fail(format!("grant removed, but the Staff badge could not be ended: {e:?}; run: zrzur-staff end-appointment")),
        }
    }
    println!(
        "{user_id} ({}): roles {}; {ended} staff session(s) ended",
        user.username,
        if after.is_empty() { "none" } else { &after }
    );
}

/// `delete-owned`: plan, typed confirmation, then the shared deletion
async fn delete_owned(db: &Database, user_id: &str, rest: &[String]) {
    let actor = AuditRecord {
        actor_id: STAFF_ACTOR_CLI.to_string(),
        ..Default::default()
    };
    // what the API sets up at start: events go out through RabbitMQ (and
    // Redis), voice rooms are closed through LiveKit. Connected before the
    // question, so a refused connection stops the run before anything.
    let setup = || async {
        let amqp = rocket::tokio::spawn(revolt_database::AMQP::new_auto());
        match rocket::tokio::time::timeout(std::time::Duration::from_secs(15), amqp).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => return Err("could not connect to RabbitMQ".to_string()),
            Err(_) => return Err("RabbitMQ did not answer within 15 seconds".to_string()),
        }
        let config = revolt_config::config().await;
        Ok(revolt_database::voice::VoiceClient::new(config.api.livekit.nodes.clone()))
    };
    let code = revolt_database::staff_deletion::run_delete_owned_command(
        db,
        &actor,
        user_id,
        rest,
        &mut std::io::BufReader::new(std::io::stdin()),
        &mut std::io::stdout(),
        setup,
    )
    .await;
    std::process::exit(code)
}

/// Take `--name value` out of `args`
fn take_flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    if at + 1 >= args.len() {
        usage();
    }
    let value = args.remove(at + 1);
    args.remove(at);
    Some(value)
}

/// Take a `--switch` out of `args`
fn take_switch(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|a| a == name) {
        Some(at) => {
            args.remove(at);
            true
        }
        None => false,
    }
}

/// The TOTP code of staff member `user_id`, typed on the terminal: checked
/// against their account and used once (the replay guard of the console).
/// A wrong code is written to the audit log and stops the command. The code
/// itself is never written anywhere.
async fn confirm_totp(db: &Database, user_id: &str, action: &str) {
    let grant = db
        .fetch_staff_grant(user_id)
        .await
        .unwrap_or_else(|e| fail(format!("{e:?}")))
        .filter(|g| g.is_active(staff_now()))
        .unwrap_or_else(|| fail(format!("{user_id} has no active staff grant")));
    let account = db
        .fetch_account(user_id)
        .await
        .unwrap_or_else(|_| fail("no account for this user"));
    let Totp::Enabled { secret } = account.mfa.totp_token else {
        fail(format!("{user_id} has no TOTP switched on"));
    };
    let code = terminal::read_hidden(&format!("TOTP code of {user_id} (not shown): "))
        .await
        .unwrap_or_else(|e| fail(e));
    let now_secs = (staff_now() / 1000).max(0) as u64;
    let claimed = match verify_staff_totp(&secret, &code, now_secs, grant.last_totp_step) {
        Some(step) => db.claim_staff_totp_step(user_id, step).await.unwrap_or(false),
        None => false,
    };
    if !claimed {
        let _ = record_platform_audit(
            db,
            AuditRecord {
                actor_id: user_id.to_string(),
                action: format!("{action}.totp"),
                summary: Some("wrong or reused TOTP code on the command line".into()),
                ..Default::default()
            },
            AuditOutcome::Denied,
        )
        .await;
        fail("wrong or reused TOTP code; nothing changed");
    }
}

/// `badge grant|revoke`: through the one gate, with the operator's rights
async fn badge_command(db: &Database, mut rest: Vec<String>) {
    let operator = take_flag(&mut rest, "--as").unwrap_or_else(|| fail("say who acts: --as <staff-user-id>"));
    if rest.len() < 3 {
        usage();
    }
    let change = match rest[0].as_str() {
        "grant" => BadgeChange::Grant,
        "revoke" => BadgeChange::Revoke,
        _ => usage(),
    };
    let (user, badge) = (rest[1].clone(), rest[2].clone());
    let reason = rest[3..].join(" ");
    confirm_totp(db, &operator, "staff.cli.badge").await;
    let mut actor = BadgeActor::from_staff_grant(db, &operator, BadgePath::Cli, staff_now())
        .await
        .unwrap_or_else(|e| fail(format!("{e:?}")));
    // the TOTP code just typed is the step-up
    actor.step_up = true;
    let outcome = mutate_badge(
        db,
        &actor,
        BadgeMutation {
            user_id: user.clone(),
            badge: badge.clone(),
            change,
            reason,
            expires_at: None,
            source_ref: None,
        },
    )
    .await
    .unwrap_or_else(|e| fail(format!("refused or failed (see the audit log): {:?}", e.error_type)));
    match outcome {
        BadgeOutcome::Applied(_) => {
            let done = if change == BadgeChange::Grant { "given" } else { "taken away" };
            println!("{user}: {badge} {done}")
        }
        BadgeOutcome::Pending(request) => println!(
            "{user}: {badge} waits for a second staff member (request {}); approve it in the admin console",
            request.id
        ),
    }
}

/// `appoint` / `end-appointment`: two people, both TOTP codes
async fn appointment_command(db: &Database, end: bool, mut rest: Vec<String>) {
    let first = take_flag(&mut rest, "--as").unwrap_or_else(|| fail("say who acts: --as <owner-user-id>"));
    let second = take_flag(&mut rest, "--second")
        .unwrap_or_else(|| fail("a second person is needed: --second <staff-user-id>"));
    let training = take_switch(&mut rest, "--training-acknowledged");
    if rest.len() < 2 {
        usage();
    }
    let (user, role) = (rest[0].clone(), rest[1].clone());
    let reason = rest[2..].join(" ");
    if first == second {
        fail("the second person must be someone else");
    }
    confirm_totp(db, &first, "staff.cli.appointment").await;
    confirm_totp(db, &second, "staff.cli.appointment").await;
    let done = if end {
        end_staff_appointment(db, &first, &second, &user, &role, &reason).await.map(|_| ())
    } else {
        appoint_staff(db, &first, &second, &user, &role, &reason, training).await.map(|_| ())
    };
    done.unwrap_or_else(|e| fail(format!("refused or failed (see the audit log): {:?}", e.error_type)));
    let what = if end { "appointment ended, badge taken away" } else { "appointed, badge given" };
    println!("{user}: {role} {what}");
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
            println!(
                "{} ({}) token: {}",
                user.id,
                revolt_models::v0::user_handle(&user.username, &user.discriminator),
                bot.token
            );
        }
        ("official-bot", Some(user)) | ("official-bot-off", Some(user)) => {
            let mark = command == "official-bot";
            record(&db, "staff.cli.official_bot", user, reason(), format!("mark {mark}")).await;
            let user = set_official_bot(&db, user, mark)
                .await
                .unwrap_or_else(|e| fail(format!("{e:?}")));
            println!("{} ({}): official bot {mark}", user.id, user.username);
        }
        ("delete-owned", Some(user)) => delete_owned(&db, user, &args[2..]).await,
        ("badge", Some(_)) => badge_command(&db, args[1..].to_vec()).await,
        ("appoint", Some(_)) => appointment_command(&db, false, args[1..].to_vec()).await,
        ("end-appointment", Some(_)) => appointment_command(&db, true, args[1..].to_vec()).await,
        ("appointments", _) => {
            let appointments = db
                .fetch_staff_appointments()
                .await
                .unwrap_or_else(|e| fail(format!("{e:?}")));
            for appointment in appointments {
                println!(
                    "{}  {}  appointed_at={}  by={}  ended_at={}",
                    appointment.user_id,
                    appointment.role,
                    appointment.appointed_at,
                    appointment.appointed_by.join(","),
                    appointment
                        .ended_at
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "-".into())
                );
            }
            let gaps = appointment_gaps(&db).await.unwrap_or_else(|e| fail(format!("{e:?}")));
            println!(
                "holders without an appointment (they keep the badge until the owner confirms or ends it): {}",
                gaps.len()
            );
            for (badge, user) in gaps {
                println!("  {user}  {badge}");
            }
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
