//! The send request envelope: [`OutboundMessage`] and [`MessageContent`].

use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{CatalogId, GroupId, MediaId, MessageId};
use meta_whatsapp_core::recipient::Recipient;
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use super::commerce::{CatalogMessage, ProductList, ProductSection, SingleProduct};
use super::contacts::{Contact, MAX_CONTACTS};
use super::content::{
    Audio, Document, Image, Location, MediaSource, Pin, Reaction, Sticker, TEXT_BODY_MAX_CHARS,
    Text, Video,
};
use super::interactive::{
    CtaUrl, FlowMessage, FlowParameters, Interactive, ListMessage, ListSection, LocationRequest,
    ReplyButton, ReplyButtons,
};
use super::validate::{self, Check};
use crate::templates::TemplateMessage;

/// A message to send with [`super::Messages::send`].
///
/// Serializes to the `POST /{phone_number_id}/messages` body:
/// `messaging_product`, the [`Recipient`] fields, `type`, the object named
/// by `type`, and the optional envelope fields.
///
/// Build one with a constructor ([`OutboundMessage::text`], …) or
/// [`OutboundMessage::new`] with any [`MessageContent`], then chain
/// [`reply_to`](Self::reply_to), [`callback_data`](Self::callback_data),
/// [`category`](Self::category). Limits are checked by
/// [`validate`](Self::validate), which `send` calls before any request.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundMessage {
    /// Who receives it (phone, BSUID, both, or a group).
    pub recipient: Recipient,
    /// Quote an earlier message (`messages/contextual-replies`).
    pub context: Option<Context>,
    /// Opaque string echoed back in this message's status webhooks.
    pub biz_opaque_callback_data: Option<String>,
    /// Direct Send (beta) category; `None` is a normal service message.
    pub category: Option<DirectSendCategory>,
    /// Direct Send only: custom time-to-live in seconds
    /// (`direct-send/configure-message-ttl`).
    pub ttl_seconds: Option<u32>,
    /// Direct Send only: business-named template
    /// (`direct-send/business-named-templates`).
    pub direct_send_config: Option<DirectSendConfig>,
    /// What is sent.
    pub content: MessageContent,
}

/// `context`: the message being replied to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Context {
    /// The `wamid` of the quoted message.
    pub message_id: MessageId,
}

/// Direct Send `category` (`direct-send/send-utility-and-authentication-messages`).
///
/// Direct Send is a beta that sends free-form content *outside* the
/// customer service window by turning it into a generated template.
/// Omitting the category is the same as [`DirectSendCategory::Service`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DirectSendCategory {
    /// `utility`, charged at utility rates.
    Utility,
    /// `authentication` (access restricted). Cannot be sent to a BSUID.
    Authentication,
    /// `service`: the normal service-message flow.
    Service,
    /// A value this crate does not know yet, sent as given.
    Other(String),
}

impl DirectSendCategory {
    /// Wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Utility => "utility",
            Self::Authentication => "authentication",
            Self::Service => "service",
            Self::Other(s) => s,
        }
    }
}

impl Serialize for DirectSendCategory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// `direct_send_config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectSendConfig {
    /// Create or reuse a template with exactly this name instead of Meta's
    /// automatic matching. Utility only; access restricted.
    pub template_name: String,
}

/// The typed content of a message: the `type` field and the object named by
/// it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MessageContent {
    /// `text`.
    Text(Text),
    /// `image`.
    Image(Image),
    /// `video`.
    Video(Video),
    /// `audio`.
    Audio(Audio),
    /// `document`.
    Document(Document),
    /// `sticker`.
    Sticker(Sticker),
    /// `location`.
    Location(Location),
    /// `contacts`: up to 257 cards.
    Contacts(Vec<Contact>),
    /// `reaction`.
    Reaction(Reaction),
    /// `template`. Its contents are the templates module's contract and are
    /// not validated here.
    Template(TemplateMessage),
    /// `interactive`.
    Interactive(Interactive),
    /// `pin` (groups only).
    Pin(Pin),
    /// Any type this crate does not model yet: serializes as
    /// `"type": message_type, message_type: body`.
    Raw {
        /// The `type` value, which is also the key of `body`.
        message_type: String,
        /// The object sent under that key.
        body: serde_json::Value,
    },
}

