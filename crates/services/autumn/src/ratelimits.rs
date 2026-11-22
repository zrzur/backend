use axum::http::{request::Parts, Method};
use revolt_ratelimits::ratelimiter::RatelimitResolver;

#[cfg(test)]
mod tests {
    use super::AutumnRatelimits;
    use axum::http::{Method, Request};
    use revolt_ratelimits::ratelimiter::RatelimitResolver;

    fn bucket(method: Method, path: &str) -> (String, Option<String>, u32) {
        let (parts, _) = Request::builder().method(method).uri(path).body(()).unwrap().into_parts();
        let (bucket, resource) = AutumnRatelimits.resolve_bucket(&parts);
        (bucket.to_string(), resource.map(str::to_string), AutumnRatelimits.resolve_bucket_limit(bucket))
    }

    #[test]
    fn webhook_uploads_have_a_limit() {
        let (name, resource, limit) = bucket(Method::POST, "/webhooks/01HOOK/attachments");
        assert_eq!(name, "webhook_upload");
        assert_eq!(resource.as_deref(), Some("01HOOK"));
        assert_eq!(limit, 125);
        // another webhook has a bucket of its own
        assert_eq!(bucket(Method::POST, "/webhooks/01OTHER/attachments").1.as_deref(), Some("01OTHER"));
    }

    #[test]
    fn person_uploads_keep_their_limit_and_downloads_stay_free() {
        assert_eq!(bucket(Method::POST, "/attachments"), ("upload".into(), Some("attachments".into()), 10));
        assert_eq!(bucket(Method::GET, "/attachments/abc").2, u32::MAX);
    }

    #[test]
    fn aac_copies_have_a_limit_of_their_own() {
        assert_eq!(
            bucket(Method::GET, "/attachments/abc/voice-message-1.ogg?format=m4a"),
            ("aac_copy".into(), None, 60)
        );
        assert_eq!(bucket(Method::GET, "/attachments/abc/original?format=m4a").0, "aac_copy");
        // the same file without the question: free, as every download
        assert_eq!(bucket(Method::GET, "/attachments/abc/voice-message-1.ogg").2, u32::MAX);
    }
}

pub struct AutumnRatelimits;

impl RatelimitResolver<Parts> for AutumnRatelimits {
    fn resolve_bucket<'a>(&self, parts: &'a Parts) -> (&'a str, Option<&'a str>) {
        let path = parts
            .uri
            .path()
            .trim_matches('/')
            .split_terminator("/")
            .collect::<Vec<&str>>();

        match (&parts.method, path.as_slice()) {
            (&Method::POST, &[tag]) => ("upload", Some(tag)),
            // a webhook's files: counted per webhook (and address), never unlimited
            (&Method::POST, &["webhooks", webhook, "attachments"]) => ("webhook_upload", Some(webhook)),
            // the AAC copy of a voice message may have to be made: counted
            // per address (or account), whatever the file
            (&Method::GET, &[_, _, _])
                if parts
                    .uri
                    .query()
                    .is_some_and(|query| query.split('&').any(|pair| pair.starts_with("format="))) =>
            {
                ("aac_copy", None)
            }
            _ => ("any_atmn", None),
        }
    }

    fn resolve_bucket_limit(&self, bucket: &str) -> u32 {
        match bucket {
            "upload" => 10,
            // delta lets a webhook send 5 messages in 2 s with 5 files each:
            // 125 files in a 10 s window, so delta's own forwarding never hits this
            "webhook_upload" => 125,
            // a conversation full of voice messages on an older iPhone: each
            // one is a few range requests and one read for the waveform
            "aac_copy" => 60,
            _ => u32::MAX,
        }
    }
}
