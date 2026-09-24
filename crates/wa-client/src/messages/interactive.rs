//! `interactive` messages: reply buttons, lists, CTA URL, location request,
//! Flows, media carousel, calling buttons, address and contact-info
//! requests. Commerce types (product, product list, catalog, product
//! carousel) live in [`super::commerce`].
//!
//! Every struct serializes to the object *inside* `"interactive"`; the
//! [`Interactive`] enum adds its `type`.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use wa_core::error::ValidationError;
use wa_core::ids::{FlowId, MediaId};

use super::commerce::{CatalogMessage, ProductCarousel, ProductList, SingleProduct};
use super::content::MediaSource;
use super::validate::{self, Check};

/// Footer limit shared by reply buttons, lists, CTA URL and catalog
/// messages ("Maximum 60 characters" on each of those pages).
pub(crate) const FOOTER_MAX: usize = 60;

/// `{"text": …}`, the shape of every `body` and `footer`.
pub(crate) struct TextObject<'a>(pub(crate) &'a str);

impl Serialize for TextObject<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("text", self.0)?;
        map.end()
    }
}

/// `{"name": …}` or `{"name": …, "parameters": …}`, the shape of the
/// `action` of named interactive types.
struct NamedAction<'a, P: Serialize> {
    name: &'a str,
    parameters: Option<P>,
}

impl<P: Serialize> Serialize for NamedAction<'_, P> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("name", self.name)?;
        if let Some(p) = &self.parameters {
            map.serialize_entry("parameters", p)?;
        }
        map.end()
    }
}

/// Header of an interactive message (`HeaderObject` in
/// `reference/whatsapp-business-phone-number/message-api`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Header {
    /// Text header.
    Text {
        /// Header text.
        text: String,
        /// Optional sub-text (reference schema only; no page shows it).
        sub_text: Option<String>,
    },
    /// Image header.
    Image(MediaSource),
    /// Video header.
    Video(MediaSource),
    /// Document header.
    Document {
        /// The asset.
        source: MediaSource,
        /// File name (`direct-send/media-headers`).
        filename: Option<String>,
    },
}

impl Header {
    /// Text header.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            sub_text: None,
        }
    }

    /// Image header from an uploaded id.
    pub fn image_id(id: impl Into<MediaId>) -> Self {
        Self::Image(MediaSource::Id(id.into()))
    }

    /// Image header from a hosted URL.
    pub fn image_link(url: impl Into<String>) -> Self {
        Self::Image(MediaSource::Link(url.into()))
    }

    /// Video header from an uploaded id.
    pub fn video_id(id: impl Into<MediaId>) -> Self {
        Self::Video(MediaSource::Id(id.into()))
    }

    /// Video header from a hosted URL.
    pub fn video_link(url: impl Into<String>) -> Self {
        Self::Video(MediaSource::Link(url.into()))
    }

    /// Document header from an uploaded id.
    pub fn document_id(id: impl Into<MediaId>) -> Self {
        Self::Document {
            source: MediaSource::Id(id.into()),
            filename: None,
        }
    }

    /// Document header from a hosted URL.
    pub fn document_link(url: impl Into<String>) -> Self {
        Self::Document {
            source: MediaSource::Link(url.into()),
            filename: None,
        }
    }

    /// `text_max`: the header text limit of the enclosing message type, when
    /// its page documents one.
    fn validate(&self, text_max: Option<usize>) -> Check {
        let field = "interactive.header";
        match self {
            Self::Text { text, .. } => match text_max {
                Some(max) => validate::text(&format!("{field}.text"), text, max),
                None => validate::non_empty(&format!("{field}.text"), text),
            },
            Self::Image(s) => source_non_empty(&format!("{field}.image"), s),
            Self::Video(s) => source_non_empty(&format!("{field}.video"), s),
            Self::Document { source, .. } => source_non_empty(&format!("{field}.document"), source),
        }
    }
}

fn source_non_empty(field: &str, source: &MediaSource) -> Check {
    match source {
        MediaSource::Id(id) => validate::non_empty(&format!("{field}.id"), id.as_str()),
        MediaSource::Link(url) => validate::non_empty(&format!("{field}.link"), url),
    }
}