/// Keys the envelope owns; a [`MessageContent::Raw`] type must not collide
/// with them or the JSON object would carry the key twice.
const ENVELOPE_KEYS: &[&str] = &[
    "messaging_product",
    "recipient_type",
    "to",
    "recipient",
    "type",
    "context",
    "biz_opaque_callback_data",
    "category",
    "ttl_seconds",
    "direct_send_config",
];

impl MessageContent {
    /// The `type` value.
    pub fn message_type(&self) -> &str {
        match self {
            Self::Text(_) => "text",
            Self::Image(_) => "image",
            Self::Video(_) => "video",
            Self::Audio(_) => "audio",
            Self::Document(_) => "document",
            Self::Sticker(_) => "sticker",
            Self::Location(_) => "location",
            Self::Contacts(_) => "contacts",
            Self::Reaction(_) => "reaction",
            Self::Template(_) => "template",
            Self::Interactive(_) => "interactive",
            Self::Pin(_) => "pin",
            Self::Raw { message_type, .. } => message_type,
        }
    }

    fn validate(&self, direct_send: bool) -> Check {
        match self {
            // Direct Send aligns text bodies with template limits: 1024
            // (direct-send/supported-features-and-limits); otherwise 4096
            // (messages/text-messages).
            Self::Text(t) => t.validate(if direct_send {
                1024
            } else {
                TEXT_BODY_MAX_CHARS
            }),
            Self::Image(m) => m.validate(),
            Self::Video(m) => m.validate(),
            Self::Audio(m) => m.validate(),
            Self::Document(m) => m.validate(),
            Self::Sticker(m) => m.validate(),
            Self::Location(m) => m.validate(),
            Self::Contacts(cards) => {
                validate::count("contacts", cards.len(), 1, MAX_CONTACTS)?;
                for (i, c) in cards.iter().enumerate() {
                    c.validate(&format!("contacts[{i}]"))?;
                }
                Ok(())
            }
            Self::Reaction(r) => r.validate(),
            // Full send-time checks (components, carousel/coupon/MPM limits),
            // not just the name.
            Self::Template(t) => t.validate(),
            Self::Interactive(i) => i.validate(direct_send),
            Self::Pin(p) => p.validate(),
            Self::Raw { message_type, .. } => {
                validate::non_empty("type", message_type)?;
                if ENVELOPE_KEYS.contains(&message_type.as_str()) {
                    return Err(ValidationError::new(
                        "type",
                        format!("`{message_type}` is an envelope field, not a message type"),
                    ));
                }
                Ok(())
            }
        }
    }
}

impl Serialize for MessageContent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let kind = self.message_type();
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("type", kind)?;
        match self {
            Self::Text(v) => map.serialize_entry(kind, v)?,
            Self::Image(v) => map.serialize_entry(kind, v)?,
            Self::Video(v) => map.serialize_entry(kind, v)?,
            Self::Audio(v) => map.serialize_entry(kind, v)?,
            Self::Document(v) => map.serialize_entry(kind, v)?,
            Self::Sticker(v) => map.serialize_entry(kind, v)?,
            Self::Location(v) => map.serialize_entry(kind, v)?,
            Self::Contacts(v) => map.serialize_entry(kind, v)?,
            Self::Reaction(v) => map.serialize_entry(kind, v)?,
            Self::Template(v) => map.serialize_entry(kind, v)?,
            Self::Interactive(v) => map.serialize_entry(kind, v)?,
            Self::Pin(v) => map.serialize_entry(kind, v)?,
            Self::Raw { body, .. } => map.serialize_entry(kind, body)?,
        }
        map.end()
    }
}

macro_rules! into_content {
    ($($ty:ty => $variant:ident),* $(,)?) => {$(
        impl From<$ty> for MessageContent {
            fn from(v: $ty) -> Self {
                Self::$variant(v)
            }
        }
    )*};
}

into_content! {
    Text => Text,
    Image => Image,
    Video => Video,
    Audio => Audio,
    Document => Document,
    Sticker => Sticker,
    Location => Location,
    Vec<Contact> => Contacts,
    Reaction => Reaction,
    TemplateMessage => Template,
    Interactive => Interactive,
    Pin => Pin,
}

