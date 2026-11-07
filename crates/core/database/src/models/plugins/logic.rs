//! Manifest validation, hashing and scanning for plugin packages.
//!
//! Mirrors `web/packages/plugin-sdk/src/manifest.ts`. The server runs this on
//! every submission and never trusts the client's own validation.

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::{PluginReleaseStatus, PluginReviewAction, PluginRisk, PluginScanFinding, PluginStatus};

pub const PLUGIN_MANIFEST_VERSION: u64 = 1;
pub const PLUGIN_API_VERSION: u64 = 1;
/// Newest app version this server accepts as `min_app_version`
pub const PLUGIN_MAX_APP_VERSION: (u64, u64, u64) = (26, 99, 9999);

pub const PLUGIN_MANIFEST_BYTES: usize = 64 * 1024;
pub const PLUGIN_CODE_BYTES: usize = 256 * 1024;
pub const PLUGIN_SETTINGS_BYTES: usize = 8 * 1024;

/// Third-party (non-official) plugins may not ship code yet: the sandbox
/// still needs an external security review.
pub const PLUGIN_THIRD_PARTY_CODE_ENABLED: bool = false;

/// `(id, risk, supported)`
pub const PLUGIN_PERMISSIONS: &[(&str, PluginRisk, bool)] = &[
    ("ui.commands", PluginRisk::Low, true),
    ("ui.panels", PluginRisk::Low, true),
    ("keybinds", PluginRisk::Low, true),
    ("theme.colors", PluginRisk::Low, true),
    ("accessibility.display", PluginRisk::Low, true),
    ("app.preferences", PluginRisk::Low, true),
    ("storage.settings", PluginRisk::Low, true),
    ("notifications.show", PluginRisk::Low, true),
    ("notifications.display", PluginRisk::Medium, true),
    ("composer.insert", PluginRisk::Medium, true),
    ("context.channel", PluginRisk::Medium, true),
    ("context.message.selected", PluginRisk::Medium, true),
    ("devtools.diagnostics", PluginRisk::Medium, true),
    ("network.fetch", PluginRisk::High, false),
    ("integration.oauth", PluginRisk::High, false),
];

pub const PLUGIN_FORBIDDEN_PERMISSIONS: &[&str] = &[
    "messages.read_all",
    "messages.read",
    "channels.hidden",
    "auth.token",
    "auth.session",
    "auth.password",
    "auth.mfa",
    "bots.token",
    "payments",
    "billing",
    "fs",
    "filesystem",
    "shell",
    "native",
    "node",
    "ipc",
    "eval",
    "dom",
    "cookies",
    "storage.local",
    "network.unrestricted",
    "admin",
];

const CATEGORIES: &[&str] = &[
    "appearance",
    "productivity",
    "accessibility",
    "notifications",
    "integration",
    "developer",
];
const PLATFORMS: &[&str] = &["web", "pwa", "desktop"];
const THEME_TOKENS: &[&str] = &[
    "primary",
    "on-primary",
    "primary-container",
    "on-primary-container",
    "secondary",
    "on-secondary",
    "secondary-container",
    "on-secondary-container",
    "tertiary",
    "on-tertiary",
    "tertiary-container",
    "on-tertiary-container",
    "background",
    "on-background",
    "surface",
    "on-surface",
    "surface-variant",
    "on-surface-variant",
    "surface-container-lowest",
    "surface-container-low",
    "surface-container",
    "surface-container-high",
    "surface-container-highest",
    "outline",
    "outline-variant",
];
const RESERVED_THEME_TOKENS: &[&str] = &[
    "error",
    "on-error",
    "error-container",
    "on-error-container",
    "shadow",
    "scrim",
];
const CONTRAST_PAIRS: &[(&str, &str, f64)] = &[
    ("on-surface", "surface", 4.5),
    ("on-surface", "surface-container", 4.5),
    ("on-surface", "surface-container-low", 4.5),
    ("on-surface", "surface-container-high", 4.5),
    ("on-surface", "surface-container-highest", 4.5),
    ("on-background", "background", 4.5),
    ("on-surface-variant", "surface-variant", 3.0),
    ("on-primary", "primary", 3.0),
    ("on-primary-container", "primary-container", 3.0),
    ("on-secondary", "secondary", 3.0),
    ("on-secondary-container", "secondary-container", 3.0),
    ("on-tertiary", "tertiary", 3.0),
    ("on-tertiary-container", "tertiary-container", 3.0),
];
const RESERVED_KEYS: &[&str] = &[
    "Ctrl+Shift+I",
    "Ctrl+Shift+J",
    "Ctrl+Shift+C",
    "Ctrl+Shift+R",
    "Ctrl+Shift+N",
    "Ctrl+Shift+T",
    "Ctrl+Shift+W",
    "Ctrl+Shift+Q",
    "Ctrl+Shift+P",
    "Ctrl+Shift+B",
    "Ctrl+Shift+Delete",
    // editing: redo, paste as plain text, Unicode input on Linux
    "Ctrl+Shift+Z",
    "Ctrl+Shift+V",
    "Ctrl+Shift+U",
    // browser tools and menus (Chrome, Firefox, Edge)
    "Ctrl+Shift+K",
    "Ctrl+Shift+E",
    "Ctrl+Shift+M",
    "Ctrl+Shift+O",
    "Ctrl+Shift+A",
    "Ctrl+Shift+D",
    "Ctrl+Shift+S",
    "Ctrl+Shift+Y",
    "Ctrl+Shift+H",
];
const NAVIGATE_TARGETS: &[&str] = &["home", "friends", "discover"];
const TOGGLEABLE_SETTINGS: &[&str] = &["appearance:compact_mode"];
const EVENTS: &[&str] = &["channel.switch", "app.locale_change", "app.theme_change"];
pub const PLUGIN_OFFICIAL_PREFIX: &str = "zrzur.";
/// Legacy official prefix (dual-read / still reserved).
pub const PLUGIN_OFFICIAL_PREFIX_LEGACY: &str = "zarzour.";

static ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z][a-z0-9]*(\.[a-z][a-z0-9-]*){1,4}$").unwrap());
static SEMVER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(0|[1-9]\d{0,3})\.(0|[1-9]\d{0,3})\.(0|[1-9]\d{0,3})(-[0-9A-Za-z.-]{1,20})?$")
        .unwrap()
});
static LOCAL_ID_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z][a-z0-9-]{0,39}$").unwrap());
static SETTING_KEY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z][a-z0-9_]{0,31}$").unwrap());
static LOCALE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z]{2,3}(-[A-Za-z0-9]{2,8})?$").unwrap());
static ICON_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z0-9_-]{1,40}$").unwrap());
static FILE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z0-9_-]{1,40}\.js$").unwrap());
static COLOR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^#[0-9a-fA-F]{6}$").unwrap());
static TIME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^([01]\d|2[0-3]):[0-5]\d$").unwrap());
static SHA256_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[0-9a-f]{64}$").unwrap());
static KEYS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(Ctrl\+Shift|Alt\+Shift|Ctrl\+Alt)\+[A-Z0-9]$").unwrap());
static BINDING_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\$setting:([a-z][a-z0-9_]{0,31})$").unwrap());
static URL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^https://[^\s/$.?#].[^\s]{0,190}$").unwrap());
static BAD_CHARS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[\x{0000}-\x{0008}\x{000b}\x{000c}\x{000e}-\x{001f}\x{202a}-\x{202e}\x{2066}-\x{2069}]")
        .unwrap()
});