impl Serialize for Header {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct DocumentObject<'a> {
            #[serde(flatten)]
            source: &'a MediaSource,
            #[serde(skip_serializing_if = "Option::is_none")]
            filename: Option<&'a str>,
        }
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Text { text, sub_text } => {
                map.serialize_entry("type", "text")?;
                map.serialize_entry("text", text)?;
                if let Some(sub) = sub_text {
                    map.serialize_entry("sub_text", sub)?;
                }
            }
            Self::Image(source) => {
                map.serialize_entry("type", "image")?;
                map.serialize_entry("image", source)?;
            }
            Self::Video(source) => {
                map.serialize_entry("type", "video")?;
                map.serialize_entry("video", source)?;
            }
            Self::Document { source, filename } => {
                map.serialize_entry("type", "document")?;
                map.serialize_entry(
                    "document",
                    &DocumentObject {
                        source,
                        filename: filename.as_deref(),
                    },
                )?;
            }
        }
        map.end()
    }
}

// ─── Reply buttons ───────────────────────────────────────────────────────

/// Reply buttons message (`messages/interactive-reply-buttons-messages`):
/// up to three predefined replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyButtons {
    /// Optional text, image, video or document header.
    pub header: Option<Header>,
    /// Body text.
    pub body: String,
    /// Footer text.
    pub footer: Option<String>,
    /// One to three buttons.
    pub buttons: Vec<ReplyButton>,
}

/// One reply button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyButton {
    /// Returned in the `button_reply` webhook. Unique within the message.
    pub id: String,
    /// Label. Unique within the message.
    pub title: String,
}

impl ReplyButton {
    /// Button `id` labelled `title`.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
        }
    }
}

impl ReplyButtons {
    /// Body plus buttons, no header or footer.
    pub fn new(body: impl Into<String>, buttons: impl IntoIterator<Item = ReplyButton>) -> Self {
        Self {
            header: None,
            body: body.into(),
            footer: None,
            buttons: buttons.into_iter().collect(),
        }
    }

    /// Set the header.
    #[must_use]
    pub fn header(mut self, header: Header) -> Self {
        self.header = Some(header);
        self
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    fn validate(&self, direct_send: bool) -> Check {
        // messages/interactive-reply-buttons-messages. The page gives no
        // header text limit; Direct Send applies the template one, 60
        // (direct-send/supported-features-and-limits). Its body (1024),
        // footer (60) and button text (20) limits equal this page's.
        if let Some(h) = &self.header {
            h.validate(direct_send.then_some(60))?;
        }
        validate::text("interactive.body.text", &self.body, 1024)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            FOOTER_MAX,
        )?;
        validate::count("interactive.action.buttons", self.buttons.len(), 1, 3)?;
        let mut ids = HashSet::new();
        let mut titles = HashSet::new();
        for (i, b) in self.buttons.iter().enumerate() {
            let path = format!("interactive.action.buttons[{i}].reply");
            validate::text(&format!("{path}.id"), &b.id, 256)?;
            validate::text(&format!("{path}.title"), &b.title, 20)?;
            // "A unique identifier for each button"; labels "Must be unique
            // if using multiple buttons".
            if !ids.insert(b.id.as_str()) {
                return Err(ValidationError::new(format!("{path}.id"), "must be unique"));
            }
            if !titles.insert(b.title.as_str()) {
                return Err(ValidationError::new(
                    format!("{path}.title"),
                    "must be unique",
                ));
            }
        }
        Ok(())
    }
}

impl Serialize for ReplyButton {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Reply<'a> {
            id: &'a str,
            title: &'a str,
        }
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("type", "reply")?;
        map.serialize_entry(
            "reply",
            &Reply {
                id: &self.id,
                title: &self.title,
            },
        )?;
        map.end()
    }
}

impl Serialize for ReplyButtons {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Action<'a> {
            buttons: &'a [ReplyButton],
        }
        let mut map = serializer.serialize_map(None)?;
        if let Some(h) = &self.header {
            map.serialize_entry("header", h)?;
        }
        map.serialize_entry("body", &TextObject(&self.body))?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &Action {
                buttons: &self.buttons,
            },
        )?;
        map.end()
    }
}

// ─── List ────────────────────────────────────────────────────────────────

/// List message (`messages/interactive-list-messages`): a button that opens
/// up to 10 rows grouped in up to 10 sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMessage {
    /// Header text (lists support text headers only).
    pub header: Option<String>,
    /// Body text.
    pub body: String,
    /// Footer text.
    pub footer: Option<String>,
    /// Label of the button that opens the list.
    pub button: String,
    /// Sections.
    pub sections: Vec<ListSection>,
}

/// A list section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListSection {
    /// Section title. The list page marks it required; the reference schema
    /// requires it only when there is more than one section, which is what
    /// we enforce.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Rows.
    pub rows: Vec<ListRow>,
}

impl ListSection {
    /// Titled section.
    pub fn new(title: impl Into<String>, rows: impl IntoIterator<Item = ListRow>) -> Self {
        Self {
            title: Some(title.into()),
            rows: rows.into_iter().collect(),
        }
    }
}