impl Serialize for OutboundMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            messaging_product: &'static str,
            #[serde(flatten)]
            recipient: &'a Recipient,
            #[serde(skip_serializing_if = "Option::is_none")]
            context: Option<&'a Context>,
            #[serde(flatten)]
            content: &'a MessageContent,
            #[serde(skip_serializing_if = "Option::is_none")]
            biz_opaque_callback_data: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            category: Option<&'a DirectSendCategory>,
            #[serde(skip_serializing_if = "Option::is_none")]
            ttl_seconds: Option<u32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            direct_send_config: Option<&'a DirectSendConfig>,
        }
        Wire {
            messaging_product: "whatsapp",
            recipient: &self.recipient,
            context: self.context.as_ref(),
            content: &self.content,
            biz_opaque_callback_data: self.biz_opaque_callback_data.as_deref(),
            category: self.category.as_ref(),
            ttl_seconds: self.ttl_seconds,
            direct_send_config: self.direct_send_config.as_ref(),
        }
        .serialize(serializer)
    }
}

impl OutboundMessage {
    /// Any content to `recipient`.
    pub fn new(recipient: impl Into<Recipient>, content: impl Into<MessageContent>) -> Self {
        Self {
            recipient: recipient.into(),
            context: None,
            biz_opaque_callback_data: None,
            category: None,
            ttl_seconds: None,
            direct_send_config: None,
            content: content.into(),
        }
    }

    /// Text message.
    pub fn text(recipient: impl Into<Recipient>, body: impl Into<String>) -> Self {
        Self::new(recipient, Text::new(body))
    }

    /// Text message with `preview_url: true`.
    pub fn text_with_preview(recipient: impl Into<Recipient>, body: impl Into<String>) -> Self {
        Self::new(recipient, Text::new(body).preview_url(true))
    }

    /// Image by uploaded media id. Add a caption with
    /// `OutboundMessage::new(to, Image::new(id).caption(..))`.
    pub fn image_id(recipient: impl Into<Recipient>, id: impl Into<MediaId>) -> Self {
        Self::new(recipient, Image::new(MediaSource::id(id)))
    }

    /// Image by public URL.
    pub fn image_link(recipient: impl Into<Recipient>, url: impl Into<String>) -> Self {
        Self::new(recipient, Image::new(MediaSource::link(url)))
    }

    /// Video by uploaded media id.
    pub fn video_id(recipient: impl Into<Recipient>, id: impl Into<MediaId>) -> Self {
        Self::new(recipient, Video::new(MediaSource::id(id)))
    }

    /// Video by public URL.
    pub fn video_link(recipient: impl Into<Recipient>, url: impl Into<String>) -> Self {
        Self::new(recipient, Video::new(MediaSource::link(url)))
    }

    /// Basic audio by uploaded media id (see [`Audio::voice`] for voice
    /// messages).
    pub fn audio_id(recipient: impl Into<Recipient>, id: impl Into<MediaId>) -> Self {
        Self::new(recipient, Audio::new(MediaSource::id(id)))
    }

    /// Basic audio by public URL.
    pub fn audio_link(recipient: impl Into<Recipient>, url: impl Into<String>) -> Self {
        Self::new(recipient, Audio::new(MediaSource::link(url)))
    }

    /// Document by uploaded media id, shown as `filename`.
    pub fn document_id(
        recipient: impl Into<Recipient>,
        id: impl Into<MediaId>,
        filename: impl Into<String>,
    ) -> Self {
        Self::new(
            recipient,
            Document::new(MediaSource::id(id)).filename(filename),
        )
    }

    /// Document by public URL, shown as `filename`.
    pub fn document_link(
        recipient: impl Into<Recipient>,
        url: impl Into<String>,
        filename: impl Into<String>,
    ) -> Self {
        Self::new(
            recipient,
            Document::new(MediaSource::link(url)).filename(filename),
        )
    }

    /// Sticker by uploaded media id.
    pub fn sticker_id(recipient: impl Into<Recipient>, id: impl Into<MediaId>) -> Self {
        Self::new(recipient, Sticker::new(MediaSource::id(id)))
    }

    /// Sticker by public URL.
    pub fn sticker_link(recipient: impl Into<Recipient>, url: impl Into<String>) -> Self {
        Self::new(recipient, Sticker::new(MediaSource::link(url)))
    }

    /// Location pin. Add a name/address with
    /// `OutboundMessage::new(to, Location::new(lat, lon).name(..))`.
    pub fn location(recipient: impl Into<Recipient>, latitude: f64, longitude: f64) -> Self {
        Self::new(recipient, Location::new(latitude, longitude))
    }

