use iso8601_timestamp::{Duration, Timestamp};
use sha2::{Digest, Sha256};

/// Secrets are stored only as SHA-256 hashes
pub fn hash_secret(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Compare two secret hashes without leaking where they differ
pub fn secret_hash_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `Timestamp::now_utc() + seconds`
pub fn in_seconds(seconds: i64) -> Timestamp {
    Timestamp::now_utc()
        .checked_add(Duration::seconds(seconds))
        .unwrap()
}

/// OAuth2 settings of an application; `_id` is the application (bot) id
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct OAuthClient {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub client_secret_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub secret_created_at: Option<Timestamp>,
}

/// Single-use authorization code; `_id` is the hash of the code
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct OAuthCode {
    #[serde(rename = "_id")]
    pub code_hash: String,
    pub application_id: String,
    pub user_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    /// PKCE S256 challenge
    pub code_challenge: String,
    pub expires_at: Timestamp,
}

/// Access + refresh token pair. A refresh creates a new pair in the same
/// family and revokes this one; using a revoked refresh token again revokes
/// the whole family (token theft).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct OAuthToken {
    #[serde(rename = "_id")]
    pub id: String,
    pub family: String,
    pub application_id: String,
    pub user_id: String,
    pub scopes: Vec<String>,
    pub access_hash: String,
    pub refresh_hash: String,
    pub access_expires_at: Timestamp,
    /// Refresh expiry; the document disappears afterwards (TTL)
    pub expires_at: Timestamp,
    #[serde(default)]
    pub revoked: bool,
}

/// That a user authorized an application, for "Authorized apps"; `_id` is `user:application`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct OAuthGrant {
    #[serde(rename = "_id")]
    pub id: String,
    pub user_id: String,
    pub application_id: String,
    pub scopes: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// An application installed in a server, and by whom; `_id` is `server:application`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AppInstall {
    #[serde(rename = "_id")]
    pub id: String,
    pub server_id: String,
    pub application_id: String,
    pub installed_by: String,
    pub permissions: i64,
    pub scopes: Vec<String>,
    /// `oauth` or `invite`
    pub via: String,
    pub installed_at: Timestamp,
}

impl AppInstall {
    pub fn key(server_id: &str, application_id: &str) -> String {
        format!("{server_id}:{application_id}")
    }
}

impl OAuthGrant {
    pub fn key(user_id: &str, application_id: &str) -> String {
        format!("{user_id}:{application_id}")
    }
}

impl OAuthToken {
    pub fn is_access_valid(&self) -> bool {
        !self.revoked && Timestamp::now_utc() < self.access_expires_at
    }

    pub fn is_refresh_valid(&self) -> bool {
        !self.revoked && Timestamp::now_utc() < self.expires_at
    }
}

/// base64url without padding (RFC 4648 §5)
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..chunk.len() + 1 {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// PKCE S256 (RFC 7636): base64url(SHA-256(verifier)) == challenge
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let valid = (43..=128).contains(&verifier.len())
        && verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c));
    valid && base64url(&Sha256::digest(verifier.as_bytes())) == challenge
}