/// A list row (an option the user can pick).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListRow {
    /// Returned in the `list_reply` webhook.
    pub id: String,
    /// Row title.
    pub title: String,
    /// Row description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ListRow {
    /// Row without description.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
        }
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl ListMessage {
    /// Body, button label and sections; no header or footer.
    pub fn new(
        body: impl Into<String>,
        button: impl Into<String>,
        sections: impl IntoIterator<Item = ListSection>,
    ) -> Self {
        Self {
            header: None,
            body: body.into(),
            footer: None,
            button: button.into(),
            sections: sections.into_iter().collect(),
        }
    }

    /// Set the (text) header.
    #[must_use]
    pub fn header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    fn validate(&self) -> Check {
        // All limits: messages/interactive-list-messages.
        validate::opt_text("interactive.header.text", self.header.as_deref(), 60)?;
        validate::text("interactive.body.text", &self.body, 4096)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            FOOTER_MAX,
        )?;
        validate::text("interactive.action.button", &self.button, 20)?;
        validate::count("interactive.action.sections", self.sections.len(), 1, 10)?;
        let rows: usize = self.sections.iter().map(|s| s.rows.len()).sum();
        // "up to 10 rows for all sections combined"; "At least one row is
        // required".
        validate::count("interactive.action.sections[].rows", rows, 1, 10)?;
        let multi = self.sections.len() > 1;
        for (i, s) in self.sections.iter().enumerate() {
            let path = format!("interactive.action.sections[{i}]");
            section_title(&format!("{path}.title"), s.title.as_deref(), multi)?;
            for (j, r) in s.rows.iter().enumerate() {
                let rp = format!("{path}.rows[{j}]");
                validate::text(&format!("{rp}.id"), &r.id, 200)?;
                validate::text(&format!("{rp}.title"), &r.title, 24)?;
                validate::opt_text(&format!("{rp}.description"), r.description.as_deref(), 72)?;
            }
        }
        Ok(())
    }
}

/// Section titles (lists and multi-product messages): at most 24
/// characters, required once there is more than one section
/// (`SectionObject` in the message API reference).
pub(crate) fn section_title(field: &str, title: Option<&str>, multi: bool) -> Check {
    match title {
        Some(t) => validate::text(field, t, 24),
        None if multi => Err(ValidationError::new(
            field,
            "is required when the message has more than one section",
        )),
        None => Ok(()),
    }
}

impl Serialize for ListMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Action<'a> {
            button: &'a str,
            sections: &'a [ListSection],
        }
        let mut map = serializer.serialize_map(None)?;
        if let Some(h) = &self.header {
            map.serialize_entry("header", &Header::text(h.clone()))?;
        }
        map.serialize_entry("body", &TextObject(&self.body))?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &Action {
                button: &self.button,
                sections: &self.sections,
            },
        )?;
        map.end()
    }
}

// ─── CTA URL ─────────────────────────────────────────────────────────────

/// Call-to-action URL button (`messages/interactive-cta-url-messages`): maps
/// a URL to a button so the raw link stays out of the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtaUrl {
    /// Optional text, image, video or document header.
    pub header: Option<Header>,
    /// Body text.
    pub body: String,
    /// Footer text.
    pub footer: Option<String>,
    /// Button label.
    pub display_text: String,
    /// URL opened in the device browser.
    pub url: String,
}

impl CtaUrl {
    /// Body, button label and URL.
    pub fn new(
        body: impl Into<String>,
        display_text: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            header: None,
            body: body.into(),
            footer: None,
            display_text: display_text.into(),
            url: url.into(),
        }
    }

    /// Set the header.
    #[must_use]
    pub fn header(mut self, header: Header) -> Self {
        self.header = Some(header);
        self
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    fn validate(&self) -> Check {
        // messages/interactive-cta-url-messages.
        if let Some(h) = &self.header {
            h.validate(Some(60))?;
        }
        validate::text("interactive.body.text", &self.body, 1024)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            FOOTER_MAX,
        )?;
        validate::text(
            "interactive.action.parameters.display_text",
            &self.display_text,
            20,
        )?;
        validate::non_empty("interactive.action.parameters.url", &self.url)
    }
}

#[derive(Serialize)]
struct UrlButton<'a> {
    display_text: &'a str,
    url: &'a str,
}

impl Serialize for CtaUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if let Some(h) = &self.header {
            map.serialize_entry("header", h)?;
        }
        map.serialize_entry("body", &TextObject(&self.body))?;
        map.serialize_entry(
            "action",
            &NamedAction {
                name: "cta_url",
                parameters: Some(UrlButton {
                    display_text: &self.display_text,
                    url: &self.url,
                }),
            },
        )?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.end()
    }
}

