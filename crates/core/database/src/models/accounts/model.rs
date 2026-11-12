use iso8601_timestamp::{Duration, Timestamp};

use nanoid::nanoid;
use revolt_config::config;
use revolt_result::Result;
use serde_json::json;

use crate::{
    events::client::EventV1,
    util::{
        email::{email_templates, normalise_email, send_email},
        password::hash_password,
    },
    Database, MFATicket, Session,
};
use crate::util::mfa_limit::claim_totp_code;
use revolt_models::v0;

/// How many days an account stays recoverable between confirming its deletion
/// and the day everything is erased.
///
/// The one place this number lives: the schedule below, the deletion e-mail,
/// the API response the app shows and the privacy policy all read it from here
/// (the owner decided 14 days, Q6, 2026-11-03).
pub const ACCOUNT_DELETION_GRACE_DAYS: i64 = 14;

/// Where people write to cancel a deletion or ask for their data by hand.
/// One value: the deletion
/// e-mail, the export README and the API response the app shows read it.
pub const ACCOUNT_CONTACT_EMAIL: &str = "privacy@zrzur.com";

/// An account younger than this, with nothing in it, may be removed at once
/// when its first age statement is under the minimum
pub const NEW_ACCOUNT_HOURS: i64 = 24;

/// Version of the Terms of Service a new account accepts: the `version` of
/// the terms page on zrzur.com (`website/src/policies`, the date of the text).
/// Change it together with the terms (T-6).
pub const TERMS_VERSION: &str = "2026-11-12";

auto_derived_partial!(
    /// Account model
    pub struct Account {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,

        /// User's email
        pub email: String,

        /// Normalised email
        ///
        /// (see https://github.com/insertish/authifier/#how-does-authifier-work)
        pub email_normalised: String,

        /// Argon2 hashed password
        pub password: String,

        /// Whether the account is disabled
        #[serde(default)]
        pub disabled: bool,

        /// Email verification status
        pub verification: EmailVerification,

        /// Password reset information
        pub password_reset: Option<PasswordReset>,

        /// Account deletion information
        pub deletion: Option<DeletionInfo>,

        /// Account lockout
        pub lockout: Option<Lockout>,

        /// Multi-factor authentication information
        pub mfa: MultiFactorAuthentication,

        /// Declared age (year and month of birth only)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub age: Option<AccountAge>,

        /// Which Terms of Service the person accepted, and when (T-6).
        /// Accounts from before this was recorded have none.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub terms: Option<TermsAcceptance>,
    },
    "PartialAccount"
);

auto_derived!(
    /// The Terms of Service version a person accepted
    pub struct TermsAcceptance {
        /// Version of the terms ([TERMS_VERSION] at the time)
        pub version: String,
        /// When they accepted it
        pub accepted_at: Timestamp,
    }
);

impl TermsAcceptance {
    /// The current terms, accepted now
    pub fn current() -> TermsAcceptance {
        TermsAcceptance {
            version: TERMS_VERSION.to_string(),
            accepted_at: Timestamp::now_utc(),
        }
    }
}

auto_derived!(
    /// Declared age
    ///
    /// Only the year and month of birth are kept, never the day.
    pub struct AccountAge {
        /// Year and month of birth, `YYYY-MM`
        pub birth_month: String,
        /// When the current value was declared
        pub declared_at: Timestamp,
        /// How the age was confirmed
        pub source: v0::AgeSource,
        /// Whether the one allowed upward correction was used
        #[serde(default)]
        pub raised: bool,
        /// Age group changes, without the dates of birth involved
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub changes: Vec<AgeChange>,
    }

    /// A change of age group
    pub struct AgeChange {
        /// When the change was made
        pub at: Timestamp,
        /// Age group before the change
        pub from: v0::AgeBand,
        /// Age group after the change
        pub to: v0::AgeBand,
    }
);

