//! Text folded for search, so a person finds a word or a name however it was
//! typed: any form of alef, ta marbuta or ha, alef maqsura or ya, with or
//! without vowel marks and tatweel, Persian letter forms, Arabic-Indic or
//! Latin digits, with or without the definite article.
//!
//! Only the copy used for the comparison is folded. Stored text never changes.

/// Marks that carry no letter: vowel marks (tashkeel), the hamza and madda
/// that stand above or below a letter, Quranic marks, tatweel, and the
/// invisible joiners and direction marks
fn is_dropped(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'
        | '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06ED}'
        | '\u{0640}'
        | '\u{061C}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2069}'
        | '\u{FEFF}')
}

/// One character, folded; `None` when it is dropped
fn fold_char(c: char) -> Option<char> {
    if is_dropped(c) {
        return None;
    }

    Some(match c {
        'أ' | 'إ' | 'آ' | 'ٱ' | 'ٲ' | 'ٳ' | 'ٵ' => 'ا',
        'ى' | 'ی' | 'ے' | 'ۓ' | 'ئ' | 'ۍ' | 'ې' => 'ي',
        'ة' | 'ۀ' | 'ہ' | 'ۂ' | 'ۃ' | 'ھ' | 'ە' => 'ه',
        'ک' | 'ڪ' | 'ػ' | 'ؼ' => 'ك',
        'ؤ' => 'و',
        '٠'..='٩' => char::from(b'0' + (c as u32 - 0x0660) as u8),
        '۰'..='۹' => char::from(b'0' + (c as u32 - 0x06F0) as u8),
        c if c.is_whitespace() => ' ',
        c => c,
    })
}

/// Fold a text for comparison
pub fn fold_text(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars().filter_map(fold_char) {
        if c == ' ' && (folded.is_empty() || folded.ends_with(' ')) {
            continue;
        }
        folded.extend(c.to_lowercase());
    }

    folded.truncate(folded.trim_end().len());
    folded
}

/// The folded word without a leading definite article, if it has one
fn without_article(word: &str) -> &str {
    match word.strip_prefix("ال") {
        Some(rest) if rest.chars().count() > 1 => rest,
        _ => word,
    }
}

/// Whether what was typed is found in a text: every word typed has to be
/// there, and a word typed with the definite article also finds the text
/// without it
pub fn text_matches(text: &str, query: &str) -> bool {
    let haystack = fold_text(text);
    fold_text(query)
        .split(' ')
        .filter(|word| !word.is_empty())
        .all(|word| haystack.contains(word) || haystack.contains(without_article(word)))
}

/// Every stored character that folds to this folded character
fn spellings(folded: char) -> Option<&'static str> {
    Some(match folded {
        'ا' => "اأإآٱٲٳٵ",
        'ي' => "يىیےۓئۍې",
        'ه' => "هةۀہۂۃھە",
        'ك' => "كکڪػؼ",
        'و' => "وؤ",
        '0' => "0٠۰",
        '1' => "1١۱",
        '2' => "2٢۲",
        '3' => "3٣۳",
        '4' => "4٤۴",
        '5' => "5٥۵",
        '6' => "6٦۶",
        '7' => "7٧۷",
        '8' => "8٨۸",
        '9' => "9٩۹",
        _ => return None,
    })
}

/// A regular expression (for a case-insensitive database match) that finds
/// what was typed in a stored name, whatever spelling the name was saved in.
/// Marks and tatweel may stand between any two letters of the stored name.
pub fn fold_regex(query: &str) -> String {
    // the marks of `is_dropped` that a stored name can hold
    const MARKS: &str = "[\u{0610}-\u{061A}\u{064B}-\u{065F}\u{0670}\u{06D6}-\u{06ED}\u{0640}\u{200C}-\u{200F}]*";

    let folded = fold_text(query);
    let mut pattern = String::new();
    for (index, word) in folded.split(' ').filter(|w| !w.is_empty()).enumerate() {
        if index > 0 {
            pattern.push_str("\\s+");
        }

        let bare = without_article(word);
        if bare.len() != word.len() {
            pattern.push_str("(?:");
            push_letters(&mut pattern, "ال", MARKS);
            pattern.push_str(")?");
        }

        push_letters(&mut pattern, bare, MARKS);
    }

    pattern
}