// ─── Location request ────────────────────────────────────────────────────

/// Location request (`messages/location-request-messages`): body text and a
/// "send location" button; the reply arrives as a `location` webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationRequest {
    /// Body text.
    pub body: String,
}

impl LocationRequest {
    /// Request with `body`.
    pub fn new(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }

    fn validate(&self) -> Check {
        // messages/location-request-messages: "Maximum 1024 characters".
        validate::text("interactive.body.text", &self.body, 1024)
    }
}

impl Serialize for LocationRequest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        map.serialize_entry(
            "action",
            &NamedAction::<()> {
                name: "send_location",
                parameters: None,
            },
        )?;
        map.end()
    }
}

// ─── Flow ────────────────────────────────────────────────────────────────

/// Flow message (`flows/guides/sendingaflow`, "User-initiated
/// conversations"): a CTA button that opens a WhatsApp Flow.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowMessage {
    /// Optional header.
    pub header: Option<Header>,
    /// Body text.
    pub body: String,
    /// Footer text.
    pub footer: Option<String>,
    /// `action.parameters`.
    pub parameters: FlowParameters,
}

impl FlowMessage {
    /// Body plus parameters.
    pub fn new(body: impl Into<String>, parameters: FlowParameters) -> Self {
        Self {
            header: None,
            body: body.into(),
            footer: None,
            parameters,
        }
    }

    /// Set the header.
    #[must_use]
    pub fn header(mut self, header: Header) -> Self {
        self.header = Some(header);
        self
    }

    /// Set the footer.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    fn validate(&self) -> Check {
        // The Flows page documents no lengths for header/body/footer or
        // `flow_cta` (it only *advises* ≤ 30 characters for the CTA), so
        // only presence is checked.
        if let Some(h) = &self.header {
            h.validate(None)?;
        }
        validate::non_empty("interactive.body.text", &self.body)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            usize::MAX,
        )?;
        self.parameters.validate()
    }
}

impl Serialize for FlowMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if let Some(h) = &self.header {
            map.serialize_entry("header", h)?;
        }
        map.serialize_entry("body", &TextObject(&self.body))?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &NamedAction {
                name: "flow",
                parameters: Some(&self.parameters),
            },
        )?;
        map.end()
    }
}

/// Which Flow to open. `flow_id` and `flow_name` are mutually exclusive, so
/// this is an enum rather than two options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FlowRef {
    /// By id.
    #[serde(rename = "flow_id")]
    Id(FlowId),
    /// By name (must be updated if the Flow is renamed).
    #[serde(rename = "flow_name")]
    Name(String),
}

/// `mode`: which version of the Flow to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowMode {
    /// The current draft.
    Draft,
    /// The last published version (Meta's default).
    Published,
}

/// `flow_action`; shared with Flow template buttons (see [`crate::common`]).
pub use crate::common::FlowAction;

/// `flow_action_payload`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlowActionPayload {
    /// Id of the first screen.
    pub screen: String,
    /// Input data for the first screen. The page types it as a string and
    /// its example sends a JSON-encoded string, while the prose asks for a
    /// "non-empty object"; either shape is passed through as given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// `action.parameters` of a Flow message.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowParameters {
    /// The Flow.
    pub flow: FlowRef,
    /// CTA button text.
    pub flow_cta: String,
    /// Business-generated token identifying this Flow session.
    pub flow_token: Option<String>,
    /// Draft or published.
    pub mode: Option<FlowMode>,
    /// Navigate or data exchange.
    pub flow_action: Option<FlowAction>,
    /// First screen and its data (for `navigate`).
    pub flow_action_payload: Option<FlowActionPayload>,
}

impl FlowParameters {
    /// `flow_message_version`, which "must be `3`".
    pub const MESSAGE_VERSION: &'static str = "3";

    /// Open `flow` from a button labelled `cta`.
    pub fn new(flow: FlowRef, cta: impl Into<String>) -> Self {
        Self {
            flow,
            flow_cta: cta.into(),
            flow_token: None,
            mode: None,
            flow_action: None,
            flow_action_payload: None,
        }
    }