auto_derived!(
    /// Email verification status
    #[serde(tag = "status")]
    pub enum EmailVerification {
        /// Account is verified
        Verified,
        /// Pending email verification
        Pending { token: String, expiry: Timestamp },
        /// Moving to a new email
        Moving {
            new_email: String,
            token: String,
            expiry: Timestamp,
        },
    }

    /// Password reset information
    pub struct PasswordReset {
        /// Token required to change password
        pub token: String,
        /// Time at which this token expires
        pub expiry: Timestamp,
    }

    /// Account deletion information
    #[serde(tag = "status")]
    pub enum DeletionInfo {
        /// The user must confirm deletion by email
        WaitingForVerification { token: String, expiry: Timestamp },
        /// The account is scheduled for deletion
        Scheduled { after: Timestamp },
        /// This account was deleted
        Deleted,
    }

    /// Lockout information
    pub struct Lockout {
        /// Attempt counter
        pub attempts: i32,
        /// Time at which this lockout expires
        pub expiry: Option<Timestamp>,
        /// Time of the last failed attempt; attempts older than an hour are forgotten
        #[serde(default)]
        pub last_attempt: Option<Timestamp>,
    }

    /// MFA configuration
    #[derive(Default)]
    pub struct MultiFactorAuthentication {
        /// Allow password-less email OTP login
        /// (1-Factor)
        // #[serde(skip_serializing_if = "is_false", default)]
        // pub enable_email_otp: bool,

        /// Allow trusted handover
        /// (1-Factor)
        // #[serde(skip_serializing_if = "is_false", default)]
        // pub enable_trusted_handover: bool,

        /// Allow email MFA
        /// (2-Factor)
        // #[serde(skip_serializing_if = "is_false", default)]
        // pub enable_email_mfa: bool,

        /// TOTP MFA token, enabled if present
        /// (2-Factor)
        #[serde(skip_serializing_if = "Totp::is_empty", default)]
        pub totp_token: Totp,

        /// Security Key MFA token, enabled if present
        /// (2-Factor)
        // #[serde(skip_serializing_if = "Option::is_none")]
        // pub security_key_token: Option<String>,

        /// Recovery codes
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub recovery_codes: Vec<String>,
    }

    /// MFA method
    #[derive(Hash)]
    pub enum MFAMethod {
        Password,
        Recovery,
        Totp,
    }

    #[derive(Default)]
    #[serde(tag = "status")]
    pub enum Totp {
        /// Disabled
        #[default]
        Disabled,
        /// Waiting for user activation
        Pending { secret: String },
        /// Required on account
        Enabled { secret: String },
    }
);

impl MultiFactorAuthentication {
    // Check whether MFA is in-use
    pub fn is_active(&self) -> bool {
        matches!(self.totp_token, Totp::Enabled { .. })
    }

    // Check whether there are still usable recovery codes
    pub fn has_recovery(&self) -> bool {
        !self.recovery_codes.is_empty()
    }

    // Get available MFA methods
    pub fn get_methods(&self) -> Vec<MFAMethod> {
        if let Totp::Enabled { .. } = self.totp_token {
            let mut methods = vec![MFAMethod::Totp];

            if self.has_recovery() {
                methods.push(MFAMethod::Recovery);
            }

            methods
        } else {
            vec![MFAMethod::Password]
        }
    }

    // Generate new recovery codes
    pub fn generate_recovery_codes(&mut self) {
        static ALPHABET: [char; 32] = [
            '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', 'a', 'b', 'c', 'd', 'e', 'f', 'g',
            'h', 'j', 'k', 'm', 'n', 'p', 'q', 'r', 's', 't', 'v', 'w', 'x', 'y', 'z',
        ];

        let mut codes = vec![];
        for _ in 1..=10 {
            codes.push(format!(
                "{}-{}",
                nanoid!(5, &ALPHABET),
                nanoid!(5, &ALPHABET)
            ));
        }

        self.recovery_codes = codes;
    }

