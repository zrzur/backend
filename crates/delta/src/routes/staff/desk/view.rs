//! What every desk command answers: a [`DeskView`]. The bot renders any
//! view the same way (title, fields, lines, page, buttons) in a private
//! answer. A view carries no personal data unless its handler adds it under
//! the console's own rules (for example an email only with `UsersPiiRead`).

use serde::Serialize;

/// One `name: value` row
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeskField {
    pub name: String,
    pub value: String,
}

/// How a button looks; the bot maps it to its own button styles
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeskButtonStyle {
    Primary,
    /// For later views (paging buttons)
    #[allow(dead_code)]
    Secondary,
    Danger,
}

/// A button the bot shows under the answer. Its `action_id` becomes the
/// button's custom id (`zd:...`); a press is handed back to the server,
/// which reads nothing from it but the id it made itself.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeskButton {
    pub label: String,
    pub action_id: String,
    pub style: DeskButtonStyle,
}

/// The generic answer of a desk command
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct DeskView {
    /// The whole answer as one message text, made by the server
    /// ([`canonical_render`]): the bot shows exactly this. A confirmation
    /// card counts only while its message is exactly this text.
    pub content: String,
    pub title: String,
    pub fields: Vec<DeskField>,
    pub lines: Vec<String>,
    /// 1-based page, for lists that page
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    pub buttons: Vec<DeskButton>,
}

impl DeskView {
    pub fn new(title: impl Into<String>) -> Self {
        DeskView {
            title: title.into(),
            ..Default::default()
        }
    }

    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push(DeskField {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    pub fn line(mut self, line: impl Into<String>) -> Self {
        self.lines.push(line.into());
        self
    }

    /// Sets `content` from the title, fields and lines
    pub fn finish(mut self) -> Self {
        self.content = canonical_render(&self.title, &self.fields, &self.lines);
        self
    }
}

/// The longest text a bot may post (the platform's message limit for bots)
pub const CONTENT_MAX: usize = 2000;

/// Markdown, maths and mention characters: no value can format, link or
/// mention
const MARKDOWN: &[char] = &[
    '\\', '*', '_', '~', '`', '|', '[', ']', '(', ')', '<', '>', '#', '@', '-', '$',
];

/// Characters that are dropped: bidirectional controls (they can reorder
/// what a reader sees) and zero-width characters (they can hide text)
fn invisible(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

/// `text` with every markdown and mention character escaped, invisible
/// controls dropped, and line breaks (also U+2028, U+2029 and U+0085)
/// turned into spaces: shown as written, on one line. The marks that set
/// the direction of one word (U+200E, U+200F, U+061C) stay: Arabic names
/// need them.
pub fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{0085}') {
            out.push(' ');
            continue;
        }
        if invisible(c) {
            continue;
        }
        if MARKDOWN.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Cuts `part` to `keep` characters and ends it with `…` (never right after
/// an escaping backslash)
fn cut(part: &str, keep: usize) -> String {
    let mut out: String = part.chars().take(keep).collect();
    while out.ends_with('\\') {
        out.pop();
    }
    out.push('…');
    out
}

/// The one text of a view: the title in bold, then `name: value` per field,
/// then each line; everything escaped (values may come from people). Never
/// longer than [`CONTENT_MAX`]: while it is, the longest field or line (the
/// first of equals) is cut with `…`, so the same view always gives the same
/// text and the card can always be posted.
pub fn canonical_render(title: &str, fields: &[DeskField], lines: &[String]) -> String {
    let head = format!(
        "**{}**",
        escape_markdown(&title.chars().take(200).collect::<String>())
    );
    let mut parts: Vec<String> = fields
        .iter()
        .map(|field| {
            format!(
                "{}: {}",
                escape_markdown(&field.name),
                escape_markdown(&field.value)
            )
        })
        .chain(lines.iter().map(|line| escape_markdown(line)))
        .collect();
    let total = |parts: &[String]| {
        head.chars().count() + parts.iter().map(|p| p.chars().count() + 1).sum::<usize>()
    };
    while total(&parts) > CONTENT_MAX {
        let excess = total(&parts) - CONTENT_MAX;
        let Some((index, longest)) = parts
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.chars().count()))
            .fold(None, |best: Option<(usize, usize)>, (i, n)| match best {
                Some((_, m)) if m >= n => best,
                _ => Some((i, n)),
            })
        else {
            break;
        };
        // nothing left to cut (only reachable with thousands of lines)
        if longest <= 2 {
            break;
        }
        // one character more for the `…`
        let keep = longest.saturating_sub(excess + 1).max(1);
        parts[index] = cut(&parts[index], keep);
    }
    std::iter::once(head)
        .chain(parts)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desk_render_escapes_people_text() {
        let lines = vec!["Note **bold** <@01ABC> [x](https://e.x) $x^2$\nnext".to_string()];
        let fields = vec![DeskField {
            name: "User".to_string(),
            value: "a_b".to_string(),
        }];
        assert_eq!(
            canonical_render("Title", &fields, &lines),
            r"**Title**
User: a\_b
Note \*\*bold\*\* \<\@01ABC\> \[x\]\(https://e.x\) \$x^2\$ next"
        );
    }

    /// Bidirectional controls and zero-width characters never reach the card
    #[test]
    fn desk_render_drops_invisible_characters() {
        let line = "safe\u{202E}gnp.exe\u{2066}x\u{2069}\u{200B}\u{200C}\u{200D}\u{2060}\u{FEFF}y"
            .to_string();
        assert_eq!(canonical_render("T", &[], &[line]), "**T**\nsafegnp.exexy");
        // other line breaks are spaces: one value never starts a new line
        let breaks = "a\u{2028}b\u{2029}c\u{0085}d".to_string();
        assert_eq!(canonical_render("T", &[], &[breaks]), "**T**\na b c d");
        // direction marks of one word stay
        let marks = "\u{200E}x\u{200F}y\u{061C}z".to_string();
        assert_eq!(
            canonical_render("T", &[], &[marks.clone()]),
            format!("**T**\n{marks}")
        );
    }

    /// A card with a huge value still fits the message limit, keeps the
    /// server's own lines whole, and is the same text every time
    #[test]
    fn desk_render_fits_the_message_limit() {
        let huge = format!("Writes the note \"{}\" about x.", "*".repeat(5000));
        let lines = vec![
            huge,
            "This cannot be undone.".to_string(),
            "Press Confirm within 10 minutes.".to_string(),
        ];
        let text = canonical_render("Test change", &[], &lines);
        assert!(
            text.chars().count() <= CONTENT_MAX,
            "{}",
            text.chars().count()
        );
        assert!(text.ends_with("This cannot be undone.\nPress Confirm within 10 minutes."));
        assert!(text.contains('…'));
        assert!(!text.contains("\\…"));
        assert_eq!(text, canonical_render("Test change", &[], &lines));
        // short ones are untouched
        assert_eq!(canonical_render("T", &[], &["a".to_string()]), "**T**\na");
    }
}
