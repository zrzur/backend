//! Calling a server someone else named, with a signed request.
//!
//! The address comes from a third party, so every request here assumes the
//! worst: same address rule as the link proxy, checked again when the name
//! is resolved (DNS rebinding), no redirects, no proxy, short timeout, capped
//! answer. Nothing logs the address or the secret. Any feature that delivers
//! to third parties builds on this; `interaction_endpoint` is the first.

use std::{
    net::{IpAddr, SocketAddr},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{
    dns::{Addrs, Name, Resolve, Resolving},
    redirect, Client,
};
use revolt_config::net::is_public_ip;
use revolt_database::util::signed_request::{
    sign, HEADER_DELIVERY, HEADER_SIGNATURE, HEADER_TIMESTAMP,
};
use revolt_result::{create_error, Error};
use url::{Host, Url};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_ANSWER_BYTES: usize = 1024 * 1024;
const MAX_URL_LENGTH: usize = 2048;

/// Why an address or its answer was refused; the code is the error's `reason`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    InvalidUrl,
    BlockedAddress,
    NotReachable,
    TimedOut,
    BadStatus,
    BadAnswer,
    AnswerTooLarge,
    AcceptsBadSignatures,
}

impl Problem {
    pub fn code(self) -> &'static str {
        match self {
            Problem::InvalidUrl => "invalid_url",
            Problem::BlockedAddress => "blocked_address",
            Problem::NotReachable => "not_reachable",
            Problem::TimedOut => "timed_out",
            Problem::BadStatus => "bad_status",
            Problem::BadAnswer => "bad_answer",
            Problem::AnswerTooLarge => "answer_too_large",
            Problem::AcceptsBadSignatures => "accepts_bad_signatures",
        }
    }
}

impl From<Problem> for Error {
    fn from(problem: Problem) -> Error {
        create_error!(SignedDeliveryRejected {
            reason: problem.code().to_string()
        })
    }
}

/// Check an address before anything connects to it. Names are checked
/// again, resolved, by [`GuardedResolver`].
pub fn parse_target(
    raw: &str,
    allow_private: bool,
    blocked_domains: &[String],
) -> Result<Url, Problem> {
    if raw.len() > MAX_URL_LENGTH {
        return Err(Problem::InvalidUrl);
    }
    let url = Url::parse(raw).map_err(|_| Problem::InvalidUrl)?;
    let scheme_ok = url.scheme() == "https" || (allow_private && url.scheme() == "http");
    // credentials and fragments in an address are never right here
    if !scheme_ok || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(Problem::InvalidUrl);
    }
    let public = |ip: IpAddr| allow_private || is_public_ip(&ip);
    match url.host() {
        Some(Host::Ipv4(ip)) if public(ip.into()) => Ok(url),
        Some(Host::Ipv6(ip)) if public(ip.into()) => Ok(url),
        Some(Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            let bare_name = !domain.contains('.');
            if !allow_private && (bare_name || blocked_domains.contains(&domain)) {
                return Err(Problem::BlockedAddress);
            }
            Ok(url)
        }
        Some(_) => Err(Problem::BlockedAddress),
        None => Err(Problem::InvalidUrl),
    }
}

/// Only ever hands out addresses that may be reached, whatever the name says
/// at this moment: this is what closes DNS rebinding.
struct GuardedResolver {
    allow_private: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let lookup = format!("{}:0", name.as_str());
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host(lookup)
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?
                .filter(|address| allow_private || is_public_ip(&address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "address not allowed",
                )) as Box<dyn std::error::Error + Send + Sync>);
            }
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

fn client(allow_private: bool) -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .dns_resolver(GuardedResolver { allow_private })
            .redirect(redirect::Policy::none())
            .no_proxy()
            // every request looks the name up again, so a name that turns
            // inside later is caught, and never rides on an old connection
            .pool_max_idle_per_host(0)
            .timeout(TOTAL_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent("Zrzur-Signed-Delivery/1")
            .build()
            .expect("reqwest Client")
    })
}

/// What the other server answered
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

/// One signed POST. Errors hold no address: they may end up in logs.
pub async fn post(
    url: &Url,
    secret: &str,
    delivery: &str,
    body: &[u8],
    allow_private: bool,
) -> Result<Answer, Problem> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs() as i64)
        .unwrap_or_default();
    let mut response = client(allow_private)
        .post(url.as_str())
        .header("Content-Type", "application/json")
        .header(HEADER_TIMESTAMP, timestamp.to_string())
        .header(HEADER_SIGNATURE, sign(secret, timestamp, body))
        .header(HEADER_DELIVERY, delivery)
        .body(body.to_vec())
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                Problem::TimedOut
            } else {
                Problem::NotReachable
            }
        })?;

    let status = response.status().as_u16();
    let mut answer = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Problem::NotReachable)? {
        if answer.len() + chunk.len() > MAX_ANSWER_BYTES {
            return Err(Problem::AnswerTooLarge);
        }
        answer.extend_from_slice(&chunk);
    }
    Ok(Answer { status, body: answer })
}

#[cfg(test)]
mod tests {
    use super::{parse_target, GuardedResolver, Problem};

    fn check(url: &str) -> Result<(), Problem> {
        parse_target(url, false, &["blocked.example".to_string()]).map(|_| ())
    }

    #[test]
    fn only_clean_public_https_addresses_pass() {
        assert_eq!(check("https://bot.example.com/interactions"), Ok(()));
        assert_eq!(check("https://bot.example.com:8443/x?y=1"), Ok(()));
        assert_eq!(check("https://1.1.1.1/x"), Ok(()));

        for bad in [
            "http://bot.example.com/x",
            "ftp://bot.example.com/x",
            "https://user:pass@bot.example.com/x",
            "https://bot.example.com/x#fragment",
            "not a url",
            "",
        ] {
            assert_eq!(check(bad), Err(Problem::InvalidUrl), "{bad}");
        }
        assert_eq!(check(&format!("https://a.example/{}", "x".repeat(2050))), Err(Problem::InvalidUrl));
    }

    #[test]
    fn inside_addresses_are_refused_in_every_spelling() {
        for bad in [
            "https://127.0.0.1/x",
            "https://10.0.0.1/x",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/x",
            "https://[::ffff:127.0.0.1]/x",
            "https://[fd00::1]/x",
            // other ways to write 127.0.0.1
            "https://2130706433/x",
            "https://0x7f.0.0.1/x",
            "https://0177.0.0.1/x",
            "https://localhost/x",
            "https://intranet/x",
            "https://blocked.example/x",
            "https://BLOCKED.example./x",
        ] {
            assert_eq!(check(bad), Err(Problem::BlockedAddress), "{bad}");
        }
    }

    #[test]
    fn private_addresses_only_with_the_private_network_switch() {
        assert!(parse_target("http://127.0.0.1:9000/x", true, &[]).is_ok());
        assert!(parse_target("http://127.0.0.1:9000/x", false, &[]).is_err());
    }

    /// A name that points inside is stopped when it is resolved, too
    #[rocket::async_test]
    async fn a_name_resolving_inside_is_not_handed_out() {
        use reqwest::dns::{Name, Resolve};
        use std::str::FromStr;
        let resolver = GuardedResolver { allow_private: false };
        assert!(resolver.resolve(Name::from_str("localhost").unwrap()).await.is_err());
        let resolver = GuardedResolver { allow_private: true };
        assert!(resolver.resolve(Name::from_str("localhost").unwrap()).await.is_ok());
    }
}