fn push_letters(pattern: &mut String, letters: &str, marks: &str) {
    for c in letters.chars() {
        match spellings(c) {
            Some(class) => {
                pattern.push('[');
                pattern.push_str(class);
                pattern.push(']');
            }
            None => pattern.push_str(&regex::escape(&c.to_string())),
        }

        pattern.push_str(marks);
    }
}

/// Most terms one search may ask the text index for
const MAX_TERMS: usize = 240;

/// Words Arabic writes joined to the next word: the article, and, with, for
const JOINED: [&str; 8] = ["", "ال", "و", "ب", "ل", "وال", "بال", "لل"];

/// The search string for the messages text index. The index holds each word
/// as it was written, so the words typed are widened to the spellings the
/// same word is stored under: forms of alef, a final ta marbuta or ha, a final
/// alef maqsura or ya, Persian kaf and ya, both digit sets, and the word with
/// and without the definite article and the joined particles.
///
/// A query that uses the index's own syntax (a quoted phrase, a `-word`) is
/// passed on as typed.
pub fn search_terms(query: &str) -> String {
    if query.contains('"') || query.split_whitespace().any(|word| word.starts_with('-')) {
        return query.to_string();
    }

    let mut terms: Vec<String> = Vec::new();
    let mut push = |terms: &mut Vec<String>, term: String| {
        if !term.is_empty() && terms.len() < MAX_TERMS && !terms.contains(&term) {
            terms.push(term);
        }
    };

    let words: Vec<&str> = query.split_whitespace().collect();

    // what was typed comes first, so the cap never drops it
    for word in &words {
        let typed: String = word.chars().filter(|c| !is_dropped(*c)).collect();
        push(&mut terms, typed);
    }

    for word in &words {
        let folded = fold_text(word);
        if !folded.chars().any(is_arabic_or_digit) {
            continue;
        }

        let bare = without_article(&folded);
        for joined in JOINED {
            for spelling in word_spellings(bare) {
                let term = format!("{joined}{spelling}");
                for digits in digit_forms(&term) {
                    push(&mut terms, persian_form(&digits));
                    push(&mut terms, digits);
                }
            }
        }
    }

    terms.join(" ")
}

fn is_arabic_or_digit(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '\u{0600}'..='\u{06FF}')
}

/// The spellings one folded word is stored under
fn word_spellings(folded: &str) -> Vec<String> {
    let letters: Vec<char> = folded.chars().collect();
    let last = letters.len().saturating_sub(1);
    let mut spellings: Vec<Vec<char>> = vec![Vec::with_capacity(letters.len())];
    let mut alefs = 0;

    for (index, &letter) in letters.iter().enumerate() {
        let forms: &[char] = match letter {
            // the first alef of a word carries any hamza; later ones seldom
            // carry one below. Past the second alef the word is kept as typed
            'ا' if alefs == 0 && index == 0 => &['ا', 'أ', 'إ', 'آ'],
            'ا' if alefs < 2 => &['ا', 'أ', 'آ'],
            'ه' if index == last && last > 0 => &['ه', 'ة'],
            'ي' if index == last && last > 0 => &['ي', 'ى'],
            _ => &[],
        };

        if letter == 'ا' {
            alefs += 1;
        }

        if forms.is_empty() {
            for spelling in &mut spellings {
                spelling.push(letter);
            }
        } else {
            spellings = spellings
                .into_iter()
                .flat_map(|spelling| {
                    forms.iter().map(move |form| {
                        let mut next = spelling.clone();
                        next.push(*form);
                        next
                    })
                })
                .collect();
        }
    }

    spellings.into_iter().map(|s| s.into_iter().collect()).collect()
}

