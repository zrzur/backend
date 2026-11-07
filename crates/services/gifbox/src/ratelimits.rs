use axum::http::request::Parts;
use revolt_ratelimits::ratelimiter::RatelimitResolver;

pub struct GifboxRatelimits;

impl RatelimitResolver<Parts> for GifboxRatelimits {
    fn resolve_bucket<'a>(&self, parts: &'a Parts) -> (&'a str, Option<&'a str>) {
        let mut segments = parts.uri.path().trim_matches('/').split_terminator("/");
        let path = segments.next();

        match path {
            Some("klipy") => match segments.next() {
                Some("media") => ("klipy_media", None),
                _ => ("klipy", None),
            },
            Some("categories") => ("categories", None),
            Some("trending") => ("trending", None),
            Some("search") => ("search", None),
            _ => ("any_gfbx", None),
        }
    }

    fn resolve_bucket_limit(&self, bucket: &str) -> u32 {
        match bucket {
            "categories" => 2,
            "trending" => 5,
            "search" => 10,
            "klipy" => 20,
            "klipy_media" => 300,
            _ => u32::MAX,
        }
    }
}