    /// Set `flow_token`.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.flow_token = Some(token.into());
        self
    }

    /// Send the draft version.
    #[must_use]
    pub fn draft(mut self) -> Self {
        self.mode = Some(FlowMode::Draft);
        self
    }

    /// `flow_action: navigate` to `screen`, with optional first-screen data.
    #[must_use]
    pub fn navigate(mut self, screen: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        self.flow_action = Some(FlowAction::Navigate);
        self.flow_action_payload = Some(FlowActionPayload {
            screen: screen.into(),
            data,
        });
        self
    }

    /// `flow_action: data_exchange` (the endpoint provides the first screen).
    #[must_use]
    pub fn data_exchange(mut self) -> Self {
        self.flow_action = Some(FlowAction::DataExchange);
        self
    }

    fn validate(&self) -> Check {
        let p = "interactive.action.parameters";
        match &self.flow {
            FlowRef::Id(id) => validate::non_empty(&format!("{p}.flow_id"), id.as_str())?,
            FlowRef::Name(n) => validate::non_empty(&format!("{p}.flow_name"), n)?,
        }
        validate::non_empty(&format!("{p}.flow_cta"), &self.flow_cta)?;
        if let Some(payload) = &self.flow_action_payload {
            validate::non_empty(&format!("{p}.flow_action_payload.screen"), &payload.screen)?;
            // "Must be a non-empty object" (string form accepted, see the
            // field docs).
            let empty = match &payload.data {
                Some(serde_json::Value::Object(m)) => m.is_empty(),
                Some(serde_json::Value::String(s)) => s.is_empty(),
                Some(serde_json::Value::Null) => true,
                _ => false,
            };
            if empty {
                return Err(ValidationError::new(
                    format!("{p}.flow_action_payload.data"),
                    "must be non-empty when present",
                ));
            }
        }
        Ok(())
    }
}

impl Serialize for FlowParameters {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            flow_message_version: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            flow_token: Option<&'a str>,
            #[serde(flatten)]
            flow: &'a FlowRef,
            flow_cta: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            mode: Option<FlowMode>,
            #[serde(skip_serializing_if = "Option::is_none")]
            flow_action: Option<&'a FlowAction>,
            #[serde(skip_serializing_if = "Option::is_none")]
            flow_action_payload: Option<&'a FlowActionPayload>,
        }
        Wire {
            flow_message_version: Self::MESSAGE_VERSION,
            flow_token: self.flow_token.as_deref(),
            flow: &self.flow,
            flow_cta: &self.flow_cta,
            mode: self.mode,
            flow_action: self.flow_action.as_ref(),
            flow_action_payload: self.flow_action_payload.as_ref(),
        }
        .serialize(serializer)
    }
}

// ─── Media carousel ──────────────────────────────────────────────────────

/// Media card carousel (`messages/interactive-media-carousel-messages`):
/// 2–10 horizontally scrolling cards, each with an image or video header
/// and either one URL button or quick-reply buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCarousel {
    /// Main body text (headers and footers are not supported).
    pub body: String,
    /// Cards, left to right; `card_index` is their position.
    pub cards: Vec<MediaCard>,
}

/// One carousel card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCard {
    /// Required image or video header.
    pub header: CardHeader,
    /// Card body text.
    pub body: Option<String>,
    /// The card's button(s).
    pub action: CardAction,
}

/// A card header. The page only documents public URLs (`link`) here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardHeader {
    /// Image at this URL.
    Image(String),
    /// Video at this URL.
    Video(String),
}

/// A card's buttons. Every card of a carousel must use the same variant
/// with the same number of buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardAction {
    /// One URL button.
    Url {
        /// Button label.
        display_text: String,
        /// URL.
        url: String,
    },
    /// One or more quick-reply buttons.
    QuickReplies(Vec<QuickReply>),
}

/// A quick-reply button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickReply {
    /// Returned in the reply webhook.
    pub id: String,
    /// Label.
    pub title: String,
}

impl QuickReply {
    /// Button `id` labelled `title`.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
        }
    }
}

impl MediaCard {
    /// Card with a header and an action, no body.
    pub fn new(header: CardHeader, action: CardAction) -> Self {
        Self {
            header,
            body: None,
            action,
        }
    }

    /// Set the card body.
    #[must_use]
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }
}

impl MediaCarousel {
    /// Body plus cards.
    pub fn new(body: impl Into<String>, cards: impl IntoIterator<Item = MediaCard>) -> Self {
        Self {
            body: body.into(),
            cards: cards.into_iter().collect(),
        }
    }

