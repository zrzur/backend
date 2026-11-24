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

/// Markdown and mention characters: no value can format, link or mention
const MARKDOWN: &[char] = &['\\', '*', '_', '~', '`', '|', '[', ']', '(', ')', '<', '>', '#', '@', '-'];

/// `text` with every markdown and mention character escaped and line
/// breaks turned into spaces: shown as written, on one line
pub fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\n' || c == '\r' {
            out.push(' ');
            continue;
        }
        if MARKDOWN.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The one text of a view: the title in bold, then `name: value` per field,
/// then each line; everything escaped (values may come from people)
pub fn canonical_render(title: &str, fields: &[DeskField], lines: &[String]) -> String {
    let mut out = vec![format!("**{}**", escape_markdown(title))];
    for field in fields {
        out.push(format!("{}: {}", escape_markdown(&field.name), escape_markdown(&field.value)));
    }
    for line in lines {
        out.push(escape_markdown(line));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desk_render_escapes_people_text() {
        let lines = vec!["Note **bold** <@01ABC> [x](https://e.x)\nnext".to_string()];
        let fields = vec![DeskField {
            name: "User".to_string(),
            value: "a_b".to_string(),
        }];
        assert_eq!(
            canonical_render("Title", &fields, &lines),
            r"**Title**
User: a\_b
Note \*\*bold\*\* \<\@01ABC\> \[x\]\(https://e.x\) next"
        );
    }
}
