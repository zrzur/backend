//! Zrzur developer platform: message components and modals.
//!
//! Only bots send components. Everything in them is untrusted data: clients
//! render text through the normal message Markdown (never as HTML) and load
//! images through the media proxy only.
//!
//! A message is a list of blocks. Rows hold buttons or one select; text,
//! sections, galleries, files and separators lay out content; a container
//! groups blocks into one card. Forms (modals) hold fields with a label.

use std::collections::HashSet;

use super::CommandChannelType;

auto_derived!(
    /// Visual style of a button
    #[derive(Copy)]
    pub enum ButtonStyle {
        Primary,
        Secondary,
        Success,
        Danger,
        /// Opens `url`; produces no interaction
        Link,
    }

    /// Size of a text input
    #[derive(Copy)]
    pub enum TextInputStyle {
        /// One line
        Short,
        /// Several lines
        Paragraph,
    }

    /// Space a separator adds
    #[derive(Copy, Default)]
    pub enum SeparatorSpacing {
        #[default]
        Small,
        Large,
    }

    /// Entry of a string select, radio group or checkbox group
    pub struct SelectOption {
        pub label: String,
        pub value: String,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub emoji: Option<String>,
        /// Selected initially
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub default: bool,
    }

    /// Who may use a component; empty lists mean anyone who can see the message
    #[derive(Default)]
    pub struct ComponentRestriction {
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub users: Vec<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
        pub roles: Vec<String>,
    }

    /// An image shown in a gallery or next to a section.
    ///
    /// Either a web link (loaded through the media proxy) or a file attached
    /// to the same message.
    pub struct MediaItem {
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub url: Option<String>,
        /// Id of a file attached to this message
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub attachment: Option<String>,
        /// Alternative text for screen readers
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub description: Option<String>,
        /// Hidden until the reader reveals it
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub spoiler: bool,
    }

    /// Interactive element: in a row, next to a section, or in a form field
    #[cfg_attr(feature = "serde", serde(tag = "type"))]
    pub enum Component {
        Button {
            style: ButtonStyle,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            label: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            emoji: Option<String>,
            /// Sent back to the bot; required unless the style is `Link`
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            custom_id: Option<String>,
            /// Only for `Link` buttons
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            url: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            disabled: bool,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            restrict: Option<ComponentRestriction>,
        },
        StringSelect {
            custom_id: String,
            options: Vec<SelectOption>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            placeholder: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            disabled: bool,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            restrict: Option<ComponentRestriction>,
        },
        UserSelect {
            custom_id: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            placeholder: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
            default_values: Vec<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            disabled: bool,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            restrict: Option<ComponentRestriction>,
        },
        RoleSelect {
            custom_id: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            placeholder: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
            default_values: Vec<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            disabled: bool,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            restrict: Option<ComponentRestriction>,
        },
        MentionableSelect {
            custom_id: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            placeholder: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
            default_values: Vec<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            disabled: bool,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            restrict: Option<ComponentRestriction>,
        },
        ChannelSelect {
            custom_id: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            placeholder: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
            default_values: Vec<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            channel_types: Option<Vec<CommandChannelType>>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            disabled: bool,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            restrict: Option<ComponentRestriction>,
        },
        /// Only inside forms
        TextInput {
            custom_id: String,
            style: TextInputStyle,
            /// Required in a bare row; inside a `Field` the field's label is used
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "String::is_empty", default))]
            label: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            placeholder: Option<String>,
            #[cfg_attr(feature = "serde", serde(default = "crate::v0::components::default_true"))]
            required: bool,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_length: Option<u16>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_length: Option<u16>,
            /// Prefilled value
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            value: Option<String>,
        },
        /// Exactly one choice; only inside forms
        RadioGroup {
            custom_id: String,
            options: Vec<SelectOption>,
            #[cfg_attr(feature = "serde", serde(default = "crate::v0::components::default_true"))]
            required: bool,
        },
        /// Any number of ticks within the range; only inside forms
        CheckboxGroup {
            custom_id: String,
            options: Vec<SelectOption>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(default = "crate::v0::components::default_true"))]
            required: bool,
        },
        /// A single yes / no tick; only inside forms
        Checkbox {
            custom_id: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            default: bool,
        },
        /// Files the user uploads with the form; only inside forms
        FileUpload {
            custom_id: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            min_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            max_values: Option<u8>,
            #[cfg_attr(feature = "serde", serde(default = "crate::v0::components::default_true"))]
            required: bool,
            /// `image`, `video`, `audio` or an extension such as `.pdf`; empty allows any file
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
            file_types: Vec<String>,
        },
    }

    /// Block of a message's components or of a form.
    ///
    /// Tagged so later blocks can be added without breaking old clients,
    /// which skip blocks they do not know.
    #[cfg_attr(feature = "serde", serde(tag = "type"))]
    pub enum ComponentBlock {
        /// Up to five buttons, or one select
        ActionRow { components: Vec<Component> },
        /// Markdown text
        Text { content: String },
        /// One to three paragraphs with a button or a thumbnail at the end
        Section {
            text: Vec<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            button: Option<Component>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            thumbnail: Option<MediaItem>,
        },
        /// One to ten images in a grid
        Gallery { items: Vec<MediaItem> },
        /// A file attached to this message, shown in place
        File {
            attachment: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            spoiler: bool,
        },
        /// Space between blocks, with or without a line
        Separator {
            #[cfg_attr(feature = "serde", serde(default = "crate::v0::components::default_true"))]
            divider: bool,
            #[cfg_attr(feature = "serde", serde(default))]
            spacing: SeparatorSpacing,
        },
        /// A card around other blocks, with an optional colour strip
        Container {
            components: Vec<ComponentBlock>,
            /// Colour of the strip, `#RRGGBB`
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            accent: Option<String>,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
            spoiler: bool,
        },
        /// A form field with its label; only inside forms
        Field {
            label: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            description: Option<String>,
            component: Component,
        },
    }

    /// Form shown to a user as the answer to an interaction
    pub struct Modal {
        pub custom_id: String,
        pub title: String,
        pub components: Vec<ComponentBlock>,
    }

    /// Value of one form field as submitted by the user
    pub struct ModalFieldValue {
        pub custom_id: String,
        /// Text of a text input, or the choice of a radio group
        #[cfg_attr(feature = "serde", serde(default))]
        pub value: Option<String>,
        /// Chosen values of a select or checkbox group, or uploaded file ids
        #[cfg_attr(feature = "serde", serde(default))]
        pub values: Option<Vec<String>>,
        /// State of a single checkbox
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub checked: Option<bool>,
    }
);