    // Generate new TOTP secret
    pub fn generate_new_totp_secret(&mut self) -> Result<String> {
        if let Totp::Enabled { .. } = self.totp_token {
            return Err(create_error!(OperationFailed));
        }

        let secret: [u8; 10] = rand::random();
        let secret = base32::encode(base32::Alphabet::RFC4648 { padding: false }, &secret);

        self.totp_token = Totp::Pending {
            secret: secret.clone(),
        };

        Ok(secret)
    }

    /// Enable TOTP using a given MFA response
    pub fn enable_totp(&mut self, response: v0::MFAResponse) -> Result<()> {
        if let v0::MFAResponse::Totp { totp_code } = response {
            if self.totp_token.accepts_code(&totp_code) {
                let mut totp = Totp::Disabled;
                std::mem::swap(&mut totp, &mut self.totp_token);

                if let Totp::Pending { secret } = totp {
                    self.totp_token = Totp::Enabled { secret };

                    Ok(())
                } else {
                    Err(create_error!(OperationFailed))
                }
            } else {
                Err(create_error!(InvalidToken))
            }
        } else {
            Err(create_error!(InvalidToken))
        }
    }
}

impl Totp {
    /// Whether TOTP information is empty
    pub fn is_empty(&self) -> bool {
        matches!(self, Totp::Disabled)
    }

    /// Whether TOTP is disabled
    pub fn is_disabled(&self) -> bool {
        !matches!(self, Totp::Enabled { .. })
    }

    /// Whether a code matches the current 30-second step or the one before
    /// or after it (RFC 6238: allow for clock drift and typing at the
    /// boundary). Compared without stopping at the first difference.
    pub fn accepts_code(&self, code: &str) -> bool {
        let (Totp::Enabled { secret } | Totp::Pending { secret }) = self else {
            return false;
        };
        let Some(key) = base32::decode(base32::Alphabet::RFC4648 { padding: false }, secret)
        else {
            return false;
        };
        let seconds: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let step = totp_lite::DEFAULT_STEP;
        let mut matched = false;
        for at in [seconds.saturating_sub(step), seconds, seconds + step] {
            let expected = totp_lite::totp_custom::<totp_lite::Sha1>(step, 6, &key, at);
            if expected.len() == code.len() {
                let difference = expected
                    .bytes()
                    .zip(code.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b));
                matched |= difference == 0;
            }
        }
        matched
    }

    // Generate a TOTP code from secret
    pub fn generate_code(&self) -> Result<String> {
        if let Totp::Enabled { secret } | Totp::Pending { secret } = &self {
            let seconds: u64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            Ok(totp_lite::totp_custom::<totp_lite::Sha1>(
                totp_lite::DEFAULT_STEP,
                6,
                &base32::decode(base32::Alphabet::RFC4648 { padding: false }, secret)
                    .expect("valid base32 secret"),
                seconds,
            ))
        } else {
            Err(create_error!(OperationFailed))
        }
    }
}

impl Lockout {
    /// Failed attempts older than this are forgotten
    pub const FORGET_AFTER_HOURS: i64 = 1;

    /// Whether the lock is running at `now`
    pub fn is_locked_at(&self, now: Timestamp) -> bool {
        self.expiry.is_some_and(|expiry| expiry > now)
    }

    /// The lockout after one more failed attempt at `now`. A lock that is
    /// still running is never extended (anyone could keep a known account
    /// locked for ever), and attempts older than an hour are forgotten.
    pub fn after_failure(previous: Option<&Lockout>, now: Timestamp) -> Lockout {
        let forget_before = now + Duration::hours(-Self::FORGET_AFTER_HOURS);
        let (mut attempts, mut expiry) = match previous {
            Some(previous) if previous.is_locked_at(now) => return previous.clone(),
            Some(previous) => {
                let last = previous.last_attempt.or(previous.expiry);
                if last.is_some_and(|last| last < forget_before) {
                    (0, None)
                } else {
                    (previous.attempts, previous.expiry)
                }
            }
            None => (0, None),
        };
        attempts += 1;
        expiry = match attempts {
            ..=2 => expiry.filter(|expiry| *expiry > now),
            3 => Some(now + Duration::minutes(1)),
            4 => Some(now + Duration::minutes(5)),
            _ => Some(now + Duration::hours(1)),
        };
        Lockout {
            attempts,
            expiry,
            last_attempt: Some(now),
        }
    }
}

