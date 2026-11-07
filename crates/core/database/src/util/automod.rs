//! AutoMod: per-server rules checked before a message is saved.
//!
//! Rules are compiled once per server and kept until they change (the cache
//! key is a fingerprint of the rules), so a message costs a few string scans.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};

use iso8601_timestamp::{Duration, Timestamp};
use redis_kiss::{get_connection, AsyncCommands};
use revolt_models::v0::{AutoModRule, AutoModTrigger};
use revolt_result::{create_error, Result};

use crate::{
    AuditLogEntryAction, Database, Member, PartialMember, Server, SystemMessage, User,
};

/// Longest timeout AutoMod gives (as for moderators)
const MAX_TIMEOUT_MINUTES: u32 = 28 * 24 * 60;

/// Lower case, Arabic letter variants folded, marks and invisible
/// characters removed, so «أهلاً» and «اهلا» compare equal
pub fn normalise(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            // tashkeel (harakat, tanwin, shadda, sukun, dagger alif), tatweel
            // and hamza on its own
            '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{0640}' | 'ء' => None,
            // invisible characters
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' => None,
            // hamza forms of alif, waw and ya
            'أ' | 'إ' | 'آ' | 'ٱ' => Some('ا'),
            'ؤ' => Some('و'),
            'ئ' => Some('ي'),
            // ta marbuta and alif maqsura
            'ة' => Some('ه'),
            'ى' => Some('ي'),
            c => Some(c),
        })
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_arabic_letter(c: char) -> bool {
    ('\u{0621}'..='\u{064A}').contains(&c)
}

/// Letters Arabizi can't tell apart become one, a final ة (already ه) goes,
/// and a doubled letter counts once
fn fold_skeleton(skeleton: &str) -> String {
    let mut letters: Vec<char> = skeleton
        .chars()
        .map(|c| match c {
            'ط' => 'ت',
            'ص' => 'س',
            'ض' => 'د',
            'ظ' => 'ز',
            c => c,
        })
        .collect();
    letters.dedup();
    if letters.len() > 1 && letters.last() == Some(&'ه') {
        letters.pop();
    }
    letters.into_iter().collect()
}

/// One word of an Arabic keyword as Arabizi writes it
#[derive(Clone)]
struct SkeletonWord {
    consonants: String,
    /// It starts with a long vowel, which Arabizi writes too: «امك» is
    /// "omak", and "mike" is something else
    starts_with_vowel: bool,
}

/// A keyword in Arabic letters without its long vowels and hamza, as Arabizi
/// usually writes it, word by word. Short skeletons would match too much.
fn skeleton_of(keyword: &str) -> Option<Vec<SkeletonWord>> {
    let words: Vec<SkeletonWord> = keyword
        .split_whitespace()
        .map(|word| {
            word.chars().all(is_arabic_letter).then(|| {
                let consonants: String = word
                    .chars()
                    .filter(|c| !matches!(c, 'ا' | 'و' | 'ي' | 'ء'))
                    .collect();
                SkeletonWord {
                    consonants: fold_skeleton(&consonants),
                    starts_with_vowel: word.starts_with(['ا', 'و', 'ي']),
                }
            })
        })
        .collect::<Option<_>>()?;
    let letters: usize = words.iter().map(|w| w.consonants.chars().count()).sum();
    (letters >= 3).then_some(words)
}

/// Whether Latin words spell an Arabic keyword: word by word, or written as
/// one word ("kosomak")
fn arabizi_matches(keyword: &[SkeletonWord], words: &[(&str, Option<String>)]) -> bool {
    let together: String = keyword.iter().map(|w| w.consonants.as_str()).collect();
    let as_one = words
        .iter()
        .any(|(_, skeleton)| skeleton.as_deref() == Some(together.as_str()));
    as_one
        || words.windows(keyword.len()).any(|window| {
            window.iter().zip(keyword).all(|((word, skeleton), wanted)| {
                skeleton.as_deref() == Some(wanted.consonants.as_str())
                    && (!wanted.starts_with_vowel || word.starts_with(['a', 'e', 'i', 'o', 'u', '2']))
            })
        })
}

