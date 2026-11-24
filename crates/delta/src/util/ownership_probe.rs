//! Reading what an applicant put up to prove they control a domain
//! (Verified Server): a DNS TXT record at `_zrzur-verification.<domain>` or a
//! file at `https://<domain>/.well-known/zrzur-verification.txt`.
//!
//! The domain comes from the applicant, so both reads assume the worst:
//! - DNS: a plain UDP query for TXT only, to the host's own resolvers
//!   (`/etc/resolv.conf`), 3 s each, the answer capped at 4 KiB and parsed
//!   by hand (no library: the lockfile stays as it is);
//! - web: https only, the same address rule as signed deliveries (public
//!   addresses only, checked when the name is resolved, so DNS rebinding
//!   finds nothing), no redirects, no proxy, 5 s, the answer capped at 1 KiB.
//!
//! Nothing here logs the domain or what was found; the caller compares the
//! values with the stored hash.

use std::{net::SocketAddr, sync::OnceLock, time::Duration};

use reqwest::{redirect, Client};

use super::signed_delivery::GuardedResolver;

const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const DNS_ANSWER_MAX: usize = 4096;
const WEB_TIMEOUT: Duration = Duration::from_secs(5);
const WEB_ANSWER_MAX: usize = 1024;
const TXT: u16 = 16;

/// A TXT query for `name` with message id `id` (recursion desired)
pub fn txt_query(id: u16, name: &str) -> Option<Vec<u8>> {
    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&TXT.to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes());
    Some(packet)
}

/// Skip a (possibly compressed) name, giving the offset after it
fn skip_name(packet: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let length = *packet.get(at)? as usize;
        if length == 0 {
            return Some(at + 1);
        }
        if length & 0xC0 == 0xC0 {
            packet.get(at + 1)?;
            return Some(at + 2);
        }
        at += 1 + length;
    }
}

fn read_u16(packet: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*packet.get(at)?, *packet.get(at + 1)?]))
}

/// The TXT strings of an answer to query `id` (each record's strings joined).
/// `None` for anything malformed, another id or an error answer.
pub fn parse_txt_answer(id: u16, packet: &[u8]) -> Option<Vec<String>> {
    if packet.len() < 12 || read_u16(packet, 0)? != id {
        return None;
    }
    let flags = read_u16(packet, 2)?;
    // an answer, and no error code
    if flags & 0x8000 == 0 || flags & 0x000F != 0 {
        return None;
    }
    let questions = read_u16(packet, 4)?;
    let answers = read_u16(packet, 6)?;
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(packet, at)? + 4;
    }
    let mut values = vec![];
    for _ in 0..answers {
        at = skip_name(packet, at)?;
        let kind = read_u16(packet, at)?;
        let length = read_u16(packet, at + 8)? as usize;
        let data = packet.get(at + 10..at + 10 + length)?;
        at += 10 + length;
        if kind != TXT {
            continue;
        }
        let mut value = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let part = data[i] as usize;
            value.extend_from_slice(data.get(i + 1..i + 1 + part)?);
            i += 1 + part;
        }
        values.push(String::from_utf8_lossy(&value).to_string());
    }
    Some(values)
}

/// The resolvers in `/etc/resolv.conf` (at most two)
#[cfg_attr(test, allow(dead_code))]
fn resolvers() -> Vec<SocketAddr> {
    std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|address| address.trim().split('%').next()?.parse().ok())
        .map(|ip| SocketAddr::new(ip, 53))
        .take(2)
        .collect()
}

/// TXT values at `name`; empty when nothing could be read. Tests never
/// reach the network: they get what they set, or nothing.
pub async fn lookup_txt(name: &str) -> Vec<String> {
    #[cfg(test)]
    return test_answer(&format!("dns:{name}")).unwrap_or_default();
    #[cfg(not(test))]
    return query_resolvers(name).await;
}

#[cfg_attr(test, allow(dead_code))]
async fn query_resolvers(name: &str) -> Vec<String> {
    for resolver in resolvers() {
        let id: u16 = rand::random();
        let Some(query) = txt_query(id, name) else { return vec![] };
        let bind = if resolver.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let Ok(socket) = tokio::net::UdpSocket::bind(bind).await else { continue };
        if socket.send_to(&query, resolver).await.is_err() {
            continue;
        }
        let mut buffer = vec![0u8; DNS_ANSWER_MAX];
        let Ok(Ok((read, from))) = tokio::time::timeout(DNS_TIMEOUT, socket.recv_from(&mut buffer)).await else {
            continue;
        };
        if from != resolver {
            continue;
        }
        if let Some(values) = parse_txt_answer(id, &buffer[..read]) {
            return values;
        }
    }
    vec![]
}

