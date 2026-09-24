//! The `messages` field: inbound messages, outbound message statuses and
//! value-level errors.
//!
//! Doc paths (relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`):
//! `webhooks/reference/messages` and every page under
//! `webhooks/reference/messages/` (text, image, audio, video, document,
//! sticker, location, contacts, interactive, button, order, reaction, system,
//! unsupported, edit, revoke, group, errors, status),
//! `reference/webhooks/whatsapp-incoming-webhook-payload`,
//! `business-scoped-user-ids` (identity fields), `pricing` (the `pricing`
//! object), `groups/webhooks` (group statuses), `flows/guides/flowswebhooks`
//! and `messages/address-messages` (`nfm_reply`),
//! `calling/user-call-permissions` (`call_permission_reply`),
//! `webhooks/reference/history` (`media_placeholder`).
//!
//! # Where the pages disagree with each other
//!
//! - `groups/webhooks` nests `pricing` inside `conversation` in its aggregated
//!   examples and elsewhere puts it next to `conversation`; both are accepted
//!   ([`Conversation::pricing`], [`Status::pricing`], [`Status::effective_pricing`]).
//! - `groups/webhooks` spells the participant `participant_recipient_id` in
//!   three examples; `status` and `business-scoped-user-ids` say
//!   `recipient_participant_id`. Both parse into
//!   [`Status::recipient_participant_id`].
//! - `order` documents `item_price` as an integer with example value `7.99`;
//!   it is an `f64` here.
//! - `status` pricing category is `authentication-international` while the
//!   conversation category is `authentication_international`; both map to
//!   [`PricingCategory::AuthenticationInternational`].

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use wa_core::GraphApiError;
use wa_core::ids::{CatalogId, GroupId, MediaId, MessageId, UserId, WaId};

use super::common::{Contact, Metadata};
use crate::open_enum::open_enum;

/// `value` of a `messages` change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number this change is about.
    pub metadata: Metadata,
    /// The WhatsApp users referenced by `messages` / `statuses`. Omitted for
    /// `failed` statuses and for system messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// Messages sent by users to the business.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<InboundMessage>,
    /// Status updates of messages the business sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<Status>,
    /// System-, app- or account-level errors (`webhooks/reference/messages/errors`).
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
}

/// One `messages[]` entry: a message a WhatsApp user sent to the business.
///
/// The type-specific part is [`InboundMessage::content`]; every other field
/// is common to all message types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Sender's phone number. Omitted for username adopters whose number Meta
    /// may not share (`business-scoped-user-ids`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<WaId>,
    /// Sender's business-scoped user id (BSUID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_user_id: Option<UserId>,
    /// Sender's parent BSUID, when parent BSUIDs are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_parent_user_id: Option<UserId>,
    /// Group the message was sent in (Groups API), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<GroupId>,
    /// Present in the button-reply examples of
    /// `templates/authentication-templates/*` and
    /// `payments/payments-br/one-click-payments`; Meta does not describe
    /// it. Kept verbatim so it is not silently dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_logical_id: Option<String>,
    /// WhatsApp message id (`wamid.…`).
    pub id: MessageId,
    /// When the webhook was triggered (reactions: when the user reacted).
    #[serde(with = "wa_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// Reply, forward or "Message business" button context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    /// Click to WhatsApp ad referral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referral: Option<Referral>,
    /// Message-level errors; set on `unsupported` messages.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
    /// The `type` and its matching payload object.
    #[serde(flatten)]
    pub content: MessageContent,
}

impl InboundMessage {
    /// The message `type` as Meta sent it, if it sent one.
    pub fn message_type(&self) -> Option<&str> {
        self.content.type_name()
    }
}

/// `context` of an inbound message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageContext {
    /// Business display number of the message replied to / referred from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Id of the message replied to (a call id for some call permission replies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MessageId>,
    /// Forwarded 5 times or fewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded: Option<bool>,
    /// Forwarded more than 5 times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequently_forwarded: Option<bool>,
    /// Product the user asked about with a "Message business" button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referred_product: Option<ReferredProduct>,
}

/// `context.referred_product`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferredProduct {
    /// Catalog id.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub catalog_id: CatalogId,
    /// Product retailer id (SKU).
    pub product_retailer_id: String,
}