/// The Arabic consonants of a word written in Latin letters and digits, such
/// as "kalb" or "7mar"; `None` for anything else
fn arabizi_skeleton(word: &str) -> Option<String> {
    if !word.is_ascii() || !word.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let bytes = word.as_bytes();
    let mut out = String::new();
    let mut at = 0;
    while at < bytes.len() {
        let digraph = match bytes.get(at..at + 2) {
            Some(b"sh") | Some(b"ch") => Some('ش'),
            Some(b"kh") => Some('خ'),
            Some(b"gh") => Some('غ'),
            Some(b"th") => Some('ث'),
            Some(b"dh") => Some('ذ'),
            _ => None,
        };
        if let Some(letter) = digraph {
            out.push(letter);
            at += 2;
            continue;
        }
        let letter = match bytes[at] {
            // vowels and the hamza digit are not part of the skeleton
            b'a' | b'e' | b'i' | b'o' | b'u' | b'y' | b'w' | b'2' => None,
            b'3' => Some('ع'),
            b'5' => Some('خ'),
            b'6' => Some('ط'),
            b'7' => Some('ح'),
            b'8' => Some('ق'),
            b'9' => Some('ص'),
            b'b' | b'p' => Some('ب'),
            b't' => Some('ت'),
            b'j' | b'g' => Some('ج'),
            b'd' => Some('د'),
            b'r' => Some('ر'),
            b'z' => Some('ز'),
            b's' => Some('س'),
            b'f' | b'v' => Some('ف'),
            b'q' => Some('ق'),
            b'k' => Some('ك'),
            b'l' => Some('ل'),
            b'm' => Some('م'),
            b'n' => Some('ن'),
            b'h' => Some('ه'),
            _ => return None,
        };
        out.extend(letter);
        at += 1;
    }
    Some(out)
}

/// Letters split apart to get past a filter, as in "س ب ا م" or "s.p.a.m",
/// joined again. Runs of three or more single letters count.
fn join_split_letters(text: &str) -> String {
    let mut out: Vec<String> = vec![];
    let mut run: Vec<&str> = vec![];
    let flush = |out: &mut Vec<String>, run: &mut Vec<&str>| {
        if run.len() >= 3 {
            out.push(run.concat());
        } else {
            out.extend(run.iter().map(|letter| letter.to_string()));
        }
        run.clear();
    };
    for token in text.split(|c: char| !is_word_char(c)).filter(|t| !t.is_empty()) {
        if token.chars().count() == 1 {
            run.push(token);
        } else {
            flush(&mut out, &mut run);
            out.push(token.to_string());
        }
    }
    flush(&mut out, &mut run);
    out.join(" ")
}

/// Whether a keyword rule matches: the text itself, with split letters
/// joined, and Arabic keywords in their Arabizi spelling
fn keywords_match(text: &str, patterns: &[Pattern], allow: &HashSet<String>) -> bool {
    let joined = join_split_letters(text);
    let spelled = [text, joined.as_str()].into_iter().any(|view| {
        patterns
            .iter()
            .filter_map(|pattern| pattern.find(view))
            .any(|word| !allow.contains(&word))
    });
    if spelled {
        return true;
    }
    let words: Vec<(&str, Option<String>)> = text
        .split(|c: char| !is_word_char(c))
        .filter(|word| !word.is_empty())
        .map(|word| {
            let skeleton = (!allow.contains(word))
                .then(|| arabizi_skeleton(word))
                .flatten();
            (word, skeleton.map(|skeleton| fold_skeleton(&skeleton)))
        })
        .collect();
    patterns
        .iter()
        .filter_map(|pattern| pattern.skeleton.as_deref())
        .any(|keyword| arabizi_matches(keyword, &words))
}

/// First entry of a word list that matches a text, with the same matching
/// as server AutoMod: folded letters, split letters, Arabizi. Words on
/// `allow` never count.
pub fn word_list_match<T: Copy>(text: &str, list: &[(String, T)], allow: &[String]) -> Option<T> {
    let text = normalise(text);
    let allow: HashSet<String> = allow.iter().map(|word| normalise(word.trim_matches('*'))).collect();
    list.iter().find_map(|(raw, tag)| {
        let pattern = Pattern::parse(raw)?;
        keywords_match(&text, std::slice::from_ref(&pattern), &allow).then_some(*tag)
    })
}

/// One keyword: `*` at the start or end lets it match part of a word
#[derive(Clone)]
struct Pattern {
    prefix_open: bool,
    suffix_open: bool,
    /// The letters of `text` with how often each comes in a row
    runs: Vec<(char, usize)>,
    /// Consonants of a whole Arabic keyword, to match its Arabizi spelling
    skeleton: Option<Vec<SkeletonWord>>,
}