pub fn default_true() -> bool {
    true
}

/// Component limits
pub mod component_limits {
    /// Rows at the top level, and rows inside one container
    pub const MAX_ROWS: usize = 5;
    /// Every block and component of a message, nested ones included
    pub const MAX_COMPONENTS: usize = 40;
    pub const MAX_BUTTONS_PER_ROW: usize = 5;
    pub const CUSTOM_ID_LENGTH: usize = 100;
    pub const LABEL_LENGTH: usize = 80;
    pub const URL_LENGTH: usize = 512;
    pub const EMOJI_LENGTH: usize = 32;
    pub const PLACEHOLDER_LENGTH: usize = 150;
    pub const MAX_SELECT_OPTIONS: usize = 25;
    pub const SELECT_TEXT_LENGTH: usize = 100;
    pub const MAX_SELECT_VALUES: u8 = 25;
    pub const MAX_RESTRICTED_IDS: usize = 25;
    pub const MODAL_TITLE_LENGTH: usize = 45;
    pub const TEXT_INPUT_LABEL_LENGTH: usize = 45;
    pub const TEXT_INPUT_LENGTH: u16 = 4000;
    /// All text of a message's text blocks and sections together
    pub const TEXT_LENGTH: usize = 4000;
    pub const MAX_SECTION_TEXTS: usize = 3;
    pub const MAX_GALLERY_ITEMS: usize = 10;
    pub const MEDIA_DESCRIPTION_LENGTH: usize = 1024;
    /// Fields of a form
    pub const MAX_MODAL_FIELDS: usize = 5;
    /// Fields and text blocks of a form
    pub const MAX_MODAL_BLOCKS: usize = 10;
    pub const FIELD_DESCRIPTION_LENGTH: usize = 100;
    pub const MAX_GROUP_OPTIONS: usize = 10;
    pub const MAX_UPLOAD_FILES: u8 = 10;
    pub const MAX_FILE_TYPES: usize = 10;
}

use component_limits as limits;

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn check_text(what: &str, text: &str, min: usize, max: usize) -> Result<(), String> {
    let len = chars(text);
    if len < min || len > max {
        return Err(format!("{what} must be {min}-{max} characters"));
    }
    if text.chars().any(|c| c.is_control() && c != '\n') {
        return Err(format!("{what} may not contain control characters"));
    }
    Ok(())
}

/// One-line text (menu entries, placeholders): no line breaks either
fn check_line(what: &str, text: &str, min: usize, max: usize) -> Result<(), String> {
    check_text(what, text, min, max)?;
    if text.contains('\n') {
        return Err(format!("{what} must be a single line"));
    }
    Ok(())
}

fn check_emoji(emoji: &Option<String>) -> Result<(), String> {
    if let Some(emoji) = emoji {
        check_text("emoji", emoji, 1, limits::EMOJI_LENGTH)?;
        if emoji.chars().any(char::is_whitespace) {
            return Err("emoji may not contain spaces".to_string());
        }
    }
    Ok(())
}

fn is_id(id: &str) -> bool {
    id.len() == 26 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn check_ids(what: &str, ids: &[String]) -> Result<(), String> {
    if ids.len() > limits::MAX_RESTRICTED_IDS {
        return Err(format!("{what}: at most {} ids", limits::MAX_RESTRICTED_IDS));
    }
    if ids.iter().any(|id| !is_id(id)) {
        return Err(format!("{what}: invalid id"));
    }
    let mut seen = HashSet::new();
    if !ids.iter().all(|id| seen.insert(id.as_str())) {
        return Err(format!("{what}: the same id twice"));
    }
    Ok(())
}

fn check_custom_id(custom_id: &str, seen: &mut HashSet<String>) -> Result<(), String> {
    check_text("custom_id", custom_id, 1, limits::CUSTOM_ID_LENGTH)?;
    if !seen.insert(custom_id.to_string()) {
        return Err(format!("duplicate custom_id '{custom_id}'"));
    }
    Ok(())
}

fn check_restrict(restrict: &Option<ComponentRestriction>) -> Result<(), String> {
    if let Some(restrict) = restrict {
        check_ids("restrict.users", &restrict.users)?;
        check_ids("restrict.roles", &restrict.roles)?;
    }
    Ok(())
}

fn check_url(url: &str) -> Result<(), String> {
    check_text("url", url, 1, limits::URL_LENGTH)?;
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.chars().any(char::is_whitespace)
    {
        return Err("url must be an http(s) link without spaces".to_string());
    }
    Ok(())
}

fn check_value_range(
    min: Option<u8>,
    max: Option<u8>,
    available: Option<usize>,
    defaults: usize,
) -> Result<(), String> {
    let min = min.unwrap_or(1);
    let max = max.unwrap_or(1);
    if max == 0 || max > limits::MAX_SELECT_VALUES || min > max {
        return Err("select needs 0 <= min_values <= max_values <= 25 and max_values >= 1".to_string());
    }
    if available.is_some_and(|n| (min as usize) > n || (max as usize) > n) {
        return Err("select asks for more values than it offers".to_string());
    }
    if defaults > max as usize {
        return Err("select has more defaults than max_values".to_string());
    }
    Ok(())
}

fn check_options(options: &[SelectOption], min: usize, max: usize) -> Result<(), String> {
    if options.len() < min || options.len() > max {
        return Err(format!("needs {min}-{max} options"));
    }
    let mut values = HashSet::new();
    for option in options {
        check_line("option label", &option.label, 1, limits::SELECT_TEXT_LENGTH)?;
        check_line("option value", &option.value, 1, limits::SELECT_TEXT_LENGTH)?;
        if let Some(description) = &option.description {
            check_line("option description", description, 1, limits::SELECT_TEXT_LENGTH)?;
        }
        check_emoji(&option.emoji)?;
        if !values.insert(option.value.as_str()) {
            return Err(format!("duplicate option value '{}'", option.value));
        }
    }
    Ok(())
}

/// `#RRGGBB`
fn check_colour(colour: &str) -> Result<(), String> {
    let bytes = colour.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
        return Err("accent must be a colour like #9075FF".to_string());
    }
    Ok(())
}