/// What the server keeps from a valid manifest
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
    pub icon: Option<String>,
    pub entrypoint: String,
    pub permissions: Vec<String>,
    pub platforms: Vec<String>,
    pub min_app_version: String,
    pub risk: PluginRisk,
    pub integrity: Option<String>,
    /// Setting key → type, for checking saved settings
    pub settings: HashMap<String, SettingSpec>,
}

/// A setting as the manifest declares it
#[derive(Debug, Clone, PartialEq)]
pub enum SettingSpec {
    Boolean,
    Text { max_length: usize },
    Number { min: f64, max: f64 },
    Select { options: Vec<String> },
    Color,
    Time,
}

/// `MAJOR.MINOR.PATCH` (pre-release ignored for ordering of the triple)
pub fn parse_plugin_version(version: &str) -> Option<(u64, u64, u64, Option<String>)> {
    let caps = SEMVER_RE.captures(version)?;
    Some((
        caps[1].parse().ok()?,
        caps[2].parse().ok()?,
        caps[3].parse().ok()?,
        caps.get(4).map(|m| m.as_str()[1..].to_string()),
    ))
}

/// Order two versions; a pre-release sorts before its release
/// Words only official plugins may use in a name: the product's name in
/// its spellings, and "official" in Arabic, English and German
const RESERVED_NAME_WORDS: [&str; 8] = [
    "zrzur", "zarzour", "zarzur", "zarzoor", "زرزور", "official", "offiziell", "رسمي",
];

/// Whether a third-party name pretends to be Zrzur's own. Spaces,
/// punctuation, digits, Arabic marks and letter case don't hide it.
pub fn reserved_plugin_name(text: &str) -> bool {
    let plain: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphabetic() && !('\u{064B}'..='\u{0652}').contains(c) && *c != '\u{0640}')
        .collect();
    RESERVED_NAME_WORDS.iter().any(|word| plain.contains(word))
}

/// Minimum account age (ms) for a report to count towards an automatic
/// suspension, so a handful of new accounts can't take a plugin down
pub const PLUGIN_REPORTER_MIN_AGE_MS: i64 = 7 * 86_400_000;

/// Reports that count towards an automatic suspension: each account once,
/// and only accounts older than [`PLUGIN_REPORTER_MIN_AGE_MS`] (from the
/// time in their id)
pub fn counted_plugin_reports(reports: &[String], now_ms: i64) -> usize {
    reports
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|id| {
            ulid::Ulid::from_string(id)
                .is_ok_and(|u| (u.timestamp_ms() as i64) <= now_ms - PLUGIN_REPORTER_MIN_AGE_MS)
        })
        .count()
}

