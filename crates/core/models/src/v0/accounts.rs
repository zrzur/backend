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

    pub struct AccountInfo {
        #[serde(rename = "_id")]
        pub id: String,
        pub email: String,
        /// Age group and what it allows; only ever sent to the account owner
        /// (absent from servers older than the age rules)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub age: Option<AccountAgeInfo>,
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