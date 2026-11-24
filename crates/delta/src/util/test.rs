use std::time::Duration;

use futures::{FutureExt, StreamExt};
use rand::Rng;
use redis_kiss::redis::aio::PubSub;
use revolt_database::util::email::normalise_email;
use revolt_database::util::password::hash_password;
use revolt_database::{
    AMQP, Bot, Channel, Database, Member, Message, PartialRole, Server, User, events::client::EventV1,
};
use revolt_database::{Account, EmailVerification, Session};
use revolt_database::{Role, util::idempotency::IdempotencyKey};
use revolt_models::v0;
use revolt_permissions::OverrideField;
use rocket::http::Header;
use rocket::local::asynchronous::{Client, LocalRequest, LocalResponse};
use rocket::tokio;
use serde::{Deserialize, Serialize};

pub struct TestHarness {
    pub client: Client,
    pub db: Database,
    pub amqp: AMQP,
}

/// Every MongoDB test has a database of its own; left behind they pile up and
/// slow down each later run (1196 of them were found on 2026-11-04). Also runs
/// when a test panics. Uses a client of its own: the test's runtime is blocked here.
impl Drop for TestHarness {
    fn drop(&mut self) {
        let Database::MongoDb(mongo) = &self.db else {
            return;
        };
        let name = mongo.1.clone();
        let cleaner = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async {
                if let Ok(Database::MongoDb(mongo)) =
                    revolt_database::DatabaseInfo::Test(name).connect().await
                {
                    mongo.db().drop().await.ok();
                }
            });
        });
        let _ = cleaner.join();
    }
}

impl TestHarness {
    pub async fn new() -> TestHarness {
        let client = Client::tracked(crate::web().await)
            .await
            .expect("valid rocket instance");

        let db = client
            .rocket()
            .state::<Database>()
            .expect("`Database`")
            .clone();

        let amqp = AMQP::new_auto().await;
        TestHarness { client, db, amqp }
    }

    /// Random, and never a name the rule for official names refuses: about
    /// one string in 400000 spells Zrzur, enough to fail a test now and then
    pub fn rand_string() -> String {
        let mut rng = rand::thread_rng();
        loop {
            let name: String = (&mut rng)
                .sample_iter(rand::distributions::Alphanumeric)
                .take(20)
                .map(char::from)
                .collect();
            if !revolt_database::passes_for_official(&name) {
                return name;
            }
        }
    }

    pub async fn new_user(&self) -> (Account, Session, User) {
        let user = User::create(&self.db, TestHarness::rand_string(), None, None)
            .await
            .expect("`User`");

        let (account, session) = self.account_from_user(user.id.clone()).await;

        (account, session, user)
    }