    fn validate(&self) -> Check {
        // All limits: messages/interactive-media-carousel-messages.
        validate::text("interactive.body.text", &self.body, 1024)?;
        validate::count("interactive.action.cards", self.cards.len(), 2, 10)?;
        let shape = |a: &CardAction| match a {
            CardAction::Url { .. } => (true, 1),
            CardAction::QuickReplies(b) => (false, b.len()),
        };
        let first = self.cards.first().map(|c| shape(&c.action));
        for (i, card) in self.cards.iter().enumerate() {
            let path = format!("interactive.action.cards[{i}]");
            let (kind, url) = match &card.header {
                CardHeader::Image(url) => ("image", url),
                CardHeader::Video(url) => ("video", url),
            };
            validate::non_empty(&format!("{path}.header.{kind}.link"), url)?;
            if let Some(body) = &card.body {
                // "Max 160 characters, and up to 2 line breaks."
                let field = format!("{path}.body.text");
                validate::text(&field, body, 160)?;
                if body.matches('\n').count() > 2 {
                    return Err(ValidationError::new(field, "has more than 2 line breaks"));
                }
            }
            match &card.action {
                CardAction::Url { display_text, url } => {
                    let p = format!("{path}.action.parameters");
                    validate::text(&format!("{p}.display_text"), display_text, 20)?;
                    validate::non_empty(&format!("{p}.url"), url)?;
                }
                CardAction::QuickReplies(buttons) => {
                    // "one or more quick-reply buttons"; no maximum given.
                    validate::count(
                        &format!("{path}.action.buttons"),
                        buttons.len(),
                        1,
                        usize::MAX,
                    )?;
                    for (j, b) in buttons.iter().enumerate() {
                        let p = format!("{path}.action.buttons[{j}].quick_reply");
                        validate::text(&format!("{p}.id"), &b.id, 256)?;
                        validate::text(&format!("{p}.title"), &b.title, 20)?;
                    }
                }
            }
            // "Button types and numbers must match across all cards."
            if first.is_some_and(|f| f != shape(&card.action)) {
                return Err(ValidationError::new(
                    format!("{path}.action"),
                    "must use the same button type and count as the first card",
                ));
            }
        }
        Ok(())
    }
}

impl Serialize for MediaCarousel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Card<'a>(usize, &'a MediaCard);
        impl Serialize for Card<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                #[derive(Serialize)]
                struct Link<'a> {
                    link: &'a str,
                }
                #[derive(Serialize)]
                struct Buttons<'a> {
                    buttons: Vec<QuickReplyWire<'a>>,
                }
                #[derive(Serialize)]
                struct QuickReplyWire<'a> {
                    #[serde(rename = "type")]
                    kind: &'static str,
                    quick_reply: QuickReplyBody<'a>,
                }
                #[derive(Serialize)]
                struct QuickReplyBody<'a> {
                    id: &'a str,
                    title: &'a str,
                }
                struct HeaderWire<'a>(&'a CardHeader);
                impl Serialize for HeaderWire<'_> {
                    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                        let (kind, link) = match self.0 {
                            CardHeader::Image(link) => ("image", link),
                            CardHeader::Video(link) => ("video", link),
                        };
                        let mut map = serializer.serialize_map(Some(2))?;
                        map.serialize_entry("type", kind)?;
                        map.serialize_entry(kind, &Link { link })?;
                        map.end()
                    }
                }
                let Card(index, card) = self;
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("card_index", index)?;
                // Both documented examples, URL *and* quick-reply, set the
                // card `type` to `cta_url`; we send exactly that.
                map.serialize_entry("type", "cta_url")?;
                map.serialize_entry("header", &HeaderWire(&card.header))?;
                if let Some(body) = &card.body {
                    map.serialize_entry("body", &TextObject(body))?;
                }
                match &card.action {
                    CardAction::Url { display_text, url } => map.serialize_entry(
                        "action",
                        &NamedAction {
                            name: "cta_url",
                            parameters: Some(UrlButton { display_text, url }),
                        },
                    )?,
                    CardAction::QuickReplies(buttons) => map.serialize_entry(
                        "action",
                        &Buttons {
                            buttons: buttons
                                .iter()
                                .map(|b| QuickReplyWire {
                                    kind: "quick_reply",
                                    quick_reply: QuickReplyBody {
                                        id: &b.id,
                                        title: &b.title,
                                    },
                                })
                                .collect(),
                        },
                    )?,
                }
                map.end()
            }
        }
        #[derive(Serialize)]
        struct Action<'a> {
            cards: Vec<Card<'a>>,
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        map.serialize_entry(
            "action",
            &Action {
                cards: self
                    .cards
                    .iter()
                    .enumerate()
                    .map(|(i, c)| Card(i, c))
                    .collect(),
            },
        )?;
        map.end()
    }
}

// ─── Calling ─────────────────────────────────────────────────────────────