open_enum! {
    /// `referral.source_type`.
    pub enum ReferralSourceType {
        /// A Click to WhatsApp ad.
        Ad => "ad",
        /// A post.
        Post => "post",
    }
}

open_enum! {
    /// `referral.media_type`.
    pub enum ReferralMediaType {
        /// Image ad.
        Image => "image",
        /// Video ad.
        Video => "video",
    }
}

/// `referral`: the Click to WhatsApp ad the user came from. Every property
/// is optional; `ctwa_clid` is omitted for WhatsApp Status ad placements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Referral {
    /// Ad URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Ad id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Source type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<ReferralSourceType>,
    /// Ad primary text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Ad headline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    /// Ad media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<ReferralMediaType>,
    /// Image URL (image ads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// Video URL (video ads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_url: Option<String>,
    /// Video thumbnail URL (video ads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    /// Click id, for the Conversions API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctwa_clid: Option<String>,
    /// Ad greeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome_message: Option<WelcomeMessage>,
}

/// `referral.welcome_message`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WelcomeMessage {
    /// Greeting text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// The type-specific part of a message: the `type` property and the object
/// named after it (`"type": "image", "image": {…}`).
///
/// Shared by inbound messages, SMB message echoes, history messages and the
/// inner message of an `edit`. Serializes back to the same wire shape.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MessageContent {
    /// `text`.
    Text(TextContent),
    /// `image`.
    Image(MediaContent),
    /// `audio` (voice notes have [`MediaContent::voice`] set).
    Audio(MediaContent),
    /// `video`.
    Video(MediaContent),
    /// `document`.
    Document(MediaContent),
    /// `sticker`.
    Sticker(MediaContent),
    /// `location`.
    Location(LocationContent),
    /// `contacts`: contact cards the user shared.
    Contacts(Vec<SharedContact>),
    /// `interactive`: a reply to an interactive message.
    Interactive(InteractiveReply),
    /// `button`: a template quick-reply button tap.
    Button(ButtonContent),
    /// `order`: a cart sent from a catalog, single- or multi-product message.
    Order(OrderContent),
    /// `reaction`.
    Reaction(ReactionContent),
    /// `system`: the user changed number or BSUID.
    System(SystemContent),
    /// `unsupported`: see [`InboundMessage::errors`] for why.
    Unsupported(UnsupportedContent),
    /// `edit`: the user edited an earlier message.
    Edit(EditContent),
    /// `revoke`: the user deleted an earlier message.
    Revoke(RevokeContent),
    /// `media_placeholder` (history only): a media message whose content
    /// arrives in a later history webhook.
    MediaPlaceholder,
    /// A documented `type` whose payload did not match its documented shape.
    /// Kept (with the reason) instead of failing the whole webhook.
    Invalid {
        /// The `type` value.
        message_type: String,
        /// Why the typed parse failed.
        error: String,
        /// The type-specific properties as received (`type`, the payload
        /// object, and any property this crate does not model).
        raw: Value,
    },
    /// A `type` this crate does not know, or no `type` at all.
    Unknown {
        /// The `type` value, if present.
        message_type: Option<String>,
        /// The type-specific properties as received (as for `Invalid`).
        raw: Value,
    },
}

impl MessageContent {
    /// The wire `type` of this content.
    pub fn type_name(&self) -> Option<&str> {
        Some(match self {
            Self::Text(_) => "text",
            Self::Image(_) => "image",
            Self::Audio(_) => "audio",
            Self::Video(_) => "video",
            Self::Document(_) => "document",
            Self::Sticker(_) => "sticker",
            Self::Location(_) => "location",
            Self::Contacts(_) => "contacts",
            Self::Interactive(_) => "interactive",
            Self::Button(_) => "button",
            Self::Order(_) => "order",
            Self::Reaction(_) => "reaction",
            Self::System(_) => "system",
            Self::Unsupported(_) => "unsupported",
            Self::Edit(_) => "edit",
            Self::Revoke(_) => "revoke",
            Self::MediaPlaceholder => "media_placeholder",
            Self::Invalid { message_type, .. } => message_type,
            Self::Unknown { message_type, .. } => return message_type.as_deref(),
        })
    }

