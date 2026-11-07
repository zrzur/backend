use reqwest::Client;
use sha1::Digest;
use std::{collections::HashSet, sync::LazyLock};

use revolt_config::config;
use revolt_result::{Result, ToRevoltError};

static CLIENT: LazyLock<Client> = LazyLock::new(Client::new);
/// Argon2id with OWASP-recommended cost (19 MiB, 2 passes). Existing hashes
/// keep verifying because their parameters are stored in the encoded string.
static ARGON_CONFIG: LazyLock<argon2::Config<'static>> = LazyLock::new(|| argon2::Config {
    variant: argon2::Variant::Argon2id,
    mem_cost: 19 * 1024,
    time_cost: 2,
    lanes: 1,
    ..argon2::Config::default()
});

/// Minimum and maximum password length (characters)
pub const MIN_PASSWORD_LENGTH: usize = 10;
pub const MAX_PASSWORD_LENGTH: usize = 128;
static TOP_100K_COMPROMISED: LazyLock<HashSet<String>> = LazyLock::new(|| {
    include_str!("../../assets/pwned100k.txt")
        .split('\n')
        .map(|x| x.into())
        .collect()
});

#[derive(Deserialize)]
struct EasyPwnedResult {
    secure: bool,
}

/// Hash a password using argon2
pub fn hash_password(plaintext_password: String) -> Result<String> {
    argon2::hash_encoded(
        plaintext_password.as_bytes(),
        nanoid::nanoid!(24).as_bytes(),
        &ARGON_CONFIG,
    )
    .to_internal_error()
}

pub async fn assert_safe(password: &str) -> Result<()> {
    // Long enough to resist guessing, short enough to keep hashing cheap.
    let length = password.chars().count();
    if length < MIN_PASSWORD_LENGTH {
        return Err(create_error!(ShortPassword));
    }
    if length > MAX_PASSWORD_LENGTH {
        return Err(revolt_result::failed_validation(vec![revolt_result::ValidationProblem {
            max: Some(MAX_PASSWORD_LENGTH as i64),
            ..revolt_result::ValidationProblem::new("password", "length")
        }]));
    }

    // Reject passwords made of a single repeated character or an obvious sequence
    if is_trivial(password) {
        return Err(create_error!(CompromisedPassword));
    }

    let config = config().await;

    if !config.api.security.easypwned.is_empty() {
        let mut hasher = sha1::Sha1::new();
        hasher.update(password);
        let pwd_hash = hasher.finalize();

        let result = match CLIENT
            .get(format!(
                "{}/hash/{pwd_hash:#02x}",
                &config.api.security.easypwned
            ))
            .send()
            .await
        {
            Ok(response) => match response.json::<EasyPwnedResult>().await {
                Ok(result) => Ok(result.secure),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };

        match &result {
            Err(e) => revolt_config::capture_error(e),
            Ok(true) => return Ok(()),
            // the breach database knows this password
            Ok(false) => return Err(create_error!(CompromisedPassword)),
        };
    };

    if TOP_100K_COMPROMISED.contains(password)
        || TOP_100K_COMPROMISED.contains(&password.to_lowercase())
    {
        return Err(create_error!(CompromisedPassword));
    };

    Ok(())
}

/// Same character repeated, or a straight run like "1234567890" / "abcdefghij"
fn is_trivial(password: &str) -> bool {
    let chars: Vec<char> = password.to_lowercase().chars().collect();
    if chars.windows(2).all(|w| w[0] == w[1]) {
        return true;
    }
    // digits wrap around, so "1234567890" counts as one run
    let step = |w: &[char]| match (w[0].to_digit(10), w[1].to_digit(10)) {
        (Some(a), Some(b)) => match (b as i64 - a as i64).rem_euclid(10) {
            1 => 1,
            9 => -1,
            other => other,
        },
        _ => (w[1] as i64) - (w[0] as i64),
    };
    let first = step(&chars[0..2]);
    (first == 1 || first == -1) && chars.windows(2).all(|w| step(w) == first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_passwords_are_detected() {
        assert!(is_trivial("aaaaaaaaaaaa"));
        assert!(is_trivial("1234567890"));
        assert!(is_trivial("abcdefghijk"));
        assert!(is_trivial("9876543210"));
        assert!(!is_trivial("correct horse battery"));
        assert!(!is_trivial("Zr7!pQ2mLw9x"));
    }

    #[test]
    fn hashes_use_argon2id_and_verify() {
        let hash = hash_password("Zr7!pQ2mLw9x-long".to_string()).unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(argon2::verify_encoded(&hash, b"Zr7!pQ2mLw9x-long").unwrap());
        assert!(!argon2::verify_encoded(&hash, b"wrong password").unwrap());
    }
}