fn check_media(item: &MediaItem, attachments: &[String]) -> Result<(), String> {
    match (&item.url, &item.attachment) {
        (Some(url), None) => check_url(url)?,
        (None, Some(id)) => check_attachment(id, attachments)?,
        _ => return Err("an image needs either a url or an attachment".to_string()),
    }
    if let Some(description) = &item.description {
        check_text(
            "image description",
            description,
            1,
            limits::MEDIA_DESCRIPTION_LENGTH,
        )?;
    }
    Ok(())
}

fn check_attachment(id: &str, attachments: &[String]) -> Result<(), String> {
    if !attachments.iter().any(|attachment| attachment == id) {
        return Err("a block refers to a file that is not attached to this message".to_string());
    }
    Ok(())
}

impl Component {
    /// Whether this is a select of any kind
    pub fn is_select(&self) -> bool {
        matches!(
            self,
            Component::StringSelect { .. }
                | Component::UserSelect { .. }
                | Component::RoleSelect { .. }
                | Component::MentionableSelect { .. }
                | Component::ChannelSelect { .. }
        )
    }

    /// Whether this can only be used inside a form
    pub fn is_form_only(&self) -> bool {
        matches!(
            self,
            Component::TextInput { .. }
                | Component::RadioGroup { .. }
                | Component::CheckboxGroup { .. }
                | Component::Checkbox { .. }
                | Component::FileUpload { .. }
        )
    }

    pub fn custom_id(&self) -> Option<&str> {
        match self {
            Component::Button { custom_id, .. } => custom_id.as_deref(),
            Component::StringSelect { custom_id, .. }
            | Component::UserSelect { custom_id, .. }
            | Component::RoleSelect { custom_id, .. }
            | Component::MentionableSelect { custom_id, .. }
            | Component::ChannelSelect { custom_id, .. }
            | Component::TextInput { custom_id, .. }
            | Component::RadioGroup { custom_id, .. }
            | Component::CheckboxGroup { custom_id, .. }
            | Component::Checkbox { custom_id, .. }
            | Component::FileUpload { custom_id, .. } => Some(custom_id),
        }
    }

    pub fn is_disabled(&self) -> bool {
        match self {
            Component::Button { disabled, .. }
            | Component::StringSelect { disabled, .. }
            | Component::UserSelect { disabled, .. }
            | Component::RoleSelect { disabled, .. }
            | Component::MentionableSelect { disabled, .. }
            | Component::ChannelSelect { disabled, .. } => *disabled,
            _ => false,
        }
    }

    pub fn restriction(&self) -> Option<&ComponentRestriction> {
        match self {
            Component::Button { restrict, .. }
            | Component::StringSelect { restrict, .. }
            | Component::UserSelect { restrict, .. }
            | Component::RoleSelect { restrict, .. }
            | Component::MentionableSelect { restrict, .. }
            | Component::ChannelSelect { restrict, .. } => restrict.as_ref(),
            _ => None,
        }
    }