/// `text` cut into runs of one letter
fn runs_of(text: &str) -> Vec<(char, usize)> {
    let mut runs: Vec<(char, usize)> = vec![];
    for letter in text.chars() {
        match runs.last_mut() {
            Some((last, count)) if *last == letter => *count += 1,
            _ => runs.push((letter, 1)),
        }
    }
    runs
}

/// How many bytes at the start of `text` spell `runs`, letting a letter be
/// repeated to get past a filter. In Arabic a doubled letter already counts,
/// because a written word uses shadda. Elsewhere three in a row do, so "as"
/// never finds "ass" and "god" never finds "good".
fn repeated_match(text: &str, runs: &[(char, usize)], open_end: bool) -> Option<usize> {
    let mut used = 0;
    for (index, &(letter, wanted)) in runs.iter().enumerate() {
        let found = text[used..].chars().take_while(|c| *c == letter).count();
        let stretched = found > wanted && (is_arabic_letter(letter) || found >= 3);
        let word_goes_on = open_end && index + 1 == runs.len() && found >= wanted;
        if found != wanted && !stretched && !word_goes_on {
            return None;
        }
        used += found * letter.len_utf8();
    }
    Some(used)
}

impl Pattern {
    fn parse(raw: &str) -> Option<Pattern> {
        let raw = raw.trim();
        let prefix_open = raw.starts_with('*');
        let suffix_open = raw.ends_with('*');
        let text = normalise(raw.trim_matches('*').trim());
        let skeleton = (!prefix_open && !suffix_open).then(|| skeleton_of(&text)).flatten();
        (!text.is_empty()).then_some(Pattern {
            runs: runs_of(&text),
            prefix_open,
            suffix_open,
            skeleton,
        })
    }

    /// The matched word (expanded to word boundaries), if any
    fn find(&self, haystack: &str) -> Option<String> {
        let first = self.runs[0].0;
        for (begin, letter) in haystack.char_indices() {
            if letter != first {
                continue;
            }
            let Some(length) = repeated_match(&haystack[begin..], &self.runs, self.suffix_open) else {
                continue;
            };
            let end = begin + length;
            let before_ok = self.prefix_open
                || haystack[..begin].chars().next_back().is_none_or(|c| !is_word_char(c));
            let after_ok =
                self.suffix_open || haystack[end..].chars().next().is_none_or(|c| !is_word_char(c));
            if before_ok && after_ok {
                // the whole word around the match, for the allow-list
                let word_start = haystack[..begin]
                    .char_indices()
                    .rev()
                    .find(|(_, c)| !is_word_char(*c))
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                let word_end = haystack[end..]
                    .char_indices()
                    .find(|(_, c)| !is_word_char(*c))
                    .map(|(i, _)| end + i)
                    .unwrap_or(haystack.len());
                return Some(haystack[word_start..word_end].to_string());
            }
        }
        None
    }
}

enum Compiled {
    Keywords {
        patterns: Vec<Pattern>,
        allow: HashSet<String>,
    },
    Other,
}

struct CompiledRule {
    rule: AutoModRule,
    compiled: Compiled,
}

type Cache = HashMap<String, (u64, Arc<Vec<CompiledRule>>)>;
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn fingerprint(rules: &[AutoModRule]) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(rules)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

fn compile(rules: &[AutoModRule]) -> Vec<CompiledRule> {
    rules
        .iter()
        .filter(|rule| rule.enabled)
        .map(|rule| CompiledRule {
            compiled: match &rule.trigger {
                AutoModTrigger::Keywords { words, allow } => Compiled::Keywords {
                    patterns: words.iter().filter_map(|w| Pattern::parse(w)).collect(),
                    allow: allow.iter().map(|w| normalise(w.trim())).collect(),
                },
                _ => Compiled::Other,
            },
            rule: rule.clone(),
        })
        .collect()
}

/// The server's rules, compiled once and reused until they change
fn compiled_rules(server: &Server) -> Arc<Vec<CompiledRule>> {
    let rules = server.automod.as_deref().unwrap_or_default();
    let print = fingerprint(rules);
    let mut cache = CACHE.lock().unwrap();
    if let Some((cached, compiled)) = cache.get(&server.id) {
        if *cached == print {
            return compiled.clone();
        }
    }
    let compiled = Arc::new(compile(rules));
    cache.insert(server.id.clone(), (print, compiled.clone()));
    compiled
}