    /// Parse the type-specific properties of a message. Never fails: an
    /// unknown type becomes [`Self::Unknown`], a malformed known one
    /// [`Self::Invalid`].
    fn from_map(map: Map<String, Value>) -> Self {
        let Some(ty) = map.get("type").and_then(Value::as_str).map(str::to_owned) else {
            return Self::Unknown {
                message_type: None,
                raw: Value::Object(map),
            };
        };
        // A type whose object is absent parses as an empty object: the
        // all-optional payloads (`unsupported`, `button`, `system`, …) are
        // then typed, and the ones with required properties still fall to
        // `Invalid`. `groups/groups-messaging` shows an `unsupported` group
        // message with no `unsupported` object at all.
        let empty = Value::Object(Map::new());
        let payload = map.get(ty.as_str()).unwrap_or(&empty);
        let parsed = match ty.as_str() {
            "text" => TextContent::deserialize(payload).map(Self::Text),
            "image" => MediaContent::deserialize(payload).map(Self::Image),
            "audio" => MediaContent::deserialize(payload).map(Self::Audio),
            "video" => MediaContent::deserialize(payload).map(Self::Video),
            "document" => MediaContent::deserialize(payload).map(Self::Document),
            "sticker" => MediaContent::deserialize(payload).map(Self::Sticker),
            "location" => LocationContent::deserialize(payload).map(Self::Location),
            "contacts" => Vec::<SharedContact>::deserialize(payload).map(Self::Contacts),
            "interactive" => InteractiveReply::deserialize(payload).map(Self::Interactive),
            "button" => ButtonContent::deserialize(payload).map(Self::Button),
            "order" => OrderContent::deserialize(payload).map(Self::Order),
            "reaction" => ReactionContent::deserialize(payload).map(Self::Reaction),
            "system" => SystemContent::deserialize(payload).map(Self::System),
            "unsupported" => UnsupportedContent::deserialize(payload).map(Self::Unsupported),
            "edit" => EditContent::deserialize(payload).map(Self::Edit),
            "revoke" => RevokeContent::deserialize(payload).map(Self::Revoke),
            "media_placeholder" => Ok(Self::MediaPlaceholder),
            _ => {
                return Self::Unknown {
                    message_type: Some(ty),
                    raw: Value::Object(map),
                };
            }
        };
        parsed.unwrap_or_else(|e| Self::Invalid {
            message_type: ty,
            error: e.to_string(),
            raw: Value::Object(map),
        })
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Map::<String, Value>::deserialize(deserializer).map(Self::from_map)
    }
}

impl Serialize for MessageContent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        fn tagged<S: Serializer, T: Serialize + ?Sized>(
            serializer: S,
            ty: &str,
            payload: &T,
        ) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry("type", ty)?;
            map.serialize_entry(ty, payload)?;
            map.end()
        }
        match self {
            Self::Text(p) => tagged(serializer, "text", p),
            Self::Image(p) => tagged(serializer, "image", p),
            Self::Audio(p) => tagged(serializer, "audio", p),
            Self::Video(p) => tagged(serializer, "video", p),
            Self::Document(p) => tagged(serializer, "document", p),
            Self::Sticker(p) => tagged(serializer, "sticker", p),
            Self::Location(p) => tagged(serializer, "location", p),
            Self::Contacts(p) => tagged(serializer, "contacts", p),
            Self::Interactive(p) => tagged(serializer, "interactive", p),
            Self::Button(p) => tagged(serializer, "button", p),
            Self::Order(p) => tagged(serializer, "order", p),
            Self::Reaction(p) => tagged(serializer, "reaction", p),
            Self::System(p) => tagged(serializer, "system", p),
            Self::Unsupported(p) => tagged(serializer, "unsupported", p),
            Self::Edit(p) => tagged(serializer, "edit", p),
            Self::Revoke(p) => tagged(serializer, "revoke", p),
            Self::MediaPlaceholder => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("type", "media_placeholder")?;
                map.end()
            }
            Self::Invalid { raw, .. } | Self::Unknown { raw, .. } => serialize_raw(serializer, raw),
        }
    }
}

/// Serialize a raw object's entries as a map (so it can be `flatten`ed back
/// into its message); a non-object is wrapped as `{"raw": …}`.
fn serialize_raw<S: Serializer>(serializer: S, raw: &Value) -> Result<S::Ok, S::Error> {
    if let Value::Object(entries) = raw {
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (k, v) in entries {
            map.serialize_entry(k, v)?;
        }
        map.end()
    } else {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("raw", raw)?;
        map.end()
    }
}

/// `text` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextContent {
    /// Message text.
    pub body: String,
}

