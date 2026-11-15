//! Linked accounts through the service's own OAuth page: Twitch, GitHub and
//! Battle.net (authorization code flow).
//!
//! The person's token is used once, inside Finish, to read who they are,
//! then dropped (GitHub: also revoked there). Nothing that lets anyone act
//! as the person is ever stored. Later reads use the server's own app
//! credentials and public data only.

use std::{sync::OnceLock, time::Duration};

use revolt_config::{config, ApiConnectionOAuth};
use revolt_result::{create_error, Result};
use serde::Deserialize;

/// A service linked through OAuth
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Twitch,
    Github,
    Battlenet,
}

pub const TWITCH_AUTH: &str = "https://id.twitch.tv";
pub const TWITCH_API: &str = "https://api.twitch.tv";
pub const GITHUB_AUTH: &str = "https://github.com";
pub const GITHUB_API: &str = "https://api.github.com";
pub const BATTLENET_AUTH: &str = "https://oauth.battle.net";

impl Service {
    pub const ALL: [Service; 3] = [Service::Twitch, Service::Github, Service::Battlenet];

    pub fn parse(name: &str) -> Option<Service> {
        match name {
            "twitch" => Some(Service::Twitch),
            "github" => Some(Service::Github),
            "battlenet" => Some(Service::Battlenet),
            _ => None,
        }
    }

    /// Id used in addresses, storage and errors
    pub fn id(self) -> &'static str {
        match self {
            Service::Twitch => "twitch",
            Service::Github => "github",
            Service::Battlenet => "battlenet",
        }
    }

    pub async fn config(self) -> ApiConnectionOAuth {
        let connections = config().await.api.connections;
        match self {
            Service::Twitch => connections.twitch,
            Service::Github => connections.github,
            Service::Battlenet => connections.battlenet,
        }
    }

    fn auth_base(self, config: &ApiConnectionOAuth) -> &str {
        match self {
            Service::Twitch => config.auth_base_or(TWITCH_AUTH),
            Service::Github => config.auth_base_or(GITHUB_AUTH),
            Service::Battlenet => config.auth_base_or(BATTLENET_AUTH),
        }
    }

    /// GitHub supports PKCE; the others rely on the client secret
    pub fn uses_pkce(self) -> bool {
        matches!(self, Service::Github)
    }

    /// The service's own sign-in page. No scope beyond reading who the
    /// person is: Twitch and GitHub need none for public data, Battle.net
    /// `openid` for the BattleTag.
    pub fn authorize_url(
        self,
        config: &ApiConnectionOAuth,
        redirect_uri: &str,
        state: &str,
        challenge: Option<&str>,
    ) -> String {
        let (path, scope) = match self {
            Service::Twitch => ("/oauth2/authorize", ""),
            Service::Github => ("/login/oauth/authorize", ""),
            Service::Battlenet => ("/authorize", "openid"),
        };
        let mut url = url::Url::parse(&format!("{}{path}", self.auth_base(config)))
            .expect("service sign-in address");
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("client_id", &config.client_id)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("response_type", "code")
                .append_pair("scope", scope)
                .append_pair("state", state);
            if let Some(challenge) = challenge {
                query
                    .append_pair("code_challenge", challenge)
                    .append_pair("code_challenge_method", "S256");
            }
            if self == Service::Github {
                // the person picks the account; no silent re-use
                query.append_pair("allow_signup", "false");
            }
            if self == Service::Twitch {
                query.append_pair("force_verify", "true");
            }
        }
        url.to_string()
    }

    fn token_url(self, config: &ApiConnectionOAuth) -> String {
        match self {
            Service::Twitch => format!("{}/oauth2/token", self.auth_base(config)),
            Service::Github => format!("{}/login/oauth/access_token", self.auth_base(config)),
            Service::Battlenet => format!("{}/token", self.auth_base(config)),
        }
    }
}

