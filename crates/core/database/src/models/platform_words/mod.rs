//! The platform-wide word list. The Zrzur team keeps it in the admin console
//! and decides appeals on it. It runs where no Server's
//! AutoMod can: on public profile text. Matching is the same as AutoMod's.

mod ops;

pub use ops::*;

use revolt_models::v0::{PlatformWord, PlatformWordCategory};
use revolt_result::Result;

use sha2::{Digest, Sha256};

use crate::{util::automod::word_list_match, Database};

/// The starter list, built in so a server can never start without it
const WORD_SEED: &str = include_str!("../../../assets/platform_words_seed.txt");

/// The category of the first entry that matches a text, if any
pub fn match_platform_words(text: &str, words: &[PlatformWord]) -> Option<PlatformWordCategory> {
    let blocked: Vec<(String, PlatformWordCategory)> = words
        .iter()
        .filter(|word| !word.allow)
        .map(|word| (word.pattern.clone(), word.category))
        .collect();
    let allow: Vec<String> = words
        .iter()
        .filter(|word| word.allow)
        .map(|word| word.pattern.clone())
        .collect();
    word_list_match(text, &blocked, &allow)
}

/// Run a text through the list as it stands now
pub async fn check_platform_words(db: &Database, text: &str) -> Result<Option<PlatformWordCategory>> {
    Ok(match_platform_words(text, &db.fetch_platform_words().await?))
}

/// Entries the second version of the starter list dropped, and those it added
pub const WORD_SEED_REMOVED_V2: &[&str] = &["يا كلب", "يا حمار", "عاهرة", "انتحر"];
pub const WORD_SEED_ADDED_V2: &[&str] = &["steam gift", "zrzur plus free", "free zrzur plus"];

/// Read the starter file: `[Category]` headings, one entry on each line
fn parse_word_seed(text: &str) -> Result<Vec<(String, PlatformWordCategory)>, String> {
    let mut category = None;
    let mut entries = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            category = Some(match name {
                "Harassment" => PlatformWordCategory::Harassment,
                "HateOrThreats" => PlatformWordCategory::HateOrThreats,
                "SexualContent" => PlatformWordCategory::SexualContent,
                "Spam" => PlatformWordCategory::Spam,
                "Other" => PlatformWordCategory::Other,
                other => return Err(format!("unknown category {other}")),
            });
            continue;
        }
        let Some(category) = category else {
            return Err(format!("{line} has no category"));
        };
        entries.push((line.to_string(), category));
    }
    Ok(entries)
}