/// Different user and role mentions in a message
fn count_mentions(content: &str) -> usize {
    let mut seen = HashSet::new();
    let mut rest = content;
    while let Some(at) = rest.find("<@") {
        rest = &rest[at + 2..];
        let role = rest.starts_with('&');
        let body = if role { &rest[1..] } else { rest };
        let id: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !id.is_empty() && body[id.len()..].starts_with('>') {
            seen.insert((role, id));
        }
    }
    seen.len()
        + content.contains("@everyone") as usize
        + content.contains("@online") as usize
}

/// Hosts of the links in a message (http, https or www.), lower case
fn link_hosts(content: &str) -> Vec<String> {
    let lower = content.to_lowercase();
    let mut hosts = vec![];
    for (at, _) in lower.match_indices("http://").chain(lower.match_indices("https://")) {
        let rest = &lower[at..];
        let rest = &rest[rest.find("//").map(|i| i + 2).unwrap_or(0)..];
        hosts.push(host_of(rest));
    }
    for (at, _) in lower.match_indices("www.") {
        // skip the www. inside a URL that was already counted
        if !lower[..at].ends_with("//") {
            hosts.push(host_of(&lower[at..]));
        }
    }
    hosts.retain(|host| host.contains('.'));
    hosts
}

fn host_of(rest: &str) -> String {
    let end = rest
        .find(|c: char| c.is_whitespace() || "/:?#>)'\"],;".contains(c))
        .unwrap_or(rest.len());
    rest[..end].trim_end_matches('.').to_string()
}

fn domain_allowed(host: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|domain| {
        let domain = domain.trim().trim_start_matches("*.").to_lowercase();
        host == domain || host.ends_with(&format!(".{domain}"))
    })
}

/// Whether the same text was posted in `channels` channels within `seconds`
async fn repeated(server: &str, user: &str, channel: &str, text: &str, channels: u32, seconds: u32) -> bool {
    if text.trim().is_empty() || channels < 2 {
        return false;
    }
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let key = format!("automod:repeat:{server}:{user}:{:x}", hasher.finish());
    let Ok(mut redis) = get_connection().await else {
        return false;
    };
    let _: std::result::Result<usize, _> = redis.sadd(&key, channel).await;
    let ttl = (seconds.max(1) as i64).try_into().unwrap_or(60);
    let _: std::result::Result<(), _> = redis.expire(&key, ttl).await;
    redis
        .scard::<_, usize>(&key)
        .await
        .is_ok_and(|count| count >= channels as usize)
}

/// What a message is checked for: a new message also counts towards the
/// repeated-messages rule, an edit doesn't
pub enum Check {
    Send,
    Edit,
}

/// Check a message against the server's rules and carry out the actions of
/// every rule it matches. Err(AutoModBlocked) when one of them blocks it.
pub async fn check_message(
    db: &Database,
    server: &Server,
    channel: &str,
    user: &User,
    member: Option<&Member>,
    content: &str,
    check: Check,
) -> Result<()> {
    // bots and the owner are not checked
    if server.automod.as_ref().is_none_or(Vec::is_empty) || user.bot.is_some() || user.id == server.owner {
        return Ok(());
    }
    let Some(member) = member else {
        return Ok(());
    };

    apply_rules(db, server, channel, &user.id, &user.id, Some(member), content, check).await
}

/// AutoMod for a webhook message (or edit). Same rules as for a member, and
/// nobody is exempt: a token can be in anyone's hands. A webhook can't be
/// timed out and has no roles; alerts and the audit log name its creator.
pub async fn check_webhook_message(
    db: &Database,
    server: &Server,
    channel: &str,
    webhook_id: &str,
    creator_id: &str,
    content: &str,
    check: Check,
) -> Result<()> {
    if server.automod.as_ref().is_none_or(Vec::is_empty) {
        return Ok(());
    }
    apply_rules(db, server, channel, webhook_id, creator_id, None, content, check).await
}