/// A term in Latin, Arabic-Indic and Persian digits (itself when it has none)
fn digit_forms(term: &str) -> Vec<String> {
    if !term.chars().any(|c| c.is_ascii_digit()) {
        return vec![term.to_string()];
    }

    let shifted = |zero: u32| -> String {
        term.chars()
            .map(|c| match c.to_digit(10) {
                Some(d) if c.is_ascii_digit() => char::from_u32(zero + d).unwrap_or(c),
                _ => c,
            })
            .collect()
    };

    vec![term.to_string(), shifted(0x0660), shifted(0x06F0)]
}

/// The term as a Persian or Urdu keyboard writes it
fn persian_form(term: &str) -> String {
    term.chars()
        .map(|c| match c {
            'ك' => 'ک',
            'ي' => 'ی',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &str, b: &str) {
        assert_eq!(fold_text(a), fold_text(b), "{a} and {b}");
    }

    #[test]
    fn letters_fold_one_by_one() {
        for (typed, folded) in [
            ("أ", "ا"),
            ("إ", "ا"),
            ("آ", "ا"),
            ("ٱ", "ا"),
            ("ة", "ه"),
            ("ى", "ي"),
            ("ی", "ي"),
            ("ے", "ي"),
            ("ئ", "ي"),
            ("ؤ", "و"),
            ("ک", "ك"),
            ("ڪ", "ك"),
            ("ہ", "ه"),
            ("ھ", "ه"),
        ] {
            assert_eq!(fold_text(typed), folded, "{typed}");
        }
    }

    #[test]
    fn marks_and_tatweel_are_dropped() {
        for mark in [
            '\u{064B}', '\u{064C}', '\u{064D}', '\u{064E}', '\u{064F}', '\u{0650}', '\u{0651}',
            '\u{0652}', '\u{0653}', '\u{0654}', '\u{0655}', '\u{0670}', '\u{06D6}', '\u{06ED}',
            '\u{0640}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{202E}', '\u{2067}',
            '\u{061C}',
        ] {
            assert_eq!(fold_text(&format!("كت{mark}اب")), "كتاب", "U+{:04X}", mark as u32);
        }
    }

    #[test]
    fn digits_fold_to_latin() {
        assert_eq!(fold_text("٠١٢٣٤٥٦٧٨٩"), "0123456789");
        assert_eq!(fold_text("۰۱۲۳۴۵۶۷۸۹"), "0123456789");
        assert_eq!(fold_text("0123456789"), "0123456789");
    }

    #[test]
    fn different_letters_stay_different() {
        for (a, b) in [("ب", "ت"), ("ح", "خ"), ("ع", "غ"), ("ك", "گ"), ("ه", "ح"), ("ء", "ا")] {
            assert_ne!(fold_text(a), fold_text(b), "{a} {b}");
        }
    }

    #[test]
    fn whole_words_fold_the_ways_people_type_them() {
        same("أحمد", "احمد");
        same("إيمان", "ايمان");
        same("آمنة", "امنه");
        same("مدرسة", "مدرسه");
        same("مصطفى", "مصطفي");
        same("مُحَمَّدٌ", "محمد");
        same("مرحبـــــا", "مرحبا");
        same("کریم", "كريم");
        same("غرفة ٢٠٢٤", "غرفه 2024");
        same("ZAID", "zaid");
        same("  two   words ", "two words");
    }

    #[test]
    fn text_matches_with_and_without_the_article() {
        assert!(text_matches("الكتاب على الطاولة", "كتاب"));
        assert!(text_matches("كتاب جديد", "الكتاب"));
        assert!(text_matches("أحمد الخطيب", "احمد خطيب"));
        assert!(!text_matches("أحمد الخطيب", "احمد سالم"));
        assert!(text_matches("الموعد يوم ١٥", "15"));
        assert!(!text_matches("سلمى", "سلمان"));
        assert!(text_matches("anything", "  "));
    }

    fn finds(stored: &str, typed: &str) -> bool {
        regex::RegexBuilder::new(&fold_regex(typed))
            .case_insensitive(true)
            .build()
            .expect("valid pattern")
            .is_match(stored)
    }

    #[test]
    fn the_name_pattern_finds_every_spelling() {
        assert!(finds("مكتبة الأصدقاء", "مكتبه الاصدقاء"));
        assert!(finds("مكتبة الأصدقاء", "اصدقاء"));
        assert!(finds("مَكتَبة", "مكتبة"));
        assert!(finds("مكتبـــة", "مكتبة"));
        assert!(finds("أصدقاء مصطفى", "مصطفي"));
        assert!(finds("کتاب", "كتاب"));
        assert!(finds("غرفة ٢٠٢٤", "2024"));
        assert!(finds("غرفة 2024", "٢٠٢٤"));
        assert!(finds("ملتقى اللاعبين", "لاعبين"));
        assert!(finds("ملتقى لاعبين", "اللاعبين"));
        assert!(finds("Zrzur Arabic", "zrzur"));
        assert!(!finds("مكتبة", "مدرسة"));
    }

    #[test]
    fn the_name_pattern_takes_typed_symbols_as_text() {
        assert!(finds("C++ (عربي)", "c++ (عربي)"));
        assert!(!finds("abc", ".*z"));
        assert!(regex::Regex::new(&fold_regex("[a-")).is_ok());
    }

    fn terms(query: &str) -> Vec<String> {
        search_terms(query).split(' ').map(str::to_string).collect()
    }

    #[test]
    fn search_terms_cover_stored_spellings() {
        // (as stored in a message, as typed in the search field)
        for (stored, typed) in [
            ("المدرسة", "المدرسه"),
            ("المدرسة", "مدرسة"),
            ("المدرسة", "مدرسه"),
            ("إلى", "الى"),
            ("أحمد", "احمد"),
            ("الكتاب", "كتاب"),
            ("كتاب", "الكتاب"),
            ("والكتاب", "كتاب"),
            ("بالكتاب", "الكتاب"),
            ("للكتاب", "كتاب"),
            ("مصطفى", "مصطفي"),
            ("المقهى", "مقهي"),
            ("١٥", "15"),
            ("15", "١٥"),
            ("۱۵", "15"),
            ("2024", "٢٠٢٤"),
            ("کتاب", "كتاب"),
            ("فارسی", "فارسي"),
            ("آمنة", "امنة"),
            ("آمنة", "آمنه"),
            ("القرآن", "قران"),
            ("مسألة", "مساله"),
            ("الكتاب", "الكتـاب"),
            ("محمد", "مُحَمَّد"),
        ] {
            assert!(terms(typed).contains(&stored.to_string()), "{typed} should reach {stored}");
        }
    }

    #[test]
    fn search_terms_keep_what_was_typed_first() {
        assert_eq!(terms("كتاب أحمد")[..2], ["كتاب".to_string(), "أحمد".to_string()]);
        assert_eq!(search_terms("hello world"), "hello world");
    }

    #[test]
    fn search_terms_leave_index_syntax_alone() {
        assert_eq!(search_terms("\"الكتاب الجديد\""), "\"الكتاب الجديد\"");
        assert_eq!(search_terms("كتاب -قديم"), "كتاب -قديم");
    }

    #[test]
    fn search_terms_are_capped() {
        let long = "استاذ اباء اخبار اسماء اعلام اثار اراء اداب اماكن ازهار";
        assert!(terms(long).len() <= MAX_TERMS);
        // every typed word survives the cap
        for word in long.split(' ') {
            assert!(terms(long).contains(&word.to_string()));
        }
    }
}