/// Starter entries, a draft for the Zrzur team to review in the admin
/// console: clear threats and abuse in Arabic, English and German. They live
/// in `assets/platform_words_seed.txt`, which moderators can edit. A seed
/// writes them once; the list is the team's from then on. The id comes from
/// the entry, so two servers starting at once write one entry, not two.
pub fn platform_word_seed(now: i64) -> Result<Vec<PlatformWord>, String> {
    Ok(parse_word_seed(WORD_SEED)?
        .into_iter()
        .map(|(pattern, category)| PlatformWord {
            id: format!("seed-{}", &hex::encode(Sha256::digest(&pattern))[..24]),
            pattern,
            category,
            allow: false,
            note: Some("Starter list".to_string()),
            added_by: "system".to_string(),
            created_at: now,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(pattern: &str, allow: bool) -> PlatformWord {
        PlatformWord {
            id: pattern.to_string(),
            pattern: pattern.to_string(),
            category: PlatformWordCategory::Harassment,
            allow,
            note: None,
            added_by: "staff".to_string(),
            created_at: 0,
        }
    }

    #[test]
    fn the_starter_list_catches_arabic_english_and_german() {
        let seed = platform_word_seed(0).expect("the starter file reads");
        assert!(match_platform_words("اقتل  نفسك", &seed).is_some());
        assert!(match_platform_words("ابن الكلب", &seed).is_some());
        assert!(match_platform_words("just KILL YOURSELF now", &seed).is_some());
        assert!(match_platform_words("du hurensohn", &seed).is_some());
        assert!(match_platform_words("صباح الخير يا صديقي", &seed).is_none());
    }

    #[test]
    fn the_allow_list_clears_a_false_positive() {
        let words = vec![word("*كلب*", false), word("كلبنا", true)];
        assert!(match_platform_words("هذا كلبك", &words).is_some());
        assert!(match_platform_words("هذا كلبنا", &words).is_none());
    }

    #[test]
    fn the_starter_file_keeps_every_entry_of_the_first_list() {
        let seed = platform_word_seed(0).expect("the starter file reads");
        assert_eq!(seed.len(), 23);
        let ids: std::collections::HashSet<_> = seed.iter().map(|word| word.id.clone()).collect();
        assert_eq!(ids.len(), seed.len(), "an entry is listed twice");
        let count = |category| seed.iter().filter(|word| word.category == category).count();
        assert_eq!(count(PlatformWordCategory::HateOrThreats), 8);
        assert_eq!(count(PlatformWordCategory::Harassment), 6);
        assert_eq!(count(PlatformWordCategory::SexualContent), 4);
        assert_eq!(count(PlatformWordCategory::Spam), 5);
        for removed in WORD_SEED_REMOVED_V2 {
            assert!(seed.iter().all(|word| word.pattern != *removed), "{removed}");
        }
        for added in WORD_SEED_ADDED_V2 {
            assert!(seed.iter().any(|word| word.pattern == *added), "{added}");
        }
        assert!(seed.iter().all(|word| !word.allow && word.added_by == "system"));
    }

    #[test]
    fn the_starter_file_refuses_what_it_cannot_read() {
        assert!(parse_word_seed("[Nonsense]\nword").is_err());
        assert!(parse_word_seed("word before a heading").is_err());
        let entries = parse_word_seed("# note\n\n[Spam]\n  free nitro  \n").unwrap();
        assert_eq!(entries, vec![("free nitro".to_string(), PlatformWordCategory::Spam)]);
    }

    #[test]
    fn the_word_that_left_the_list_is_refused_nowhere() {
        let seed = platform_word_seed(0).expect("the starter file reads");
        for text in ["سوف انتحر", "سوف إنتحر", "سوف انتحـــر"] {
            assert_eq!(match_platform_words(text, &seed), None, "{text}");
        }
    }

    #[test]
    fn spam_entries_are_on_the_list() {
        let seed = platform_word_seed(0).expect("the starter file reads");
        for text in ["steam gift", "zrzur plus free", "free zrzur plus"] {
            assert_eq!(match_platform_words(text, &seed), Some(PlatformWordCategory::Spam), "{text}");
        }
    }

    /// One matching rule, in the list's own terms
    fn listed(text: &str, entry: &str) -> bool {
        match_platform_words(text, &[word(entry, false)]).is_some()
    }

    #[test]
    fn hamza_forms_and_bare_hamza_are_ignored_in_text_and_entry() {
        assert!(listed("أبن الكلب", "ابن الكلب"));
        assert!(listed("سأقتلك", "ساقتلك"));
        assert!(listed("سئول", "سيول"));
        assert!(listed("شيء سيء", "شي"));
        assert!(listed("سماء", "سما"));
        assert!(listed("سما", "سماء"));
    }

    #[test]
    fn tashkeel_tatweel_ta_marbuta_and_alif_maqsura_are_ignored() {
        assert!(listed("حَقِيْر", "حقير"));
        assert!(listed("حقـــير", "حقير"));
        assert!(listed("شرموطه", "شرموطة"));
        assert!(listed("مستوي", "مستوى"));
    }

    #[test]
    fn repeated_letters_are_ignored_and_doubled_words_still_match() {
        assert!(listed("حقييييير", "حقير"));
        assert!(listed("حققير", "حقير"));
        assert!(listed("fuuuuck you", "fuck you"));
        assert!(listed("freeeee nitrooo", "free nitro"));
        assert!(listed("killlll yourself", "kill yourself"));
        // real words with doubled letters
        assert!(listed("ممتاز", "ممتاز"));
        assert!(listed("كل مدد", "مدد"));
        assert!(listed("kill yourself", "kill yourself"));
        assert!(listed("ass", "ass"));
        assert!(listed("asss", "ass"));
    }

    #[test]
    fn arabizi_spellings_match_the_starter_list() {
        let seed = platform_word_seed(0).expect("the starter file reads");
        for text in [
            "kos omak",
            "kos ommak",
            "kos ummak",
            "kosomak",
            "sharmota",
            "sharmouta",
            "sharmoota",
            "charmouta",
            "ya 7aqeer ya",
        ] {
            assert!(match_platform_words(text, &seed).is_some(), "{text}");
        }
    }

    #[test]
    fn normal_words_stay_clean() {
        let seed = platform_word_seed(0).expect("the starter file reads");
        for text in [
            "صباح الخير يا صديقي",
            "مدرسة جميلة وممتازة",
            "كلب لطيف",
            "حمار صغير",
            "kiss mike",
            "as good as it gets",
            "sharm el sheikh",
            "class passed",
            "ابن عمي",
        ] {
            assert_eq!(match_platform_words(text, &seed), None, "{text}");
        }
        // two letters are no repetition outside Arabic
        assert!(!listed("as", "ass"));
        assert!(!listed("ass", "as"));
        assert!(!listed("good", "god"));
    }
}