impl Account {
    /// Save model
    pub async fn save(&self, db: &Database) -> Result<()> {
        db.save_account(self).await
    }

    /// Create a new account
    pub async fn new(
        db: &Database,
        email: String,
        plaintext_password: String,
        verify_email: bool,
    ) -> Result<Account> {
        Account::new_with_age(db, email, plaintext_password, verify_email, None).await
    }

    /// Create a new account with a declared age
    ///
    /// The age is only stored on a newly created account, never on an
    /// existing account that shares the email address.
    pub async fn new_with_age(
        db: &Database,
        email: String,
        plaintext_password: String,
        verify_email: bool,
        age: Option<AccountAge>,
    ) -> Result<Account> {
        Account::new_with_age_and_terms(db, email, plaintext_password, verify_email, age, None).await
    }

    /// Create a new account with a declared age and the terms its owner
    /// accepted at sign-up
    ///
    /// Like the age, the terms are only stored on a newly created account,
    /// never on an existing account that shares the email address.
    pub async fn new_with_age_and_terms(
        db: &Database,
        email: String,
        plaintext_password: String,
        verify_email: bool,
        age: Option<AccountAge>,
        terms: Option<TermsAcceptance>,
    ) -> Result<Account> {
        // Get a normalised representation of the user's email
        let email_normalised = normalise_email(email.clone());

        // Try to find an existing account
        if let Some(mut account) = db
            .fetch_account_by_normalised_email(&email_normalised)
            .await?
        {
            // Resend account verification or send password reset
            if let EmailVerification::Pending { .. } = &account.verification {
                account.start_email_verification(db).await?;
            } else {
                account.start_password_reset(db, true).await?;
            }

            Ok(account)
        } else {
            // Hash the user's password
            let password = hash_password(plaintext_password)?;

            // Create a new account
            let mut account = Account {
                id: ulid::Ulid::new().to_string(),

                email,
                email_normalised,
                password,

                disabled: false,
                verification: EmailVerification::Verified,
                password_reset: None,
                deletion: None,
                lockout: None,

                mfa: Default::default(),
                age,
                terms,
            };

            // Send email verification
            if verify_email {
                account.start_email_verification(db).await?;
            } else {
                account.save(db).await?;
            }

            // Create and push event
            EventV1::CreateAccount {
                account: account.clone(),
            }
            .global()
            .await;

            Ok(account)
        }
    }

    /// Create a new session
    pub async fn create_session(&self, db: &Database, name: String) -> Result<Session> {
        let config = config().await;

        let now = Timestamp::now_utc();
        let token = nanoid!(64);
        let session = Session {
            id: ulid::Ulid::new().to_string(),
            token_hash: crate::models::hash_secret(&token),
            token,

            user_id: self.id.clone(),
            name,

            last_seen: now,
            idle_grace_until: Some(now),

            origin: Some(config.environment),
            subscription: None,
        };

        // Save to database
        db.insert_session(&session).await?;

        // Create and push event
        EventV1::CreateSession {
            session: session.clone(),
        }
        .global()
        .await;

        // A failure here must not cost the person their sign-in: the next
        // sign-in closes what this one could not
        if let Err(error) = self
            .close_sessions_over_limit(db, &session.id, config.api.sessions.limit)
            .await
        {
            log::warn!("Could not apply the session limit to {}: {error:?}", self.id);
        }

        Ok(session)
    }