/// Run the server's rules. `sender` tells repeats apart, `shown` is who
/// alerts and the audit log name.
#[allow(clippy::too_many_arguments)]
async fn apply_rules(
    db: &Database,
    server: &Server,
    channel: &str,
    sender: &str,
    shown: &str,
    member: Option<&Member>,
    content: &str,
    check: Check,
) -> Result<()> {
    let rules = compiled_rules(server);
    let text = normalise(content);
    let mut blocked: Option<String> = None;

    for CompiledRule { rule, compiled } in rules.iter() {
        if rule.exempt_channels.iter().any(|id| id == channel)
            || member.is_some_and(|member| {
                rule.exempt_roles.iter().any(|role| member.roles.contains(role))
            })
        {
            continue;
        }

        let matched = match (&rule.trigger, compiled) {
            (AutoModTrigger::Keywords { .. }, Compiled::Keywords { patterns, allow }) => {
                keywords_match(&text, patterns, allow)
            }
            (AutoModTrigger::MentionSpam { max }, _) => count_mentions(content) > *max as usize,
            (
                AutoModTrigger::LinkSpam {
                    new_member_minutes,
                    allowed_domains,
                },
                _,
            ) => {
                let hosts = link_hosts(content);
                !hosts.is_empty()
                    && ((!allowed_domains.is_empty()
                        && hosts.iter().any(|host| !domain_allowed(host, allowed_domains)))
                        || new_member_minutes.zip(member).is_some_and(|(minutes, member)| {
                            member.joined_at + Duration::minutes(minutes as i64)
                                > Timestamp::now_utc()
                        }))
            }
            (AutoModTrigger::RepeatedMessages { channels, seconds }, _) => {
                matches!(check, Check::Send)
                    && repeated(&server.id, sender, channel, &text, *channels, *seconds).await
            }
            _ => false,
        };
        if !matched {
            continue;
        }

        let mut done = vec![];

        if let Some(alert) = &rule.actions.alert_channel {
            SystemMessage::AutoModAlert {
                user: shown.to_string(),
                rule: rule.name.clone(),
                channel: channel.to_string(),
                content: content.chars().take(300).collect(),
            }
            .into_message(alert.clone())
            .send_without_notifications(db, None, None, false, false, false)
            .await
            .ok();
            done.push("alert".to_string());
        }

        if let Some((minutes, member)) = rule.actions.timeout_minutes.zip(member) {
            let until = Timestamp::now_utc()
                + Duration::minutes(minutes.clamp(1, MAX_TIMEOUT_MINUTES) as i64);
            let mut member = member.clone();
            if member
                .update(
                    db,
                    PartialMember {
                        timeout: Some(until),
                        ..Default::default()
                    },
                    vec![],
                )
                .await
                .is_ok()
            {
                done.push("timeout".to_string());
            }
        }

        if rule.actions.block {
            done.push("block".to_string());
            blocked.get_or_insert(rule.name.clone());
        }

        AuditLogEntryAction::AutoModAction {
            rule: rule.name.clone(),
            user: shown.to_string(),
            channel: channel.to_string(),
            actions: done,
        }
        .insert(db, server.id.clone(), None, shown.to_string(), Some(shown.to_string()))
        .await;
    }

    match blocked {
        Some(rule) => Err(create_error!(AutoModBlocked { rule })),
        None => Ok(()),
    }
}