/// Payload of `image`, `audio`, `video`, `document` and `sticker`.
///
/// One struct for the five media types: they share `id`/`mime_type`/
/// `sha256`/`url` and each adds at most one or two properties, noted below.
/// `id` is optional because history webhooks describe media messages whose
/// asset ids arrive separately (`webhooks/reference/history`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaContent {
    /// Media asset id; `GET /{id}` returns a download URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MediaId>,
    /// MIME type, e.g. `image/jpeg`, `audio/ogg; codecs=opus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// SHA-256 of the asset, as Meta encodes it (base64 in the examples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Direct download URL (needs your access token). Rolled out gradually
    /// from November 2025, so often absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Caption (`image`, `video`, `document`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// File name (`document`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Recorded with the WhatsApp voice recorder (`audio`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<bool>,
    /// Animated sticker (`sticker`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animated: Option<bool>,
}

/// `location` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocationContent {
    /// Latitude, decimal degrees.
    pub latitude: f64,
    /// Longitude, decimal degrees.
    pub longitude: f64,
    /// Place name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// URL, usually only for business locations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

open_enum! {
    /// `contacts[].origin` (`business-scoped-user-ids`, contacts webhook).
    pub enum ContactShareOrigin {
        /// The user tapped a `REQUEST_CONTACT_INFO` button.
        ContactRequest => "contact_request",
        /// The user shared a contact directly in the chat (wire value `other`).
        SharedDirectly => "other",
    }
}

/// One shared contact card (`contacts` message). Many properties are
/// omitted when the user or their device does not share them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedContact {
    /// Addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<ContactAddress>,
    /// Birthday, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birthday: Option<String>,
    /// Email addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emails: Vec<ContactEmail>,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<ContactName>,
    /// Organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<ContactOrg>,
    /// Phone numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phones: Vec<ContactPhone>,
    /// Websites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<ContactUrl>,
    /// Raw vCard, when the user shared a contact directly (`origin: other`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcard: Option<String>,
    /// How the card was shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ContactShareOrigin>,
}

/// `contacts[].addresses[]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactAddress {
    /// Street.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    /// City.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// State.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// ZIP code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    /// Country name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// ISO country code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    /// Address type, e.g. `Home`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub address_type: Option<String>,
}

/// `contacts[].emails[]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactEmail {
    /// Email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Email type, e.g. `Personal`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub email_type: Option<String>,
}

/// `contacts[].name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactName {
    /// Formatted full name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formatted_name: Option<String>,
    /// First name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    /// Last name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// Middle name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub middle_name: Option<String>,
    /// Suffix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// Prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// `contacts[].org`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactOrg {
    /// Company.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    /// Department.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    /// Job title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// `contacts[].phones[]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactPhone {
    /// Phone number as shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// WhatsApp number of that phone, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// Phone type, e.g. `CELL`, `MOBILE`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub phone_type: Option<String>,
}

/// `contacts[].urls[]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactUrl {
    /// URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// URL type, e.g. `Company`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub url_type: Option<String>,
}

/// `interactive` payload: the user's answer to an interactive message,
/// tagged by `interactive.type`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum InteractiveReply {
    /// `button_reply`: a reply button was tapped.
    ButtonReply(ButtonReply),
    /// `list_reply`: a list row was selected.
    ListReply(ListReply),
    /// `nfm_reply`: a Flow was completed or an address message answered.
    NfmReply(NfmReply),
    /// `call_permission_reply`: the user answered a call permission request.
    CallPermissionReply(CallPermissionReply),
    /// An `interactive.type` this crate does not know.
    Unknown {
        /// `interactive.type`, if present.
        reply_type: Option<String>,
        /// The whole `interactive` object.
        raw: Value,
    },
}

impl<'de> Deserialize<'de> for InteractiveReply {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(deserializer)?;
        let ty = map.get("type").and_then(Value::as_str);
        let payload = ty.and_then(|t| map.get(t)).unwrap_or(&Value::Null);
        let reply = match ty {
            Some("button_reply") => ButtonReply::deserialize(payload).map(Self::ButtonReply),
            Some("list_reply") => ListReply::deserialize(payload).map(Self::ListReply),
            Some("nfm_reply") => NfmReply::deserialize(payload).map(Self::NfmReply),
            Some("call_permission_reply") => {
                CallPermissionReply::deserialize(payload).map(Self::CallPermissionReply)
            }
            other => Ok(Self::Unknown {
                reply_type: other.map(str::to_owned),
                raw: Value::Object(map.clone()),
            }),
        };
        reply.map_err(D::Error::custom)
    }
}