    /// Bring this account back to `limit` sessions, closing the least
    /// recently used and never `keep`.
    ///
    /// This runs after the new session is saved and takes no lock. Of sign-ins
    /// at the same moment, the one that reads last has seen them all, so when
    /// the last of them returns the account is at the limit; until then it is
    /// over by at most the number of sign-ins in flight. Those that read
    /// earlier close the same oldest sessions, which is harmless. Bots and
    /// OAuth have their own tokens and never come here.
    async fn close_sessions_over_limit(
        &self,
        db: &Database,
        keep: &str,
        limit: usize,
    ) -> Result<()> {
        let sessions = db.fetch_sessions(&self.id).await?;

        for session in Session::over_limit(sessions, keep, limit) {
            // another sign-in may have closed it a moment ago
            if let Err(error) = session.delete(db).await {
                log::warn!("Could not close a session over the limit: {error:?}");
            }
        }

        Ok(())
    }

    /// Send account verification email
    pub async fn start_email_verification(&mut self, db: &Database) -> Result<()> {
        let config = config().await;

        if !config.api.smtp.host.is_empty() {
            let templates = email_templates().await;

            let token = nanoid!(32);
            let url = format!("{}{}", templates.verify.url, token);

            send_email(
                &config.api.smtp,
                self.email.clone(),
                &templates.verify,
                json!({
                    "email": self.email.clone(),
                    "url": url
                }),
            )?;

            self.verification = EmailVerification::Pending {
                token,
                expiry: Timestamp::now_utc()
                    .checked_add(Duration::seconds(
                        config.api.smtp.expiry.expire_verification,
                    ))
                    .unwrap(),
            };
        } else {
            self.verification = EmailVerification::Verified;
        }

        self.save(db).await
    }

    /// Send account verification to new email
    pub async fn start_email_move(&mut self, db: &Database, new_email: String) -> Result<()> {
        // This method should and will never be called on an unverified account,
        // but just validate this just in case.
        if let EmailVerification::Pending { .. } = self.verification {
            return Err(create_error!(UnverifiedAccount));
        }

        let config = config().await;

        if !config.api.smtp.host.is_empty() {
            let templates = email_templates().await;

            let token = nanoid!(32);
            let url = format!("{}{}", templates.verify.url, token);

            send_email(
                &config.api.smtp,
                new_email.clone(),
                &templates.verify,
                json!({
                    "email": self.email.clone(),
                    "url": url
                }),
            )?;

            self.verification = EmailVerification::Moving {
                new_email,
                token,
                expiry: Timestamp::now_utc()
                    .checked_add(Duration::seconds(
                        config.api.smtp.expiry.expire_verification,
                    ))
                    .unwrap(),
            };
        } else {
            self.email_normalised = normalise_email(new_email.clone());
            self.email = new_email;
        }

        self.save(db).await
    }

    /// Send password reset email
    pub async fn start_password_reset(
        &mut self,
        db: &Database,
        existing_account: bool,
    ) -> Result<()> {
        let config = config().await;

        if !config.api.smtp.host.is_empty() {
            let templates = email_templates().await;

            let template = if existing_account {
                &templates.reset_existing
            } else {
                &templates.reset
            };

            let token = nanoid!(32);
            let url = format!("{}{}", template.url, token);

            send_email(
                &config.api.smtp,
                self.email.clone(),
                template,
                json!({
                    "email": self.email.clone(),
                    "url": url
                }),
            )?;

            self.password_reset = Some(PasswordReset {
                token,
                expiry: Timestamp::now_utc()
                    .checked_add(Duration::seconds(
                        config.api.smtp.expiry.expire_password_reset,
                    ))
                    .unwrap(),
            });
        } else {
            return Err(create_error!(OperationFailed));
        }

        self.save(db).await
    }

