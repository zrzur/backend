//! Zrzur developer platform: OAuth2 (authorization code + PKCE) and app installs.

use iso8601_timestamp::Timestamp;

use super::{CommandApplication, File};

/// Scopes an application may request
pub mod oauth_scopes {
    /// Read the user's id, name and avatar
    pub const IDENTIFY: &str = "identify";
    /// List the servers the user is in (id, name, icon)
    pub const SERVERS_READ: &str = "servers.read";
    /// Let the app's commands be used in the server it is installed in
    pub const APPLICATIONS_COMMANDS: &str = "applications.commands";
    /// Add the app's bot to a server the user manages
    pub const BOT_INSTALL: &str = "bot.install";

    pub const ALL: [&str; 4] = [IDENTIFY, SERVERS_READ, APPLICATIONS_COMMANDS, BOT_INSTALL];
}

/// Lifetimes and limits of the OAuth2 implementation
pub mod oauth_limits {
    /// Authorization codes are single use and live 10 minutes
    pub const CODE_LIFETIME_SECS: i64 = 10 * 60;
    /// Access tokens live 1 hour
    pub const ACCESS_TOKEN_LIFETIME_SECS: i64 = 60 * 60;
    /// Refresh tokens live 30 days and are replaced on every use
    pub const REFRESH_TOKEN_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;
    pub const MAX_REDIRECT_URIS: usize = 10;
    pub const REDIRECT_URI_LENGTH: usize = 512;
    pub const STATE_LENGTH: usize = 512;
}

auto_derived!(
    /// OAuth2 settings of an application, as its owner sees them
    pub struct OAuthClientSettings {
        /// Same as the application (bot) id
        pub client_id: String,
        pub redirect_uris: Vec<String>,
        /// Whether a client secret exists (confidential client); it is never shown again
        pub has_secret: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub secret_created_at: Option<Timestamp>,
    }

    /// Change the registered redirect URIs
    pub struct DataEditOAuthClient {
        pub redirect_uris: Vec<String>,
    }

    /// A new client secret, shown once
    pub struct OAuthClientSecret {
        pub client_secret: String,
    }

    /// A server the user can install the app into
    pub struct OAuthServer {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub name: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub icon: Option<File>,
    }

    /// What the consent page shows
    pub struct AuthorizationPreview {
        pub application: CommandApplication,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description: Option<String>,
        pub verified: bool,
        pub scopes: Vec<String>,
        pub redirect_uri: String,
        /// Bot permissions requested with `bot.install`
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub permissions: Option<i64>,
        /// Servers the user manages, for `bot.install`
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub servers: Vec<OAuthServer>,
    }

    /// The user's answer on the consent page
    pub struct DataAuthorize {
        pub client_id: String,
        pub redirect_uri: String,
        pub response_type: String,
        /// Space separated
        pub scope: String,
        pub state: String,
        pub code_challenge: String,
        pub code_challenge_method: String,
        /// Server to install the bot into (`bot.install`)
        #[cfg_attr(feature = "serde", serde(default))]
        pub server_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(default))]
        pub permissions: Option<i64>,
        pub approve: bool,
    }

    /// Where the browser goes next (the registered redirect URI with `code` or `error`, and `state`)
    pub struct AuthorizeRedirect {
        pub redirect: String,
    }

    /// Token endpoint answer (RFC 6749 §5.1)
    pub struct OAuthTokenResponse {
        pub access_token: String,
        pub token_type: String,
        pub expires_in: i64,
        pub refresh_token: String,
        pub scope: String,
    }

    /// `identify`: the user behind an access token
    pub struct OAuthUser {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub username: String,
        pub discriminator: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub display_name: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub avatar: Option<File>,
    }

    /// An application the user authorized
    pub struct AuthorizedApplication {
        pub application: CommandApplication,
        pub scopes: Vec<String>,
        pub authorized_at: Timestamp,
    }

    /// An app installed in a server
    pub struct AppInstallation {
        pub server_id: String,
        pub application: CommandApplication,
        pub installed_by: String,
        pub permissions: i64,
        pub scopes: Vec<String>,
        /// `oauth` or `invite`
        pub via: String,
        pub installed_at: Timestamp,
    }
);

/// Check a redirect URI an app wants to register: https, or http only for this computer
pub fn validate_redirect_uri(uri: &str) -> Result<(), String> {
    if uri.is_empty() || uri.len() > oauth_limits::REDIRECT_URI_LENGTH {
        return Err("redirect URIs must be 1-512 characters".to_string());
    }
    if uri.chars().any(|c| c.is_whitespace() || c.is_control()) || uri.contains('#') {
        return Err("redirect URIs may not contain spaces or a fragment".to_string());
    }
    let rest = if let Some(rest) = uri.strip_prefix("https://") {
        rest
    } else if let Some(rest) = uri.strip_prefix("http://") {
        let host = rest.split(['/', '?']).next().unwrap_or_default();
        let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
        if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
            return Err("http redirect URIs are only allowed for localhost".to_string());
        }
        rest
    } else {
        return Err("redirect URIs must use https".to_string());
    };
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    if host.is_empty() || host.contains('@') {
        return Err("redirect URI has no valid host".to_string());
    }
    Ok(())
}

/// Parse a space separated scope list: known scopes only, no duplicates, at least one
pub fn parse_scopes(scope: &str) -> Result<Vec<String>, String> {
    let mut scopes: Vec<String> = Vec::new();
    for part in scope.split(' ').filter(|s| !s.is_empty()) {
        if !oauth_scopes::ALL.contains(&part) {
            return Err(format!("unknown scope '{part}'"));
        }
        if !scopes.iter().any(|s| s == part) {
            scopes.push(part.to_string());
        }
    }
    if scopes.is_empty() {
        return Err("at least one scope is required".to_string());
    }
    Ok(scopes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uris() {
        for ok in [
            "https://example.com/callback",
            "http://localhost:3000/cb",
            "http://127.0.0.1/cb?x=1",
        ] {
            assert!(validate_redirect_uri(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://example.com/cb",
            "javascript:alert(1)",
            "https://example.com/cb#frag",
            "https://user@evil.com/",
            "https://",
            "http://localhost.evil.com/cb",
            "",
        ] {
            assert!(validate_redirect_uri(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn scopes() {
        assert_eq!(
            parse_scopes("identify  bot.install identify").unwrap(),
            vec!["identify", "bot.install"]
        );
        assert!(parse_scopes("identify email").is_err());
        assert!(parse_scopes("").is_err());
    }
}