impl Serialize for InteractiveReply {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        fn tagged<S: Serializer, T: Serialize>(
            serializer: S,
            ty: &str,
            payload: &T,
        ) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry("type", ty)?;
            map.serialize_entry(ty, payload)?;
            map.end()
        }
        match self {
            Self::ButtonReply(p) => tagged(serializer, "button_reply", p),
            Self::ListReply(p) => tagged(serializer, "list_reply", p),
            Self::NfmReply(p) => tagged(serializer, "nfm_reply", p),
            Self::CallPermissionReply(p) => tagged(serializer, "call_permission_reply", p),
            Self::Unknown { raw, .. } => raw.serialize(serializer),
        }
    }
}

/// `interactive.button_reply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonReply {
    /// Button id you set when sending.
    pub id: String,
    /// Button label.
    pub title: String,
}

/// `interactive.list_reply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListReply {
    /// Row id you set when sending.
    pub id: String,
    /// Row title.
    pub title: String,
    /// Row description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `interactive.nfm_reply` (Flows, address messages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NfmReply {
    /// `flow` for Flows, `address_message` for address messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Text the user sees (`Sent` for Flows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// JSON **string** with the Flow's / address form's data.
    pub response_json: String,
}

impl NfmReply {
    /// Parse [`Self::response_json`]. Meta delivers it as a string whose
    /// shape your Flow defines, so it is decoded on demand, not eagerly.
    pub fn response<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.response_json)
    }
}

open_enum! {
    /// `call_permission_reply.response`.
    pub enum CallPermissionResponse {
        /// Permission granted.
        Accept => "accept",
        /// Permission refused.
        Reject => "reject",
    }
}

open_enum! {
    /// `call_permission_reply.response_source`.
    pub enum CallPermissionSource {
        /// The user decided.
        UserAction => "user_action",
        /// Granted automatically because the user called the business.
        Automatic => "automatic",
    }
}

/// `interactive.call_permission_reply` (`calling/user-call-permissions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallPermissionReply {
    /// The user's answer.
    pub response: CallPermissionResponse,
    /// Permanent permission (always `false` for temporary ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_permanent: Option<bool>,
    /// When an accepted temporary permission expires.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiration_timestamp: Option<OffsetDateTime>,
    /// Who decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_source: Option<CallPermissionSource>,
}

/// `button` payload: a template quick-reply button tap.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonContent {
    /// Payload set on the button when the template was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// Button label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// `order` payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrderContent {
    /// Catalog id.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::id_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub catalog_id: Option<CatalogId>,
    /// Text accompanying the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Ordered items.
    #[serde(default)]
    pub product_items: Vec<ProductItem>,
}

/// `order.product_items[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductItem {
    /// Product retailer id (SKU).
    pub product_retailer_id: String,
    /// Quantity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
    /// Unit price. The page says integer but its example is `7.99`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_price: Option<f64>,
    /// Currency code, e.g. `USD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

/// `reaction` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionContent {
    /// The message reacted to.
    pub message_id: MessageId,
    /// The emoji; `None` when the user removed their reaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
}

open_enum! {
    /// `system.type`.
    pub enum SystemMessageType {
        /// The user changed their phone number (`webhooks/reference/messages/system`).
        UserChangedNumber => "user_changed_number",
        /// The user's BSUID changed (`business-scoped-user-ids`).
        UserChangedUserId => "user_changed_user_id",
    }
}

/// `system` payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemContent {
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The user's new phone number, when Meta can share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// The user's new BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    /// The user's new parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<UserId>,
    /// What changed.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub system_type: Option<SystemMessageType>,
}

/// `unsupported` payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedContent {
    /// The type Cloud API cannot deliver (e.g. `poll_update`, `edit`).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub unsupported_type: Option<String>,
}

/// `edit` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditContent {
    /// The message that was edited.
    pub original_message_id: MessageId,
    /// Its new content.
    pub message: EditedMessage,
}

/// `edit.message`: the edited message's new content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditedMessage {
    /// Context of the edited message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    /// New `type` and payload.
    #[serde(flatten)]
    pub content: Box<MessageContent>,
}

