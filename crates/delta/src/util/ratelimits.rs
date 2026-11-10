use revolt_ratelimits::ratelimiter::RatelimitResolver;
use rocket::{http::Method, Request};

pub struct DeltaRatelimits {
    /// `api.security.ratelimits.auth`
    pub auth_limit: u32,
}

/// Header the public-site servers (Discover, Designs) send with the shared
/// secret from `ZRZUR_INTERNAL_TOKEN`, so their server-side rendering does
/// not share the visitors' per-address buckets.
const INTERNAL_HEADER: &str = "x-zrzur-internal";

/// True if the request carries the configured internal secret
fn is_internal(request: &Request<'_>) -> bool {
    let Ok(secret) = std::env::var("ZRZUR_INTERNAL_TOKEN") else {
        return false;
    };
    let Some(given) = request.headers().get_one(INTERNAL_HEADER) else {
        return false;
    };
    // compare without stopping at the first difference
    secret.len() >= 16
        && secret.len() == given.len()
        && secret
            .bytes()
            .zip(given.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

impl<'a> RatelimitResolver<Request<'a>> for DeltaRatelimits {
    fn resolve_bucket<'r>(&self, request: &'r Request<'_>) -> (&'r str, Option<&'r str>) {
        if is_internal(request) && request.method() == Method::Get {
            return ("internal_sites", None);
        }

        let (segment, resource, extra) = (
            request.routed_segment(0),
            request.routed_segment(1),
            request.routed_segment(2),
        );

        if let Some(segment) = segment {
            #[allow(clippy::redundant_locals)]
            let resource = resource;

            let method = request.method();
            match (segment, resource, method) {
                ("users", target, Method::Patch) => ("user_edit", target),
                ("users", _, _) => {
                    if let Some("default_avatar") = extra {
                        return ("default_avatar", None);
                    }

                    ("users", None)
                }
                // these make the server call out, or change a secret
                ("bots", Some(id), _) if extra == Some("interactions") => {
                    ("bot_endpoints", Some(id))
                }
                ("bots", _, _) => ("bots", None),
                // Interactions: users creating them share one bucket, autocomplete
                // (one request per pause in typing) has its own, and a bot's
                // answers and edits are limited per interaction
                ("interactions", Some("autocomplete"), _) => ("interactions_autocomplete", None),
                ("interactions", Some(id), _) => ("interaction_callbacks", Some(id)),
                ("interactions", _, _) => ("interactions", None),
                ("channels", Some(id), _) => {
                    if request.method() == Method::Post {
                        if let Some("messages") = extra {
                            return ("messaging", Some(id));
                        }
                        if let Some("typing") = extra {
                            return ("typing", Some(id));
                        }
                    }

                    ("channels", Some(id))
                }
                // member lists are paged and searched: their own bucket, so
                // scrolling a big list doesn't block server actions
                ("servers", Some(id), Method::Get)
                    if matches!(extra, Some("members" | "members_experimental_query")) =>
                {
                    ("member_list", Some(id))
                }
                // server-wide message search reads many channels at once: one
                // small bucket per person, shared by every server, apart from
                // the server actions
                ("servers", Some(_), Method::Post) if extra == Some("messages") => {
                    ("message_search", None)
                }
                // scheduled events: reading the list (and the interested
                // list) is common, marking "Interested" a little less;
                // neither should hold up server actions
                ("servers", Some(_), Method::Get) if extra == Some("events") => {
                    ("server_events_read", None)
                }
                ("servers", Some(id), _) if extra == Some("events") => {
                    ("server_events", Some(id))
                }
                ("servers", Some(id), _) => ("servers", Some(id)),
                ("auth", _, _) => {
                    if request.method() == Method::Delete {
                        ("auth_delete", None)
                    } else {
                        ("auth", None)
                    }
                }
                ("swagger", _, _) => ("swagger", None),
                ("discover", _, _) => ("discover", None),
                // Creators submitting or revising designs
                ("community", Some("decorations" | "nameplates"), Method::Post) => ("community_submit", None),
                ("community", Some("items"), Method::Patch) => ("community_submit", None),
                ("community", _, _) => ("community", None),
                // payment provider callbacks must never queue behind user purchases
                ("billing", Some("webhooks"), Method::Post) => ("billing_webhook", None),
                ("billing", _, Method::Get) => ("billing_read", None),
                ("billing", _, _) => ("billing", None),
                // Posting through a webhook has limits per webhook and per
                // channel in the route; this one only stops floods from one
                // address, which may serve many webhooks (a CI, a forge)
                ("webhooks", Some(_), Method::Post) if extra.is_some() => ("webhook_send", None),
                ("safety", Some("report"), _) => ("safety_report", Some("report")),
                // new reports and appeals share the strict bucket
                ("safety", Some("reports"), Method::Post) => ("safety_report", Some("report")),
                ("safety", Some("standing"), Method::Post) => ("safety_report", Some("report")),
                ("safety", Some("admin"), _) => ("safety_admin", None),
                ("safety", _, _) => ("safety", None),
                // platform staff: signing in and stepping up are strict
                ("staff", Some("session"), Method::Post) => ("staff_auth", None),
                ("staff", _, _) => ("staff", None),
                // plugins: own buckets so store traffic never slows chat start-up
                ("plugins", _, Method::Post) => ("plugins_write", None),
                ("plugins", _, _) => ("plugins", None),
                _ => ("any_dlta", None),
            }
        } else {
            ("any_dlta", None)
        }
    }

    fn resolve_bucket_limit(&self, bucket: &str) -> u32 {
        match bucket {
            // server-side rendering of the public sites (cached there)
            "internal_sites" => 600,
            "user_edit" => 2,
            "users" => 20,
            "bots" => 10,
            "bot_endpoints" => 3,
            "interactions" => 15,
            "interactions_autocomplete" => 30,
            "interaction_callbacks" => 10,
            "messaging" => 10,
            "typing" => 5,
            "webhook_send" => 100,
            "channels" => 15,
            "servers" => 5,
            "member_list" => 15,
            "message_search" => 5,
            "server_events_read" => 20,
            "server_events" => 10,
            "auth" => self.auth_limit,
            "auth_delete" => 255,
            "default_avatar" => 255,
            "swagger" => 100,
            // Public browsing, also from the standalone Discover website
            "discover" => 40,
            // Gallery browsing, uploads and reports
            "community" => 30,
            "community_submit" => 5,
            // Plus, supercharges and Discover promotion: purchases and
            // changes are rare, reading the plans and stats less so
            "billing" => 8,
            "billing_read" => 30,
            "billing_webhook" => 120,
            "safety" => 15,
            "safety_report" => 3,
            "safety_admin" => 60,
            "staff_auth" => 5,
            "staff" => 60,
            "plugins" => 40,
            "plugins_write" => 10,
            _ => 20,
        }
    }
}

#[cfg(test)]
mod tests {
    use revolt_ratelimits::ratelimiter::{RatelimitInformation, Ratelimiter};

    #[test]
    fn a_rate_limit_says_when_to_retry_and_has_no_type() {
        let body = serde_json::to_value(RatelimitInformation::failure(&Ratelimiter {
            key: 1,
            limit: 10,
            remaining: 0,
            reset: 1500,
            global: true,
        }))
        .unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "retry_after": 1500, "global": true })
        );
    }
}