    /// Contact cards.
    pub fn contacts(
        recipient: impl Into<Recipient>,
        contacts: impl IntoIterator<Item = Contact>,
    ) -> Self {
        Self::new(
            recipient,
            MessageContent::Contacts(contacts.into_iter().collect()),
        )
    }

    /// React to `message_id` with `emoji` (empty string: remove the
    /// reaction).
    pub fn reaction(
        recipient: impl Into<Recipient>,
        message_id: impl Into<MessageId>,
        emoji: impl Into<String>,
    ) -> Self {
        Self::new(recipient, Reaction::new(message_id, emoji))
    }

    /// Template message.
    pub fn template(recipient: impl Into<Recipient>, template: TemplateMessage) -> Self {
        Self::new(recipient, template)
    }

    /// Reply buttons (1–3).
    pub fn reply_buttons(
        recipient: impl Into<Recipient>,
        body: impl Into<String>,
        buttons: impl IntoIterator<Item = ReplyButton>,
    ) -> Self {
        Self::new(recipient, ReplyButtons::new(body, buttons))
    }

    /// List message.
    pub fn list(
        recipient: impl Into<Recipient>,
        body: impl Into<String>,
        button: impl Into<String>,
        sections: impl IntoIterator<Item = ListSection>,
    ) -> Self {
        Self::new(recipient, ListMessage::new(body, button, sections))
    }

    /// CTA URL button.
    pub fn cta_url(
        recipient: impl Into<Recipient>,
        body: impl Into<String>,
        display_text: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self::new(recipient, CtaUrl::new(body, display_text, url))
    }

    /// Location request.
    pub fn location_request(recipient: impl Into<Recipient>, body: impl Into<String>) -> Self {
        Self::new(recipient, LocationRequest::new(body))
    }

    /// Flow message.
    pub fn flow(
        recipient: impl Into<Recipient>,
        body: impl Into<String>,
        parameters: FlowParameters,
    ) -> Self {
        Self::new(recipient, FlowMessage::new(body, parameters))
    }

    /// Single-product message.
    pub fn product(
        recipient: impl Into<Recipient>,
        catalog_id: impl Into<CatalogId>,
        product_retailer_id: impl Into<String>,
    ) -> Self {
        Self::new(
            recipient,
            SingleProduct::new(catalog_id, product_retailer_id),
        )
    }

    /// Multi-product message.
    pub fn product_list(
        recipient: impl Into<Recipient>,
        header: impl Into<String>,
        body: impl Into<String>,
        catalog_id: impl Into<CatalogId>,
        sections: impl IntoIterator<Item = ProductSection>,
    ) -> Self {
        Self::new(
            recipient,
            ProductList::new(header, body, catalog_id, sections),
        )
    }

    /// Catalog message.
    pub fn catalog(recipient: impl Into<Recipient>, body: impl Into<String>) -> Self {
        Self::new(recipient, CatalogMessage::new(body))
    }

    /// Pin `message_id` in `group` for `expiration_days` (1–30).
    pub fn pin(
        group: impl Into<GroupId>,
        message_id: impl Into<MessageId>,
        expiration_days: u32,
    ) -> Self {
        Self::new(
            Recipient::Group(group.into()),
            Pin::Pin {
                message_id: message_id.into(),
                expiration_days,
            },
        )
    }

    /// Unpin `message_id` in `group`.
    pub fn unpin(group: impl Into<GroupId>, message_id: impl Into<MessageId>) -> Self {
        Self::new(
            Recipient::Group(group.into()),
            Pin::Unpin {
                message_id: message_id.into(),
            },
        )
    }

    /// Send as a contextual reply to `message_id`.
    #[must_use]
    pub fn reply_to(mut self, message_id: impl Into<MessageId>) -> Self {
        self.context = Some(Context {
            message_id: message_id.into(),
        });
        self
    }

    /// Set `biz_opaque_callback_data` (echoed in status webhooks).
    #[must_use]
    pub fn callback_data(mut self, data: impl Into<String>) -> Self {
        self.biz_opaque_callback_data = Some(data.into());
        self
    }

    /// Send through Direct Send in `category`.
    #[must_use]
    pub fn category(mut self, category: DirectSendCategory) -> Self {
        self.category = Some(category);
        self
    }

