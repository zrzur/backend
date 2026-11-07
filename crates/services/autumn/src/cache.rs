//! What Cloudflare and browsers may keep. Cloudflare follows these headers
//! (its rules only choose which paths are eligible), and a purge reaches
//! Cloudflare only, so browsers keep files for an hour and the edge for
//! longer. Anything not marked otherwise is not stored at all.

use axum::{
    http::{HeaderValue, header},
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
