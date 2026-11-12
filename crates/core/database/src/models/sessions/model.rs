use iso8601_timestamp::{Duration, Timestamp};

use crate::{events::client::EventV1, Database};
use revolt_config::config;
use revolt_result::{ErrorType, Result};

auto_derived_partial!(
    /// Session information
    pub struct Session {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,

        /// User Id
        pub user_id: String,

        /// Session token, in memory only: the client gets it once, at sign-in,
        /// and it is never stored or sent back in an API response
        #[serde(skip)]
        pub token: String,

        /// SHA-256 of the token: the only form kept in the database. A session
        /// stored before this field existed has none, so it no longer works
        /// and its owner signs in again (migration 87 removes those rows).
        #[serde(rename = "token_hash", default)]
        pub token_hash: String,

        /// Display name
        pub name: String,

        /// When the session was last used, to within about an hour
        pub last_seen: Timestamp,

        /// The session does not expire before this, whatever `last_seen`
        /// says. A new session has its creation time; one that existed when
        /// expiry was introduced gets a date from the migration, because HTTP
        /// use was not recorded until then. Missing means the migration has
        /// not reached it, and it does not expire. Eviction ignores this, so
        /// it never makes an old session look recently used.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub idle_grace_until: Option<Timestamp>,

        /// Where the session originated from
        ///
        /// This could be used to differentiate sessions that come from staging/test vs prod, etc.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub origin: Option<String>,

        /// Web Push subscription
        #[serde(skip_serializing_if = "Option::is_none")]
        pub subscription: Option<WebPushSubscription>,
    },
    "PartialSession"
);

/// `last_seen` is written at most this often per session
pub const SESSION_TOUCH_EVERY_HOURS: i64 = 1;

/// How many idle sessions the daily sweep reads at a time
const IDLE_SWEEP_BATCH: i64 = 1000;

auto_derived!(
    /// Web Push subscription
    pub struct WebPushSubscription {
        pub endpoint: String,
        pub p256dh: String,
        pub auth: String,
    }
);

/// Web Push goes only to the push services browsers use. The endpoint comes
/// from the client and pushd sends to it, so anything else would let a client
/// aim our servers at any address.
static WEB_PUSH_ENDPOINT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"^https://(?:[a-z0-9-]+\.)*(?:fcm\.googleapis\.com|push\.apple\.com|push\.services\.mozilla\.com|notify\.windows\.com)(?::443)?/[\x21-\x7e]*$",
    )
    .expect("valid regex")
});

impl WebPushSubscription {
    const MAX_ENDPOINT: usize = 1024;
    const MAX_P256DH: usize = 128;
    const MAX_AUTH: usize = 64;

    /// Whether pushd may send to this web push endpoint
    pub fn web_endpoint_is_allowed(endpoint: &str) -> bool {
        endpoint.len() <= Self::MAX_ENDPOINT && WEB_PUSH_ENDPOINT.is_match(endpoint)
    }

    /// Whether a client may register this subscription. The endpoints `apn`
    /// and `fcm` mean the phone services, with a device token in `auth`.
    pub fn is_allowed(&self) -> bool {
        let mobile = matches!(self.endpoint.as_str(), "apn" | "fcm");
        (mobile || Self::web_endpoint_is_allowed(&self.endpoint))
            && self.p256dh.len() <= Self::MAX_P256DH
            && (mobile || self.auth.len() <= Self::MAX_AUTH)
            && self.auth.len() <= Self::MAX_ENDPOINT
    }
}

#[cfg(test)]
mod push_endpoint_tests {
    use super::WebPushSubscription;

    fn subscription(endpoint: &str) -> WebPushSubscription {
        WebPushSubscription {
            endpoint: endpoint.to_owned(),
            p256dh: "key".to_owned(),
            auth: "auth".to_owned(),
        }
    }