    /// Name of the component kind, as sent to bots
    pub fn kind_name(&self) -> &'static str {
        match self {
            Component::Button { .. } => "Button",
            Component::StringSelect { .. } => "StringSelect",
            Component::UserSelect { .. } => "UserSelect",
            Component::RoleSelect { .. } => "RoleSelect",
            Component::MentionableSelect { .. } => "MentionableSelect",
            Component::ChannelSelect { .. } => "ChannelSelect",
            Component::TextInput { .. } => "TextInput",
            Component::RadioGroup { .. } => "RadioGroup",
            Component::CheckboxGroup { .. } => "CheckboxGroup",
            Component::Checkbox { .. } => "Checkbox",
            Component::FileUpload { .. } => "FileUpload",
        }
    }

    /// `labelled`: inside a form field that carries the label
    fn validate(
        &self,
        in_modal: bool,
        labelled: bool,
        seen: &mut HashSet<String>,
    ) -> Result<(), String> {
        if self.is_form_only() && !in_modal {
            return Err(format!("{} is only allowed in forms", self.kind_name()));
        }
        if in_modal && self.restriction().is_some() {
            return Err("form fields cannot be restricted".to_string());
        }
        match self {
            Component::Button {
                style,
                label,
                emoji,
                custom_id,
                url,
                restrict,
                ..
            } => {
                if in_modal {
                    return Err("buttons are not allowed in forms".to_string());
                }
                if label.is_none() && emoji.is_none() {
                    return Err("a button needs a label or an emoji".to_string());
                }
                if let Some(label) = label {
                    check_text("button label", label, 1, limits::LABEL_LENGTH)?;
                }
                check_emoji(emoji)?;
                match (style, custom_id, url) {
                    (ButtonStyle::Link, None, Some(url)) => {
                        check_url(url)?;
                        if restrict.is_some() {
                            return Err("link buttons cannot be restricted".to_string());
                        }
                    }
                    (ButtonStyle::Link, _, _) => {
                        return Err("link buttons need a url and no custom_id".to_string())
                    }
                    (_, Some(custom_id), None) => check_custom_id(custom_id, seen)?,
                    _ => return Err("buttons need a custom_id and no url".to_string()),
                }
                check_restrict(restrict)
            }
            Component::StringSelect {
                custom_id,
                options,
                placeholder,
                min_values,
                max_values,
                restrict,
                ..
            } => {
                check_custom_id(custom_id, seen)?;
                if let Some(placeholder) = placeholder {
                    check_line("placeholder", placeholder, 1, limits::PLACEHOLDER_LENGTH)?;
                }
                check_options(options, 1, limits::MAX_SELECT_OPTIONS)?;
                let defaults = options.iter().filter(|o| o.default).count();
                check_value_range(*min_values, *max_values, Some(options.len()), defaults)?;
                check_restrict(restrict)
            }
            Component::UserSelect {
                custom_id,
                placeholder,
                min_values,
                max_values,
                default_values,
                restrict,
                ..
            }
            | Component::RoleSelect {
                custom_id,
                placeholder,
                min_values,
                max_values,
                default_values,
                restrict,
                ..
            }
            | Component::MentionableSelect {
                custom_id,
                placeholder,
                min_values,
                max_values,
                default_values,
                restrict,
                ..
            }
            | Component::ChannelSelect {
                custom_id,
                placeholder,
                min_values,
                max_values,
                default_values,
                restrict,
                ..
            } => {
                check_custom_id(custom_id, seen)?;
                if let Some(placeholder) = placeholder {
                    check_line("placeholder", placeholder, 1, limits::PLACEHOLDER_LENGTH)?;
                }
                check_ids("default_values", default_values)?;
                check_value_range(*min_values, *max_values, None, default_values.len())?;
                if let Component::ChannelSelect {
                    channel_types: Some(types),
                    ..
                } = self
                {
                    if types.is_empty() {
                        return Err("channel_types must not be empty".to_string());
                    }
                }
                check_restrict(restrict)
            }
            Component::TextInput {
                custom_id,
                label,
                placeholder,
                min_length,
                max_length,
                value,
                ..
            } => {
                check_custom_id(custom_id, seen)?;
                if !labelled || !label.is_empty() {
                    check_text("text input label", label, 1, limits::TEXT_INPUT_LABEL_LENGTH)?;
                }
                if let Some(placeholder) = placeholder {
                    check_text("placeholder", placeholder, 1, limits::SELECT_TEXT_LENGTH)?;
                }
                let min = min_length.unwrap_or(0);
                let max = max_length.unwrap_or(limits::TEXT_INPUT_LENGTH);
                if max == 0 || max > limits::TEXT_INPUT_LENGTH || min > max {
                    return Err("text input length range is invalid".to_string());
                }
                if let Some(value) = value {
                    if chars(value) > max as usize {
                        return Err("prefilled value is longer than max_length".to_string());
                    }
                }
                Ok(())
            }
            Component::RadioGroup {
                custom_id, options, ..
            } => {
                if !labelled {
                    return Err("a radio group needs a field label".to_string());
                }
                check_custom_id(custom_id, seen)?;
                check_options(options, 2, limits::MAX_GROUP_OPTIONS)?;
                if options.iter().filter(|o| o.default).count() > 1 {
                    return Err("a radio group has at most one default".to_string());
                }
                Ok(())
            }
            Component::CheckboxGroup {
                custom_id,
                options,
                min_values,
                max_values,
                required,
            } => {
                if !labelled {
                    return Err("a checkbox group needs a field label".to_string());
                }
                check_custom_id(custom_id, seen)?;
                check_options(options, 1, limits::MAX_GROUP_OPTIONS)?;
                let min = min_values.unwrap_or(if *required { 1 } else { 0 });
                let max = max_values.unwrap_or(options.len() as u8);
                if max == 0 || min > max || max as usize > options.len() {
                    return Err("checkbox group value range is invalid".to_string());
                }
                if *required && min == 0 {
                    return Err("a required checkbox group needs min_values >= 1".to_string());
                }
                Ok(())
            }
            Component::Checkbox { custom_id, .. } => {
                if !labelled {
                    return Err("a checkbox needs a field label".to_string());
                }
                check_custom_id(custom_id, seen)
            }
            Component::FileUpload {
                custom_id,
                min_values,
                max_values,
                required,
                file_types,
            } => {
                if !labelled {
                    return Err("a file upload needs a field label".to_string());
                }
                check_custom_id(custom_id, seen)?;
                let min = min_values.unwrap_or(if *required { 1 } else { 0 });
                let max = max_values.unwrap_or(1);
                if max == 0 || max > limits::MAX_UPLOAD_FILES || min > max {
                    return Err("file upload count range is invalid".to_string());
                }
                if *required && min == 0 {
                    return Err("a required file upload needs min_values >= 1".to_string());
                }
                if file_types.len() > limits::MAX_FILE_TYPES {
                    return Err("too many file types".to_string());
                }
                for kind in file_types {
                    let valid = matches!(kind.as_str(), "image" | "video" | "audio")
                        || (kind.len() >= 2
                            && kind.len() <= 16
                            && kind.starts_with('.')
                            && kind[1..].chars().all(|c| c.is_ascii_alphanumeric()));
                    if !valid {
                        return Err(format!("unknown file type '{kind}'"));
                    }
                }
                Ok(())
            }
        }
    }
}

/// Running totals while checking a message
struct Budget<'a> {
    seen: HashSet<String>,
    count: usize,
    text: usize,
    attachments: &'a [String],
}

impl Budget<'_> {
    fn take(&mut self, n: usize) -> Result<(), String> {
        self.count += n;
        if self.count > limits::MAX_COMPONENTS {
            return Err(format!(
                "at most {} components in a message",
                limits::MAX_COMPONENTS
            ));
        }
        Ok(())
    }

    fn take_text(&mut self, text: &str) -> Result<(), String> {
        check_text("text", text, 1, limits::TEXT_LENGTH)?;
        self.text += chars(text);
        if self.text > limits::TEXT_LENGTH {
            return Err(format!(
                "the text of all blocks is limited to {} characters",
                limits::TEXT_LENGTH
            ));
        }
        Ok(())
    }
}

fn check_row(components: &[Component], budget: &mut Budget) -> Result<(), String> {
    if components.is_empty() {
        return Err("a row needs at least one component".to_string());
    }
    let selects = components.iter().filter(|c| c.is_select()).count();
    if selects > 0 && components.len() > 1 {
        return Err("a select takes a whole row".to_string());
    }
    if components.len() > limits::MAX_BUTTONS_PER_ROW {
        return Err(format!(
            "at most {} buttons per row",
            limits::MAX_BUTTONS_PER_ROW
        ));
    }
    budget.take(components.len())?;
    for component in components {
        component.validate(false, false, &mut budget.seen)?;
    }
    Ok(())
}

