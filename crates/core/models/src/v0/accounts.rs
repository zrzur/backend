auto_derived!(
    /// # Change Email Data
    pub struct DataChangeEmail {
        /// Valid email address
        pub email: String,
        /// Current password
        pub current_password: String,
    }

    /// # Change Data
    pub struct DataChangePassword {
        /// New password
        pub password: String,
        /// Current password
        pub current_password: String,
    }

    /// # Account Deletion Token
    pub struct DataAccountDeletion {
        /// Deletion token
        pub token: String,
    }

    /// # Account Data
    pub struct DataCreateAccount {
        /// Valid email address
        pub email: String,
        /// Password
        pub password: String,
        /// Invite code
        pub invite: Option<String>,
        /// Captcha verification code
        pub captcha: Option<String>,
        /// Year and month of birth, `YYYY-MM` (the day is never collected)
        pub birth_month: Option<String>,
    }

    /// # Create Account And Join Data
    ///
    /// Sign-up from an invite link: no password yet (the confirmation mail
    /// sets it), and the account joins the invite at once
    pub struct DataCreateAndJoin {
        /// Valid email address
        pub email: String,
        /// Name shown to other people; the username is made from it
        pub display_name: String,
        /// Year and month of birth, `YYYY-MM` (the day is never collected)
        pub birth_month: String,
        /// Code of the invite to join
        pub invite: String,
        /// Captcha verification code
        pub captcha: Option<String>,
    }

    /// # Create Account And Join Response
    #[serde(tag = "result")]
    #[allow(clippy::large_enum_variant)]
    pub enum ResponseCreateAndJoin {
        /// The account exists and is signed in
        Entered {
            /// Session of the new account
            session: super::Session,
            /// What joining the invite gave; missing when it was refused
            #[serde(skip_serializing_if = "Option::is_none")]
            joined: Option<super::InviteJoinResponse>,
            /// Why joining was refused (an error type such as `Banned`)
            #[serde(skip_serializing_if = "Option::is_none")]
            join_error: Option<String>,
            /// When the account is removed unless its e-mail is confirmed
            confirm_by: iso8601_timestamp::Timestamp,
        },
        /// Nothing more is said here: a mail was sent to the address
        CheckEmail,
    }

    /// # Verify Email Data
    pub struct DataVerifyEmail {
        /// Password to set, for an account that has none yet
        pub password: Option<String>,
    }

    /// # Verification Link Information
    pub struct VerifyEmailInfo {
        /// Whether confirming needs a password to be chosen
        pub needs_password: bool,
    }

    pub struct AccountInfo {
        #[serde(rename = "_id")]
        pub id: String,
        pub email: String,
        /// Age group and what it allows; only ever sent to the account owner
        /// (absent from servers older than the age rules)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub age: Option<AccountAgeInfo>,
        /// When the account is removed unless its e-mail is confirmed
        /// (only on an account made from an invite link, until confirmed)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub confirm_by: Option<iso8601_timestamp::Timestamp>,
    }

    /// # Age group
    ///
    /// Worked out on the server from the year and month of birth, each time
    /// it is needed, so it moves on as time passes
    #[derive(Copy)]
    #[serde(rename_all = "snake_case")]
    pub enum AgeBand {
        /// No age on record (accounts made before ages were asked)
        Unknown,
        /// Below the minimum age for an account
        UnderMinimum,
        /// At least the minimum age, not yet 18
        Teen,
        /// 18 or older by the user's own statement (not verified)
        AdultDeclared,
    }

    /// # Where the age comes from
    #[derive(Copy)]
    pub enum AgeSource {
        /// Stated by the user; never proof of age
        #[serde(rename = "self")]
        SelfDeclared,
    }

    /// # Access to mature content
    #[derive(Copy)]
    #[serde(rename_all = "snake_case")]
    pub enum MatureAccess {
        /// Channels and Servers marked as mature may be opened
        Allowed,
        /// No age on record
        AgeUnknown,
        /// Under 18
        Minor,
        /// Needs an age check that this instance does not offer
        VerificationRequired,
    }

    /// # Age Information
    ///
    /// Only ever returned to the account owner
    pub struct AccountAgeInfo {
        /// Year and month of birth, `YYYY-MM`, if declared
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub birth_month: Option<String>,
        /// Current age group
        pub band: AgeBand,
        /// Whether content marked as mature may be opened, and if not, why
        pub mature_access: MatureAccess,
        /// Whether the one allowed upward correction is still available
        pub can_raise: bool,
    }

    /// # Change Age Data
    pub struct DataChangeAge {
        /// Year and month of birth, `YYYY-MM`
        pub birth_month: String,
        /// Current password, required once an age is on record
        pub current_password: Option<String>,
    }

    /// # Password Reset
    pub struct DataPasswordReset {
        /// Reset token
        pub token: String,

        /// New password
        pub password: String,

        /// Kept for older clients: every session now always ends on a reset
        #[serde(default)]
        pub remove_sessions: bool,
    }

    /// # Resend Information
    pub struct DataResendVerification {
        /// Email associated with the account
        pub email: String,
        /// Captcha verification code
        pub captcha: Option<String>,
    }

    /// # Reset Information
    pub struct DataSendPasswordReset {
        /// Email associated with the account
        pub email: String,
        /// Captcha verification code
        pub captcha: Option<String>,
    }
);