    #[test]
    fn browser_push_services_are_allowed() {
        for endpoint in [
            "https://fcm.googleapis.com/fcm/send/abc:DEF-1_2",
            "https://updates.push.services.mozilla.com/wpush/v2/gAAAA",
            "https://web.push.apple.com/QAbc",
            "https://wns2-par02p.notify.windows.com/w/?token=BQYAAA%3d",
        ] {
            assert!(subscription(endpoint).is_allowed(), "{endpoint}");
        }
    }

    #[test]
    fn the_phone_services_keep_working() {
        for endpoint in ["apn", "fcm"] {
            let mut phone = subscription(endpoint);
            phone.auth = "t".repeat(200);
            assert!(phone.is_allowed(), "{endpoint}");
        }
    }

    #[test]
    fn anything_else_is_refused() {
        for endpoint in [
            "http://fcm.googleapis.com/fcm/send/a",
            "https://127.0.0.1/",
            "https://169.254.169.254/latest/meta-data",
            "https://mongo:27017/",
            "https://evil.example/",
            "https://fcm.googleapis.com.evil.example/",
            "https://fcm.googleapis.com@evil.example/",
            "https://evilfcm.googleapis.com.evil.example/x",
            "https://notfcm.googleapis.com:8080/",
            "https://fcm.googleapis.com/a b",
            "ftp://fcm.googleapis.com/",
            "",
        ] {
            assert!(!subscription(endpoint).is_allowed(), "{endpoint}");
        }
    }

    #[test]
    fn long_values_are_refused() {
        let long = format!("https://fcm.googleapis.com/{}", "a".repeat(1024));
        assert!(!subscription(&long).is_allowed());

        let mut big_key = subscription("https://fcm.googleapis.com/a");
        big_key.p256dh = "k".repeat(129);
        assert!(!big_key.is_allowed());

        let mut big_auth = subscription("https://fcm.googleapis.com/a");
        big_auth.auth = "k".repeat(65);
        assert!(!big_auth.is_allowed());
    }
}

impl Session {
    /// Which sessions to close so at most `limit` stay, `keep` among them:
    /// the least recently used first, the older id when times are equal
    pub fn over_limit(mut sessions: Vec<Session>, keep: &str, limit: usize) -> Vec<Session> {
        sessions.retain(|session| session.id != keep);

        // `keep` takes one place, and a limit of 0 must not lock anyone out
        let excess = sessions.len().saturating_sub(limit.saturating_sub(1));
        sessions.sort_by(|a, b| (a.last_seen, &a.id).cmp(&(b.last_seen, &b.id)));
        sessions.truncate(excess);
        sessions
    }

    /// Whether this session has gone unused for `idle_days` at `now`.
    /// With 0 days nothing expires, and neither does a session with no
    /// `idle_grace_until`: only delta runs migrations, so other services can
    /// be ahead of it, and a client told `InvalidSession` drops its token.
    pub fn is_idle_at(&self, now: Timestamp, idle_days: u32) -> bool {
        idle_days != 0
            && now >= self.last_seen + Duration::days(idle_days.into())
            && self.idle_grace_until.is_some_and(|until| now >= until)
    }

    /// Rename. Writes only the name: a session closed in the meantime stays
    /// closed, and this fails.
    pub async fn rename(&mut self, db: &Database, name: String) -> Result<()> {
        db.update_session_name(&self.id, &name).await?;
        self.name = name;
        Ok(())
    }

    /// Set the push subscription, same as `rename`
    pub async fn set_subscription(
        &mut self,
        db: &Database,
        subscription: WebPushSubscription,
    ) -> Result<()> {
        db.update_session_subscription(&self.id, &subscription).await?;
        self.subscription = Some(subscription);
        Ok(())
    }

    /// Remove the push subscription
    pub async fn remove_subscription(&mut self, db: &Database) -> Result<()> {
        db.remove_push_subscription_by_session_id(&self.id).await?;
        self.subscription = None;
        Ok(())
    }