/// Where the service sends the person back to
pub fn return_to(origin: &str, service: Service) -> String {
    format!(
        "{}/connections/{}/return",
        origin.trim_end_matches('/'),
        service.id()
    )
}

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("Zrzur-Linked-Accounts/1")
            .build()
            .expect("reqwest Client")
    })
}

/// Text from a service, made safe to show: no control characters and no
/// direction overrides, trimmed, at most `max` characters
pub fn clean_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// What a service shows publicly about the account, beyond the name.
/// Pictures are kept only when they are on the service's own image host.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Details {
    /// Profile picture (Twitch: static-cdn.jtvnw.net, GitHub:
    /// avatars.githubusercontent.com); shown through Zrzur's media proxy
    pub avatar_url: Option<String>,
    /// GitHub: the person's own name, when they set one
    pub real_name: Option<String>,
    /// Twitch description, GitHub bio
    pub bio: Option<String>,
    /// GitHub followers
    pub followers: Option<u32>,
    /// GitHub public repositories
    pub public_repos: Option<u32>,
    /// Twitch: `partner` or `affiliate`
    pub kind: Option<String>,
}

/// Who the person is on the service
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Stable id on the service
    pub external_id: String,
    /// Name to show (Battle.net: the BattleTag)
    pub display_name: String,
    /// Login used in addresses (Twitch, GitHub); empty for Battle.net
    pub login: String,
    /// Public page, built by us from the login; empty for Battle.net
    pub profile_url: String,
    pub details: Details,
}

/// A picture address only when it is https on exactly this host
pub fn image_on(url: &str, host: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    (parsed.scheme() == "https"
        && parsed.host_str() == Some(host)
        && parsed.port().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && url.len() <= 512)
        .then(|| parsed.to_string())
}

/// Optional text from a service: cleaned, `None` when empty
pub fn clean_opt(text: Option<String>, max: usize) -> Option<String> {
    text.map(|text| clean_text(&text, max)).filter(|text| !text.is_empty())
}

/// A Twitch login: 1-25 of `a-z`, `0-9`, `_`
pub fn valid_twitch_login(login: &str) -> bool {
    (1..=25).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A GitHub login: 1-39 of letters, digits and single inner hyphens
pub fn valid_github_login(login: &str) -> bool {
    (1..=39).contains(&login.len())
        && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--")
}

/// A BattleTag: a name of 2-16 letters or digits, `#`, 3-8 digits
pub fn valid_battletag(tag: &str) -> bool {
    let Some((name, number)) = tag.split_once('#') else {
        return false;
    };
    let length = name.chars().count();
    (2..=16).contains(&length)
        && name.chars().all(char::is_alphanumeric)
        && (3..=8).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit())
}

/// An id made of digits only, as all three services use
pub fn valid_numeric_id(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
}

/// Swap the code the service sent back for the person's token
pub async fn exchange_code(
    service: Service,
    config: &ApiConnectionOAuth,
    code: &str,
    redirect_uri: &str,
    verifier: Option<&str>,
) -> Result<String> {
    #[cfg(test)]
    {
        let _ = (config, redirect_uri, verifier);
        return test_services::exchange_code(service, code);
    }

    #[allow(unreachable_code)]
    {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ];
        if let Some(verifier) = verifier {
            form.push(("code_verifier", verifier));
        }
        let mut request = client()
            .post(service.token_url(config))
            .header("accept", "application/json");
        request = match service {
            // Battle.net takes the client by HTTP basic
            Service::Battlenet => {
                request.basic_auth(&config.client_id, Some(&config.client_secret))
            }
            _ => {
                form.push(("client_id", config.client_id.as_str()));
                form.push(("client_secret", config.client_secret.as_str()));
                request
            }
        };
        let answer = request
            .form(&form)
            .send()
            .await
            .map_err(|_| create_error!(LinkedServiceFailed))?;
        let status = answer.status();
        if status.is_server_error() {
            return Err(create_error!(LinkedServiceFailed));
        }
        if !status.is_success() {
            // a code that was used, expired or not ours
            return Err(create_error!(LinkedSignInFailed));
        }
        // GitHub answers 200 with an error body for a bad code
        let token: TokenAnswer = answer
            .json()
            .await
            .map_err(|_| create_error!(LinkedSignInFailed))?;
        if token.access_token.is_empty() {
            return Err(create_error!(LinkedSignInFailed));
        }
        Ok(token.access_token)
    }
}