/// `revoke` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeContent {
    /// The message that was deleted.
    pub original_message_id: MessageId,
}

open_enum! {
    /// `statuses[].status`.
    pub enum MessageStatus {
        /// Sent from Meta's servers (one check mark).
        Sent => "sent",
        /// Delivered to the device (two check marks).
        Delivered => "delivered",
        /// Displayed in an open chat (two blue check marks).
        Read => "read",
        /// First play of a voice message.
        Played => "played",
        /// Could not be sent or delivered; see [`Status::errors`].
        Failed => "failed",
    }
}

/// One `statuses[]` entry: the status of a message the business sent.
///
/// The user's `username`, `user_id` and `parent_user_id` are not on this
/// object: they are in the change's `contacts` array, which
/// [`crate::WebhookEvent::StatusUpdated`] matches for you.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// Id of the message this status is about.
    pub id: MessageId,
    /// The status.
    pub status: MessageStatus,
    /// When the webhook was triggered.
    #[serde(with = "wa_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// Recipient phone number, or the group id for group messages. Omitted
    /// when sent to a BSUID whose phone number Meta may not share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    /// Recipient BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_user_id: Option<UserId>,
    /// Recipient parent BSUID. Accepts the `parent_recipient_user_id`
    /// spelling the BSUID page used until its 2026-05-05 correction.
    #[serde(
        default,
        alias = "parent_recipient_user_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub recipient_parent_user_id: Option<UserId>,
    /// `group` for messages sent to a group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_type: Option<String>,
    /// Group participant's phone number (group messages).
    #[serde(
        default,
        alias = "participant_recipient_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub recipient_participant_id: Option<String>,
    /// Group participant's BSUID (group messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_participant_user_id: Option<UserId>,
    /// Group participant's parent BSUID (group messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_participant_parent_user_id: Option<UserId>,
    /// Recipient identity key hash (identity change check).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_identity_key_hash: Option<String>,
    /// Group id, in the incoming-payload reference schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<GroupId>,
    /// The `biz_opaque_callback_data` you sent the message with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biz_opaque_callback_data: Option<String>,
    /// Conversation; omitted from v24.0 except in free entry point windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Conversation>,
    /// Pricing; on `sent` and on one of `delivered`/`read`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
    /// Why a `failed` message failed.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
}

impl Status {
    /// [`Self::pricing`], or the `pricing` that some `groups/webhooks`
    /// examples nest inside `conversation`.
    pub fn effective_pricing(&self) -> Option<&Pricing> {
        self.pricing
            .as_ref()
            .or_else(|| self.conversation.as_ref()?.pricing.as_ref())
    }
}

/// `statuses[].conversation`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Conversation id (per free entry point window from v24.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// When the conversation expires (`sent` status only).
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiration_timestamp: Option<OffsetDateTime>,
    /// Conversation origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ConversationOrigin>,
    /// Pricing, where `groups/webhooks` nests it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
}

/// `conversation.origin`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationOrigin {
    /// Conversation category.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub origin_type: Option<PricingCategory>,
}

open_enum! {
    /// `pricing.pricing_model`.
    pub enum PricingModel {
        /// Conversation-based pricing (webhooks sent before 2025-07-01).
        Cbp => "CBP",
        /// Per-message pricing.
        Pmp => "PMP",
    }
}

open_enum! {
    /// `pricing.type` (`pricing`).
    pub enum PricingType {
        /// Billable.
        Regular => "regular",
        /// Free: sent inside a customer service window.
        FreeCustomerService => "free_customer_service",
        /// Free: sent inside a free entry point window.
        FreeEntryPoint => "free_entry_point",
    }
}

open_enum! {
    /// Pricing / conversation category (`pricing.category`,
    /// `conversation.origin.type`).
    pub enum PricingCategory {
        /// Authentication.
        Authentication => "authentication",
        /// Authentication-international.
        AuthenticationInternational => "authentication-international" | "authentication_international",
        /// Marketing.
        Marketing => "marketing",
        /// Marketing Messages API for WhatsApp.
        MarketingLite => "marketing_lite",
        /// Free entry point conversation.
        ReferralConversion => "referral_conversion",
        /// Service.
        Service => "service",
        /// Utility.
        Utility => "utility",
        /// Group marketing (`groups/webhooks`).
        GroupMarketing => "group_marketing",
        /// Group utility (`groups/webhooks`).
        GroupUtility => "group_utility",
        /// Group service (`groups/webhooks`).
        GroupService => "group_service",
    }
}

