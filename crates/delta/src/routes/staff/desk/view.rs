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
}