    /// Record use of a session that is not expired, for a caller that does
    /// not hold it: an open socket. False means the session is closed or
    /// expired, and nothing was written, so a ping cannot revive it.
    pub async fn touch(db: &Database, id: &str, now: Timestamp) -> Result<bool> {
        let idle_days = config().await.api.sessions.idle_days;
        let idle_before =
            (idle_days != 0).then(|| now + Duration::days(-i64::from(idle_days)));
        db.touch_session(id, now, idle_before).await
    }

    /// Whether a session is there and not expired, reading only
    pub async fn is_open(db: &Database, id: &str, now: Timestamp) -> Result<bool> {
        match db.fetch_session(id).await {
            Ok(session) => Ok(!session.is_idle_at(now, config().await.api.sessions.idle_days)),
            Err(error) if matches!(error.error_type, ErrorType::UnknownUser) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Sessions of these users that push may go to: those with a subscription
    /// that have not expired, swept or not
    pub async fn fetch_for_push(db: &Database, user_ids: &[String]) -> Result<Vec<Session>> {
        let now = Timestamp::now_utc();
        let idle_days = config().await.api.sessions.idle_days;
        let mut sessions = db.fetch_sessions_with_subscription(user_ids).await?;
        sessions.retain(|session| !session.is_idle_at(now, idle_days));
        Ok(sessions)
    }

    /// Whether use at `now` should be written down
    pub fn needs_touch_at(&self, now: Timestamp) -> bool {
        now >= self.last_seen + Duration::hours(SESSION_TOUCH_EVERY_HOURS)
    }

    /// The session a token stands for. Every session token passes here, so
    /// this is where an idle one is refused and where HTTP use is recorded.
    ///
    /// The session is already in hand, so this adds no query to a request:
    /// only a write, at most once an hour per session. Refusing writes
    /// nothing; crond removes the session.
    pub async fn from_token(db: &Database, token: &str) -> Result<Session> {
        let mut session = db.fetch_session_by_token(token).await?;
        let now = Timestamp::now_utc();

        if session.is_idle_at(now, config().await.api.sessions.idle_days) {
            return Err(create_error!(InvalidSession));
        }

        if session.needs_touch_at(now) {
            // losing this write costs at most an hour of accuracy, it must
            // not refuse a good session
            match db.update_session_last_seen(&session.id, now).await {
                Ok(()) => session.last_seen = now,
                Err(error) => log::warn!("Could not record use of a session: {error:?}"),
            }
        }

        Ok(session)
    }

    /// Close every session that has gone unused, each with the normal delete,
    /// and return how many were closed
    pub async fn delete_idle(db: &Database) -> Result<usize> {
        let idle_days = config().await.api.sessions.idle_days;
        if idle_days == 0 {
            return Ok(0);
        }

        let now = Timestamp::now_utc();
        let before = now + Duration::days(-i64::from(idle_days));
        let mut closed = 0;

        loop {
            let batch = db.fetch_idle_sessions(before, now, IDLE_SWEEP_BATCH).await?;
            let full = batch.len() as i64 == IDLE_SWEEP_BATCH;
            let mut closed_now = 0;

            for session in batch {
                // someone may have logged it out since it was read
                match session.delete(db).await {
                    Ok(()) => closed_now += 1,
                    Err(error) => log::warn!("Could not close an idle session: {error:?}"),
                }
            }

            closed += closed_now;
            if !full || closed_now == 0 {
                return Ok(closed);
            }
        }
    }

    /// Delete session
    pub async fn delete(self, db: &Database) -> Result<()> {
        // Delete from database
        db.delete_session(&self.id).await?;

        // Create and push event
        EventV1::DeleteSession {
            user_id: self.user_id.clone(),
            session_id: self.id,
        }
        .p(self.user_id)
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod session_rule_tests {
    use super::Session;
    use crate::Account;
    use iso8601_timestamp::{Duration, Timestamp};
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};
    use revolt_result::ErrorType;

    fn session(id: &str, last_seen: Timestamp) -> Session {
        Session {
            id: id.to_owned(),
            user_id: "user".to_owned(),
            token: format!("token-{id}"),
            token_hash: crate::models::hash_secret(&format!("token-{id}")),
            name: id.to_owned(),
            last_seen,
            idle_grace_until: Some(Timestamp::UNIX_EPOCH),
            origin: None,
            subscription: None,
        }
    }

    /// Stored times keep milliseconds
    fn ms(time: Timestamp) -> i128 {
        time.duration_since(Timestamp::UNIX_EPOCH).whole_milliseconds()
    }

    fn ids(sessions: &[Session]) -> Vec<&str> {
        sessions.iter().map(|session| session.id.as_str()).collect()
    }

    async fn account(db: &crate::Database) -> Account {
        Account::new(
            db,
            format!("{}@zrzur.cc", ulid::Ulid::new()),
            "password_insecure".to_owned(),
            false,
        )
        .await
        .unwrap()
    }

    #[test]
    fn the_least_recently_used_go_first() {
        let now = Timestamp::now_utc();
        let sessions = vec![
            session("used-today", now),
            session("used-long-ago", now + Duration::days(-9)),
            session("new", now),
            session("used-last-week", now + Duration::days(-7)),
        ];
        let closed = Session::over_limit(sessions, "new", 2);
        assert_eq!(ids(&closed), ["used-long-ago", "used-last-week"]);
    }

    #[test]
    fn the_new_session_is_never_closed() {
        let now = Timestamp::now_utc();
        // a clock that jumped: the new session looks like the oldest
        let sessions = vec![
            session("new", now + Duration::days(-1)),
            session("a", now),
            session("b", now),
        ];
        assert_eq!(ids(&Session::over_limit(sessions.clone(), "new", 1)), ["a", "b"]);
        // a limit of 0 in a config must not lock anyone out
        assert_eq!(ids(&Session::over_limit(sessions, "new", 0)), ["a", "b"]);
    }

    #[test]
    fn nothing_closes_at_or_under_the_limit() {
        let now = Timestamp::now_utc();
        let sessions = vec![session("a", now), session("b", now)];
        assert!(Session::over_limit(sessions.clone(), "b", 2).is_empty());
        assert!(Session::over_limit(sessions, "b", 3).is_empty());
    }

    #[test]
    fn equal_times_close_the_older_id_first() {
        let now = Timestamp::now_utc();
        let sessions = vec![session("2", now), session("1", now), session("3", now)];
        assert_eq!(ids(&Session::over_limit(sessions, "3", 2)), ["1"]);
    }

    #[test]
    fn a_session_is_idle_from_exactly_the_last_allowed_day() {
        let now = Timestamp::now_utc();
        let used = session("a", now);
        assert!(!used.is_idle_at(now + Duration::days(30) + Duration::seconds(-1), 30));
        assert!(used.is_idle_at(now + Duration::days(30), 30));
        // 0 switches expiry off
        assert!(!used.is_idle_at(now + Duration::days(3000), 0));
    }

    #[test]
    fn the_grace_date_keeps_an_old_session_alive_until_it_passes() {
        let now = Timestamp::now_utc();
        let mut old = session("a", now + Duration::days(-400));
        old.idle_grace_until = Some(now + Duration::days(30));
        assert!(!old.is_idle_at(now, 30));
        assert!(old.is_idle_at(now + Duration::days(30), 30));
        // once used again it counts from the use, not from the grace date
        old.last_seen = now + Duration::days(29);
        assert!(!old.is_idle_at(now + Duration::days(31), 30));
    }

    #[test]
    fn a_session_without_a_grace_date_has_not_been_migrated_and_is_not_idle() {
        let now = Timestamp::now_utc();
        let mut old = session("a", now + Duration::days(-400));
        old.idle_grace_until = None;
        assert!(!old.is_idle_at(now, 30));
    }

    #[tokio::test]
    async fn a_new_session_is_ready_to_expire_from_the_start() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let new = account.create_session(&db, "new".to_owned()).await.unwrap();
            let stored = db.fetch_session(&new.id).await.unwrap();
            assert!(stored.idle_grace_until.is_some());
        });
    }