/// `statuses[].pricing` (`pricing`, `webhooks/reference/messages/status`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pricing {
    /// Billable. Meta plans to deprecate it; prefer `pricing_type` + `category`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billable: Option<bool>,
    /// Pricing model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_model: Option<PricingModel>,
    /// Whether and why the message is free.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub pricing_type: Option<PricingType>,
    /// Rate applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<PricingCategory>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(v: &Value) -> InboundMessage {
        InboundMessage::deserialize(v).unwrap()
    }

    #[test]
    fn unknown_type_keeps_type_specific_properties_only() {
        let m = message(&json!({
            "from": "1", "id": "wamid.X", "timestamp": "1700000000",
            "type": "poll", "poll": {"q": "?"}
        }));
        let MessageContent::Unknown { message_type, raw } = &m.content else {
            panic!("{:?}", m.content)
        };
        assert_eq!(message_type.as_deref(), Some("poll"));
        assert_eq!(raw, &json!({"type": "poll", "poll": {"q": "?"}}));
        assert_eq!(m.message_type(), Some("poll"));
        // Round-trips to the same wire shape.
        let back = serde_json::to_value(&m).unwrap();
        assert_eq!(back["poll"], json!({"q": "?"}));
        assert_eq!(message(&back), m);
    }

    #[test]
    fn known_type_with_bad_payload_is_invalid_not_an_error() {
        let m = message(&json!({
            "id": "wamid.X", "timestamp": "1700000000",
            "type": "location", "location": {"latitude": "north"}
        }));
        let MessageContent::Invalid {
            message_type,
            error,
            ..
        } = &m.content
        else {
            panic!("{:?}", m.content)
        };
        assert_eq!(message_type, "location");
        assert!(!error.is_empty());
    }

    #[test]
    fn missing_type_is_unknown_without_type() {
        let m = message(&json!({"id": "wamid.X", "timestamp": 1700000000, "interactive": {}}));
        assert!(matches!(
            m.content,
            MessageContent::Unknown {
                message_type: None,
                ..
            }
        ));
    }

    #[test]
    fn unknown_interactive_reply_is_kept() {
        let m = message(&json!({
            "id": "wamid.X", "timestamp": "1700000000", "type": "interactive",
            "interactive": {"type": "interactive", "action": "address_message", "nfm_reply": {"response_json": "{}"}}
        }));
        let MessageContent::Interactive(InteractiveReply::Unknown { reply_type, raw }) = &m.content
        else {
            panic!("{:?}", m.content)
        };
        assert_eq!(reply_type.as_deref(), Some("interactive"));
        assert_eq!(raw["action"], "address_message");
    }

    #[test]
    fn edited_message_content_is_typed_and_round_trips() {
        let m = message(&json!({
            "id": "wamid.E", "timestamp": "1749854575", "type": "edit",
            "edit": {"original_message_id": "wamid.O", "message": {
                "context": {"id": "M0"}, "type": "image",
                "image": {"caption": "new", "mime_type": "image/jpeg", "sha256": "x", "id": "1"}
            }}
        }));
        let MessageContent::Edit(edit) = &m.content else {
            panic!("{:?}", m.content)
        };
        assert_eq!(edit.original_message_id.as_str(), "wamid.O");
        let MessageContent::Image(img) = edit.message.content.as_ref() else {
            panic!()
        };
        assert_eq!(img.caption.as_deref(), Some("new"));
        let back = serde_json::to_value(&m).unwrap();
        assert_eq!(message(&back), m);
    }

    #[test]
    fn pricing_nested_in_conversation_is_found() {
        let s: Status = serde_json::from_value(json!({
            "id": "wamid.X", "status": "read", "timestamp": "1",
            "participant_recipient_id": "1650",
            "conversation": {"id": "c", "pricing": {"category": "group_marketing"}}
        }))
        .unwrap();
        assert_eq!(s.recipient_participant_id.as_deref(), Some("1650"));
        assert_eq!(
            s.effective_pricing().and_then(|p| p.category.clone()),
            Some(PricingCategory::GroupMarketing)
        );
    }
}