    /// Begin account deletion process by sending confirmation email
    ///
    /// If email verification is not on, the account will be marked for deletion instantly
    pub async fn start_account_deletion(&mut self, db: &Database) -> Result<()> {
        let config = config().await;

        if !config.api.smtp.host.is_empty() {
            let templates = email_templates().await;

            let token = nanoid!(32);
            let url = format!("{}{}", templates.deletion.url, token);

            send_email(
                &config.api.smtp,
                self.email.clone(),
                &templates.deletion,
                json!({
                    "email": self.email.clone(),
                    "url": url,
                    "grace_days": ACCOUNT_DELETION_GRACE_DAYS,
                    "contact": ACCOUNT_CONTACT_EMAIL
                }),
            )?;

            self.deletion =Some(DeletionInfo::WaitingForVerification {
                token,
                expiry: Timestamp::now_utc()
                    .checked_add(Duration::seconds(
                        config.api.smtp.expiry.expire_password_reset,
                    ))
                    .unwrap(),
            });

            self.save(db).await
        } else {
            self.schedule_deletion(db).await
        }
    }

    /// Does the work of checking a password against a hash nobody knows
    /// the password of, so an unknown e-mail takes as long as a wrong password
    pub fn spend_password_check_time(plaintext_password: &str) {
        static DUMMY_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            let salt: [u8; 16] = rand::random();
            let secret: [u8; 32] = rand::random();
            argon2::hash_encoded(&secret, &salt, &argon2::Config::default()).unwrap_or_default()
        });
        let _ = argon2::verify_encoded(&DUMMY_HASH, plaintext_password.as_bytes());
    }

    /// Verify a user's password is correct
    pub fn verify_password(&self, plaintext_password: &str) -> Result<()> {
        argon2::verify_encoded(&self.password, plaintext_password.as_bytes())
            .map(|v| {
                if v {
                    Ok(())
                } else {
                    Err(create_error!(InvalidCredentials))
                }
            })
            // To prevent user enumeration, we should ignore
            // the error and pretend the password is wrong.
            .map_err(|_| create_error!(InvalidCredentials))?
    }

    /// Validate an MFA response
    pub async fn consume_mfa_response(
        &mut self,
        db: &Database,
        response: v0::MFAResponse,
        ticket: Option<&mut MFATicket>,
    ) -> Result<()> {
        let allowed_methods = self.mfa.get_methods();

        match response {
            v0::MFAResponse::Password { password } => {
                if allowed_methods.contains(&MFAMethod::Password) {
                    self.verify_password(&password)
                } else {
                    Err(create_error!(DisallowedMFAMethod))
                }
            }
            v0::MFAResponse::Totp { totp_code } => {
                if allowed_methods.contains(&MFAMethod::Totp) {
                    if let Totp::Enabled { .. } = &self.mfa.totp_token {
                        // Use TOTP code at generation if applicable
                        if let Some(ref ticket) = ticket {
                            if ticket.attempts >= 3 {
                                db.delete_ticket(&ticket.id).await?;
                                return Err(create_error!(InvalidToken));
                            }
                            if let Some(code) = &ticket.last_totp_code {
                                if code == &totp_code {
                                    db.delete_ticket(&ticket.id).await?;
                                    return Ok(());
                                }
                            }
                        }

                        // Otherwise read current TOTP token
                        if self.mfa.totp_token.accepts_code(&totp_code)
                            && claim_totp_code(&self.id, &totp_code).await
                        {
                            if let Some(ticket) = ticket {
                                db.delete_ticket(&ticket.id).await?;
                            }
                            Ok(())
                        } else {
                            if let Some(ticket) = ticket {
                                ticket.attempts += 1;

                                if ticket.attempts >= 3 {
                                    db.delete_ticket(&ticket.id).await?;
                                } else {
                                    db.save_ticket(ticket).await?;
                                }
                            }
                            Err(create_error!(InvalidToken))
                        }
                    } else {
                        unreachable!()
                    }
                } else {
                    Err(create_error!(DisallowedMFAMethod))
                }
            }
            v0::MFAResponse::Recovery { recovery_code } => {
                if allowed_methods.contains(&MFAMethod::Recovery) {
                    if let Some(index) = self
                        .mfa
                        .recovery_codes
                        .iter()
                        .position(|x| x == &recovery_code)
                    {
                        self.mfa.recovery_codes.remove(index);
                        self.save(db).await
                    } else {
                        Err(create_error!(InvalidToken))
                    }
                } else {
                    Err(create_error!(DisallowedMFAMethod))
                }
            }
        }
    }

    /// Delete all sessions for an account
    pub async fn delete_all_sessions(
        &self,
        db: &Database,
        exclude_session_id: Option<String>,
    ) -> Result<()> {
        db.delete_all_sessions(&self.id, exclude_session_id.clone())
            .await?;

        // Create and push event
        EventV1::DeleteAllSessions {
            user_id: self.id.clone(),
            exclude_session_id,
        }
        .p(self.id.clone())
        .await;

        Ok(())
    }

    /// Disable an account
    pub async fn disable(&mut self, db: &Database) -> Result<()> {
        self.disabled = true;
        self.delete_all_sessions(db, None).await?;
        self.save(db).await
    }

    /// Schedule an account for deletion
    pub async fn schedule_deletion(&mut self, db: &Database) -> Result<()> {
        self.deletion = Some(DeletionInfo::Scheduled {
            after: Timestamp::now_utc()
                .checked_add(Duration::days(ACCOUNT_DELETION_GRACE_DAYS))
                .unwrap(),
        });

        self.disable(db).await
    }

    /// Removes all information from the account and marks it as fully deleted
    ///
    /// Besides the email, this clears everything that identifies the person or
    /// lets someone sign in: the password hash, MFA secrets and recovery codes,
    /// the declared age (birth month and the age-change log), pending tokens,
    /// sessions (which carry the push subscriptions) and any data export. Their
    /// designs and the files of those go too.
    pub async fn mark_deleted(&mut self, db: &Database) -> Result<()> {
        // Clean up first and mark the account Deleted last: if a step fails,
        // the account stays Scheduled and crond tries again the next hour
        // Sessions hold the push subscriptions, so this removes those too
        self.delete_all_sessions(db, None).await?;
        db.remove_data_exports_for_user(&self.id).await?;
        crate::erase_community_items_of(db, &self.id).await?;

        self.email = format!("Deleted User {}", &self.id);
        self.email_normalised = format!("Deleted User {}", &self.id);
        self.password = String::new();
        self.mfa = Default::default();
        self.age = None;
        self.password_reset = None;
        self.lockout = None;
        self.verification = EmailVerification::Verified;
        self.disabled = true;
        self.deletion = Some(DeletionInfo::Deleted);

        self.save(db).await?;
        // `save` does not unset fields that are now empty (age, MFA, ...)
        db.scrub_account(&self.id).await?;

        Ok(())
    }

    /// Whether this is a genuinely new account: created less than a day ago,
    /// owns no servers or bots, has sent no messages. Only such an account may
    /// be removed at once when its first age statement is under the minimum.
    pub async fn is_new_and_empty(&self, db: &Database) -> bool {
        let created = ulid::Ulid::from_string(&self.id)
            .map(|u| u.timestamp_ms() as i64)
            .unwrap_or(0);
        let now = Timestamp::now_utc()
            .duration_since(Timestamp::UNIX_EPOCH)
            .whole_milliseconds() as i64;
        if now - created > NEW_ACCOUNT_HOURS * 3_600_000 {
            return false;
        }
        // anything at all (a server, a bot, a message) means it is not empty;
        // an error counts as "not empty" so nothing is removed by mistake
        !db.user_has_content(&self.id).await.unwrap_or(true)
    }

    /// Remove everything created for a person who turned out to be under the
    /// minimum age: the account (email, password hash, MFA, tokens), the user,
    /// its settings, sessions, push subscriptions and any data export. Only
    /// for a new, empty account (see `is_new_and_empty`).
    ///
    /// The account document goes first, so no session works any more; the
    /// rest is best effort and logged. `save_account` creates a missing
    /// account, so a request from the same person that saves the account at
    /// the very same moment could bring it back; the document is removed
    /// again at the end to make that window small. Nothing is kept on the
    /// server. Designs go first and a failure stops here: without the account
    /// document nothing would try again.
    pub async fn erase_completely(&mut self, db: &Database) -> Result<()> {
        crate::erase_community_items_of(db, &self.id).await?;
        db.remove_account(&self.id).await?;

        if let Err(error) = self.delete_all_sessions(db, None).await {
            log::warn!("Could not remove the sessions of {}: {error:?}", self.id);
        }
        if let Ok(mut user) = db.fetch_user(&self.id).await {
            if let Err(error) = user.delete(db).await {
                log::warn!("Could not remove user {}: {error:?}", self.id);
            }
            if let Err(error) = db.delete_user(&self.id).await {
                log::warn!("Could not remove user {}: {error:?}", self.id);
            }
        }
        if let Err(error) = db.delete_user_settings(&self.id).await {
            log::warn!("Could not remove the settings of {}: {error:?}", self.id);
        }
        db.remove_data_exports_for_user(&self.id).await.ok();
        // again, in case a request in flight saved it in the meantime
        db.remove_account(&self.id).await
    }
}