    #[tokio::test]
    async fn before_the_migration_old_sessions_work_and_are_not_swept() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let mut old = session("not-migrated", Timestamp::now_utc() + Duration::days(-400));
            old.user_id = account.id.clone();
            old.idle_grace_until = None;
            db.save_session(&old).await.unwrap();

            assert_eq!(Session::delete_idle(&db).await.unwrap(), 0);
            assert!(Session::from_token(&db, &old.token).await.is_ok());
        });
    }

    #[tokio::test]
    async fn a_write_after_a_close_does_not_bring_the_session_back() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let open = account.create_session(&db, "open".to_owned()).await.unwrap();

            // two requests that hold the session while it is closed
            let mut renaming = open.clone();
            let mut subscribing = open.clone();
            open.clone().delete(&db).await.unwrap();

            assert!(renaming.rename(&db, "renamed".to_owned()).await.is_err());
            assert!(db.fetch_session(&open.id).await.is_err());

            let subscription = crate::WebPushSubscription {
                endpoint: "https://fcm.googleapis.com/fcm/send/a".to_owned(),
                p256dh: "key".to_owned(),
                auth: "auth".to_owned(),
            };
            assert!(subscribing.set_subscription(&db, subscription).await.is_err());
            assert!(db.fetch_session(&open.id).await.is_err());
            assert!(db.fetch_session_by_token(&open.token).await.is_err());

            // nothing to remove, nothing to report
            subscribing.remove_subscription(&db).await.unwrap();
            assert!(db.fetch_session(&open.id).await.is_err());
        });
    }

    #[tokio::test]
    async fn a_closed_session_gives_the_error_each_route_needs() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let open = account.create_session(&db, "open".to_owned()).await.unwrap();
            let mut renaming = open.clone();
            let mut subscribing = open.clone();
            open.clone().delete(&db).await.unwrap();

            // rename is about another device: not found, as the same route
            // answers one read a moment later. Not the caller's session.
            let error = renaming.rename(&db, "renamed".to_owned()).await.unwrap_err();
            assert!(matches!(error.error_type, ErrorType::UnknownUser));

            // subscribe is about the caller's own session
            let subscription = crate::WebPushSubscription {
                endpoint: "https://fcm.googleapis.com/fcm/send/a".to_owned(),
                p256dh: "key".to_owned(),
                auth: "auth".to_owned(),
            };
            let error = subscribing
                .set_subscription(&db, subscription)
                .await
                .unwrap_err();
            assert!(matches!(error.error_type, ErrorType::InvalidSession));
        });
    }

    #[tokio::test]
    async fn a_new_session_never_replaces_another() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let open = account.create_session(&db, "open".to_owned()).await.unwrap();

            let mut same = open.clone();
            same.name = "other".to_owned();
            assert!(db.insert_session(&same).await.is_err());
            assert_eq!(db.fetch_session(&open.id).await.unwrap().name, "open");
        });
    }

    #[tokio::test]
    async fn is_open_reads_and_writes_nothing() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let now = Timestamp::now_utc();

            let mut open = account.create_session(&db, "open".to_owned()).await.unwrap();
            open.last_seen = now + Duration::days(-2);
            db.save_session(&open).await.unwrap();
            assert!(Session::is_open(&db, &open.id, now).await.unwrap());
            let stored = db.fetch_session(&open.id).await.unwrap();
            assert_eq!(ms(stored.last_seen), ms(open.last_seen));

            let mut idle = account.create_session(&db, "idle".to_owned()).await.unwrap();
            idle.last_seen = now + Duration::days(-31);
            idle.idle_grace_until = Some(idle.last_seen);
            db.save_session(&idle).await.unwrap();
            assert!(!Session::is_open(&db, &idle.id, now).await.unwrap());

            assert!(!Session::is_open(&db, "gone", now).await.unwrap());
        });
    }

    #[tokio::test]
    async fn writes_change_only_what_they_are_for() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let open = account.create_session(&db, "open".to_owned()).await.unwrap();

            // a request that read the session before it was last used
            let mut late = open.clone();
            let used = Timestamp::now_utc() + Duration::days(1);
            db.update_session_last_seen(&open.id, used).await.unwrap();
            late.rename(&db, "renamed".to_owned()).await.unwrap();

            let stored = db.fetch_session(&open.id).await.unwrap();
            assert_eq!(stored.name, "renamed");
            assert_eq!(ms(stored.last_seen), ms(used));
        });
    }

    #[tokio::test]
    async fn a_subscription_can_be_set_and_removed() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let mut open = account.create_session(&db, "open".to_owned()).await.unwrap();
            let subscription = crate::WebPushSubscription {
                endpoint: "https://fcm.googleapis.com/fcm/send/a".to_owned(),
                p256dh: "key".to_owned(),
                auth: "auth".to_owned(),
            };

            open.set_subscription(&db, subscription).await.unwrap();
            assert!(db.fetch_session(&open.id).await.unwrap().subscription.is_some());

            open.remove_subscription(&db).await.unwrap();
            assert!(open.subscription.is_none());
            assert!(db.fetch_session(&open.id).await.unwrap().subscription.is_none());
        });
    }

    #[tokio::test]
    async fn touch_records_use_and_says_when_a_session_is_finished() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let now = Timestamp::now_utc();

            let open = account.create_session(&db, "open".to_owned()).await.unwrap();
            assert!(Session::touch(&db, &open.id, now).await.unwrap());
            let stored = db.fetch_session(&open.id).await.unwrap();
            assert_eq!(ms(stored.last_seen), ms(now));

            // closed
            open.clone().delete(&db).await.unwrap();
            assert!(!Session::touch(&db, &open.id, now).await.unwrap());
            assert!(db.fetch_session(&open.id).await.is_err());

            // expired: a ping must not bring it back to life
            let mut idle = account.create_session(&db, "idle".to_owned()).await.unwrap();
            idle.last_seen = now + Duration::days(-31);
            idle.idle_grace_until = Some(idle.last_seen);
            db.save_session(&idle).await.unwrap();
            assert!(!Session::touch(&db, &idle.id, now).await.unwrap());
            let stored = db.fetch_session(&idle.id).await.unwrap();
            assert_eq!(ms(stored.last_seen), ms(idle.last_seen));

            // still in its grace
            let mut grace = account.create_session(&db, "grace".to_owned()).await.unwrap();
            grace.last_seen = now + Duration::days(-400);
            grace.idle_grace_until = Some(now + Duration::days(1));
            db.save_session(&grace).await.unwrap();
            assert!(Session::touch(&db, &grace.id, now).await.unwrap());

            // the migration has not reached it
            let mut old = session("not-migrated", now + Duration::days(-400));
            old.user_id = account.id.clone();
            old.idle_grace_until = None;
            db.save_session(&old).await.unwrap();
            assert!(Session::touch(&db, &old.id, now).await.unwrap());
        });
    }

    #[tokio::test]
    async fn an_expired_session_gets_no_push() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let now = Timestamp::now_utc();
            let subscription = crate::WebPushSubscription {
                endpoint: "https://fcm.googleapis.com/fcm/send/a".to_owned(),
                p256dh: "key".to_owned(),
                auth: "auth".to_owned(),
            };

            let mut live = account.create_session(&db, "live".to_owned()).await.unwrap();
            live.subscription = Some(subscription.clone());
            db.save_session(&live).await.unwrap();

            let mut idle = account.create_session(&db, "idle".to_owned()).await.unwrap();
            idle.subscription = Some(subscription);
            idle.last_seen = now + Duration::days(-31);
            db.save_session(&idle).await.unwrap();

            let sent = Session::fetch_for_push(&db, &[account.id.clone()]).await.unwrap();
            assert_eq!(ids(&sent), [live.id.as_str()]);
        });
    }

    #[test]
    fn last_seen_is_written_at_most_once_an_hour() {
        let now = Timestamp::now_utc();
        let used = session("a", now);
        assert!(!used.needs_touch_at(now + Duration::minutes(59)));
        assert!(used.needs_touch_at(now + Duration::minutes(60)));
    }

    #[tokio::test]
    async fn a_sign_in_over_the_limit_closes_the_least_recently_used() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let now = Timestamp::now_utc();
            let mut oldest_by_use = None;
            for day in 1..=10 {
                let mut session = account.create_session(&db, format!("s{day}")).await.unwrap();
                // created first does not mean used last: the 5th is the oldest
                session.last_seen = now + Duration::days(if day == 5 { -20 } else { -day });
                if day == 5 {
                    oldest_by_use = Some(session.id.clone());
                }
                db.save_session(&session).await.unwrap();
            }

            let new = account.create_session(&db, "new".to_owned()).await.unwrap();

            let left = db.fetch_sessions(&account.id).await.unwrap();
            assert_eq!(left.len(), 10);
            assert!(left.iter().any(|session| session.id == new.id));
            assert!(!left.iter().any(|session| Some(&session.id) == oldest_by_use.as_ref()));
        });
    }

    #[tokio::test]
    async fn a_large_excess_goes_in_one_sign_in() {
        database_test!(|db| async move {
            let account = account(&db).await;
            // sessions saved before the limit existed
            for number in 0..25 {
                let mut extra = session(&format!("extra{number:02}"), Timestamp::now_utc());
                extra.user_id = account.id.clone();
                db.save_session(&extra).await.unwrap();
            }
            let new = account.create_session(&db, "new".to_owned()).await.unwrap();
            let left = db.fetch_sessions(&account.id).await.unwrap();
            assert_eq!(left.len(), 10);
            assert!(left.iter().any(|session| session.id == new.id));
        });
    }

    #[tokio::test]
    async fn sign_ins_at_once_leave_the_account_at_the_limit() {
        database_test!(|db| async move {
            let account = account(&db).await;
            for number in 0..9 {
                account.create_session(&db, format!("s{number}")).await.unwrap();
            }

            let created = futures::future::join_all(
                (0..5).map(|number| account.create_session(&db, format!("new{number}"))),
            )
            .await;

            let left = db.fetch_sessions(&account.id).await.unwrap();
            assert_eq!(left.len(), 10);
            for session in created {
                let session = session.unwrap();
                assert!(left.iter().any(|kept| kept.id == session.id), "{}", session.name);
            }
        });
    }

    #[tokio::test]
    async fn another_account_is_not_counted_or_touched() {
        database_test!(|db| async move {
            let first = account(&db).await;
            let second = account(&db).await;
            for number in 0..10 {
                first.create_session(&db, format!("s{number}")).await.unwrap();
            }
            second.create_session(&db, "other".to_owned()).await.unwrap();
            assert_eq!(db.fetch_sessions(&first.id).await.unwrap().len(), 10);
            assert_eq!(db.fetch_sessions(&second.id).await.unwrap().len(), 1);
        });
    }

    #[tokio::test]
    async fn an_idle_token_is_refused_at_once_and_stays_for_the_sweep() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let mut idle = account.create_session(&db, "idle".to_owned()).await.unwrap();
            idle.last_seen = Timestamp::now_utc() + Duration::days(-31);
            db.save_session(&idle).await.unwrap();

            let error = Session::from_token(&db, &idle.token).await.unwrap_err();
            assert!(matches!(error.error_type, ErrorType::InvalidSession));

            // refusing writes nothing, the sweep removes it. The client drops
            // its token on this answer, which is why a session the migration
            // has not reached is never refused (see below)
            let stored = db.fetch_session(&idle.id).await.unwrap();
            assert_eq!(ms(stored.last_seen), ms(idle.last_seen));
        });
    }

    #[tokio::test]
    async fn an_old_session_in_its_grace_works_and_is_touched() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let mut old = account.create_session(&db, "old".to_owned()).await.unwrap();
            old.last_seen = Timestamp::now_utc() + Duration::days(-400);
            old.idle_grace_until = Some(Timestamp::now_utc() + Duration::days(3));
            db.save_session(&old).await.unwrap();

            let used = Session::from_token(&db, &old.token).await.unwrap();
            assert!(used.last_seen > old.last_seen);
            let stored = db.fetch_session(&old.id).await.unwrap();
            assert_eq!(ms(stored.last_seen), ms(used.last_seen));
        });
    }

    #[tokio::test]
    async fn use_within_the_hour_writes_nothing() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let mut recent = account.create_session(&db, "recent".to_owned()).await.unwrap();
            recent.last_seen = Timestamp::now_utc() + Duration::minutes(-30);
            db.save_session(&recent).await.unwrap();

            let used = Session::from_token(&db, &recent.token).await.unwrap();
            assert_eq!(ms(used.last_seen), ms(recent.last_seen));
            let stored = db.fetch_session(&recent.id).await.unwrap();
            assert_eq!(ms(stored.last_seen), ms(recent.last_seen));
        });
    }

    #[tokio::test]
    async fn use_after_the_hour_is_recorded() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let mut used = account.create_session(&db, "used".to_owned()).await.unwrap();
            used.last_seen = Timestamp::now_utc() + Duration::minutes(-90);
            db.save_session(&used).await.unwrap();

            let touched = Session::from_token(&db, &used.token).await.unwrap();
            assert!(touched.last_seen > used.last_seen);
            let stored = db.fetch_session(&used.id).await.unwrap();
            assert_eq!(ms(stored.last_seen), ms(touched.last_seen));
        });
    }

    #[tokio::test]
    async fn the_sweep_closes_only_idle_sessions() {
        database_test!(|db| async move {
            let account = account(&db).await;
            let now = Timestamp::now_utc();

            let fresh = account.create_session(&db, "fresh".to_owned()).await.unwrap();

            let mut idle = account.create_session(&db, "idle".to_owned()).await.unwrap();
            idle.last_seen = now + Duration::days(-31);
            db.save_session(&idle).await.unwrap();

            let mut almost = account.create_session(&db, "almost".to_owned()).await.unwrap();
            almost.last_seen = now + Duration::days(-29);
            db.save_session(&almost).await.unwrap();

            let mut in_grace = account.create_session(&db, "in grace".to_owned()).await.unwrap();
            in_grace.last_seen = now + Duration::days(-400);
            in_grace.idle_grace_until = Some(now + Duration::days(1));
            db.save_session(&in_grace).await.unwrap();

            let mut grace_over = account.create_session(&db, "grace over".to_owned()).await.unwrap();
            grace_over.last_seen = now + Duration::days(-400);
            grace_over.idle_grace_until = Some(now + Duration::days(-1));
            db.save_session(&grace_over).await.unwrap();

            assert_eq!(Session::delete_idle(&db).await.unwrap(), 2);

            let mut left: Vec<String> = db
                .fetch_sessions(&account.id)
                .await
                .unwrap()
                .into_iter()
                .map(|session| session.name)
                .collect();
            left.sort();
            assert_eq!(left, ["almost", "fresh", "in grace"]);
            assert!(db.fetch_session(&fresh.id).await.is_ok());

            // nothing left to do
            assert_eq!(Session::delete_idle(&db).await.unwrap(), 0);
        });
    }
}