#[derive(Deserialize)]
struct TwitchUsers {
    data: Vec<TwitchUser>,
}

#[derive(Deserialize, Default)]
pub struct TwitchUser {
    pub id: String,
    pub login: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub profile_image_url: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub broadcaster_type: String,
}

#[derive(Deserialize, Default)]
pub struct GithubUser {
    pub id: u64,
    pub login: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub followers: Option<u32>,
    #[serde(default)]
    pub public_repos: Option<u32>,
}

/// Turn what GitHub says into an identity, or refuse it
pub fn github_identity(user: GithubUser) -> Result<Identity> {
    if !valid_github_login(&user.login) {
        return Err(create_error!(LinkedServiceFailed));
    }
    Ok(Identity {
        external_id: user.id.to_string(),
        display_name: user.login.clone(),
        profile_url: format!("https://github.com/{}", user.login),
        login: user.login,
        details: Details {
            avatar_url: user
                .avatar_url
                .as_deref()
                .and_then(|url| image_on(url, "avatars.githubusercontent.com")),
            real_name: clean_opt(user.name, 64),
            bio: clean_opt(user.bio, 160),
            followers: user.followers,
            public_repos: user.public_repos,
            kind: None,
        },
    })
}

#[derive(Deserialize)]
struct BattlenetUser {
    id: u64,
    battletag: String,
}

/// Turn what Twitch says into an identity, or refuse it
pub fn twitch_identity(user: TwitchUser) -> Result<Identity> {
    let login = user.login.to_ascii_lowercase();
    if !valid_numeric_id(&user.id) || !valid_twitch_login(&login) {
        return Err(create_error!(LinkedServiceFailed));
    }
    let mut display_name = clean_text(&user.display_name, 64);
    if display_name.is_empty() {
        display_name = login.clone();
    }
    let kind = match user.broadcaster_type.as_str() {
        "partner" | "affiliate" => Some(user.broadcaster_type.clone()),
        _ => None,
    };
    Ok(Identity {
        external_id: user.id,
        profile_url: format!("https://www.twitch.tv/{login}"),
        display_name,
        login,
        details: Details {
            avatar_url: image_on(&user.profile_image_url, "static-cdn.jtvnw.net"),
            bio: clean_opt(Some(user.description), 300),
            kind,
            ..Default::default()
        },
    })
}

/// Read who the person is, with their token. The token is not kept.
pub async fn identify(service: Service, config: &ApiConnectionOAuth, token: &str) -> Result<Identity> {
    #[cfg(test)]
    {
        let _ = config;
        return test_services::identify(service, token);
    }

    #[allow(unreachable_code)]
    match service {
        Service::Twitch => {
            let users: TwitchUsers = get_json(
                client()
                    .get(format!("{}/helix/users", config.api_base_or(TWITCH_API)))
                    .header("client-id", &config.client_id)
                    .bearer_auth(token),
            )
            .await?;
            let user = users
                .data
                .into_iter()
                .next()
                .ok_or_else(|| create_error!(LinkedServiceFailed))?;
            twitch_identity(user)
        }
        Service::Github => {
            let user: GithubUser = get_json(
                client()
                    .get(format!("{}/user", config.api_base_or(GITHUB_API)))
                    .header("accept", "application/vnd.github+json")
                    .header("x-github-api-version", "2022-11-28")
                    .bearer_auth(token),
            )
            .await?;
            github_identity(user)
        }
        Service::Battlenet => {
            let user: BattlenetUser = get_json(
                client()
                    .get(format!("{}/userinfo", config.auth_base_or(BATTLENET_AUTH)))
                    .bearer_auth(token),
            )
            .await?;
            let tag = clean_text(&user.battletag, 32);
            if !valid_battletag(&tag) {
                return Err(create_error!(LinkedServiceFailed));
            }
            Ok(Identity {
                external_id: user.id.to_string(),
                display_name: tag,
                login: String::new(),
                profile_url: String::new(),
                details: Details::default(),
            })
        }
    }
}

