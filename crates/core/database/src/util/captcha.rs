use reqwest::Client;
use revolt_config::config;
use revolt_result::Result;
use std::sync::LazyLock;

static CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

/// Cloudflare Turnstile's server-side check
const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

#[derive(Serialize, Deserialize)]
struct CaptchaResponse {
    success: bool,
}

/// Verify a Cloudflare Turnstile token. Without a configured secret the check
/// is off (development only: a release start refuses that, see settings).
pub async fn check_captcha(token: Option<&str>) -> Result<()> {
    let config = config().await;
    let secret = config.api.security.captcha.turnstile_secret.as_str();

    if secret.is_empty() {
        return Ok(());
    }

    let Some(token) = token else {
        return Err(create_error!(CaptchaFailed));
    };

    let response = CLIENT
        .post(SITEVERIFY_URL)
        .form(&[("secret", secret), ("response", token)])
        .send()
        .await
        .map_err(|_| create_error!(CaptchaFailed))?
        .json::<CaptchaResponse>()
        .await
        .map_err(|_| create_error!(CaptchaFailed))?;

    if response.success {
        Ok(())
    } else {
        Err(create_error!(CaptchaFailed))
    }
}