#[cfg(test)]
mod totp_tests {
    use super::Totp;

    const SECRET: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";

    fn code_at(seconds: u64) -> String {
        let key = base32::decode(base32::Alphabet::RFC4648 { padding: false }, SECRET).unwrap();
        totp_lite::totp_custom::<totp_lite::Sha1>(totp_lite::DEFAULT_STEP, 6, &key, seconds)
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn a_code_from_the_step_before_or_after_is_accepted() {
        let totp = Totp::Enabled {
            secret: SECRET.to_string(),
        };
        // a phone a little behind or ahead, or a code typed at the boundary
        assert!(totp.accepts_code(&code_at(now())));
        assert!(totp.accepts_code(&code_at(now() - 30)));
        assert!(totp.accepts_code(&code_at(now() + 30)));
        // older codes stay refused
        assert!(!totp.accepts_code(&code_at(now() - 90)));
        assert!(!totp.accepts_code("000000x"));
        assert!(!Totp::Disabled.accepts_code(&code_at(now())));
    }
}

#[cfg(test)]
mod lockout_tests {
    use super::Lockout;
    use iso8601_timestamp::{Duration, Timestamp};

    #[test]
    fn a_running_lock_is_not_extended() {
        let now = Timestamp::now_utc();
        let running = Lockout {
            attempts: 5,
            expiry: Some(now + Duration::minutes(10)),
            last_attempt: Some(now),
        };
        let later = now + Duration::minutes(1);
        let after = Lockout::after_failure(Some(&running), later);
        assert_eq!(after.attempts, 5);
        assert_eq!(after.expiry, running.expiry);
    }

    #[test]
    fn old_attempts_are_forgotten() {
        let now = Timestamp::now_utc();
        let old = Lockout {
            attempts: 9,
            expiry: Some(now + Duration::hours(-3)),
            last_attempt: Some(now + Duration::hours(-3)),
        };
        let after = Lockout::after_failure(Some(&old), now);
        assert_eq!(after.attempts, 1);
        assert_eq!(after.expiry, None);
    }

    #[test]
    fn the_third_failure_locks_for_a_minute() {
        let now = Timestamp::now_utc();
        let mut lockout = None;
        for _ in 0..3 {
            lockout = Some(Lockout::after_failure(lockout.as_ref(), now));
        }
        let lockout = lockout.unwrap();
        assert_eq!(lockout.attempts, 3);
        assert_eq!(lockout.expiry, Some(now + Duration::minutes(1)));
    }
}