/// Send a request and read JSON. Any refusal is the service failing: the
/// person's token was fresh. Errors never carry the address or the token.
pub async fn get_json<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T> {
    let answer = request
        .send()
        .await
        .map_err(|_| create_error!(LinkedServiceFailed))?;
    if !answer.status().is_success() {
        return Err(create_error!(LinkedServiceFailed));
    }
    answer
        .json()
        .await
        .map_err(|_| create_error!(LinkedServiceFailed))
}

/// The services as the tests see them: no network, fixed people. A code
/// `c-<id>` signs in as `<id>`; `refused` is a code the service refuses;
/// `down` is the service failing.
#[cfg(test)]
pub mod test_services {
    use super::*;

    pub const REFUSED: &str = "refused";
    pub const DOWN: &str = "down";

    pub fn exchange_code(service: Service, code: &str) -> Result<String> {
        match code {
            REFUSED => Err(create_error!(LinkedSignInFailed)),
            DOWN => Err(create_error!(LinkedServiceFailed)),
            _ => Ok(format!("token:{}:{code}", service.id())),
        }
    }

    pub fn identify(service: Service, token: &str) -> Result<Identity> {
        let id = token
            .rsplit(":c-")
            .next()
            .filter(|id| valid_numeric_id(id))
            .ok_or_else(|| create_error!(LinkedServiceFailed))?;
        Ok(match service {
            Service::Twitch => twitch_identity(TwitchUser {
                id: id.to_string(),
                login: format!("streamer_{id}"),
                display_name: format!("Streamer{id}"),
                profile_image_url: format!("https://static-cdn.jtvnw.net/jtv_user_pictures/{id}.png"),
                description: "Ranked nights\u{0007}".into(),
                broadcaster_type: "partner".into(),
            })?,
            Service::Github => github_identity(GithubUser {
                id: id.parse().unwrap_or_default(),
                login: format!("dev-{id}"),
                avatar_url: Some(format!("https://avatars.githubusercontent.com/u/{id}?v=4")),
                name: Some("Dev Person".into()),
                bio: Some("Builds things".into()),
                followers: Some(128),
                public_repos: Some(42),
            })?,
            Service::Battlenet => Identity {
                external_id: id.to_string(),
                display_name: format!("Player#{id}"),
                login: String::new(),
                profile_url: String::new(),
                details: Details::default(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ApiConnectionOAuth {
        ApiConnectionOAuth {
            client_id: "client".into(),
            client_secret: "secret".into(),
            return_origins: vec!["https://app.zrzur.com".into()],
            ..Default::default()
        }
    }

    fn query(url: &str) -> std::collections::BTreeMap<String, String> {
        url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
    }

    #[test]
    fn sign_in_pages_are_the_services_own_and_ask_for_nothing_extra() {
        let back = return_to("https://app.zrzur.com/", Service::Twitch);
        assert_eq!(back, "https://app.zrzur.com/connections/twitch/return");

        let twitch = Service::Twitch.authorize_url(&config(), &back, "state-1", None);
        assert!(twitch.starts_with("https://id.twitch.tv/oauth2/authorize?"));
        let q = query(&twitch);
        assert_eq!(q["scope"], "");
        assert_eq!(q["state"], "state-1");
        assert_eq!(q["redirect_uri"], back);
        assert_eq!(q["response_type"], "code");
        assert!(!q.contains_key("code_challenge"));

        let github = Service::Github.authorize_url(&config(), "x", "s", Some("challenge"));
        assert!(github.starts_with("https://github.com/login/oauth/authorize?"));
        let q = query(&github);
        assert_eq!(q["scope"], "");
        assert_eq!(q["code_challenge"], "challenge");
        assert_eq!(q["code_challenge_method"], "S256");

        let battlenet = Service::Battlenet.authorize_url(&config(), "x", "s", None);
        assert!(battlenet.starts_with("https://oauth.battle.net/authorize?"));
        assert_eq!(query(&battlenet)["scope"], "openid");

        // a test stack's mock
        let mut mock = config();
        mock.auth_base = "http://127.0.0.1:9000/".into();
        assert!(Service::Twitch
            .authorize_url(&mock, "x", "s", None)
            .starts_with("http://127.0.0.1:9000/oauth2/authorize?"));
    }

    #[test]
    fn names_from_services_are_checked() {
        assert!(valid_twitch_login("streamer_42"));
        assert!(!valid_twitch_login("Streamer"));
        assert!(!valid_twitch_login("a/b"));
        assert!(!valid_twitch_login(""));
        assert!(valid_github_login("maher-xs"));
        assert!(!valid_github_login("-x"));
        assert!(!valid_github_login("a--b"));
        assert!(!valid_github_login("a/b"));
        assert!(valid_battletag("Maher#21794"));
        assert!(valid_battletag("زرزور#1234"));
        assert!(!valid_battletag("Maher"));
        assert!(!valid_battletag("M#123"));
        assert!(!valid_battletag("Maher#12"));
        assert!(!valid_battletag("Ma her#1234"));
        assert!(valid_numeric_id("123"));
        assert!(!valid_numeric_id("12a"));
        assert!(!valid_numeric_id(""));
    }

    #[test]
    fn twitch_identity_builds_its_own_link() {
        let id = twitch_identity(TwitchUser {
            id: "99".into(),
            login: "MaherLive".into(),
            display_name: "Maher\u{202e}Live\n".into(),
            profile_image_url: "https://evil.example/x.png".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(id.profile_url, "https://www.twitch.tv/maherlive");
        assert_eq!(id.display_name, "MaherLive");
        assert!(twitch_identity(TwitchUser {
            id: "99".into(),
            login: "evil.com/x".into(),
            ..Default::default()
        })
        .is_err());
    }

    #[test]
    fn pictures_only_from_the_services_own_host() {
        assert!(image_on("https://static-cdn.jtvnw.net/jtv_user_pictures/a.png", "static-cdn.jtvnw.net").is_some());
        assert!(image_on("http://static-cdn.jtvnw.net/a.png", "static-cdn.jtvnw.net").is_none());
        assert!(image_on("https://static-cdn.jtvnw.net.evil.example/a.png", "static-cdn.jtvnw.net").is_none());
        assert!(image_on("https://user@static-cdn.jtvnw.net/a.png", "static-cdn.jtvnw.net").is_none());
        assert!(image_on("https://static-cdn.jtvnw.net:8443/a.png", "static-cdn.jtvnw.net").is_none());
        assert!(image_on("not a url", "static-cdn.jtvnw.net").is_none());
        let id = github_identity(GithubUser {
            id: 5,
            login: "dev".into(),
            avatar_url: Some("https://evil.example/a.png".into()),
            name: Some("  ".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(id.details.avatar_url, None);
        assert_eq!(id.details.real_name, None);
    }

    #[test]
    fn clean_text_drops_control_characters_and_cuts() {
        assert_eq!(clean_text("  a\u{0}b\tc  ", 10), "abc");
        assert_eq!(clean_text("abcdef", 3), "abc");
    }
}