pub fn compare_plugin_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (parse_plugin_version(a), parse_plugin_version(b)) {
        (Some(x), Some(y)) => (x.0, x.1, x.2).cmp(&(y.0, y.1, y.2)).then_with(|| match (&x.3, &y.3) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(p), Some(q)) => p.cmp(q),
        }),
        _ => Ordering::Equal,
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn luminance(hex: &str) -> f64 {
    let channel = |i: usize| {
        let c = u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

/// WCAG contrast ratio of two `#rrggbb` colours
pub fn plugin_contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

fn risk_rank(risk: &PluginRisk) -> u8 {
    match risk {
        PluginRisk::Low => 0,
        PluginRisk::Medium => 1,
        PluginRisk::High => 2,
    }
}

struct Checker {
    issues: Vec<String>,
}

impl Checker {
    fn fail(&mut self, path: &str, message: impl AsRef<str>) {
        if self.issues.len() < 100 {
            self.issues.push(if path.is_empty() {
                message.as_ref().to_string()
            } else {
                format!("{path}: {}", message.as_ref())
            });
        }
    }

    fn known(&mut self, object: &Map<String, Value>, path: &str, keys: &[&str]) {
        for key in object.keys() {
            if !keys.contains(&key.as_str()) {
                let full = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if key == "dependencies" || key == "scripts" {
                    self.fail(&full, "dependencies and install scripts are not supported");
                } else {
                    self.fail(&full, "unknown field");
                }
            }
        }
    }

    fn text(&mut self, value: Option<&Value>, path: &str, min: usize, max: usize) {
        match value.and_then(Value::as_str) {
            None => self.fail(path, "must be a string"),
            Some(text) => {
                let length = text.trim().chars().count();
                if length < min || length > max {
                    self.fail(path, format!("must be {min}-{max} characters"));
                }
                if BAD_CHARS_RE.is_match(text) {
                    self.fail(path, "contains control or direction-override characters");
                }
            }
        }
    }

    fn local_text(&mut self, value: Option<&Value>, path: &str, max: usize) {
        match value {
            Some(Value::String(_)) => self.text(value, path, 1, max),
            Some(Value::Object(map)) => {
                if map.is_empty() {
                    self.fail(path, "must not be empty");
                }
                if !map.contains_key("en") {
                    self.fail(path, "needs an `en` entry");
                }
                for (locale, entry) in map {
                    let p = format!("{path}.{locale}");
                    if !LOCALE_RE.is_match(locale) {
                        self.fail(&p, "bad locale");
                    }
                    self.text(Some(entry), &p, 1, max);
                }
            }
            _ => self.fail(path, "must be a string or a locale map"),
        }
    }
}

fn object<'a>(value: Option<&'a Value>) -> Option<&'a Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// Validate a manifest (and the code that comes with it). Returns every
/// problem found, so a developer can fix them in one go.
pub fn validate_plugin_manifest(
    input: &Value,
    code: Option<&str>,
    official: bool,
) -> Result<ValidatedManifest, Vec<String>> {
    let mut c = Checker { issues: vec![] };
    let Some(m) = input.as_object() else {
        return Err(vec!["manifest must be an object".to_string()]);
    };
    if serde_json::to_string(input).map(|s| s.len()).unwrap_or(usize::MAX) > PLUGIN_MANIFEST_BYTES {
        c.fail("", format!("manifest is larger than {PLUGIN_MANIFEST_BYTES} bytes"));
    }

    c.known(
        m,
        "",
        &[
            "$schema",
            "manifest_version",
            "id",
            "name",
            "version",
            "description",
            "author",
            "category",
            "icon",
            "api_version",
            "min_app_version",
            "platforms",
            "entrypoint",
            "permissions",
            "contributes",
            "locales",
            "integrity",
        ],
    );

    if m.get("manifest_version").and_then(Value::as_u64) != Some(PLUGIN_MANIFEST_VERSION) {
        c.fail("manifest_version", format!("must be {PLUGIN_MANIFEST_VERSION}"));
    }
    let id = m.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
    if !ID_RE.is_match(&id) || id.len() > 64 {
        c.fail("id", "must look like `publisher.plugin-name` (lowercase, 2-5 parts)");
    } else if (id.starts_with(PLUGIN_OFFICIAL_PREFIX) || id.starts_with(PLUGIN_OFFICIAL_PREFIX_LEGACY)) && !official {
        c.fail("id", "the `zrzur.` prefix is reserved for official plugins");
    }
    c.text(m.get("name"), "name", 2, 48);
    if !official && m.get("name").and_then(Value::as_str).is_some_and(reserved_plugin_name) {
        c.fail("name", "is reserved for official plugins");
    }
    let version = m.get("version").and_then(Value::as_str).unwrap_or_default().to_string();
    if parse_plugin_version(&version).is_none() {
        c.fail("version", "must be MAJOR.MINOR.PATCH");
    }
    c.text(m.get("description"), "description", 1, 300);
    match object(m.get("author")) {
        None => c.fail("author", "must be an object"),
        Some(author) => {
            c.known(author, "author", &["name", "url"]);
            c.text(author.get("name"), "author.name", 1, 64);
            if !official && author.get("name").and_then(Value::as_str).is_some_and(reserved_plugin_name) {
                c.fail("author.name", "is reserved for official plugins");
            }
            if let Some(url) = author.get("url") {
                if !url.as_str().is_some_and(|u| URL_RE.is_match(u)) {
                    c.fail("author.url", "must be an https URL");
                }
            }
        }
    }
    let category = m.get("category").and_then(Value::as_str).unwrap_or_default().to_string();
    if !CATEGORIES.contains(&category.as_str()) {
        c.fail("category", format!("must be one of {}", CATEGORIES.join(", ")));
    }
    let icon = m.get("icon").map(|v| v.as_str().unwrap_or("<invalid>").to_string());
    if let Some(icon) = &icon {
        if !ICON_RE.is_match(icon) {
            c.fail("icon", "must be a Material Symbols name (no images or URLs)");
        }
    }
    if m.get("api_version").and_then(Value::as_u64) != Some(PLUGIN_API_VERSION) {
        c.fail("api_version", format!("this Zrzur supports plugin API {PLUGIN_API_VERSION}"));
    }
    let min_app_version = m
        .get("min_app_version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match parse_plugin_version(&min_app_version) {
        None => c.fail("min_app_version", "must be MAJOR.MINOR.PATCH"),
        Some((a, b, p, _)) if (a, b, p) > PLUGIN_MAX_APP_VERSION => {
            c.fail("min_app_version", "is newer than any Zrzur this server supports")
        }
        _ => {}
    }

    let mut platforms = vec![];
    match m.get("platforms").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => {
            for p in list {
                match p.as_str() {
                    Some(p) if PLATFORMS.contains(&p) && !platforms.contains(&p.to_string()) => {
                        platforms.push(p.to_string())
                    }
                    _ => c.fail("platforms", format!("must list some of {}", PLATFORMS.join(", "))),
                }
            }
        }
        _ => c.fail("platforms", format!("must list some of {}", PLATFORMS.join(", "))),
    }

    // entrypoint
    let mut worker = false;
    let mut entrypoint = String::new();
    match object(m.get("entrypoint")) {
        None => c.fail("entrypoint", "must be an object"),
        Some(e) => match e.get("type").and_then(Value::as_str) {
            Some("declarative") => {
                c.known(e, "entrypoint", &["type"]);
                entrypoint = "declarative".into();
            }
            Some("worker") => {
                worker = true;
                entrypoint = "worker".into();
                c.known(e, "entrypoint", &["type", "file"]);
                if !e.get("file").and_then(Value::as_str).is_some_and(|f| FILE_RE.is_match(f)) {
                    c.fail("entrypoint.file", "must be a plain file name like `main.js` (no folders)");
                }
            }
            _ => c.fail("entrypoint.type", "must be `declarative` or `worker`"),
        },
    }

    // permissions
    let mut permissions: Vec<String> = vec![];
    let mut risk = PluginRisk::Low;
    match m.get("permissions").and_then(Value::as_array) {
        None => c.fail("permissions", "must be a list"),
        Some(list) => {
            for (i, permission) in list.iter().enumerate() {
                let path = format!("permissions[{i}]");
                let Some(permission) = permission.as_str() else {
                    c.fail(&path, "must be a string");
                    continue;
                };
                if permissions.iter().any(|p| p == permission) {
                    c.fail(&path, "listed twice");
                    continue;
                }
                permissions.push(permission.to_string());
                if PLUGIN_FORBIDDEN_PERMISSIONS.contains(&permission) {
                    c.fail(&path, format!("`{permission}` can never be granted to a plugin"));
                    continue;
                }
                match PLUGIN_PERMISSIONS.iter().find(|(id, _, _)| *id == permission) {
                    None => c.fail(&path, format!("unknown permission `{permission}`")),
                    Some((_, entry_risk, supported)) => {
                        if !supported {
                            c.fail(&path, format!("`{permission}` is not supported by this Zrzur version"));
                        }
                        if risk_rank(entry_risk) > risk_rank(&risk) {
                            risk = entry_risk.clone();
                        }
                    }
                }
            }
        }
    }
    if worker {
        risk = PluginRisk::High;
    }
    let has = |p: &str| permissions.iter().any(|x| x == p);
    let needs = |c: &mut Checker, permission: &str, path: &str| {
        if !has(permission) {
            c.fail(path, format!("needs the `{permission}` permission"));
        }
    };

    // contributes
    let mut settings: HashMap<String, SettingSpec> = HashMap::new();
    let mut command_ids: HashSet<String> = HashSet::new();
    let mut panel_ids: HashSet<String> = HashSet::new();

    fn setting_type(spec: &SettingSpec) -> &'static str {
        match spec {
            SettingSpec::Boolean => "boolean",
            SettingSpec::Text { .. } => "text",
            SettingSpec::Number { .. } => "number",
            SettingSpec::Select { .. } => "select",
            SettingSpec::Color => "color",
            SettingSpec::Time => "time",
        }
    }
    let bindable = |c: &mut Checker,
                    settings: &HashMap<String, SettingSpec>,
                    value: Option<&Value>,
                    path: &str,
                    kind: &str,
                    check: &dyn Fn(&Value) -> bool| {
        if let Some(caps) = value.and_then(Value::as_str).and_then(|s| BINDING_RE.captures(s)) {
            match settings.get(&caps[1]) {
                None => c.fail(path, format!("no setting called `{}`", &caps[1])),
                Some(spec) if setting_type(spec) != kind => {
                    c.fail(path, format!("setting `{}` must be a {kind}", &caps[1]))
                }
                _ => {}
            }
            return;
        }
        if !value.is_some_and(check) {
            c.fail(path, format!("must be a {kind}"));
        }
    };
    let is_color = |v: &Value| v.as_str().is_some_and(|s| COLOR_RE.is_match(s));
    let is_time = |v: &Value| v.as_str().is_some_and(|s| TIME_RE.is_match(s));
    let is_bool = |v: &Value| v.is_boolean();

    if let Some(contributes) = m.get("contributes") {
        match contributes.as_object() {
            None => c.fail("contributes", "must be an object"),
            Some(cm) => {
                c.known(
                    cm,
                    "contributes",
                    &[
                        "theme",
                        "accessibility",
                        "notifications",
                        "commands",
                        "keybinds",
                        "settings",
                        "panels",
                        "message_actions",
                        "events",
                    ],
                );

                if let Some(value) = cm.get("settings") {
                    needs(&mut c, "storage.settings", "contributes.settings");
                    match value.as_array() {
                        Some(list) if list.len() <= 20 => {
                            for (i, s) in list.iter().enumerate() {
                                let path = format!("contributes.settings[{i}]");
                                let Some(s) = s.as_object() else {
                                    c.fail(&path, "must be an object");
                                    continue;
                                };
                                let Some(key) = s.get("key").and_then(Value::as_str).filter(|k| SETTING_KEY_RE.is_match(k)) else {
                                    c.fail(&format!("{path}.key"), "must be lowercase letters, digits and _");
                                    continue;
                                };
                                if settings.contains_key(key) {
                                    c.fail(&format!("{path}.key"), "listed twice");
                                }
                                c.local_text(s.get("title"), &format!("{path}.title"), 60);
                                if s.get("description").is_some() {
                                    c.local_text(s.get("description"), &format!("{path}.description"), 300);
                                }
                                let common = ["key", "type", "title", "description", "default"];
                                let with = |extra: &[&'static str]| -> Vec<&'static str> {
                                    common.iter().copied().chain(extra.iter().copied()).collect()
                                };
                                let default = s.get("default");
                                let spec = match s.get("type").and_then(Value::as_str) {
                                    Some("boolean") => {
                                        c.known(s, &path, &common);
                                        if !default.is_some_and(Value::is_boolean) {
                                            c.fail(&format!("{path}.default"), "must be true or false");
                                        }
                                        SettingSpec::Boolean
                                    }
                                    Some("text") => {
                                        c.known(s, &path, &with(&["max_length"]));
                                        let max = s.get("max_length").map(|v| v.as_u64().unwrap_or(0)).unwrap_or(200);
                                        if !(1..=500).contains(&max) {
                                            c.fail(&format!("{path}.max_length"), "must be 1-500");
                                        }
                                        if !default.and_then(Value::as_str).is_some_and(|d| d.chars().count() as u64 <= max) {
                                            c.fail(&format!("{path}.default"), "must be a string within max_length");
                                        }
                                        SettingSpec::Text { max_length: max as usize }
                                    }
                                    Some("number") => {
                                        c.known(s, &path, &with(&["min", "max", "step"]));
                                        let min = s.get("min").and_then(Value::as_f64);
                                        let max = s.get("max").and_then(Value::as_f64);
                                        match (min, max) {
                                            (Some(min), Some(max)) if min.is_finite() && max.is_finite() && min < max => {
                                                if !default.and_then(Value::as_f64).is_some_and(|d| d >= min && d <= max) {
                                                    c.fail(&format!("{path}.default"), "must be between min and max");
                                                }
                                                if let Some(step) = s.get("step") {
                                                    if !step.as_f64().is_some_and(|s| s > 0.0) {
                                                        c.fail(&format!("{path}.step"), "must be positive");
                                                    }
                                                }
                                                SettingSpec::Number { min, max }
                                            }
                                            _ => {
                                                c.fail(&path, "needs finite min < max");
                                                SettingSpec::Number { min: 0.0, max: 0.0 }
                                            }
                                        }
                                    }
                                    Some("select") => {
                                        c.known(s, &path, &with(&["options"]));
                                        let mut values = vec![];
                                        match s.get("options").and_then(Value::as_array) {
                                            Some(options) if !options.is_empty() && options.len() <= 20 => {
                                                for (j, option) in options.iter().enumerate() {
                                                    let p = format!("{path}.options[{j}]");
                                                    let Some(option) = option.as_object() else {
                                                        c.fail(&p, "must be an object");
                                                        continue;
                                                    };
                                                    c.known(option, &p, &["value", "label"]);
                                                    match option.get("value").and_then(Value::as_str) {
                                                        Some(v) if SETTING_KEY_RE.is_match(v) => values.push(v.to_string()),
                                                        _ => c.fail(&format!("{p}.value"), "must be a short lowercase id"),
                                                    }
                                                    c.local_text(option.get("label"), &format!("{p}.label"), 60);
                                                }
                                                if !default.and_then(Value::as_str).is_some_and(|d| values.iter().any(|v| v == d)) {
                                                    c.fail(&format!("{path}.default"), "must be one of the options");
                                                }
                                            }
                                            _ => c.fail(&format!("{path}.options"), "must list 1-20 options"),
                                        }
                                        SettingSpec::Select { options: values }
                                    }
                                    Some("color") => {
                                        c.known(s, &path, &common);
                                        if !default.is_some_and(is_color) {
                                            c.fail(&format!("{path}.default"), "must be #rrggbb");
                                        }
                                        SettingSpec::Color
                                    }
                                    Some("time") => {
                                        c.known(s, &path, &common);
                                        if !default.is_some_and(is_time) {
                                            c.fail(&format!("{path}.default"), "must be HH:MM");
                                        }
                                        SettingSpec::Time
                                    }
                                    _ => {
                                        c.fail(&format!("{path}.type"), "must be boolean, text, number, select, color or time");
                                        continue;
                                    }
                                };
                                settings.insert(key.to_string(), spec);
                            }
                        }
                        _ => c.fail("contributes.settings", "must be a list of at most 20"),
                    }
                }

                if let Some(theme) = cm.get("theme") {
                    needs(&mut c, "theme.colors", "contributes.theme");
                    match theme.as_object().and_then(|t| t.get("colors").and_then(Value::as_object).map(|colors| (t, colors))) {
                        None => c.fail("contributes.theme", "needs a `colors` object"),
                        Some((t, colors)) => {
                            c.known(t, "contributes.theme", &["colors"]);
                            if colors.is_empty() {
                                c.fail("contributes.theme.colors", "is empty");
                            }
                            for (token, value) in colors {
                                let path = format!("contributes.theme.colors.{token}");
                                if RESERVED_THEME_TOKENS.contains(&token.as_str()) {
                                    c.fail(&path, "warning and error colours can't be changed by plugins");
                                } else if !THEME_TOKENS.contains(&token.as_str()) {
                                    c.fail(&path, "unknown colour token");
                                } else {
                                    bindable(&mut c, &settings, Some(value), &path, "color", &is_color);
                                }
                            }
                            for (fg, bg, min) in CONTRAST_PAIRS {
                                if let (Some(a), Some(b)) = (
                                    colors.get(*fg).and_then(Value::as_str).filter(|s| COLOR_RE.is_match(s)),
                                    colors.get(*bg).and_then(Value::as_str).filter(|s| COLOR_RE.is_match(s)),
                                ) {
                                    let ratio = plugin_contrast(a, b);
                                    if ratio < *min {
                                        c.fail(
                                            &format!("contributes.theme.colors.{fg}"),
                                            format!("contrast with {bg} is {ratio:.2}:1, needs {min}:1"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(a) = cm.get("accessibility") {
                    needs(&mut c, "accessibility.display", "contributes.accessibility");
                    match a.as_object() {
                        None => c.fail("contributes.accessibility", "must be an object"),
                        Some(a) => {
                            c.known(
                                a,
                                "contributes.accessibility",
                                &["font_scale", "line_height", "letter_spacing", "underline_links", "reduce_motion", "focus_ring"],
                            );
                            for (key, min, max) in [("font_scale", 0.85, 1.5), ("line_height", 1.2, 2.0), ("letter_spacing", 0.0, 0.12)] {
                                if a.contains_key(key) {
                                    let in_range = move |v: &Value| v.as_f64().is_some_and(|n| n >= min && n <= max);
                                    bindable(&mut c, &settings, a.get(key), &format!("contributes.accessibility.{key}"), "number", &in_range);
                                }
                            }
                            for key in ["underline_links", "reduce_motion", "focus_ring"] {
                                if a.contains_key(key) {
                                    bindable(&mut c, &settings, a.get(key), &format!("contributes.accessibility.{key}"), "boolean", &is_bool);
                                }
                            }
                        }
                    }
                }

                if let Some(n) = cm.get("notifications") {
                    needs(&mut c, "notifications.display", "contributes.notifications");
                    match n.as_object() {
                        None => c.fail("contributes.notifications", "must be an object"),
                        Some(n) => {
                            c.known(n, "contributes.notifications", &["quiet_hours", "quiet_hours_enabled", "hide_previews", "mute_sound"]);
                            if let Some(q) = n.get("quiet_hours") {
                                match q.as_object() {
                                    None => c.fail("contributes.notifications.quiet_hours", "needs start and end"),
                                    Some(q) => {
                                        c.known(q, "contributes.notifications.quiet_hours", &["start", "end"]);
                                        bindable(&mut c, &settings, q.get("start"), "contributes.notifications.quiet_hours.start", "time", &is_time);
                                        bindable(&mut c, &settings, q.get("end"), "contributes.notifications.quiet_hours.end", "time", &is_time);
                                    }
                                }
                            }
                            for key in ["quiet_hours_enabled", "hide_previews", "mute_sound"] {
                                if n.contains_key(key) {
                                    bindable(&mut c, &settings, n.get(key), &format!("contributes.notifications.{key}"), "boolean", &is_bool);
                                }
                            }
                        }
                    }
                }

                if let Some(panels) = cm.get("panels") {
                    needs(&mut c, "ui.panels", "contributes.panels");
                    match panels.as_array() {
                        Some(list) if list.len() <= 5 => {
                            for panel in list {
                                if let Some(id) = panel.get("id").and_then(Value::as_str) {
                                    panel_ids.insert(id.to_string());
                                }
                            }
                        }
                        _ => c.fail("contributes.panels", "must be a list of at most 5"),
                    }
                }

                if let Some(commands) = cm.get("commands") {
                    needs(&mut c, "ui.commands", "contributes.commands");
                    match commands.as_array() {
                        Some(list) if list.len() <= 20 => {
                            for (i, command) in list.iter().enumerate() {
                                let path = format!("contributes.commands[{i}]");
                                let Some(command) = command.as_object() else {
                                    c.fail(&path, "must be an object");
                                    continue;
                                };
                                c.known(command, &path, &["id", "title", "action"]);
                                let Some(cid) = command.get("id").and_then(Value::as_str).filter(|s| LOCAL_ID_RE.is_match(s)) else {
                                    c.fail(&format!("{path}.id"), "must be a short lowercase id");
                                    continue;
                                };
                                if !command_ids.insert(cid.to_string()) {
                                    c.fail(&format!("{path}.id"), "listed twice");
                                }
                                c.local_text(command.get("title"), &format!("{path}.title"), 60);
                                let ap = format!("{path}.action");
                                let Some(action) = object(command.get("action")) else {
                                    c.fail(&ap, "must be an object");
                                    continue;
                                };
                                match action.get("type").and_then(Value::as_str) {
                                    Some("setting.toggle") => {
                                        c.known(action, &ap, &["type", "key"]);
                                        needs(&mut c, "app.preferences", &ap);
                                        if !action.get("key").and_then(Value::as_str).is_some_and(|k| TOGGLEABLE_SETTINGS.contains(&k)) {
                                            c.fail(&format!("{ap}.key"), format!("must be one of {}", TOGGLEABLE_SETTINGS.join(", ")));
                                        }
                                    }
                                    Some("theme.toggle") => {
                                        c.known(action, &ap, &["type"]);
                                        if object(cm.get("theme")).is_none() {
                                            c.fail(&ap, "the plugin has no theme to toggle");
                                        }
                                    }
                                    Some("accessibility.toggle") => {
                                        c.known(action, &ap, &["type"]);
                                        if object(cm.get("accessibility")).is_none() {
                                            c.fail(&ap, "the plugin has no accessibility settings");
                                        }
                                    }
                                    Some("panel.open") => {
                                        c.known(action, &ap, &["type", "panel"]);
                                        if !action.get("panel").and_then(Value::as_str).is_some_and(|p| panel_ids.contains(p)) {
                                            c.fail(&format!("{ap}.panel"), "no such panel");
                                        }
                                    }
                                    Some("notify") => {
                                        c.known(action, &ap, &["type", "text"]);
                                        needs(&mut c, "notifications.show", &ap);
                                        c.local_text(action.get("text"), &format!("{ap}.text"), 140);
                                    }
                                    Some("composer.insert") => {
                                        c.known(action, &ap, &["type", "text"]);
                                        needs(&mut c, "composer.insert", &ap);
                                        c.text(action.get("text"), &format!("{ap}.text"), 1, 2000);
                                    }
                                    Some("navigate") => {
                                        c.known(action, &ap, &["type", "to"]);
                                        if !action.get("to").and_then(Value::as_str).is_some_and(|t| NAVIGATE_TARGETS.contains(&t)) {
                                            c.fail(&format!("{ap}.to"), format!("must be one of {}", NAVIGATE_TARGETS.join(", ")));
                                        }
                                    }
                                    Some("worker") => {
                                        c.known(action, &ap, &["type", "handler"]);
                                        if !worker {
                                            c.fail(&ap, "needs a worker entrypoint");
                                        }
                                        if !action.get("handler").and_then(Value::as_str).is_some_and(|h| LOCAL_ID_RE.is_match(h)) {
                                            c.fail(&format!("{ap}.handler"), "must be a short lowercase id");
                                        }
                                    }
                                    _ => c.fail(&format!("{ap}.type"), "unknown action"),
                                }
                            }
                        }
                        _ => c.fail("contributes.commands", "must be a list of at most 20"),
                    }
                }

                if let Some(list) = cm.get("panels").and_then(Value::as_array) {
                    let mut ids = HashSet::new();
                    for (i, panel) in list.iter().enumerate() {
                        let path = format!("contributes.panels[{i}]");
                        let Some(panel) = panel.as_object() else {
                            c.fail(&path, "must be an object");
                            continue;
                        };
                        c.known(panel, &path, &["id", "title", "blocks"]);
                        match panel.get("id").and_then(Value::as_str) {
                            Some(id) if LOCAL_ID_RE.is_match(id) => {
                                if !ids.insert(id.to_string()) {
                                    c.fail(&format!("{path}.id"), "listed twice");
                                }
                            }
                            _ => c.fail(&format!("{path}.id"), "must be a short lowercase id"),
                        }
                        c.local_text(panel.get("title"), &format!("{path}.title"), 60);
                        let Some(blocks) = panel.get("blocks").and_then(Value::as_array).filter(|b| b.len() <= 40) else {
                            c.fail(&format!("{path}.blocks"), "must be a list of at most 40");
                            continue;
                        };
                        for (j, block) in blocks.iter().enumerate() {
                            let bp = format!("{path}.blocks[{j}]");
                            let Some(block) = block.as_object() else {
                                c.fail(&bp, "must be an object");
                                continue;
                            };
                            match block.get("type").and_then(Value::as_str) {
                                Some(kind @ ("heading" | "text")) => {
                                    c.known(block, &bp, &["type", "text"]);
                                    c.local_text(block.get("text"), &format!("{bp}.text"), if kind == "text" { 600 } else { 60 });
                                }
                                Some("divider") => c.known(block, &bp, &["type"]),
                                Some("button") => {
                                    c.known(block, &bp, &["type", "label", "command"]);
                                    c.local_text(block.get("label"), &format!("{bp}.label"), 60);
                                    if !block.get("command").and_then(Value::as_str).is_some_and(|x| command_ids.contains(x)) {
                                        c.fail(&format!("{bp}.command"), "no such command");
                                    }
                                }
                                Some("setting") => {
                                    c.known(block, &bp, &["type", "key"]);
                                    if !block.get("key").and_then(Value::as_str).is_some_and(|k| settings.contains_key(k)) {
                                        c.fail(&format!("{bp}.key"), "no such setting");
                                    }
                                }
                                _ => c.fail(&format!("{bp}.type"), "must be heading, text, divider, button or setting"),
                            }
                        }
                    }
                }

                if let Some(keybinds) = cm.get("keybinds") {
                    needs(&mut c, "keybinds", "contributes.keybinds");
                    match keybinds.as_array() {
                        Some(list) if list.len() <= 10 => {
                            let mut seen = HashSet::new();
                            for (i, bind) in list.iter().enumerate() {
                                let path = format!("contributes.keybinds[{i}]");
                                let Some(bind) = bind.as_object() else {
                                    c.fail(&path, "must be an object");
                                    continue;
                                };
                                c.known(bind, &path, &["command", "keys"]);
                                if !bind.get("command").and_then(Value::as_str).is_some_and(|x| command_ids.contains(x)) {
                                    c.fail(&format!("{path}.command"), "no such command");
                                }
                                match bind.get("keys").and_then(Value::as_str) {
                                    Some(keys) if KEYS_RE.is_match(keys) => {
                                        if RESERVED_KEYS.contains(&keys) {
                                            c.fail(&format!("{path}.keys"), "is used by the browser or Zrzur");
                                        } else if !seen.insert(keys.to_string()) {
                                            c.fail(&format!("{path}.keys"), "listed twice");
                                        }
                                    }
                                    _ => c.fail(&format!("{path}.keys"), "must be Ctrl+Shift+X, Alt+Shift+X or Ctrl+Alt+X (X = A-Z or 0-9)"),
                                }
                            }
                        }
                        _ => c.fail("contributes.keybinds", "must be a list of at most 10"),
                    }
                }

                if let Some(actions) = cm.get("message_actions") {
                    needs(&mut c, "context.message.selected", "contributes.message_actions");
                    if !worker {
                        c.fail("contributes.message_actions", "needs a worker entrypoint");
                    }
                    match actions.as_array() {
                        Some(list) if list.len() <= 5 => {
                            for (i, action) in list.iter().enumerate() {
                                let path = format!("contributes.message_actions[{i}]");
                                let Some(action) = action.as_object() else {
                                    c.fail(&path, "must be an object");
                                    continue;
                                };
                                c.known(action, &path, &["id", "title", "handler"]);
                                for key in ["id", "handler"] {
                                    if !action.get(key).and_then(Value::as_str).is_some_and(|s| LOCAL_ID_RE.is_match(s)) {
                                        c.fail(&format!("{path}.{key}"), "bad id");
                                    }
                                }
                                c.local_text(action.get("title"), &format!("{path}.title"), 60);
                            }
                        }
                        _ => c.fail("contributes.message_actions", "must be a list of at most 5"),
                    }
                }

                if let Some(events) = cm.get("events") {
                    if !worker {
                        c.fail("contributes.events", "needs a worker entrypoint");
                    }
                    match events.as_array() {
                        None => c.fail("contributes.events", "must be a list"),
                        Some(list) => {
                            for (i, event) in list.iter().enumerate() {
                                let path = format!("contributes.events[{i}]");
                                match event.as_str() {
                                    Some(e) if EVENTS.contains(&e) => {
                                        if e == "channel.switch" {
                                            needs(&mut c, "context.channel", &path);
                                        }
                                    }
                                    _ => c.fail(&path, "unknown event"),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // locales
    if let Some(locales) = m.get("locales") {
        match locales.as_object() {
            Some(map) if map.len() <= 40 => {
                for (locale, entry) in map {
                    let path = format!("locales.{locale}");
                    if !LOCALE_RE.is_match(locale) {
                        c.fail(&path, "bad locale");
                    }
                    let Some(entry) = entry.as_object() else {
                        c.fail(&path, "must be an object");
                        continue;
                    };
                    c.known(entry, &path, &["name", "description", "strings"]);
                    if entry.contains_key("name") {
                        c.text(entry.get("name"), &format!("{path}.name"), 2, 48);
                        if !official && entry.get("name").and_then(Value::as_str).is_some_and(reserved_plugin_name) {
                            c.fail(&format!("{path}.name"), "is reserved for official plugins");
                        }
                    }
                    if entry.contains_key("description") {
                        c.text(entry.get("description"), &format!("{path}.description"), 1, 300);
                    }
                    if let Some(strings) = entry.get("strings") {
                        match strings.as_object() {
                            Some(strings) if strings.len() <= 200 => {
                                for (key, value) in strings {
                                    if !SETTING_KEY_RE.is_match(key) {
                                        c.fail(&format!("{path}.strings.{key}"), "bad key");
                                    }
                                    c.text(Some(value), &format!("{path}.strings.{key}"), 1, 300);
                                }
                            }
                            _ => c.fail(&format!("{path}.strings"), "must map at most 200 strings"),
                        }
                    }
                }
            }
            _ => c.fail("locales", "must map at most 40 locales"),
        }
    }

    // integrity and code
    let mut integrity = None;
    if let Some(value) = m.get("integrity") {
        match value.as_object() {
            Some(i) => {
                c.known(i, "integrity", &["sha256"]);
                match i.get("sha256").and_then(Value::as_str) {
                    Some(h) if SHA256_RE.is_match(h) => integrity = Some(h.to_string()),
                    _ => c.fail("integrity.sha256", "must be a lowercase hex SHA-256"),
                }
            }
            None => c.fail("integrity.sha256", "must be a lowercase hex SHA-256"),
        }
    }
    if worker {
        match (code, &integrity) {
            (None, _) | (Some(""), _) => c.fail("code", "worker plugins need code"),
            (Some(code), _) if code.len() > PLUGIN_CODE_BYTES => {
                c.fail("code", format!("code is larger than {PLUGIN_CODE_BYTES} bytes"))
            }
            (Some(_), None) => c.fail("integrity", "worker plugins need integrity.sha256 (run `zrzur-plugin pack`)"),
            (Some(code), Some(hash)) => {
                if &sha256_hex(code.as_bytes()) != hash {
                    c.fail("integrity.sha256", "does not match the code (package was changed after packing)");
                }
            }
        }
    } else if code.is_some_and(|c| !c.is_empty()) {
        c.fail("code", "declarative plugins can't include code");
    }

    if !c.issues.is_empty() {
        return Err(c.issues);
    }
    Ok(ValidatedManifest {
        id,
        name: m["name"].as_str().unwrap_or_default().trim().to_string(),
        version,
        description: m["description"].as_str().unwrap_or_default().trim().to_string(),
        category,
        icon,
        entrypoint,
        permissions,
        platforms,
        min_app_version,
        risk,
        integrity,
        settings,
    })
}

struct ScanRule {
    id: &'static str,
    pattern: Regex,
    block: bool,
    message: &'static str,
}

static SCAN_RULES: Lazy<Vec<ScanRule>> = Lazy::new(|| {
    let rule = |id, pattern: &str, block, message| ScanRule {
        id,
        pattern: Regex::new(pattern).unwrap(),
        block,
        message,
    };
    vec![
        rule("private-key", r"-----BEGIN ([A-Z]+ )?PRIVATE KEY-----", true, "contains a private key"),
        rule("aws-key", r"AKIA[0-9A-Z]{16}", true, "contains an AWS access key"),
        rule("github-token", r"gh[pousr]_[A-Za-z0-9]{36}", true, "contains a GitHub token"),
        rule("slack-token", r"xox[baprs]-[A-Za-z0-9-]{10,}", true, "contains a Slack token"),
        rule("stripe-live", r"sk_live_[0-9a-zA-Z]{16,}", true, "contains a Stripe live key"),
        rule("session-header", r"(?i)x-session-token|x-bot-token", true, "references Zrzur auth headers"),
        rule("eval", r#"\beval\s*\(|new\s+Function\s*\(|\bFunction\s*\(\s*["'`]"#, false, "dynamic code evaluation (blocked by the sandbox CSP)"),
        rule("network", r"\bfetch\s*\(|XMLHttpRequest|WebSocket|EventSource|importScripts", false, "network access (blocked by the sandbox CSP)"),
        rule("storage", r"localStorage|sessionStorage|indexedDB|document\.cookie", false, "browser storage access (unavailable in the sandbox)"),
        rule("prototype", r"__proto__|constructor\s*\.\s*constructor|Object\.setPrototypeOf", false, "prototype manipulation"),
        rule("obfuscation", r"[A-Za-z0-9+/]{400,}={0,2}|(\\x[0-9a-fA-F]{2}){20,}", false, "long encoded blob (possible obfuscation)"),
    ]
});

/// Heuristic scan of a package. Not malware detection: blocking findings
/// stop obvious leaks, the rest is shown to the human reviewer.
pub fn scan_plugin_package(manifest_text: &str, code: Option<&str>) -> Vec<PluginScanFinding> {
    let mut findings = vec![];
    for source in [Some(manifest_text), code].into_iter().flatten() {
        for rule in SCAN_RULES.iter() {
            if let Some(line) = source.lines().position(|l| rule.pattern.is_match(l)) {
                findings.push(PluginScanFinding {
                    rule: rule.id.to_string(),
                    block: rule.block,
                    message: rule.message.to_string(),
                    line: line as u32 + 1,
                });
            }
        }
    }
    findings
}

/// Check saved settings against the manifest. Unknown keys are dropped.
pub fn clean_plugin_settings(
    specs: &HashMap<String, SettingSpec>,
    values: &Value,
) -> Result<Map<String, Value>, String> {
    let Some(values) = values.as_object() else {
        return Err("settings must be an object".into());
    };
    let mut clean = Map::new();
    for (key, value) in values {
        let Some(spec) = specs.get(key) else {
            continue;
        };
        let ok = match spec {
            SettingSpec::Boolean => value.is_boolean(),
            SettingSpec::Text { max_length } => value
                .as_str()
                .is_some_and(|s| s.chars().count() <= *max_length && !BAD_CHARS_RE.is_match(s)),
            SettingSpec::Number { min, max } => value.as_f64().is_some_and(|n| n >= *min && n <= *max),
            SettingSpec::Select { options } => value.as_str().is_some_and(|s| options.iter().any(|o| o == s)),
            SettingSpec::Color => value.as_str().is_some_and(|s| COLOR_RE.is_match(s)),
            SettingSpec::Time => value.as_str().is_some_and(|s| TIME_RE.is_match(s)),
        };
        if !ok {
            return Err(format!("`{key}` has an invalid value"));
        }
        clean.insert(key.clone(), value.clone());
    }
    if serde_json::to_string(&clean).map(|s| s.len()).unwrap_or(usize::MAX) > PLUGIN_SETTINGS_BYTES {
        return Err(format!("settings are larger than {PLUGIN_SETTINGS_BYTES} bytes"));
    }
    Ok(clean)
}

/// Allowed review moves: `(from, action) → to`
pub fn plugin_release_transition(
    from: &PluginReleaseStatus,
    action: &PluginReviewAction,
) -> Option<PluginReleaseStatus> {
    use PluginReleaseStatus::*;
    use PluginReviewAction as A;
    match (from, action) {
        (PendingReview, A::Approve) => Some(Approved),
        (PendingReview, A::Reject) => Some(Rejected),
        (PendingReview, A::Withdraw) => Some(Withdrawn),
        (Approved, A::Revoke) => Some(Revoked),
        _ => None,
    }
}

/// Whether installs of a plugin in this state may run
pub fn plugin_runnable(status: &PluginStatus) -> bool {
    matches!(status, PluginStatus::Listed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme() -> Value {
        json!({
            "manifest_version": 1,
            "id": "dev.example.midnight",
            "name": "Midnight",
            "version": "1.0.0",
            "description": "A deep blue theme",
            "author": { "name": "Example" },
            "category": "appearance",
            "api_version": 1,
            "min_app_version": "26.0.0",
            "platforms": ["web", "desktop"],
            "entrypoint": { "type": "declarative" },
            "permissions": ["theme.colors", "ui.commands"],
            "contributes": {
                "theme": { "colors": { "primary": "#7aa2ff", "surface": "#0b1020", "on-surface": "#e6e9f5" } },
                "commands": [{ "id": "toggle", "title": { "en": "Toggle", "ar": "تبديل" }, "action": { "type": "theme.toggle" } }]
            }
        })
    }

    #[test]
    fn accepts_valid_theme() {
        let v = validate_plugin_manifest(&theme(), None, false).expect("valid");
        assert_eq!(v.risk, PluginRisk::Low);
        assert_eq!(v.entrypoint, "declarative");
    }

    #[test]
    fn rejects_forbidden_unknown_and_unsupported() {
        let mut m = theme();
        m["permissions"] = json!(["theme.colors", "ui.commands", "auth.token", "telepathy", "network.fetch"]);
        let issues = validate_plugin_manifest(&m, None, false).unwrap_err();
        assert!(issues.iter().any(|i| i.contains("can never be granted")));
        assert!(issues.iter().any(|i| i.contains("unknown permission")));
        assert!(issues.iter().any(|i| i.contains("not supported")));
    }

    #[test]
    fn rejects_unknown_fields_and_dependencies() {
        let mut m = theme();
        m["dependencies"] = json!({ "left-pad": "*" });
        m["contributes"]["theme"]["css"] = json!("body{display:none}");
        let issues = validate_plugin_manifest(&m, None, false).unwrap_err();
        assert!(issues.iter().any(|i| i.contains("dependencies")));
        assert!(issues.iter().any(|i| i.contains("contributes.theme.css")));
    }

    #[test]
    fn editing_and_browser_shortcuts_stay_with_the_user() {
        for keys in ["Ctrl+Shift+Z", "Ctrl+Shift+V", "Ctrl+Shift+U", "Ctrl+Shift+K"] {
            let mut m = theme();
            m["permissions"] = json!(["theme.colors", "ui.commands", "keybinds"]);
            m["contributes"]["keybinds"] = json!([{ "command": "toggle", "keys": keys }]);
            let issues = validate_plugin_manifest(&m, None, false).unwrap_err();
            assert!(issues.iter().any(|i| i.contains("used by the browser")), "{keys}");
        }
    }

    #[test]
    fn rejects_error_colours_and_low_contrast() {
        let mut m = theme();
        m["contributes"]["theme"]["colors"] = json!({ "error": "#000000", "surface": "#101010", "on-surface": "#151515" });
        let issues = validate_plugin_manifest(&m, None, false).unwrap_err();
        assert!(issues.iter().any(|i| i.contains("warning and error")));
        assert!(issues.iter().any(|i| i.contains("contrast")));
    }

    #[test]
    fn rejects_path_traversal_and_bad_ids() {
        let mut m = theme();
        m["id"] = json!("../../etc");
        m["entrypoint"] = json!({ "type": "worker", "file": "../main.js" });
        let issues = validate_plugin_manifest(&m, Some("x"), false).unwrap_err();
        assert!(issues.iter().any(|i| i.starts_with("id")));
        assert!(issues.iter().any(|i| i.starts_with("entrypoint.file")));
    }

    #[test]
    fn official_prefix_is_reserved() {
        let mut m = theme();
        m["id"] = json!("zrzur.fake");
        assert!(validate_plugin_manifest(&m, None, false).is_err());
        assert!(validate_plugin_manifest(&m, None, true).is_ok());
    }

    #[test]
    fn third_parties_cant_look_official() {
        for (field, value) in [
            ("name", "Zrzur Security"),
            ("name", "ZR-ZUR tools"),
            ("name", "أدوات زرزور"),
            ("name", "Official Themes"),
            ("name", "الثيمات الرسمية"),
            ("name", "Offizielle Farben"),
            ("author", "Zarzour Team"),
            ("locale", "زرزور"),
        ] {
            let mut m = theme();
            match field {
                "author" => m["author"]["name"] = json!(value),
                "locale" => m["locales"] = json!({ "ar": { "name": value } }),
                _ => m["name"] = json!(value),
            }
            let issues = validate_plugin_manifest(&m, None, false).unwrap_err();
            assert!(
                issues.iter().any(|i| i.contains("reserved for official plugins")),
                "{value}: {issues:?}"
            );
            // the same text is fine on an official plugin
            assert!(validate_plugin_manifest(&m, None, true).is_ok(), "{value}");
        }
        // ordinary names that merely contain the letters stay allowed
        let mut m = theme();
        m["name"] = json!("Night Owl for chats");
        assert!(validate_plugin_manifest(&m, None, false).is_ok());
    }

    #[test]
    fn only_settled_accounts_count_towards_a_takedown() {
        let now = 1_800_000_000_000;
        let old = ulid::Ulid::from_parts((now - 30 * 86_400_000) as u64, 1).to_string();
        let new = ulid::Ulid::from_parts((now - 86_400_000) as u64, 1).to_string();
        let reports = vec![old.clone(), new.clone(), "not-an-id".to_string(), old];
        // each account once, however often it appears
        assert_eq!(counted_plugin_reports(&reports, now), 1);
    }

    #[test]
    fn worker_integrity_must_match() {
        let mut m = theme();
        m["entrypoint"] = json!({ "type": "worker", "file": "main.js" });
        m["integrity"] = json!({ "sha256": sha256_hex(b"console.log(1)") });
        assert!(validate_plugin_manifest(&m, Some("console.log(1)"), false).is_ok());
        let issues = validate_plugin_manifest(&m, Some("console.log(2)"), false).unwrap_err();
        assert!(issues.iter().any(|i| i.contains("does not match")));
        // declarative can't smuggle code
        assert!(validate_plugin_manifest(&theme(), Some("x"), false).is_err());
    }

    #[test]
    fn unsupported_versions() {
        let mut m = theme();
        m["api_version"] = json!(2);
        m["min_app_version"] = json!("99.0.0");
        m["manifest_version"] = json!(7);
        let issues = validate_plugin_manifest(&m, None, false).unwrap_err();
        assert_eq!(issues.len(), 3);
    }

    #[test]
    fn scanner_blocks_secrets() {
        let findings = scan_plugin_package("{}", Some("const t = 'x-session-token';\nfetch('x')"));
        assert!(findings.iter().any(|f| f.block && f.rule == "session-header"));
        assert!(findings.iter().any(|f| !f.block && f.rule == "network" && f.line == 2));
    }

    #[test]
    fn settings_are_checked() {
        let mut m = theme();
        m["permissions"] = json!(["theme.colors", "ui.commands", "storage.settings"]);
        m["contributes"]["settings"] = json!([
            { "key": "accent", "type": "color", "title": "Accent", "default": "#7aa2ff" },
            { "key": "level", "type": "number", "title": "Level", "default": 1, "min": 0, "max": 3 }
        ]);
        m["contributes"]["theme"]["colors"]["primary"] = json!("$setting:accent");
        let v = validate_plugin_manifest(&m, None, false).expect("valid");
        assert!(clean_plugin_settings(&v.settings, &json!({ "accent": "#000000", "x": 1 })).is_ok());
        assert!(clean_plugin_settings(&v.settings, &json!({ "level": 9 })).is_err());
        m["contributes"]["theme"]["colors"]["primary"] = json!("$setting:level");
        assert!(validate_plugin_manifest(&m, None, false).is_err());
    }

    #[test]
    fn versions_compare() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_plugin_versions("1.2.0", "1.10.0"), Less);
        assert_eq!(compare_plugin_versions("1.0.0-beta", "1.0.0"), Less);
        assert_eq!(compare_plugin_versions("2.0.0", "1.9.9"), Greater);
    }
}