#[cfg_attr(test, allow(dead_code))]
fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .dns_resolver(GuardedResolver { allow_private: false })
            .redirect(redirect::Policy::none())
            .no_proxy()
            .pool_max_idle_per_host(0)
            .timeout(WEB_TIMEOUT)
            .connect_timeout(Duration::from_secs(2))
            .user_agent("Zrzur-Verification/1")
            .build()
            .expect("reqwest Client")
    })
}

/// The lines of the verification file on `domain`; empty when it could not
/// be read. Tests never reach the network.
pub async fn fetch_website_file(domain: &str, path: &str) -> Vec<String> {
    #[cfg(test)]
    return {
        let _ = path;
        test_answer(&format!("web:{domain}")).unwrap_or_default()
    };
    #[cfg(not(test))]
    return fetch_file(domain, path).await;
}

#[cfg_attr(test, allow(dead_code))]
async fn fetch_file(domain: &str, path: &str) -> Vec<String> {
    let Ok(url) = reqwest::Url::parse(&format!("https://{domain}{path}")) else { return vec![] };
    let Ok(mut response) = client().get(url).send().await else { return vec![] };
    if !response.status().is_success() {
        return vec![];
    }
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        body.extend_from_slice(&chunk);
        if body.len() > WEB_ANSWER_MAX {
            return vec![];
        }
    }
    String::from_utf8_lossy(&body).lines().map(|line| line.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

#[cfg(test)]
static TEST_ANSWERS: std::sync::Mutex<Option<std::collections::HashMap<String, Vec<String>>>> = std::sync::Mutex::new(None);

/// Tests: what `dns:<name>` or `web:<domain>` answers
#[cfg(test)]
pub fn set_test_answer(key: &str, values: Vec<String>) {
    TEST_ANSWERS.lock().unwrap().get_or_insert_with(Default::default).insert(key.to_string(), values);
}

#[cfg(test)]
fn test_answer(key: &str) -> Option<Vec<String>> {
    TEST_ANSWERS.lock().unwrap().as_ref().and_then(|answers| answers.get(key).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer to `query` with the given TXT records (each one string)
    fn answer(query: &[u8], records: &[&str], rcode: u8) -> Vec<u8> {
        let mut packet = query.to_vec();
        packet[2] = 0x81;
        packet[3] = 0x80 | rcode;
        packet[6..8].copy_from_slice(&(records.len() as u16).to_be_bytes());
        for record in records {
            packet.extend_from_slice(&[0xC0, 12]);
            packet.extend_from_slice(&TXT.to_be_bytes());
            packet.extend_from_slice(&1u16.to_be_bytes());
            packet.extend_from_slice(&300u32.to_be_bytes());
            packet.extend_from_slice(&((record.len() + 1) as u16).to_be_bytes());
            packet.push(record.len() as u8);
            packet.extend_from_slice(record.as_bytes());
        }
        packet
    }

    #[test]
    fn a_txt_answer_is_read_and_nothing_else_is() {
        let query = txt_query(7, "_zrzur-verification.example.org").unwrap();
        assert_eq!(&query[12..14], &[19, b'_']);
        let values = parse_txt_answer(7, &answer(&query, &["zrzur-verification=abc", "v=spf1 -all"], 0)).unwrap();
        assert_eq!(values, vec!["zrzur-verification=abc", "v=spf1 -all"]);
        // another id, an error answer, a cut-off packet: nothing
        assert!(parse_txt_answer(8, &answer(&query, &["x"], 0)).is_none());
        assert!(parse_txt_answer(7, &answer(&query, &["x"], 3)).is_none());
        let full = answer(&query, &["zrzur-verification=abc"], 0);
        assert!(parse_txt_answer(7, &full[..full.len() - 3]).is_none());
        // a query is not an answer
        assert!(parse_txt_answer(7, &query).is_none());
        // labels over 63 characters are never sent
        assert!(txt_query(1, &format!("{}.org", "a".repeat(64))).is_none());
    }
}