/// Check a list of message blocks; `nested` inside a container
fn check_blocks(blocks: &[ComponentBlock], nested: bool, budget: &mut Budget) -> Result<(), String> {
    let rows = blocks
        .iter()
        .filter(|block| matches!(block, ComponentBlock::ActionRow { .. }))
        .count();
    if rows > limits::MAX_ROWS {
        return Err(format!("at most {} rows", limits::MAX_ROWS));
    }
    for block in blocks {
        budget.take(1)?;
        match block {
            ComponentBlock::ActionRow { components } => check_row(components, budget)?,
            ComponentBlock::Text { content } => budget.take_text(content)?,
            ComponentBlock::Section {
                text,
                button,
                thumbnail,
            } => {
                if text.is_empty() || text.len() > limits::MAX_SECTION_TEXTS {
                    return Err(format!(
                        "a section needs 1-{} paragraphs",
                        limits::MAX_SECTION_TEXTS
                    ));
                }
                for paragraph in text {
                    budget.take_text(paragraph)?;
                }
                match (button, thumbnail) {
                    (Some(_), Some(_)) => {
                        return Err("a section has a button or a thumbnail, not both".to_string())
                    }
                    (Some(button), None) => {
                        if !matches!(button, Component::Button { .. }) {
                            return Err("only a button can sit next to a section".to_string());
                        }
                        budget.take(1)?;
                        button.validate(false, false, &mut budget.seen)?;
                    }
                    (None, Some(thumbnail)) => {
                        budget.take(1)?;
                        check_media(thumbnail, budget.attachments)?;
                    }
                    (None, None) => {}
                }
            }
            ComponentBlock::Gallery { items } => {
                if items.is_empty() || items.len() > limits::MAX_GALLERY_ITEMS {
                    return Err(format!(
                        "a gallery needs 1-{} images",
                        limits::MAX_GALLERY_ITEMS
                    ));
                }
                for item in items {
                    check_media(item, budget.attachments)?;
                }
            }
            ComponentBlock::File { attachment, .. } => {
                check_attachment(attachment, budget.attachments)?
            }
            ComponentBlock::Separator { .. } => {}
            ComponentBlock::Container {
                components, accent, ..
            } => {
                if nested {
                    return Err("containers cannot be nested".to_string());
                }
                if components.is_empty() {
                    return Err("a container needs at least one block".to_string());
                }
                if let Some(accent) = accent {
                    check_colour(accent)?;
                }
                check_blocks(components, true, budget)?;
            }
            ComponentBlock::Field { .. } => {
                return Err("fields are only allowed in forms".to_string())
            }
        }
    }
    Ok(())
}

/// Whether nothing in these blocks can be used: only link buttons, text,
/// images, files, separators and cards. Webhooks, which no bot answers for,
/// may post these.
pub fn components_are_static(blocks: &[ComponentBlock]) -> bool {
    let link = |component: &Component| {
        matches!(
            component,
            Component::Button {
                style: ButtonStyle::Link,
                ..
            }
        )
    };
    blocks.iter().all(|block| match block {
        ComponentBlock::ActionRow { components } => components.iter().all(link),
        ComponentBlock::Section { button, .. } => button.as_ref().is_none_or(link),
        ComponentBlock::Container { components, .. } => components_are_static(components),
        ComponentBlock::Field { .. } => false,
        _ => true,
    })
}

/// Check the components of a message.
///
/// `attachments`: ids of the files attached to the same message, which
/// `File` blocks and images may refer to.
pub fn validate_message_components(
    blocks: &[ComponentBlock],
    attachments: &[String],
) -> Result<(), String> {
    let mut budget = Budget {
        seen: HashSet::new(),
        count: 0,
        text: 0,
        attachments,
    };
    check_blocks(blocks, false, &mut budget)
}

/// Every interactive component of these blocks, nested ones included
fn each_component<'a>(blocks: &'a [ComponentBlock], out: &mut Vec<&'a Component>) {
    for block in blocks {
        match block {
            ComponentBlock::ActionRow { components } => out.extend(components.iter()),
            ComponentBlock::Section {
                button: Some(button),
                ..
            } => out.push(button),
            ComponentBlock::Container { components, .. } => each_component(components, out),
            ComponentBlock::Field { component, .. } => out.push(component),
            _ => {}
        }
    }
}

impl Modal {
    pub fn validate_modal(&self) -> Result<(), String> {
        check_text("modal custom_id", &self.custom_id, 1, limits::CUSTOM_ID_LENGTH)?;
        check_text("modal title", &self.title, 1, limits::MODAL_TITLE_LENGTH)?;
        if self.components.is_empty() || self.components.len() > limits::MAX_MODAL_BLOCKS {
            return Err(format!(
                "a form needs 1-{} blocks",
                limits::MAX_MODAL_BLOCKS
            ));
        }
        let mut seen = HashSet::new();
        let mut fields = 0;
        let mut text = 0;
        for block in &self.components {
            match block {
                // older form layout: a row with one field that carries its own label
                ComponentBlock::ActionRow { components } => {
                    if components.len() != 1 {
                        return Err("each form row holds exactly one field".to_string());
                    }
                    fields += 1;
                    components[0].validate(true, false, &mut seen)?;
                }
                ComponentBlock::Field {
                    label,
                    description,
                    component,
                } => {
                    check_text("field label", label, 1, limits::TEXT_INPUT_LABEL_LENGTH)?;
                    if let Some(description) = description {
                        check_text(
                            "field description",
                            description,
                            1,
                            limits::FIELD_DESCRIPTION_LENGTH,
                        )?;
                    }
                    if matches!(component, Component::Button { .. }) {
                        return Err("buttons are not allowed in forms".to_string());
                    }
                    if component.is_disabled() {
                        return Err("form fields cannot be disabled".to_string());
                    }
                    fields += 1;
                    component.validate(true, true, &mut seen)?;
                }
                ComponentBlock::Text { content } => {
                    check_text("text", content, 1, limits::TEXT_LENGTH)?;
                    text += chars(content);
                    if text > limits::TEXT_LENGTH {
                        return Err("form text is too long".to_string());
                    }
                }
                _ => return Err("forms hold fields and text only".to_string()),
            }
        }
        if fields == 0 || fields > limits::MAX_MODAL_FIELDS {
            return Err(format!(
                "a form needs 1-{} fields",
                limits::MAX_MODAL_FIELDS
            ));
        }
        Ok(())
    }

