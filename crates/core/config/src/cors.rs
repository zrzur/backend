/// The Zrzur sites and app. Never `*`: a browser on any other site gets no
/// answer it may read from the API, the file server or the GIF service.
const ZRZUR_ORIGINS: [&str; 8] = [
    "https://zrzur.com",
    "https://www.zrzur.com",
    "https://app.zrzur.com",
    "https://beta.zrzur.com",
    "https://discover.zrzur.com",
    "https://designs.zrzur.com",
    "https://discover-beta.zrzur.com",
    "https://designs-beta.zrzur.com",
];

/// Local addresses as patterns, for libraries that take them
pub const LOCAL_ORIGIN_PATTERNS: [&str; 2] = [
    r"^http://localhost(:\d+)?$",
    r"^http://127\.0\.0\.1(:\d+)?$",
];

/// Websites that may call a Zrzur service from a browser (S-02): the Zrzur
/// sites and app, this server's own web app, the admin console
/// (`api.security.staff_origins`), extra origins from `ZRZUR_CORS_ORIGINS`
/// (comma separated), and localhost when the app is served over plain http
/// (development).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CorsOrigins {
    /// Allowed as written
    pub exact: Vec<String>,
    /// Any port on localhost and 127.0.0.1 too
    pub local: bool,
}

impl CorsOrigins {
    pub fn new(app_host: &str, staff_origins: &[String], extra: Option<&str>) -> Self {
        let mut exact: Vec<String> = ZRZUR_ORIGINS.iter().map(|s| s.to_string()).collect();
        let app = app_host.trim_end_matches('/');
        if !app.is_empty() {
            exact.push(app.to_string());
        }
        exact.extend(staff_origins.iter().cloned());
        exact.extend(
            extra
                .unwrap_or_default()
                .split(',')
                .map(|o| o.trim().trim_end_matches('/'))
                .filter(|o| o.starts_with("https://") || o.starts_with("http://"))
                .map(String::from),
        );
        Self {
            exact,
            local: app.starts_with("http://"),
        }
    }

    /// The list for this server, from its settings and `ZRZUR_CORS_ORIGINS`
    pub fn from_settings(app_host: &str, staff_origins: &[String]) -> Self {
        let extra = std::env::var("ZRZUR_CORS_ORIGINS").ok();
        Self::new(app_host, staff_origins, extra.as_deref())
    }

    /// Whether a browser's `Origin` header is on the list
    pub fn allows(&self, origin: &str) -> bool {
        self.exact.iter().any(|allowed| allowed == origin) || (self.local && is_local(origin))
    }
}

fn is_local(origin: &str) -> bool {
    ["http://localhost", "http://127.0.0.1"].iter().any(|host| {
        origin.strip_prefix(host).is_some_and(|rest| {
            rest.is_empty()
                || rest.strip_prefix(':').is_some_and(|port| {
                    !port.is_empty() && port.chars().all(|c| c.is_ascii_digit())
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::CorsOrigins;

    #[test]
    fn production_lists_the_zrzur_sites_and_nothing_else() {
        let origins = CorsOrigins::new("https://app.zrzur.com/", &[], None);
        assert!(origins.allows("https://discover.zrzur.com"));
        assert!(origins.allows("https://app.zrzur.com"));
        assert!(!origins.allows("https://evil.example"));
        assert!(!origins.allows("https://zrzur.com.evil.example"));
        assert!(!origins.allows("http://zrzur.com"));
        assert!(!origins.allows("http://localhost:3000"));
        assert!(!origins.allows("null"));
    }

    #[test]
    fn local_addresses_only_when_the_app_is_on_plain_http() {
        let origins = CorsOrigins::new("http://127.0.0.1:31060", &[], None);
        assert!(origins.allows("http://localhost:5173"));
        assert!(origins.allows("http://127.0.0.1"));
        assert!(!origins.allows("http://localhost.evil.example"));
        assert!(!origins.allows("http://localhost:"));
        assert!(!origins.allows("http://127.0.0.1:80.evil.example"));
    }

    #[test]
    fn the_app_host_the_admin_console_and_the_extra_list_count() {
        let staff = ["https://admin.zrzur.com".to_string()];
        let extra = " https://one.example/ , not-an-origin,http://two.example";
        let origins = CorsOrigins::new("https://chat.example", &staff, Some(extra));
        for origin in [
            "https://chat.example",
            "https://admin.zrzur.com",
            "https://one.example",
            "http://two.example",
        ] {
            assert!(origins.allows(origin), "{origin}");
        }
        assert!(!origins.allows("not-an-origin"));
    }
}