/// WhatsApp call button (`calling/call-button-messages-deep-links`): tapping
/// it calls the sending business number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceCall {
    /// Body text.
    pub body: String,
    /// Optional button parameters.
    pub parameters: Option<VoiceCallParameters>,
}

/// `action.parameters` of a call button.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VoiceCallParameters {
    /// Button label (Meta's default: `Call Now`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// Button lifetime in minutes (Meta's default: 10080, 7 days).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<u32>,
    /// Tracking string echoed as `cta_payload` in `calls` webhooks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

impl VoiceCall {
    /// Call button with default parameters.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            parameters: None,
        }
    }

    /// Set the parameters.
    #[must_use]
    pub fn parameters(mut self, parameters: VoiceCallParameters) -> Self {
        self.parameters = Some(parameters);
        self
    }

    fn validate(&self) -> Check {
        validate::non_empty("interactive.body.text", &self.body)?;
        if let Some(p) = &self.parameters {
            let path = "interactive.action.parameters";
            // "Max length: 20 characters."
            validate::opt_text(
                &format!("{path}.display_text"),
                p.display_text.as_deref(),
                20,
            )?;
            // "Must be between 1 and 43200 (30 days)."
            if let Some(ttl) = p.ttl_minutes
                && !(1..=43200).contains(&ttl)
            {
                return Err(ValidationError::new(
                    format!("{path}.ttl_minutes"),
                    format!("is {ttl}; the documented range is 1..=43200"),
                ));
            }
            // "Maximum 512 characters."
            validate::opt_text(&format!("{path}.payload"), p.payload.as_deref(), 512)?;
        }
        Ok(())
    }
}

impl Serialize for VoiceCall {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        map.serialize_entry(
            "action",
            &NamedAction {
                name: "voice_call",
                parameters: self.parameters.as_ref(),
            },
        )?;
        map.end()
    }
}

/// Call permission request (`calling/user-call-permissions`). Only the body
/// can be customized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallPermissionRequest {
    /// Body text; optional, but Meta advises giving the user context.
    pub body: Option<String>,
}

impl CallPermissionRequest {
    /// Request with a body.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: Some(body.into()),
        }
    }
}

impl Serialize for CallPermissionRequest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry(
            "action",
            &NamedAction::<()> {
                name: "call_permission_request",
                parameters: None,
            },
        )?;
        if let Some(b) = &self.body {
            map.serialize_entry("body", &TextObject(b))?;
        }
        map.end()
    }
}

// ─── Address (India) ─────────────────────────────────────────────────────

/// Address message (`messages/address-messages`): asks the user for a
/// shipping address. India-based businesses and India customers only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressMessage {
    /// Optional header.
    pub header: Option<Header>,
    /// Body text.
    pub body: String,
    /// Footer text.
    pub footer: Option<String>,
    /// `action.parameters`.
    pub parameters: AddressParameters,
}

/// `action.parameters` of an address message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddressParameters {
    /// ISO country code (`IN`). Mandatory.
    pub country: String,
    /// Prefilled field values (`name`, `phone_number`, `in_pin_code`, …).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
    /// Addresses the user can pick instead of typing one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub saved_addresses: Vec<SavedAddress>,
    /// Per-field errors that block submission until fixed.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub validation_errors: BTreeMap<String, String>,
}

/// A saved address offered to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedAddress {
    /// Returned as `saved_address_id` in the reply.
    pub id: String,
    /// Field values.
    pub value: BTreeMap<String, String>,
}

impl AddressMessage {
    /// Ask for an address in `country`.
    pub fn new(body: impl Into<String>, country: impl Into<String>) -> Self {
        Self {
            header: None,
            body: body.into(),
            footer: None,
            parameters: AddressParameters {
                country: country.into(),
                values: BTreeMap::new(),
                saved_addresses: Vec::new(),
                validation_errors: BTreeMap::new(),
            },
        }
    }

    fn validate(&self) -> Check {
        if let Some(h) = &self.header {
            h.validate(None)?;
        }
        validate::non_empty("interactive.body.text", &self.body)?;
        validate::opt_text(
            "interactive.footer.text",
            self.footer.as_deref(),
            usize::MAX,
        )?;
        // "The country attribute is a mandatory field in the action
        // parameters."
        validate::non_empty(
            "interactive.action.parameters.country",
            &self.parameters.country,
        )
    }
}

impl Serialize for AddressMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if let Some(h) = &self.header {
            map.serialize_entry("header", h)?;
        }
        map.serialize_entry("body", &TextObject(&self.body))?;
        if let Some(f) = &self.footer {
            map.serialize_entry("footer", &TextObject(f))?;
        }
        map.serialize_entry(
            "action",
            &NamedAction {
                name: "address_message",
                parameters: Some(&self.parameters),
            },
        )?;
        map.end()
    }
}

