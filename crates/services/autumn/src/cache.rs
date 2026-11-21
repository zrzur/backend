//! What Cloudflare and browsers may keep. Cloudflare follows these headers
//! (its rules only choose which paths are eligible), and a purge reaches
//! Cloudflare only, so browsers keep files for an hour and the edge for
//! longer. Anything not marked otherwise is not stored at all.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};

/// Drafts, private designs, errors and the moderation route
pub static PRIVATE_CACHE_CONTROL: &str = "private, no-store";

/// Until purge is configured nothing stays at the edge for over an hour
static UNPURGEABLE: &str = "public, max-age=3600, s-maxage=3600";
static PUBLIC_IMAGE: &str = "public, max-age=3600, s-maxage=2592000";
static PUBLIC_ATTACHMENT: &str = "public, max-age=3600, s-maxage=86400";
/// Designs show on their own for people who did not ask to see them, so a
/// taken-down file is gone within minutes even where a purge fails
static PUBLIC_EFFECT: &str = "public, max-age=300, s-maxage=300";

/// `Cache-Control` of a public file. Images change link when they change,
/// so they stay longest; attachments stay a day.
pub fn public_cache_control(tag: &str, purge_configured: bool) -> &'static str {
    match (tag, purge_configured) {
        ("effects" | "decorations", _) => PUBLIC_EFFECT,
        (_, false) => UNPURGEABLE,
        ("attachments", true) => PUBLIC_ATTACHMENT,
        (_, true) => PUBLIC_IMAGE,
    }
}

/// `Cache-Control` of a served file
pub fn file_cache_control(tag: &str, private: bool, purge_configured: bool) -> &'static str {
    if private {
        PRIVATE_CACHE_CONTROL
    } else {
        public_cache_control(tag, purge_configured)
    }
}

/// Validator of a stored file. The content hash names the bytes, so the hash
/// and the variant (original, preview, small preview, per tag: tags render
/// previews differently) are enough. A browser whose copy is older than its
/// `max-age` asks again with `If-None-Match` and gets a 304 without the file.
pub fn file_etag(hash_id: &str, variant: &str) -> String {
    format!("\"{variant}-{hash_id}\"")
}

/// The request already holds this version: `If-None-Match` lists the
/// validator (weak or strong, the comparison is weak as RFC 9110 asks for
/// GET) or is `*`
pub fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    let bare = etag.trim_start_matches("W/");
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate.trim_start_matches("W/") == bare)
}

/// 304 for a request that already holds this version: no body, the same
/// validator and caching as the full answer
pub fn not_modified_response(etag: &str, cache_control: &str) -> Response {
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::empty())
        .expect("valid headers")
}

/// Last step of every answer: no cookies on files, and an answer that
/// doesn't say it may be stored (errors, redirects) is not stored
pub async fn seal_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.remove(header::SET_COOKIE);
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(PRIVATE_CACHE_CONTROL),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
    };

    use super::*;

    #[test]
    fn effects_leave_every_cache_within_minutes() {
        for purge in [true, false] {
            assert_eq!(public_cache_control("effects", purge), "public, max-age=300, s-maxage=300");
            assert_eq!(file_cache_control("effects", true, purge), "private, no-store");
        }
    }

    #[test]
    fn a_design_taken_down_leaves_every_cache_within_minutes() {
        for purge in [true, false] {
            assert_eq!(
                public_cache_control("decorations", purge),
                "public, max-age=300, s-maxage=300"
            );
        }
    }

    #[test]
    fn public_images_stay_a_month_and_attachments_a_day() {
        for tag in ["avatars", "icons", "banners", "backgrounds", "emojis"] {
            assert_eq!(
                public_cache_control(tag, true),
                "public, max-age=3600, s-maxage=2592000",
                "{tag}"
            );
        }
        assert_eq!(
            public_cache_control("attachments", true),
            "public, max-age=3600, s-maxage=86400"
        );
    }

    #[test]
    fn without_purge_nothing_stays_over_an_hour() {
        for tag in ["avatars", "attachments"] {
            assert_eq!(
                public_cache_control(tag, false),
                "public, max-age=3600, s-maxage=3600",
                "{tag}"
            );
        }
    }

    #[test]
    fn private_files_are_never_stored() {
        for purge in [true, false] {
            assert_eq!(file_cache_control("decorations", true, purge), "private, no-store");
        }
        assert_eq!(
            file_cache_control("avatars", false, true),
            "public, max-age=3600, s-maxage=2592000"
        );
    }

    #[tokio::test]
    async fn errors_and_redirects_are_not_stored() {
        for response in [
            StatusCode::NOT_FOUND.into_response(),
            axum::response::Redirect::permanent("/x").into_response(),
        ] {
            let sealed = seal_response(response).await;
            assert_eq!(sealed.headers()[header::CACHE_CONTROL], "private, no-store");
        }
    }

    #[test]
    fn an_etag_names_the_content_and_the_variant() {
        assert_eq!(file_etag("abc", "o"), "\"o-abc\"");
        assert_ne!(file_etag("abc", "p-avatars"), file_etag("abc", "s-avatars"));
        assert_ne!(file_etag("abc", "p-avatars"), file_etag("abc", "p-icons"));
        assert_ne!(file_etag("abc", "o"), file_etag("abd", "o"));
    }

    fn asking(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::IF_NONE_MATCH, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn if_none_match_is_compared_weakly_in_lists_and_with_a_star() {
        let tag = file_etag("abc", "o");
        assert!(not_modified(&asking(&[&tag]), &tag));
        assert!(not_modified(&asking(&[&format!("W/{tag}")]), &tag));
        assert!(not_modified(&asking(&[&format!("\"x\", {tag}")]), &tag));
        assert!(not_modified(&asking(&["\"x\"", &tag]), &tag));
        assert!(not_modified(&asking(&["*"]), &tag));
        assert!(!not_modified(&asking(&[]), &tag));
        assert!(!not_modified(&asking(&["\"o-abd\""]), &tag));
        assert!(!not_modified(&asking(&["\"p-abc\""]), &tag));
    }

    #[test]
    fn a_304_has_no_body_and_the_same_validator_and_caching() {
        let tag = file_etag("abc", "o");
        let response = not_modified_response(&tag, public_cache_control("avatars", true));
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], tag.as_str());
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=3600, s-maxage=2592000"
        );
    }

    #[tokio::test]
    async fn a_stated_cache_header_stays_and_cookies_go() {
        let response = Response::builder()
            .header(header::CACHE_CONTROL, public_cache_control("avatars", true))
            .header(header::SET_COOKIE, "a=b")
            .body(Body::empty())
            .unwrap();
        let sealed = seal_response(response).await;
        assert_eq!(
            sealed.headers()[header::CACHE_CONTROL],
            "public, max-age=3600, s-maxage=2592000"
        );
        assert!(!sealed.headers().contains_key(header::SET_COOKIE));
    }
}