    /// The fields of this form
    pub fn fields(&self) -> impl Iterator<Item = &Component> {
        let mut out = Vec::new();
        each_component(&self.components, &mut out);
        out.into_iter()
    }
}

/// Find a component by `custom_id`, nested ones included
pub fn find_component<'a>(blocks: &'a [ComponentBlock], custom_id: &str) -> Option<&'a Component> {
    let mut out = Vec::new();
    each_component(blocks, &mut out);
    out.into_iter()
        .find(|component| component.custom_id() == Some(custom_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(custom_id: &str) -> Component {
        Component::Button {
            style: ButtonStyle::Primary,
            label: Some("فتح تذكرة".into()),
            emoji: None,
            custom_id: Some(custom_id.into()),
            url: None,
            disabled: false,
            restrict: None,
        }
    }

    fn row(components: Vec<Component>) -> ComponentBlock {
        ComponentBlock::ActionRow { components }
    }

    fn text(content: &str) -> ComponentBlock {
        ComponentBlock::Text {
            content: content.into(),
        }
    }

    fn validate(blocks: &[ComponentBlock]) -> Result<(), String> {
        validate_message_components(blocks, &[])
    }

    fn option(value: &str) -> SelectOption {
        SelectOption {
            label: value.to_uppercase(),
            value: value.into(),
            description: None,
            emoji: None,
            default: false,
        }
    }

    #[test]
    fn buttons() {
        assert!(validate(&[row(vec![button("a"), button("b")])]).is_ok());
        assert!(validate(&[row(vec![button("a"), button("a")])]).is_err());
        assert!(validate(&[row((0..6).map(|i| button(&i.to_string())).collect())]).is_err());
        assert!(validate(&(0..6).map(|i| row(vec![button(&i.to_string())])).collect::<Vec<_>>()).is_err());

        let link = Component::Button {
            style: ButtonStyle::Link,
            label: Some("Docs".into()),
            emoji: None,
            custom_id: None,
            url: Some("javascript:alert(1)".into()),
            disabled: false,
            restrict: None,
        };
        assert!(validate(&[row(vec![link])]).is_err());
    }

    #[test]
    fn selects_and_modals() {
        let select = Component::StringSelect {
            custom_id: "role".into(),
            options: vec![option("a")],
            placeholder: None,
            min_values: Some(1),
            max_values: Some(2),
            disabled: false,
            restrict: None,
        };
        // asks for more values than it offers
        assert!(validate(&[row(vec![select.clone()])]).is_err());
        // a select with a button in one row
        assert!(validate(&[row(vec![button("x"), select])]).is_err());

        let input = Component::TextInput {
            custom_id: "topic".into(),
            style: TextInputStyle::Paragraph,
            label: "الموضوع".into(),
            placeholder: None,
            required: true,
            min_length: Some(5),
            max_length: Some(500),
            value: None,
        };
        let modal = Modal {
            custom_id: "ticket".into(),
            title: "تذكرة جديدة".into(),
            components: vec![row(vec![input.clone()])],
        };
        assert!(modal.validate_modal().is_ok());
        assert!(validate(&[row(vec![input])]).is_err());
        let with_button = Modal {
            components: vec![row(vec![button("x")])],
            ..modal
        };
        assert!(with_button.validate_modal().is_err());
    }

    #[test]
    fn layout_blocks() {
        let card = ComponentBlock::Container {
            components: vec![
                text("# مرحباً"),
                ComponentBlock::Section {
                    text: vec!["اختر خطة".into()],
                    button: Some(button("plan")),
                    thumbnail: None,
                },
                ComponentBlock::Separator {
                    divider: true,
                    spacing: SeparatorSpacing::Small,
                },
                ComponentBlock::Gallery {
                    items: vec![MediaItem {
                        url: Some("https://zrzur.cc/a.png".into()),
                        attachment: None,
                        description: None,
                        spoiler: true,
                    }],
                },
                row(vec![button("yes"), button("no")]),
            ],
            accent: Some("#9075FF".into()),
            spoiler: false,
        };
        assert!(validate(std::slice::from_ref(&card)).is_ok());
        // the section's button is found like any other
        assert!(find_component(std::slice::from_ref(&card), "plan").is_some());
        // duplicate ids across a container and the top level
        assert!(validate(&[card.clone(), row(vec![button("yes")])]).is_err());
        // no nesting, no bad colours
        assert!(validate(&[ComponentBlock::Container {
            components: vec![card.clone()],
            accent: None,
            spoiler: false,
        }])
        .is_err());
        assert!(validate(&[ComponentBlock::Container {
            components: vec![text("x")],
            accent: Some("red".into()),
            spoiler: false,
        }])
        .is_err());
        // a file block must point at this message's attachments
        let file = ComponentBlock::File {
            attachment: "a".repeat(26),
            spoiler: false,
        };
        assert!(validate(std::slice::from_ref(&file)).is_err());
        assert!(validate_message_components(&[file], &["a".repeat(26)]).is_ok());
        // text budget and component count
        assert!(validate(&[text(&"x".repeat(3000)), text(&"y".repeat(1500))]).is_err());
        assert!(validate(&(0..41).map(|_| text("x")).collect::<Vec<_>>()).is_err());
        // images only from the web or this message
        assert!(validate(&[ComponentBlock::Gallery {
            items: vec![MediaItem {
                url: Some("file:///etc/passwd".into()),
                attachment: None,
                description: None,
                spoiler: false,
            }],
        }])
        .is_err());
        // form fields stay in forms
        assert!(validate(&[ComponentBlock::Field {
            label: "x".into(),
            description: None,
            component: Component::Checkbox {
                custom_id: "c".into(),
                default: false,
            },
        }])
        .is_err());
    }

    #[test]
    fn form_fields() {
        let field = |label: &str, component: Component| ComponentBlock::Field {
            label: label.into(),
            description: Some("اختر ما يناسبك".into()),
            component,
        };
        let modal = Modal {
            custom_id: "survey".into(),
            title: "استبيان".into(),
            components: vec![
                text("هذه الإجابات تصل إلى البوت فقط."),
                field(
                    "Name",
                    Component::TextInput {
                        custom_id: "name".into(),
                        style: TextInputStyle::Short,
                        label: String::new(),
                        placeholder: None,
                        required: true,
                        min_length: None,
                        max_length: Some(32),
                        value: None,
                    },
                ),
                field(
                    "Class",
                    Component::RadioGroup {
                        custom_id: "class".into(),
                        options: vec![option("a"), option("b")],
                        required: true,
                    },
                ),
                field(
                    "Days",
                    Component::CheckboxGroup {
                        custom_id: "days".into(),
                        options: vec![option("mon"), option("tue")],
                        min_values: None,
                        max_values: None,
                        required: true,
                    },
                ),
                field(
                    "Agree",
                    Component::Checkbox {
                        custom_id: "agree".into(),
                        default: false,
                    },
                ),
                field(
                    "Screenshot",
                    Component::FileUpload {
                        custom_id: "shot".into(),
                        min_values: None,
                        max_values: Some(3),
                        required: true,
                        file_types: vec!["image".into(), ".pdf".into()],
                    },
                ),
            ],
        };
        assert!(modal.validate_modal().is_ok());
        assert_eq!(modal.fields().count(), 5);

        // six fields is one too many
        let mut crowded = modal.clone();
        crowded.components.push(field(
            "More",
            Component::Checkbox {
                custom_id: "more".into(),
                default: false,
            },
        ));
        assert!(crowded.validate_modal().is_err());

        // a radio group needs two options, a bare row needs a label
        let lonely = Modal {
            components: vec![field(
                "Class",
                Component::RadioGroup {
                    custom_id: "class".into(),
                    options: vec![option("a")],
                    required: true,
                },
            )],
            ..modal.clone()
        };
        assert!(lonely.validate_modal().is_err());
        let unlabelled = Modal {
            components: vec![row(vec![Component::Checkbox {
                custom_id: "c".into(),
                default: false,
            }])],
            ..modal
        };
        assert!(unlabelled.validate_modal().is_err());
    }
}

/// Every select menu limit at its maximum (accepted) and one past it
/// (refused), in characters (Arabic letters count once)
#[cfg(test)]
mod select_limit_tests {
    use super::*;

    const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    fn opt(label: &str, value: &str) -> SelectOption {
        SelectOption {
            label: label.into(),
            value: value.into(),
            description: None,
            emoji: None,
            default: false,
        }
    }

    fn options(n: usize) -> Vec<SelectOption> {
        (0..n).map(|i| opt(&format!("Map {i}"), &format!("m{i}"))).collect()
    }

    fn string_select(
        options: Vec<SelectOption>,
        placeholder: Option<String>,
        min: Option<u8>,
        max: Option<u8>,
    ) -> Component {
        Component::StringSelect {
            custom_id: "pick".into(),
            options,
            placeholder,
            min_values: min,
            max_values: max,
            disabled: false,
            restrict: None,
        }
    }

    fn role_select(defaults: Vec<String>, max: Option<u8>) -> Component {
        Component::RoleSelect {
            custom_id: "roles".into(),
            placeholder: None,
            min_values: Some(0),
            max_values: max,
            default_values: defaults,
            disabled: false,
            restrict: None,
        }
    }

    fn check(component: Component) -> Result<(), String> {
        validate_message_components(&[ComponentBlock::ActionRow { components: vec![component] }], &[])
    }

    fn ok(what: &str, component: Component) {
        assert_eq!(check(component), Ok(()), "{what} should be accepted");
    }

    fn refused(what: &str, component: Component) {
        assert!(check(component).is_err(), "{what} should be refused");
    }

    #[test]
    fn option_count() {
        ok("1 option", string_select(options(1), None, None, None));
        ok("25 options", string_select(options(25), None, None, None));
        refused("26 options", string_select(options(26), None, None, None));
        refused("no options", string_select(vec![], None, None, None));
    }

    #[test]
    fn option_texts() {
        ok("label of 100 Arabic letters", string_select(vec![opt(&"خ".repeat(100), "v")], None, None, None));
        refused("label of 101", string_select(vec![opt(&"x".repeat(101), "v")], None, None, None));
        refused("empty label", string_select(vec![opt("", "v")], None, None, None));
        ok("value of 100", string_select(vec![opt("L", &"v".repeat(100))], None, None, None));
        refused("value of 101", string_select(vec![opt("L", &"v".repeat(101))], None, None, None));
        refused("duplicate values", string_select(vec![opt("A", "same"), opt("B", "same")], None, None, None));

        let mut described = opt("L", "v");
        described.description = Some("و".repeat(100));
        ok("description of 100", string_select(vec![described.clone()], None, None, None));
        described.description = Some("x".repeat(101));
        refused("description of 101", string_select(vec![described.clone()], None, None, None));
        described.description = Some(String::new());
        refused("empty description", string_select(vec![described], None, None, None));
    }

    #[test]
    fn single_line_texts() {
        // a menu shows one line per entry: line breaks and control
        // characters would break it
        for bad in ["two\nlines", "tab\there", "bell\u{7}"] {
            refused(&format!("label {bad:?}"), string_select(vec![opt(bad, "v")], None, None, None));
            refused(&format!("value {bad:?}"), string_select(vec![opt("L", bad)], None, None, None));
            let mut described = opt("L", "v");
            described.description = Some(bad.into());
            refused(&format!("description {bad:?}"), string_select(vec![described], None, None, None));
            refused(&format!("placeholder {bad:?}"), string_select(options(1), Some(bad.into()), None, None));
        }
    }

    #[test]
    fn emoji() {
        let mut with = opt("L", "v");
        with.emoji = Some("🗺️".into());
        ok("unicode emoji", string_select(vec![with.clone()], None, None, None));
        with.emoji = Some(ID.into());
        ok("custom emoji id", string_select(vec![with.clone()], None, None, None));
        with.emoji = Some("e".repeat(33));
        refused("emoji of 33", string_select(vec![with.clone()], None, None, None));
        with.emoji = Some("a b".into());
        refused("emoji with a space", string_select(vec![with], None, None, None));
    }

    #[test]
    fn placeholder() {
        ok("placeholder of 150", string_select(options(1), Some("ض".repeat(150)), None, None));
        refused("placeholder of 151", string_select(options(1), Some("x".repeat(151)), None, None));
        refused("empty placeholder", string_select(options(1), Some(String::new()), None, None));
    }

    #[test]
    fn custom_id() {
        let mut long = string_select(options(1), None, None, None);
        if let Component::StringSelect { custom_id, .. } = &mut long {
            *custom_id = "c".repeat(100);
        }
        ok("custom_id of 100", long.clone());
        if let Component::StringSelect { custom_id, .. } = &mut long {
            *custom_id = "c".repeat(101);
        }
        refused("custom_id of 101", long);
        // the same id twice in one message
        let blocks = [
            ComponentBlock::ActionRow { components: vec![string_select(options(1), None, None, None)] },
            ComponentBlock::ActionRow { components: vec![string_select(options(1), None, None, None)] },
        ];
        assert!(validate_message_components(&blocks, &[]).is_err(), "duplicate custom_id");
    }

    #[test]
    fn value_counts() {
        ok("default 1..1", string_select(options(3), None, None, None));
        ok("0..1 (may choose nothing)", string_select(options(3), None, Some(0), Some(1)));
        ok("25..25 of 25", string_select(options(25), None, Some(25), Some(25)));
        refused("max 26", string_select(options(25), None, Some(1), Some(26)));
        refused("max 0", string_select(options(3), None, Some(0), Some(0)));
        refused("min above max", string_select(options(3), None, Some(2), Some(1)));
        refused("max above the options", string_select(options(2), None, Some(1), Some(3)));
        refused("min above the options", string_select(options(2), None, Some(3), Some(3)));

        let mut two_defaults = options(3);
        two_defaults[0].default = true;
        two_defaults[1].default = true;
        refused("2 defaults, max 1", string_select(two_defaults.clone(), None, None, None));
        ok("2 defaults, max 2", string_select(two_defaults, None, Some(1), Some(2)));
    }

    #[test]
    fn typed_selects() {
        let ids = |n: usize| (0..n).map(|i| format!("01ARZ3NDEKTSV4RRFFQ69G{:04}", i)).collect::<Vec<_>>();
        ok("25 defaults, max 25", role_select(ids(25), Some(25)));
        refused("26 defaults", role_select(ids(26), Some(25)));
        refused("defaults above max", role_select(ids(3), Some(2)));
        refused("max 26", role_select(vec![], Some(26)));
        refused("a default that is not an id", role_select(vec!["admin".into()], Some(1)));
        refused("the same default twice", role_select(vec![ID.into(), ID.into()], Some(2)));
        for (what, component) in [
            ("user", Component::UserSelect { custom_id: "u".into(), placeholder: Some("x".repeat(151)), min_values: None, max_values: None, default_values: vec![], disabled: false, restrict: None }),
            ("mentionable", Component::MentionableSelect { custom_id: "m".into(), placeholder: Some("x".repeat(151)), min_values: None, max_values: None, default_values: vec![], disabled: false, restrict: None }),
            ("channel", Component::ChannelSelect { custom_id: "c".into(), placeholder: Some("x".repeat(151)), min_values: None, max_values: None, default_values: vec![], channel_types: None, disabled: false, restrict: None }),
        ] {
            refused(&format!("{what} placeholder of 151"), component);
        }
        refused(
            "channel select with an empty channel_types",
            Component::ChannelSelect { custom_id: "c".into(), placeholder: None, min_values: None, max_values: None, default_values: vec![], channel_types: Some(vec![]), disabled: false, restrict: None },
        );
        ok(
            "voice channels only",
            Component::ChannelSelect { custom_id: "c".into(), placeholder: None, min_values: None, max_values: None, default_values: vec![], channel_types: Some(vec![CommandChannelType::Voice]), disabled: false, restrict: None },
        );
    }

    #[test]
    fn rows() {
        let five: Vec<ComponentBlock> = (0..5)
            .map(|i| {
                let mut s = string_select(options(1), None, None, None);
                if let Component::StringSelect { custom_id, .. } = &mut s {
                    *custom_id = format!("s{i}");
                }
                ComponentBlock::ActionRow { components: vec![s] }
            })
            .collect();
        assert_eq!(validate_message_components(&five, &[]), Ok(()), "5 menus");
        let mut six = five.clone();
        six.push(ComponentBlock::ActionRow { components: vec![role_select(vec![], Some(1))] });
        assert!(validate_message_components(&six, &[]).is_err(), "6 menus");
        let two_in_a_row = [ComponentBlock::ActionRow {
            components: vec![string_select(options(1), None, None, None), role_select(vec![], Some(1))],
        }];
        assert!(validate_message_components(&two_in_a_row, &[]).is_err(), "2 menus in a row");
    }
}

#[cfg(test)]
mod static_component_tests {
    use super::*;

    fn link() -> Component {
        Component::Button {
            style: ButtonStyle::Link,
            label: Some("Open".into()),
            emoji: None,
            custom_id: None,
            url: Some("https://zrzur.com".into()),
            disabled: false,
            restrict: None,
        }
    }

    #[test]
    fn only_links_text_and_media_are_static() {
        let mut press = link();
        if let Component::Button { style, custom_id, url, .. } = &mut press {
            *style = ButtonStyle::Primary;
            *custom_id = Some("press".into());
            *url = None;
        }
        let row = |c: Component| ComponentBlock::ActionRow { components: vec![c] };
        assert!(components_are_static(&[row(link()), ComponentBlock::Text { content: "x".into() }]));
        assert!(components_are_static(&[ComponentBlock::Container {
            components: vec![row(link())],
            accent: None,
            spoiler: false,
        }]));
        assert!(!components_are_static(&[row(press.clone())]));
        assert!(!components_are_static(&[ComponentBlock::Container {
            components: vec![row(press)],
            accent: None,
            spoiler: false,
        }]));
    }
}
