//! Custom invite link names kept for Zrzur itself, so nobody can take a
//! name that looks official or that the project needs (zrzur.cc/<name>).

/// Whole names kept for the project
const RESERVED: &[&str] = &[
    // brand, old spellings and look-alikes
    "zrzur", "zarzour", "zarzur", "zarzoor", "zrzour", "zrzoor", "zurzur", "starling",
    "starlings",
    // product terms (Feather, Wings, Discover tiers, lobby, perks)
    "feather", "feathers", "wing", "wings", "featured", "spotlight", "headline", "top-row",
    "lobby", "lobbies", "plus", "premium", "pro", "boost", "boosts", "supercharge",
    "supercharges", "nitro", "gift", "gifts", "badge", "badges", "emoji", "emojis",
    "sticker", "stickers", "theme", "themes", "plugin", "plugins", "bot", "bots",
    "webhook", "webhooks", "community", "communities", "creator", "creators", "studio",
    "design", "designs", "designer", "marketplace", "market", "library", "templates",
    "template",
    // pages, services and routes
    "invite", "invites", "app", "apps", "api", "ws", "cdn", "proxy", "gif", "gifs",
    "media", "files", "file", "upload", "uploads", "voice", "video", "call", "calls",
    "updates", "update", "download", "downloads", "desktop", "android", "ios", "mobile",
    "apk", "windows", "mac", "linux", "web", "www", "mail", "email", "smtp", "assets",
    "static", "docs", "documentation", "developer", "developers", "dev", "devs", "sdk",
    "status", "discover", "discovery", "explore", "search", "share", "link", "links",
    "go", "url", "home", "server", "servers", "channel", "channels", "dm", "dms", "group",
    "groups", "friends", "notifications", "inbox", "me", "profile", "profiles", "user",
    "users", "account", "accounts", "settings", "auth", "oauth", "sso", "login", "logout",
    "signin", "signup", "register", "verify", "beta", "alpha", "canary", "nightly",
    "staging", "preview", "sandbox", "demo", "test", "testing",
    // money
    "billing", "pay", "payment", "payments", "checkout", "shop", "store", "pricing",
    "subscribe", "subscription", "subscriptions", "donate", "wallet",
    // legal, help and trust
    "legal", "terms", "tos", "privacy", "policy", "policies", "cookies", "security",
    "safety", "trust", "abuse", "report", "reports", "appeal", "appeals", "dmca",
    "copyright", "license", "licence", "guidelines", "rules", "about", "contact", "press",
    "media-kit", "careers", "jobs", "blog", "news", "announcements", "changelog",
    "roadmap", "brand", "partners", "partner", "help", "helpdesk", "faq", "support",
    "feedback", "official", "verified", "staff", "team", "admin", "admins",
    "administrator", "mod", "mods", "moderator", "moderators", "owner", "founder",
    "system", "everyone", "here", "root", "null", "undefined", "anonymous", "unknown",
    "error",
    // other platforms (no impersonation)
    "discord", "stoat", "revolt", "guilded", "telegram", "whatsapp", "signal", "slack",
    "teams", "skype", "matrix", "element",
];

/// Words that make any name look official, wherever they stand between hyphens
const RESERVED_PARTS: &[&str] = &[
    "zrzur", "zarzour", "zarzur", "zarzoor", "zrzour", "zrzoor", "official", "verified",
    "staff", "admin", "admins", "administrator", "moderator", "moderators", "support",
    "helpdesk", "security", "safety", "billing", "discord", "stoat", "revolt",
];

/// The brand inside a word (myzrzur, zrzurfans)
const BRAND_FRAGMENTS: &[&str] = &["zrzur", "zarzour", "zarzur", "zarzoor", "zrzour", "zrzoor"];

/// Whether a lowercase, already-validated link name is kept for Zrzur
pub fn is_reserved(code: &str) -> bool {
    RESERVED.contains(&code)
        || code.split('-').any(|part| RESERVED_PARTS.contains(&part))
        || BRAND_FRAGMENTS.iter().any(|brand| code.contains(brand))
}

#[cfg(test)]
mod tests {
    use super::is_reserved;

    #[test]
    fn project_words_are_kept() {
        for name in [
            // brand and old spellings
            "zrzur", "zarzour", "zarzur", "starling",
            // product terms
            "feather", "wings", "lobby", "featured", "spotlight", "headline", "boost",
            // pages, services and routes
            "design", "designs", "designer", "discover", "explore", "invite", "api", "cdn",
            "docs", "download", "status", "updates", "settings", "billing", "shop",
            // legal, help and trust
            "terms", "privacy", "support", "help", "safety", "security", "report", "appeals",
            "official", "staff", "admin", "moderator", "verified", "team",
            // other platforms (impersonation)
            "discord", "stoat", "revolt",
        ] {
            assert!(is_reserved(name), "{name} should be reserved");
        }
    }

    #[test]
    fn official_looking_parts_are_kept() {
        for name in [
            "zrzur-fans", "official-gamers", "arab-staff", "support-team", "the-admins",
            "discord-arabia", "verified-club",
        ] {
            assert!(is_reserved(name), "{name} should be reserved");
        }
    }

    #[test]
    fn brand_anywhere_is_kept() {
        for name in ["myzrzur", "zrzurfans", "thezarzour", "zarzurhub"] {
            assert!(is_reserved(name), "{name} should be reserved");
        }
    }

    #[test]
    fn ordinary_names_stay_free() {
        for name in [
            "arab-gamers", "team42", "badminton-club", "designers-hub", "chess-lovers",
            "syria-devs", "minecraft-ar", "book-club",
        ] {
            assert!(!is_reserved(name), "{name} should be free");
        }
    }
}
