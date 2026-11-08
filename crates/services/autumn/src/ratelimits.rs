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
        assert_eq!(limit, 10);
        // another webhook has a bucket of its own
        assert_eq!(bucket(Method::POST, "/webhooks/01OTHER/attachments").1.as_deref(), Some("01OTHER"));
    }

    #[test]
    fn person_uploads_keep_their_limit_and_downloads_stay_free() {
        assert_eq!(bucket(Method::POST, "/attachments"), ("upload".into(), Some("attachments".into()), 10));
        assert_eq!(bucket(Method::GET, "/attachments/abc").2, u32::MAX);
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
            _ => ("any_atmn", None),
        }
    }

    fn resolve_bucket_limit(&self, bucket: &str) -> u32 {
        match bucket {
            "upload" => 10,
            "webhook_upload" => 10,
            _ => u32::MAX,
        }
    }
}