#[cfg(test)]
mod test {
    use rocket::http::{Header, Status};

    use crate::util::test::TestHarness;

    /// What a browser sends before a cross-site call
    async fn preflight<'c>(
        harness: &'c TestHarness,
        path: &'static str,
    ) -> rocket::local::asynchronous::LocalResponse<'c> {
        harness
            .client
            .options(path)
            .header(Header::new("origin", "https://app.zrzur.com"))
            .header(Header::new("access-control-request-method", "POST"))
            .header(Header::new("access-control-request-headers", "content-type,x-session-token"))
            .dispatch()
            .await
    }

    /// The default stays strict whatever a test stack sets
    #[rocket::async_test]
    async fn auth_limit_defaults_to_fifteen() {
        assert_eq!(revolt_config::config().await.api.security.ratelimits.auth, 15);
    }

    /// A preflight must never meet the limiter: it would use up the bucket of
    /// the call it announces, and a 429 here carries no CORS headers, so the
    /// browser reports a failed fetch instead of a limit.
    #[rocket::async_test]
    async fn preflight_is_not_rate_limited() {
        revolt_config::overwrite_config(|config| {
            config.hosts.app = "https://app.zrzur.com".to_string();
        })
        .await;
        let harness = TestHarness::new().await;

        for path in ["/auth/account/", "/auth/session/login", "/users/@me"] {
            let res = preflight(&harness, path).await;
            assert!(res.status().class().is_success(), "{path}: {}", res.status());
            assert_eq!(
                res.headers().get_one("access-control-allow-origin"),
                Some("https://app.zrzur.com"),
                "{path}"
            );
            assert_eq!(res.headers().get_one("x-ratelimit-bucket"), None, "{path}");
        }

        // and a real call is still counted
        let res = harness
            .client
            .post("/auth/session/login")
            .header(Header::new("content-type", "application/json"))
            .body("{}")
            .dispatch()
            .await;
        assert_ne!(res.status(), Status::NotFound);
        assert!(res.headers().get_one("x-ratelimit-bucket").is_some());
    }
}