/// Check new rules before they are saved
pub fn validate_rules(server: &Server, rules: &[AutoModRule]) -> Result<()> {
    let invalid = || Err(create_error!(InvalidProperty));
    if rules.len() > 10 {
        return invalid();
    }
    let mut ids = HashSet::new();
    for rule in rules {
        if rule.id.is_empty()
            || rule.id.len() > 32
            || !ids.insert(&rule.id)
            || rule.name.trim().is_empty()
            || rule.name.len() > 64
        {
            return invalid();
        }
        match &rule.trigger {
            AutoModTrigger::Keywords { words, allow } => {
                if words.is_empty()
                    || words.len() > 1000
                    || allow.len() > 1000
                    || words.iter().chain(allow).any(|w| w.len() > 60)
                {
                    return invalid();
                }
            }
            AutoModTrigger::MentionSpam { max } => {
                if *max == 0 || *max > 50 {
                    return invalid();
                }
            }
            AutoModTrigger::LinkSpam {
                new_member_minutes,
                allowed_domains,
            } => {
                if new_member_minutes.is_none() && allowed_domains.is_empty()
                    || new_member_minutes.is_some_and(|m| m == 0 || m > 7 * 24 * 60)
                    || allowed_domains.len() > 100
                {
                    return invalid();
                }
            }
            AutoModTrigger::RepeatedMessages { channels, seconds } => {
                if *channels < 2 || *channels > 20 || *seconds == 0 || *seconds > 3600 {
                    return invalid();
                }
            }
        }
        let actions = &rule.actions;
        if !actions.block && actions.alert_channel.is_none() && actions.timeout_minutes.is_none()
        {
            return invalid();
        }
        if actions
            .timeout_minutes
            .is_some_and(|m| m == 0 || m > MAX_TIMEOUT_MINUTES)
        {
            return invalid();
        }
        let channels = actions.alert_channel.iter().chain(&rule.exempt_channels);
        if channels.into_iter().any(|id| !server.channels.contains(id))
            || rule.exempt_roles.iter().any(|id| !server.roles.contains_key(id))
        {
            return Err(create_error!(NotFound));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{count_mentions, normalise, Pattern};
    use super::word_list_match;

    #[test]
    fn a_word_list_reports_the_entry_that_matched() {
        let list = vec![("spamword".to_string(), 1), ("كلمة".to_string(), 2)];
        assert_eq!(word_list_match("this is S P A M W O R D", &list, &[]), Some(1));
        assert_eq!(word_list_match("هذه كلمه", &list, &[]), Some(2));
        assert_eq!(word_list_match("harmless", &list, &[]), None);
        let open = vec![("*ass*".to_string(), 1)];
        assert_eq!(word_list_match("class", &open, &[]), Some(1));
        assert_eq!(word_list_match("class", &open, &["class".to_string()]), None);
    }

    #[test]
    fn arabic_letters_are_folded() {
        assert_eq!(normalise("أهلاً"), normalise("اهلا"));
        assert_eq!(normalise("إسلام"), normalise("اسلام"));
        assert_eq!(normalise("مدرسة"), normalise("مدرسه"));
        assert_eq!(normalise("على"), normalise("علي"));
        assert_eq!(normalise("كـــلام"), normalise("كلام"));
        assert_eq!(normalise("مُحَمَّد"), normalise("محمد"));
        assert_eq!(normalise("SPAM"), "spam");
    }

    #[test]
    fn split_letters_are_joined() {
        assert_eq!(super::join_split_letters("هذا ك ل ب حقا"), "هذا كلب حقا");
        assert_eq!(super::join_split_letters("s.p.a.m"), "spam");
        assert_eq!(super::join_split_letters("a b is fine"), "a b is fine");
    }

    fn matches(word: &str, text: &str) -> bool {
        let patterns = vec![Pattern::parse(word).unwrap()];
        super::keywords_match(&normalise(text), &patterns, &Default::default())
    }

    #[test]
    fn spacing_tricks_do_not_hide_a_word() {
        assert!(matches("كلب", "انت ك ل ب"));
        assert!(matches("كلب", "انت ك.ل.ب"));
        assert!(matches("spam", "s p a m"));
        assert!(!matches("spam", "s pam"));
    }

    #[test]
    fn arabizi_spells_arabic_words() {
        assert!(matches("كلب", "you are kalb"));
        assert!(matches("حمار", "ya 7mar"));
        assert!(!matches("كلب", "a club meeting"));
        // words of two letters are too short to tell apart
        assert!(!matches("أب", "ab"));
    }

    #[test]
    fn the_allow_list_covers_arabizi() {
        let patterns = vec![Pattern::parse("كلب").unwrap()];
        let allow = HashSet::from(["kalb".to_string()]);
        assert!(!super::keywords_match("kalb", &patterns, &allow));
    }

    #[test]
    fn whole_words_unless_a_star_opens_them() {
        let word = Pattern::parse("spam").unwrap();
        assert!(word.find("this is spam!").is_some());
        assert!(word.find("spammer").is_none());
        let prefix = Pattern::parse("spam*").unwrap();
        assert_eq!(prefix.find("a spammer").as_deref(), Some("spammer"));
        let inner = Pattern::parse("*oo*").unwrap();
        assert!(inner.find("food").is_some());
        let phrase = Pattern::parse("free nitro").unwrap();
        assert!(phrase.find("get free nitro now").is_some());
    }

    #[test]
    fn link_hosts_are_found() {
        assert_eq!(
            super::link_hosts("see https://Example.com/x and www.spam.io, http://a.b:80"),
            vec!["a.b", "example.com", "www.spam.io"]
        );
        assert!(super::link_hosts("no links here.").is_empty());
        assert!(super::domain_allowed("cdn.zrzur.com", &["zrzur.com".to_string()]));
        assert!(!super::domain_allowed("zrzur.com.evil.io", &["zrzur.com".to_string()]));
    }

    #[test]
    fn mentions_are_counted_once_each() {
        assert_eq!(count_mentions("<@A> <@A> <@B> <@&R>"), 3);
        assert_eq!(count_mentions("hi @everyone @everyone"), 1);
        assert_eq!(count_mentions("no mentions"), 0);
    }
}