    /// Make a user `privileged` the way it works in production: the flag,
    /// an Owner staff grant, and an open staff session for each of their
    /// current sign-ins (the flag alone counts for nothing)
    pub async fn make_privileged(&self, user_id: &str) {
        use revolt_database::{staff_now, staff_token_hash, StaffGrant, StaffRole, StaffSession};
        self.db
            .update_user(
                user_id,
                &revolt_database::PartialUser {
                    privileged: Some(true),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("privileged");
        let now = staff_now();
        let current = self.db.fetch_staff_grant(user_id).await.expect("grant");
        let mut grant = current.clone().unwrap_or(StaffGrant {
            id: user_id.to_string(),
            roles: vec![],
            disabled: false,
            expires_at: None,
            last_totp_step: 0,
            version: 0,
            updated_at: now,
            updated_by: "test".to_string(),
            active_since: None,
            activated_by: None,
        });
        if !grant.roles.contains(&StaffRole::Owner) {
            grant.roles.push(StaffRole::Owner);
        }
        grant.version += 1;
        assert!(self
            .db
            .save_staff_grant(&grant, current.map(|g| g.version))
            .await
            .expect("grant"));
        for session in self.db.fetch_sessions(user_id).await.expect("sessions") {
            self.db
                .insert_staff_session(&StaffSession {
                    id: ulid::Ulid::new().to_string(),
                    token_hash: staff_token_hash(&TestHarness::rand_string()),
                    user_id: user_id.to_string(),
                    session_id: session.id.clone(),
                    created_at: now,
                    last_seen_at: now,
                    expires_at: now + revolt_database::STAFF_SESSION_MAX_MS,
                    // a fresh step-up too: edits of other people need one
                    step_up_until: Some(now + revolt_database::STAFF_SESSION_MAX_MS),
                    revoked_at: None,
                    user_agent: None,
                    kind: Default::default(),
                    bot_id: None,
                })
                .await
                .expect("staff session");
        }
    }

    /// A person who has not declared an age yet
    pub async fn new_user_without_age(&self) -> (Account, Session, User) {
        let (mut account, session, user) = self.new_user().await;
        account.age = None;
        // a save only sets the fields it has, so on MongoDB the old age would stay
        self.db.remove_account(&account.id).await.expect("`Account`");
        self.db.save_account(&account).await.expect("`Account`");
        (account, session, user)
    }

    pub async fn account_from_user(&self, id: String) -> (Account, Session) {
        let email = format!("{}@zrzur.cc", TestHarness::rand_string());
        let account = Account {
            id,
            email: email.clone(),
            password: hash_password("password_insecure".to_string()).unwrap(),
            email_normalised: normalise_email(email),
            deletion: None,
            disabled: false,
            lockout: None,
            mfa: Default::default(),
            password_reset: None,
            verification: EmailVerification::Verified,
            // an adult with a declared age, like every account after its
            // first entry (new_user_without_age for the other case)
            age: Some(revolt_database::AccountAge::declare((
                iso8601_timestamp::Timestamp::now_utc().year() - 30,
                1,
            ))),
            terms: Some(revolt_database::TermsAcceptance::current()),
            provisional: None,
        };

        self.db.save_account(&account).await.expect("`Account`");

        let session = account
            .create_session(&self.db, String::new())
            .await
            .expect("`Session`");

        (account, session)
    }

    pub async fn new_server(&self, user: &User) -> (Server, Vec<Channel>) {
        Server::create(
            &self.db,
            v0::DataCreateServer {
                name: "Test Server".to_string(),
                ..Default::default()
            },
            user,
            true,
        )
        .await
        .expect("Failed to create test server")
    }

    pub async fn new_role(
        &self,
        server: &Server,
        rank: i64,
        overrides: Option<OverrideField>,
    ) -> Role {
        let mut role = Role::create(&self.db, &server, TestHarness::rand_string())
            .await
            .expect("Failed to create test role");

        if let Some(overrides) = overrides {
            role.update(
                &self.db,
                &server.id,
                PartialRole {
                    permissions: Some(overrides),
                    ..Default::default()
                },
                Vec::new(),
            )
            .await
            .expect("Failed to set test role overrides");
        };

        role
    }

    pub async fn new_channel(&self, server: &Server) -> Channel {
        Channel::create_server_channel(
            &self.db,
            &mut server.clone(),
            v0::DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Text,
                name: "Test Channel".to_string(),
                description: None,
                nsfw: Some(false),
                voice: None,
                announcement: None,
                category: None,
                private: None,
            },
            true,
        )
        .await
        .expect("Failed to make test channel")
    }

    pub async fn new_message(
        &self,
        user: &User,
        server: &Server,
        channels: Vec<Channel>,
    ) -> (Channel, Member, Message) {
        let (member, channels) = Member::create(&self.db, server, user, Some(channels))
            .await
            .expect("Failed to create member");
        let channel = &channels[0];
        let message = Message::create_from_api(
            &self.db,
            None,
            channel.clone(),
            v0::DataMessageSend {
                content: Some("Test message".to_string()),
                nonce: None,
                attachments: None,
                replies: None,
                embeds: None,
                masquerade: None,
                interactions: None,
                components: None,
                interaction: None,
                poll: None,
                flags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&self.db, Some(user)).await),
            Some(user.clone().into(&self.db, Some(user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string("0".to_string()),
            false,
            false,
        )
        .await
        .expect("Failed to create message");
        (channel.clone(), member, message)
    }

    pub async fn with_session(session: Session, request: LocalRequest<'_>) -> LocalResponse<'_> {
        request
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await
    }

    pub async fn assert_email(&self, mailbox: &str) -> (Mail, String) {
        // Wait a moment for maildev to catch the email

        tokio::time::sleep(Duration::from_secs(1)).await;

        let client = reqwest::Client::new();
        let results = client
            .get("http://localhost:14080/email")
            .send()
            .await
            .unwrap()
            .json::<Vec<Mail>>()
            .await
            .unwrap();

        let re = regex::Regex::new(r"\[\[([A-Za-z0-9_-]*)\]\]").unwrap();

        for entry in results.into_iter().rev() {
            if entry.envelope.to[0].address == mailbox {
                client
                    .delete(format!("http://localhost:14080/delete/{}", &entry.id))
                    .send()
                    .await
                    .unwrap();

                let code = re.captures_iter(&entry.text).next().unwrap()[1].to_string();

                return (entry, code);
            }
        }

        panic!("Email not found.")
    }

    pub async fn new_bot(&self, user: &User,) -> (Bot, User) {
        let (bot, bot_user) = Bot::create(&self.db, TestHarness::rand_string(), user, None)
            .await
            .expect("`Bot`");

        (bot, bot_user)
    }

    /// A person who counts everywhere: an account `days_old` days old with
    /// a confirmed e-mail and a declared adult age (`n` keeps the creation
    /// times of several apart)
    pub async fn genuine_person(&self, days_old: i64, n: i64) -> User {
        let now = revolt_database::staff_now();
        let made = now - days_old * 86_400_000 - n * 3 * 3_600_000;
        let id = ulid::Ulid::from_parts(made as u64, rand::random::<u128>() & 0xffff_ffff).to_string();
        let user = User::create(&self.db, TestHarness::rand_string(), Some(id.clone()), None)
            .await
            .expect("`User`");
        self.account_from_user(id).await;
        user
    }

    /// A server of a genuine person with five genuine members (joined days
    /// apart, so no burst), with `bot` in it: one legitimate server for the
    /// Verified Bot Developer numbers (`Revolt.test.toml` asks for one)
    pub async fn legitimate_server_for(&self, bot: &str) -> Server {
        let now = revolt_database::staff_now();
        let owner = self.genuine_person(60, 0).await;
        let (server, _) = self.new_server(&owner).await;
        for n in 0..6 {
            let member = if n < 5 {
                self.genuine_person(30 + n * 2, n).await.id
            } else {
                bot.to_string()
            };
            self.db
                .insert_or_merge_member(&Member {
                    id: revolt_database::MemberCompositeKey {
                        server: server.id.clone(),
                        user: member,
                    },
                    joined_at: revolt_database::from_epoch_ms(now - (48 + n * 24) * 3_600_000),
                    ..Default::default()
                })
                .await
                .expect("`Member`");
        }
        server
    }

    /// An approved Partner application of `user_id`: the record the badge
    /// gate wants before anyone may give Partner (tests of the two-person
    /// rule use Partner as their sensitive badge)
    pub async fn back_partner(&self, user_id: &str) {
        let now = revolt_database::staff_now();
        self.db
            .insert_badge_application(&revolt_database::BadgeApplication {
                id: ulid::Ulid::new().to_string(),
                program: revolt_database::BadgeProgram::Partner,
                applicant_id: user_id.to_string(),
                target_id: ulid::Ulid::new().to_string(),
                status: revolt_database::ApplicationStatus::Approved,
                exception: false,
                statement: String::new(),
                links: vec![],
                authorized: true,
                ownership: None,
                creator: None,
                creator_figures: None,
                eligibility: vec![],
                created_at: now,
                updated_at: now,
                submitted_at: Some(now),
                reviewer_id: None,
                first_approval_by: None,
                badge_request_id: None,
                decided_at: Some(now),
                decided_by: None,
                reasons: vec![],
                note: None,
                reapply_after: None,
                appeal_until: None,
                appeal: None,
                next_review_at: None,
                recheck_due: false,
                improve_until: None,
                ended_at: None,
                end_reason: None,
                evidence_purge_at: None,
                evidence_purged: false,
                purge_at: None,
            })
            .await
            .expect("`BadgeApplication`");
    }
}

pub struct PubSubTestHelper {
    sub: PubSub,
    event_buffer: Vec<(String, EventV1)>,
}

impl PubSubTestHelper {
    pub async fn new(topic: &str) -> Self {
        let mut sub = redis_kiss::open_pubsub_connection()
            .await
            .expect("`PubSub`");

        sub.subscribe(topic).await.unwrap();

        PubSubTestHelper {
            sub,
            event_buffer: vec![],
        }
    }

    pub async fn wait_for_event<F>(&mut self, predicate: F) -> EventV1
    where
        F: Fn(&EventV1) -> bool,
    {
        for (_, event) in &self.event_buffer {
            if predicate(event) {
                // does not remove from buffer
                return event.clone();
            }
        }

        let mut stream = self.sub.on_message();
        while let Some(item) = stream.next().await {
            let msg_topic = item.get_channel_name();
            let payload: EventV1 = redis_kiss::decode_payload(&item).unwrap();

            if predicate(&payload) {
                let matched = payload;

                // Saw this used while chasing ghosts in the Redis stream, s/o Alice Ryhl
                // Seems to be necessary when events on the same channel arrive too quickly
                while let Some(Some(item)) = stream.next().now_or_never() {
                    let msg_topic = item.get_channel_name().to_string();
                    let payload: EventV1 = redis_kiss::decode_payload(&item).unwrap();

                    self.event_buffer.push((msg_topic, payload));
                }

                return matched;
            }

            self.event_buffer.push((msg_topic.to_string(), payload));
        }

        // WARNING: if predicate is never satisfied, this will never return
        //          should add a timeout for events so tests can fail gracefully

        unreachable!()
    }

    pub async fn wait_for_message(&mut self) -> v0::Message {
        dbg!(&self.event_buffer);

        match self
            .wait_for_event(|event| match event {
                EventV1::Message(v0::Message { channel, .. }) => !channel.trim().is_empty(),
                _ => false,
            })
            .await
        {
            EventV1::Message(message) => message,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Mail {
    pub id: String,
    pub envelope: MailEnvelope,
    pub subject: String,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MailEnvelope {
    pub from: MailAddress,
    pub to: Vec<MailAddress>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MailAddress {
    pub address: String,
}

/// With `production` on, the server itself stops on example values, in `web()`
/// and before it connects to anything, so this holds on every test database
#[rocket::async_test]
async fn production_stops_the_server_on_example_values() {
    revolt_config::overwrite_config(|config| config.production = true).await;
    let started = std::panic::AssertUnwindSafe(crate::web()).catch_unwind().await;
    let Err(panic) = started else {
        panic!("the server started on example values");
    };
    let message = panic.downcast_ref::<String>().expect("the refusal text");
    assert!(message.contains("refusing to start"), "{message}");
    for setting in ["rabbit.password", "database.mongodb", "database.redis"] {
        assert!(message.contains(setting), "{setting} missing: {message}");
    }
    assert!(!message.contains("rabbitpass"), "never print secrets");
}

/// With `production` on and values of its own, the same server starts. The
/// test broker knows one password, so it gets that one spelled another way,
/// which is not the shipped text
#[rocket::async_test]
async fn production_starts_on_own_values() {
    // a database URL needs a password here, so the test database gets a user
    if std::env::var("TEST_DB").as_deref() == Ok("MONGODB") {
        let uri = revolt_config::config().await.database.mongodb;
        let client = revolt_database::mongodb::Client::with_uri_str(uri).await.expect("MongoDB");
        let created = client
            .database("admin")
            .run_command(revolt_database::mongodb::bson::doc! {
                "createUser": "zrzur_test",
                "pwd": "own",
                "roles": [{ "role": "root", "db": "admin" }],
            })
            .await;
        // left by an earlier run of this test
        assert!(
            created.as_ref().map_or_else(|e| e.to_string().contains("already exists"), |_| true),
            "{created:?}"
        );
    }
    revolt_config::overwrite_config(|config| {
        config.production = true;
        config.pushd.vapid.private_key = "not-an-example".into();
        config.files.encryption_key = "not-an-example".into();
        config.files.s3.access_key_id = "not-an-example".into();
        config.files.s3.secret_access_key = "not-an-example".into();
        let last = config.rabbit.password.pop().expect("a password");
        config.rabbit.password = format!("{}%{:02X}", config.rabbit.password, last as u32);
        config.database.mongodb = OWN_MONGODB.into();
        config.database.redis = OWN_REDIS.into();
        for node in config.api.livekit.nodes.values_mut() {
            node.secret = "not-an-example".into();
        }
        config.api.security.captcha.turnstile_secret = "not-an-example".into();
        config.api.security.captcha.turnstile_sitekey = "not-an-example".into();
        config.january.embed_key = "not-an-example".into();
    })
    .await;
    let harness = TestHarness::new().await;
    let response = harness.client.get("/").dispatch().await;
    assert_eq!(response.status(), rocket::http::Status::Ok);
}

const OWN_MONGODB: &str = "mongodb://zrzur_test:own@127.0.0.1:27018/?directConnection=true";
const OWN_REDIS: &str = "redis://:own@localhost/";