// ─── Request contact info ────────────────────────────────────────────────

/// Ask a user who messages you by username for their phone number
/// (`business-scoped-user-ids`, "Requesting phone numbers from users").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContactInfo {
    /// Body text.
    pub body: String,
}

impl RequestContactInfo {
    /// Request with `body`.
    pub fn new(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }
}

impl Serialize for RequestContactInfo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("body", &TextObject(&self.body))?;
        map.serialize_entry(
            "action",
            &NamedAction::<()> {
                name: "request_contact_info",
                parameters: None,
            },
        )?;
        map.end()
    }
}

// ─── The `interactive` object ────────────────────────────────────────────

/// The `interactive` object, tagged by its `type`.
///
/// Interactive types this crate does not model yet (payments
/// `order_details`, Direct Send mixed buttons, …) can be sent with
/// [`super::MessageContent::Raw`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Interactive {
    /// `button`: reply buttons.
    #[serde(rename = "button")]
    Buttons(ReplyButtons),
    /// `list`.
    #[serde(rename = "list")]
    List(ListMessage),
    /// `cta_url`.
    #[serde(rename = "cta_url")]
    CtaUrl(CtaUrl),
    /// `location_request_message`.
    #[serde(rename = "location_request_message")]
    LocationRequest(LocationRequest),
    /// `flow`.
    #[serde(rename = "flow")]
    Flow(FlowMessage),
    /// `product`: single product.
    #[serde(rename = "product")]
    Product(SingleProduct),
    /// `product_list`: multi-product.
    #[serde(rename = "product_list")]
    ProductList(ProductList),
    /// `catalog_message`.
    #[serde(rename = "catalog_message")]
    Catalog(CatalogMessage),
    /// `carousel` of media cards.
    #[serde(rename = "carousel")]
    MediaCarousel(MediaCarousel),
    /// `carousel` of product cards.
    #[serde(rename = "carousel")]
    ProductCarousel(ProductCarousel),
    /// `voice_call`: WhatsApp call button.
    #[serde(rename = "voice_call")]
    VoiceCall(VoiceCall),
    /// `call_permission_request`.
    #[serde(rename = "call_permission_request")]
    CallPermissionRequest(CallPermissionRequest),
    /// `address_message` (India only).
    #[serde(rename = "address_message")]
    Address(AddressMessage),
    /// `request_contact_info`.
    #[serde(rename = "request_contact_info")]
    RequestContactInfo(RequestContactInfo),
}

impl Interactive {
    /// `direct_send`: sent with a utility or authentication Direct Send
    /// category, which tightens some limits.
    pub(crate) fn validate(&self, direct_send: bool) -> Check {
        match self {
            Self::Buttons(m) => m.validate(direct_send),
            Self::List(m) => m.validate(),
            Self::CtaUrl(m) => m.validate(),
            Self::LocationRequest(m) => m.validate(),
            Self::Flow(m) => m.validate(),
            Self::Product(m) => m.validate(),
            Self::ProductList(m) => m.validate(),
            Self::Catalog(m) => m.validate(),
            Self::MediaCarousel(m) => m.validate(),
            Self::ProductCarousel(m) => m.validate(),
            Self::VoiceCall(m) => m.validate(),
            Self::CallPermissionRequest(m) => {
                validate::opt_text("interactive.body.text", m.body.as_deref(), usize::MAX)
            }
            Self::Address(m) => m.validate(),
            Self::RequestContactInfo(m) => validate::non_empty("interactive.body.text", &m.body),
        }
    }
}

macro_rules! into_interactive {
    ($($ty:ident => $variant:ident),* $(,)?) => {$(
        impl From<$ty> for Interactive {
            fn from(m: $ty) -> Self {
                Self::$variant(m)
            }
        }
        impl From<$ty> for super::MessageContent {
            fn from(m: $ty) -> Self {
                Self::Interactive(Interactive::$variant(m))
            }
        }
    )*};
}

into_interactive! {
    ReplyButtons => Buttons,
    ListMessage => List,
    CtaUrl => CtaUrl,
    LocationRequest => LocationRequest,
    FlowMessage => Flow,
    SingleProduct => Product,
    ProductList => ProductList,
    CatalogMessage => Catalog,
    MediaCarousel => MediaCarousel,
    ProductCarousel => ProductCarousel,
    VoiceCall => VoiceCall,
    CallPermissionRequest => CallPermissionRequest,
    AddressMessage => Address,
    RequestContactInfo => RequestContactInfo,
}