    /// Direct Send time-to-live in seconds.
    #[must_use]
    pub fn ttl_seconds(mut self, seconds: u32) -> Self {
        self.ttl_seconds = Some(seconds);
        self
    }

    /// Direct Send business-named template.
    #[must_use]
    pub fn direct_send_template_name(mut self, name: impl Into<String>) -> Self {
        self.direct_send_config = Some(DirectSendConfig {
            template_name: name.into(),
        });
        self
    }

    /// Check every limit Meta documents for this message, without sending
    /// it. [`super::Messages::send`] calls this first.
    ///
    /// The error's `field` is the JSON path of the offending value, e.g.
    /// `interactive.action.buttons[3]`.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_recipient(&self.recipient)?;
        if let Some(ctx) = &self.context {
            validate::non_empty("context.message_id", ctx.message_id.as_str())?;
            // messages/contextual-replies: "You cannot send a reaction
            // message as a contextual reply."
            if matches!(self.content, MessageContent::Reaction(_)) {
                return Err(ValidationError::new(
                    "context",
                    "a reaction cannot be sent as a contextual reply",
                ));
            }
        }
        // groups/groups-messaging: pins go to groups, and a mismatched
        // `recipient_type`/`to` is rejected.
        if matches!(self.content, MessageContent::Pin(_))
            && !matches!(self.recipient, Recipient::Group(_))
        {
            return Err(ValidationError::new(
                "recipient_type",
                "pin and unpin can only be sent to a group",
            ));
        }
        self.validate_direct_send()?;
        let direct_send = matches!(
            self.category,
            Some(DirectSendCategory::Utility | DirectSendCategory::Authentication)
        );
        self.content.validate(direct_send)
    }

    fn validate_direct_send(&self) -> Check {
        // direct-send/send-utility-and-authentication-messages:
        // "Authentication-category messages can't be sent to a
        // business-scoped user ID — use `to` with a phone number."
        if self.category == Some(DirectSendCategory::Authentication)
            && !self.recipient.supports_otp_buttons()
        {
            return Err(ValidationError::new(
                "recipient",
                "authentication-category Direct Send messages need a phone number (`to`)",
            ));
        }
        if let Some(ttl) = self.ttl_seconds {
            // direct-send/configure-message-ttl: TTL is Direct Send only;
            // utility 30..=43200, authentication 30..=900.
            let max = match &self.category {
                Some(DirectSendCategory::Utility) => Some(43200),
                Some(DirectSendCategory::Authentication) => Some(900),
                Some(DirectSendCategory::Other(_)) => None,
                Some(DirectSendCategory::Service) | None => {
                    return Err(ValidationError::new(
                        "ttl_seconds",
                        "custom TTL is only supported with a utility or authentication Direct Send category",
                    ));
                }
            };
            if let Some(max) = max
                && !(30..=max).contains(&ttl)
            {
                return Err(ValidationError::new(
                    "ttl_seconds",
                    format!("is {ttl}; the documented range for this category is 30..={max}"),
                ));
            }
        }
        if let Some(config) = &self.direct_send_config {
            // direct-send/business-named-templates: utility only;
            // `^[a-z0-9_]+$`, at most 512 characters.
            if !matches!(
                self.category,
                Some(DirectSendCategory::Utility | DirectSendCategory::Other(_))
            ) {
                return Err(ValidationError::new(
                    "direct_send_config",
                    "business-named templates are only supported for the utility category",
                ));
            }
            let name = &config.template_name;
            let field = "direct_send_config.template_name";
            validate::text(field, name, 512)?;
            if !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(ValidationError::new(
                    field,
                    "may only contain lowercase letters, digits and underscores",
                ));
            }
        }
        Ok(())
    }
}

fn validate_recipient(recipient: &Recipient) -> Check {
    match recipient {
        Recipient::Phone(p) => validate::non_empty("to", p),
        Recipient::User(u) => validate::non_empty("recipient", u.as_str()),
        Recipient::PhoneAndUser { phone, user } => {
            validate::non_empty("to", phone)?;
            validate::non_empty("recipient", user.as_str())
        }
        Recipient::Group(g) => validate::non_empty("to", g.as_str()),
        // `Recipient` is non-exhaustive; a future addressing mode is
        // Meta's to judge.
        _ => Ok(()),
    }
}
