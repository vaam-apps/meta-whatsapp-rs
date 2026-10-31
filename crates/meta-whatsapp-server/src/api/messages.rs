//! Messages (scope `send`; docs/design/server.md, sections 4.2 and 4.3).
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `POST /v1/numbers/{pn}/messages` | `POST /{pn}/messages` (`messages/send-messages`) |
//! | `POST /v1/numbers/{pn}/messages/{message_id}/read` | `POST /{pn}/messages` with `status: read` (`messages/mark-message-as-read`), and a typing indicator (`typing-indicators`) when asked |
//!
//! The message is a union the service owns, named after Meta's Cloud API
//! message object (so Meta's pages describe each type), mapped onto the
//! library's builders and checked by `OutboundMessage::validate` before
//! any request: `text`; `image`, `video`, `audio`, `document`, `sticker`
//! by uploaded `id` or `https://` `link` (Meta fetches it, never the
//! service); `location`, `contacts`, `reaction`; `template` (Meta's
//! object); `interactive` `button`, `list` and `cta_url`. Any other type,
//! Direct Send included, is `422 unsupported_message_type`. Meta enforces
//! the 24-hour window: outside it a free-form message is `409
//! customer_service_window_closed`.
//!
//! Recipients: `{"phone"}` (E.164 **with** `+`: a digits-only number is
//! refused before any request, since Meta would read it as local to the
//! sender's country), `{"user_id"}` (a BSUID), both (Meta uses the phone
//! number), or `{"group_id"}`.
//!
//! Sends take an `Idempotency-Key` ([`crate::idempotency`]).
//!
//! Since roadmap S5b the rules are the core's
//! ([`meta_whatsapp_server_core::messages`]: the message building, the
//! recipient and E.164 rules, sending and marking read as the `Messages`
//! service). This module keeps the request's types with their OpenAPI
//! schema, reads the body into them, converts them into the core's
//! request (field by field: a field on one side only fails to compile),
//! calls the service, and renders its answer (`202` with the accepted
//! message, `204` once marked read).

use meta_whatsapp_rs::client::messages::OutboundMessage;
use meta_whatsapp_rs::core::ids::{UserId, WaId};
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequest, Path, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode, Uri};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::messages::{self as domain, Messages};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::idempotency::{self, Fingerprint, KeyHeader, Success};
use crate::state::AppState;

// ─── The request ─────────────────────────────────────────────────────────

/// Who receives a message: a phone number, a business-scoped user id
/// (BSUID), both (Meta then uses the phone number), or a group.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = MessageRecipient)]
pub struct RecipientObject {
    /// E.164 **with** its `+` (`+16505551234`): a number without it is
    /// refused, as Meta would read it as local to the sender's country.
    pub phone: Option<String>,
    /// Business-scoped user id (e.g. `US.13491208655302741918`), from a
    /// webhook's `user_id`.
    pub user_id: Option<String>,
    /// A group's id (Groups API); alone.
    pub group_id: Option<String>,
}

/// The message types the service sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    /// `text`.
    Text,
    /// `image`.
    Image,
    /// `video`.
    Video,
    /// `audio`.
    Audio,
    /// `document`.
    Document,
    /// `sticker`.
    Sticker,
    /// `location`.
    Location,
    /// `contacts`.
    Contacts,
    /// `reaction`.
    Reaction,
    /// `template`.
    Template,
    /// `interactive` (`button`, `list`, `cta_url`).
    Interactive,
}

/// `text` (`messages/text-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = TextContent)]
pub struct TextObject {
    /// Body text, at most 4,096 characters.
    pub body: String,
    /// Ask WhatsApp to render a preview of the first URL.
    pub preview_url: Option<bool>,
}

/// `image` (`messages/image-messages`): an uploaded `id` or an `https://`
/// `link`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ImageContent)]
pub struct ImageObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// Caption, at most 1,024 characters.
    pub caption: Option<String>,
}

/// `video` (`messages/video-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = VideoContent)]
pub struct VideoObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// Caption, at most 1,024 characters.
    pub caption: Option<String>,
}

/// `audio` (`messages/audio-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AudioContent)]
pub struct AudioObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// `true`: a voice message (an Ogg file with the OPUS codec).
    pub voice: Option<bool>,
}

/// `document` (`messages/document-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = DocumentContent)]
pub struct DocumentObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// Caption, at most 1,024 characters.
    pub caption: Option<String>,
    /// File name shown, with its extension.
    pub filename: Option<String>,
}

/// `sticker` (`messages/sticker-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = StickerContent)]
pub struct StickerObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
}

/// A coordinate in decimal degrees: Meta's pages write it as a string
/// (`"37.44216251868683"`); a JSON number is accepted too.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum Coordinate {
    /// A JSON number.
    Number(f64),
    /// A string, as Meta's pages write it.
    Text(String),
}

/// `location` (`messages/location-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = LocationContent)]
pub struct LocationObject {
    /// Latitude, -90 to 90.
    pub latitude: Coordinate,
    /// Longitude, -180 to 180.
    pub longitude: Coordinate,
    /// Place name.
    pub name: Option<String>,
    /// Address.
    pub address: Option<String>,
}

/// A contact card's `name`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ContactName)]
pub struct ContactNameObject {
    /// Full name, as shown. Required.
    pub formatted_name: String,
    /// First name.
    pub first_name: Option<String>,
    /// Last name.
    pub last_name: Option<String>,
    /// Middle name.
    pub middle_name: Option<String>,
    /// Suffix.
    pub suffix: Option<String>,
    /// Prefix.
    pub prefix: Option<String>,
}

/// A contact card's address.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ContactAddress)]
pub struct ContactAddressObject {
    /// Street and number.
    pub street: Option<String>,
    /// City.
    pub city: Option<String>,
    /// State.
    pub state: Option<String>,
    /// Postal code.
    pub zip: Option<String>,
    /// Country.
    pub country: Option<String>,
    /// Two-letter country code.
    pub country_code: Option<String>,
    /// Label, e.g. `Home`.
    #[serde(rename = "type")]
    pub label: Option<String>,
}

/// A contact card's email address.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ContactEmail)]
pub struct ContactEmailObject {
    /// Address.
    pub email: String,
    /// Label, e.g. `Work`.
    #[serde(rename = "type")]
    pub label: Option<String>,
}

/// A contact card's organization.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ContactOrg)]
pub struct ContactOrgObject {
    /// Company.
    pub company: Option<String>,
    /// Department.
    pub department: Option<String>,
    /// Job title.
    pub title: Option<String>,
}

/// A contact card's phone number (content: shown as written).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ContactPhone)]
pub struct ContactPhoneObject {
    /// The number, as shown.
    pub phone: String,
    /// Label, e.g. `Mobile`.
    #[serde(rename = "type")]
    pub label: Option<String>,
    /// The contact's WhatsApp id: WhatsApp then offers "Message".
    pub wa_id: Option<String>,
}

/// A contact card's website.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ContactUrl)]
pub struct ContactUrlObject {
    /// URL.
    pub url: String,
    /// Label, e.g. `Company`.
    #[serde(rename = "type")]
    pub label: Option<String>,
}

/// One card of a `contacts` message (`messages/contacts-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactCard {
    /// Addresses.
    #[serde(default)]
    pub addresses: Vec<ContactAddressObject>,
    /// Birthday, `YYYY-MM-DD`.
    pub birthday: Option<String>,
    /// Email addresses.
    #[serde(default)]
    pub emails: Vec<ContactEmailObject>,
    /// Name.
    pub name: ContactNameObject,
    /// Organization.
    pub org: Option<ContactOrgObject>,
    /// Phone numbers.
    #[serde(default)]
    pub phones: Vec<ContactPhoneObject>,
    /// Websites.
    #[serde(default)]
    pub urls: Vec<ContactUrlObject>,
}

/// `reaction` (`messages/reaction-messages`): an emoji on a received
/// message; an empty `emoji` removes it.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ReactionContent)]
pub struct ReactionObject {
    /// The received message's id (`wamid.…`).
    pub message_id: String,
    /// The emoji.
    pub emoji: String,
}

/// The interactive types the service sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InteractiveType {
    /// Reply buttons (`messages/interactive-reply-buttons-messages`).
    Button,
    /// A list (`messages/interactive-list-messages`).
    List,
    /// A URL button (`messages/interactive-cta-url-messages`).
    CtaUrl,
}

/// `{"text": …}`: an interactive message's body or footer.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractiveText {
    /// The text.
    pub text: String,
}

/// A media asset of an interactive header: an uploaded `id` or an
/// `https://` `link`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderMedia {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// File name (documents only).
    pub filename: Option<String>,
}

/// The kinds of interactive header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeaderType {
    /// Text.
    Text,
    /// Image.
    Image,
    /// Video.
    Video,
    /// Document.
    Document,
}

/// An interactive message's header: `list` takes `text` only.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractiveHeader {
    /// Which kind.
    #[serde(rename = "type")]
    pub kind: HeaderType,
    /// The text, for `text`.
    pub text: Option<String>,
    /// The image, for `image`.
    pub image: Option<HeaderMedia>,
    /// The video, for `video`.
    pub video: Option<HeaderMedia>,
    /// The document, for `document`.
    pub document: Option<HeaderMedia>,
}

/// A reply button's `reply`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = Reply)]
pub struct ReplyObject {
    /// Returned in the reply's webhook; unique in the message.
    pub id: String,
    /// Label, at most 20 characters.
    pub title: String,
}

/// A reply button: `{"type": "reply", "reply": {…}}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ReplyButton)]
pub struct ReplyButtonObject {
    /// `reply`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The button.
    pub reply: ReplyObject,
}

/// A list row.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ListRow)]
pub struct ListRowObject {
    /// Returned in the reply's webhook.
    pub id: String,
    /// Title, at most 24 characters.
    pub title: String,
    /// Description, at most 72 characters.
    pub description: Option<String>,
}

/// A list section.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ListSection)]
pub struct ListSectionObject {
    /// Title, at most 24 characters.
    pub title: Option<String>,
    /// Its rows.
    pub rows: Vec<ListRowObject>,
}

/// A URL button's `parameters`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CtaUrlParameters {
    /// Button label, at most 20 characters.
    pub display_text: String,
    /// The URL the button opens.
    pub url: String,
}

/// An interactive message's `action`: `buttons` for `button`; `button` and
/// `sections` for `list`; `name` (`cta_url`) and `parameters` for
/// `cta_url`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractiveAction {
    /// Reply buttons, 1 to 3.
    pub buttons: Option<Vec<ReplyButtonObject>>,
    /// The list's button label.
    pub button: Option<String>,
    /// The list's sections.
    pub sections: Option<Vec<ListSectionObject>>,
    /// `cta_url`.
    pub name: Option<String>,
    /// The URL button.
    pub parameters: Option<CtaUrlParameters>,
}

/// `interactive`, as Meta writes it: `button`, `list` or `cta_url`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = InteractiveContent)]
pub struct InteractiveObject {
    /// Which kind.
    #[serde(rename = "type")]
    pub kind: InteractiveType,
    /// Header.
    pub header: Option<InteractiveHeader>,
    /// Body.
    pub body: InteractiveText,
    /// Footer.
    pub footer: Option<InteractiveText>,
    /// Buttons, list or URL button.
    pub action: InteractiveAction,
}

/// A message to send: `type` names the one content object present.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SendMessage {
    /// Who receives it.
    pub to: RecipientObject,
    /// Which content object follows.
    #[serde(rename = "type")]
    pub kind: MessageType,
    /// For `text`.
    pub text: Option<TextObject>,
    /// For `image`.
    pub image: Option<ImageObject>,
    /// For `video`.
    pub video: Option<VideoObject>,
    /// For `audio`.
    pub audio: Option<AudioObject>,
    /// For `document`.
    pub document: Option<DocumentObject>,
    /// For `sticker`.
    pub sticker: Option<StickerObject>,
    /// For `location`.
    pub location: Option<LocationObject>,
    /// For `contacts`: 1 to 257 cards.
    pub contacts: Option<Vec<ContactCard>>,
    /// For `reaction`.
    pub reaction: Option<ReactionObject>,
    /// For `template`: Meta's template object (`name`, `language`,
    /// `components`), as the send-template pages write it.
    #[schema(value_type = Option<HashMap<String, Value>>)]
    pub template: Option<Value>,
    /// For `interactive`.
    pub interactive: Option<InteractiveObject>,
    /// Reply to this received message (`wamid.…`); not with `reaction`.
    pub reply_to: Option<String>,
    /// Echoed in this message's status events: set it to your own
    /// reference (the `Idempotency-Key`, say) to reconcile.
    pub callback_data: Option<String>,
}

// ─── The answer ──────────────────────────────────────────────────────────

/// The addressee, as Meta resolved it.
#[derive(Debug, Serialize, ToSchema)]
pub struct AcceptedContact {
    /// What the request addressed (phone number, BSUID or group id).
    pub input: String,
    /// The user's WhatsApp id, when addressed by phone number.
    #[schema(required = true)]
    pub wa_id: Option<String>,
    /// The user's BSUID, when addressed by BSUID.
    #[schema(required = true)]
    pub user_id: Option<String>,
    /// The parent BSUID, for portfolios that use them.
    #[schema(required = true)]
    pub parent_user_id: Option<String>,
}

/// Meta accepted the message; delivery comes later, as status events.
#[derive(Debug, Serialize, ToSchema)]
pub struct MessageAccepted {
    /// The message's id (`wamid.…`): the key of its status events.
    pub message_id: String,
    /// The addressee, as Meta resolved it.
    pub contacts: Vec<AcceptedContact>,
}

/// `POST /v1/numbers/{pn}/messages/{message_id}/read`.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkRead {
    /// Also show "typing…" (until the reply, or 25 seconds): only when
    /// about to reply. Never replayed.
    #[serde(default)]
    pub typing_indicator: bool,
}

// ─── Into the core's request ─────────────────────────────────────────────

/// `From` for the types whose fields are the core's, value for value:
/// every field named, so that a field on one side only fails to compile.
macro_rules! same_fields {
    ($($name:ident { $($field:ident),* $(,)? })*) => {$(
        impl From<$name> for domain::$name {
            fn from(value: $name) -> Self {
                let $name { $($field),* } = value;
                Self { $($field),* }
            }
        }
    )*};
}

same_fields! {
    RecipientObject { phone, user_id, group_id }
    TextObject { body, preview_url }
    ImageObject { id, link, caption }
    VideoObject { id, link, caption }
    AudioObject { id, link, voice }
    DocumentObject { id, link, caption, filename }
    StickerObject { id, link }
    ContactNameObject { formatted_name, first_name, last_name, middle_name, suffix, prefix }
    ContactAddressObject { street, city, state, zip, country, country_code, label }
    ContactEmailObject { email, label }
    ContactOrgObject { company, department, title }
    ContactPhoneObject { phone, label, wa_id }
    ContactUrlObject { url, label }
    ReactionObject { message_id, emoji }
    InteractiveText { text }
    HeaderMedia { id, link, filename }
    ReplyObject { id, title }
    ListRowObject { id, title, description }
    CtaUrlParameters { display_text, url }
}

/// Each of `items`, converted.
fn each<T: Into<U>, U>(items: Vec<T>) -> Vec<U> {
    items.into_iter().map(Into::into).collect()
}

impl From<MessageType> for domain::MessageType {
    fn from(kind: MessageType) -> Self {
        match kind {
            MessageType::Text => Self::Text,
            MessageType::Image => Self::Image,
            MessageType::Video => Self::Video,
            MessageType::Audio => Self::Audio,
            MessageType::Document => Self::Document,
            MessageType::Sticker => Self::Sticker,
            MessageType::Location => Self::Location,
            MessageType::Contacts => Self::Contacts,
            MessageType::Reaction => Self::Reaction,
            MessageType::Template => Self::Template,
            MessageType::Interactive => Self::Interactive,
        }
    }
}

impl From<Coordinate> for domain::Coordinate {
    fn from(value: Coordinate) -> Self {
        match value {
            Coordinate::Number(n) => Self::Number(n),
            Coordinate::Text(s) => Self::Text(s),
        }
    }
}

impl From<LocationObject> for domain::LocationObject {
    fn from(value: LocationObject) -> Self {
        let LocationObject {
            latitude,
            longitude,
            name,
            address,
        } = value;
        Self {
            latitude: latitude.into(),
            longitude: longitude.into(),
            name,
            address,
        }
    }
}

impl From<ContactCard> for domain::ContactCard {
    fn from(card: ContactCard) -> Self {
        let ContactCard {
            addresses,
            birthday,
            emails,
            name,
            org,
            phones,
            urls,
        } = card;
        Self {
            addresses: each(addresses),
            birthday,
            emails: each(emails),
            name: name.into(),
            org: org.map(Into::into),
            phones: each(phones),
            urls: each(urls),
        }
    }
}

impl From<InteractiveType> for domain::InteractiveType {
    fn from(kind: InteractiveType) -> Self {
        match kind {
            InteractiveType::Button => Self::Button,
            InteractiveType::List => Self::List,
            InteractiveType::CtaUrl => Self::CtaUrl,
        }
    }
}

impl From<HeaderType> for domain::HeaderType {
    fn from(kind: HeaderType) -> Self {
        match kind {
            HeaderType::Text => Self::Text,
            HeaderType::Image => Self::Image,
            HeaderType::Video => Self::Video,
            HeaderType::Document => Self::Document,
        }
    }
}

impl From<InteractiveHeader> for domain::InteractiveHeader {
    fn from(value: InteractiveHeader) -> Self {
        let InteractiveHeader {
            kind,
            text,
            image,
            video,
            document,
        } = value;
        Self {
            kind: kind.into(),
            text,
            image: image.map(Into::into),
            video: video.map(Into::into),
            document: document.map(Into::into),
        }
    }
}

impl From<ReplyButtonObject> for domain::ReplyButtonObject {
    fn from(value: ReplyButtonObject) -> Self {
        let ReplyButtonObject { kind, reply } = value;
        Self {
            kind,
            reply: reply.into(),
        }
    }
}

impl From<ListSectionObject> for domain::ListSectionObject {
    fn from(value: ListSectionObject) -> Self {
        let ListSectionObject { title, rows } = value;
        Self {
            title,
            rows: each(rows),
        }
    }
}

impl From<InteractiveAction> for domain::InteractiveAction {
    fn from(value: InteractiveAction) -> Self {
        let InteractiveAction {
            buttons,
            button,
            sections,
            name,
            parameters,
        } = value;
        Self {
            buttons: buttons.map(each),
            button,
            sections: sections.map(each),
            name,
            parameters: parameters.map(Into::into),
        }
    }
}

impl From<InteractiveObject> for domain::InteractiveObject {
    fn from(value: InteractiveObject) -> Self {
        let InteractiveObject {
            kind,
            header,
            body,
            footer,
            action,
        } = value;
        Self {
            kind: kind.into(),
            header: header.map(Into::into),
            body: body.into(),
            footer: footer.map(Into::into),
            action: action.into(),
        }
    }
}

impl From<SendMessage> for domain::SendMessage {
    fn from(value: SendMessage) -> Self {
        let SendMessage {
            to,
            kind,
            text,
            image,
            video,
            audio,
            document,
            sticker,
            location,
            contacts,
            reaction,
            template,
            interactive,
            reply_to,
            callback_data,
        } = value;
        Self {
            to: to.into(),
            kind: kind.into(),
            text: text.map(Into::into),
            image: image.map(Into::into),
            video: video.map(Into::into),
            audio: audio.map(Into::into),
            document: document.map(Into::into),
            sticker: sticker.map(Into::into),
            location: location.map(Into::into),
            contacts: contacts.map(each),
            reaction: reaction.map(Into::into),
            template,
            interactive: interactive.map(Into::into),
            reply_to,
            callback_data,
        }
    }
}

impl From<domain::Accepted> for MessageAccepted {
    fn from(accepted: domain::Accepted) -> Self {
        let domain::Accepted {
            message_id,
            contacts,
        } = accepted;
        Self {
            message_id: message_id.as_str().to_owned(),
            contacts: contacts
                .into_iter()
                .map(|c| AcceptedContact {
                    input: c.input,
                    wa_id: c.wa_id.map(WaId::into_inner),
                    user_id: c.user_id.map(UserId::into_inner),
                    parent_user_id: c.parent_user_id.map(UserId::into_inner),
                })
                .collect(),
        }
    }
}

/// The message a request body asks for, checked against every limit Meta
/// documents: nothing is sent when this fails. In the core's order: a
/// type the service does not send ([`domain::refuse_unsupported`]), then
/// the body read into [`SendMessage`] (`422 invalid_request` on `body`,
/// unknown fields included), then the core's checks ([`domain::message`]).
pub(crate) fn parse_message(body: Value) -> Result<OutboundMessage, ApiError> {
    domain::refuse_unsupported(&body)?;
    let request: SendMessage =
        serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))?;
    Ok(domain::message(request.into())?)
}

// ─── The routes ──────────────────────────────────────────────────────────

/// The core's message operations on `owned`, counted in `state`'s metrics
/// (`owned` is the state's own: the extractor made it with
/// `state.authz()`, which admits it).
fn messages<'a>(state: &'a AppState, owned: &'a OwnedNumber) -> Result<Messages<'a>, ApiError> {
    Ok(Messages::new(state.authz(), owned.capability())?.observed_by(state.metrics()))
}

/// Send `message` with the number's token: `202` with its id, or the
/// error, with Meta's `details` (the core's [`Messages::send`]).
pub(crate) async fn send(
    state: &AppState,
    owned: &OwnedNumber,
    message: &OutboundMessage,
) -> Result<Success, ApiError> {
    let accepted = messages(state, owned)?.send(message).await?;
    Success::json(StatusCode::ACCEPTED, &MessageAccepted::from(accepted))
}

/// `POST /v1/numbers/{pn}/messages`: send a message.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/messages",
    tag = "messages",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never send twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = SendMessage,
    responses(
        (status = 202, description = "Accepted by Meta; delivery arrives as status events", body = MessageAccepted),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`permission`, `marketing_not_allowed`, …)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`customer_service_window_closed`, `marketing_opted_out`, `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`, …", body = ErrorBody),
        (status = 422, description = "`invalid_request` (with `field`: `to.phone` without `+`, a `template` key the service would not send to Meta, …), `unsupported_message_type`, `idempotency_key_reused`, Meta's `template_*`, …: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests` (the service's limits, `Retry-After`) or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the message may have been sent (`may_have_been_sent: true`)", body = ErrorBody),
    )
)]
pub(crate) async fn send_message(
    State(state): State<AppState>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    uri: Uri,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::json(&Method::POST, uri.path(), &body);
    let message = match parse_message(body) {
        Ok(message) => message,
        Err(error) => return error.into_response(),
    };
    idempotency::run(
        &state,
        &caller,
        key,
        fingerprint,
        send(&state, &owned, &message),
    )
    .await
}

/// A JSON body that may be empty (then `T::default()`).
#[derive(Debug)]
pub struct OptionalJson<T>(pub T);

impl<T, S> FromRequest<S> for OptionalJson<T>
where
    T: DeserializeOwned + Default,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| {
                if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    ApiError::new(ErrorCode::PAYLOAD_TOO_LARGE)
                } else {
                    ApiError::invalid("body")
                }
            })?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self(T::default()));
        }
        serde_json::from_slice(&bytes)
            .map(Self)
            .map_err(|_| ApiError::invalid("body"))
    }
}

/// `POST /v1/numbers/{pn}/messages/{message_id}/read`: show the blue ticks
/// on a received message (and every earlier one); with
/// `typing_indicator`, "typing…" too.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/messages/{message_id}/read",
    tag = "messages",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("message_id" = String, Path, description = "A received message's id (`wamid.…`), percent-encoded"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body(content = Option<MarkRead>, description = "Optional; an empty body marks the message read"),
    responses(
        (status = 204, description = "Marked read"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`body`, `message_id`), or Meta's `invalid_parameter` (an unknown or too old message)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn mark_read(
    State(state): State<AppState>,
    owned: OwnedNumber,
    Path((_, message_id)): Path<(String, String)>,
    OptionalJson(body): OptionalJson<MarkRead>,
) -> Result<StatusCode, ApiError> {
    messages(&state, &owned)?
        .mark_read(&message_id, body.typing_indicator)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn built(body: Value) -> Result<Value, String> {
        parse_message(body)
            .map(|m| serde_json::to_value(&m).unwrap())
            .map_err(|e| format!("{} {}", e.code(), e.field().unwrap_or_default()))
    }

    /// The names this request reads for `type` and `interactive.type` are
    /// the core's, each read into the variant the core names so (roadmap
    /// S5b: the list of supported types is the core's, and this schema's
    /// enums must not drift from it); a name outside them is not read.
    #[test]
    fn the_request_reads_the_cores_type_names() {
        for (name, kind) in domain::MessageType::ALL {
            let read: MessageType = serde_json::from_value(json!(name)).unwrap();
            assert_eq!(domain::MessageType::from(read), kind, "{name}");
        }
        for (name, kind) in domain::InteractiveType::ALL {
            let read: InteractiveType = serde_json::from_value(json!(name)).unwrap();
            assert_eq!(domain::InteractiveType::from(read), kind, "{name}");
        }
        assert!(serde_json::from_value::<MessageType>(json!("pin")).is_err());
        assert!(serde_json::from_value::<InteractiveType>(json!("flow")).is_err());
    }

    /// E.164 with its `+`, strictly. Decisive: the `+` check.
    #[test]
    fn recipients_are_strict_e164_bsuids_or_groups() {
        let text = |to: Value| json!({"to": to, "type": "text", "text": {"body": "hi"}});
        for (to, expected) in [
            (
                json!({"phone": "+16505551234"}),
                Ok(json!({"recipient_type": "individual", "to": "+16505551234"})),
            ),
            (
                json!({"user_id": "US.13491208655302741918"}),
                Ok(json!({"recipient_type": "individual", "recipient": "US.13491208655302741918"})),
            ),
            (
                json!({"phone": "+16505551234", "user_id": "US.1"}),
                Ok(
                    json!({"recipient_type": "individual", "to": "+16505551234", "recipient": "US.1"}),
                ),
            ),
            (
                json!({"group_id": "Y2FwaV9ncm91cDox"}),
                Ok(json!({"recipient_type": "group", "to": "Y2FwaV9ncm91cDox"})),
            ),
            (
                json!({"phone": "16505551234"}),
                Err("invalid_request to.phone"),
            ),
            (
                json!({"phone": "+1 650 555 1234"}),
                Err("invalid_request to.phone"),
            ),
            (
                json!({"phone": "+06505551234"}),
                Err("invalid_request to.phone"),
            ),
            (
                json!({"phone": "+1234567890123456"}),
                Err("invalid_request to.phone"),
            ),
            (json!({"phone": "+"}), Err("invalid_request to.phone")),
            (json!({"user_id": " "}), Err("invalid_request to.user_id")),
            (
                json!({"group_id": "g", "phone": "+16505551234"}),
                Err("invalid_request to"),
            ),
            (json!({}), Err("invalid_request to")),
        ] {
            let got = built(text(to.clone()));
            match expected {
                Ok(fields) => {
                    let got = got.unwrap_or_else(|e| panic!("{to}: {e}"));
                    for (k, v) in fields.as_object().unwrap() {
                        assert_eq!(&got[k], v, "{to}");
                    }
                }
                Err(e) => assert_eq!(got.unwrap_err(), e, "{to}"),
            }
        }
    }

    #[test]
    fn unsupported_types_and_direct_send_are_refused() {
        let to = json!({"phone": "+16505551234"});
        for body in [
            json!({"to": to, "type": "pin", "pin": {}}),
            json!({"to": to, "type": "order_details"}),
            json!({"to": to, "type": "interactive", "interactive": {"type": "flow"}}),
            json!({"to": to, "type": "text", "text": {"body": "x"}, "category": "utility"}),
        ] {
            assert_eq!(
                built(body.clone()).unwrap_err(),
                "unsupported_message_type ",
                "{body}"
            );
        }
    }

    /// Each type maps to Meta's object, byte for byte as the pages write it.
    #[test]
    fn each_type_maps_to_metas_object() {
        let to = json!({"phone": "+16505551234"});
        let with = |kind: &str, object: Value| {
            let mut body = json!({"to": to, "type": kind});
            body[kind] = object;
            built(body).unwrap_or_else(|e| panic!("{kind}: {e}"))
        };
        assert_eq!(
            with("text", json!({"preview_url": true, "body": "As requested"}))["text"],
            json!({"preview_url": true, "body": "As requested"})
        );
        assert_eq!(
            with("image", json!({"id": "1479537139650973", "caption": "c"}))["image"],
            json!({"id": "1479537139650973", "caption": "c"})
        );
        assert_eq!(
            with(
                "audio",
                json!({"link": "https://x.example/a.ogg", "voice": true})
            )["audio"],
            json!({"link": "https://x.example/a.ogg", "voice": true})
        );
        assert_eq!(
            with(
                "location",
                json!({"latitude": "37.44216251868683", "longitude": -122.16153582049394, "name": "Philz Coffee"})
            )["location"],
            json!({"latitude": "37.44216251868683", "longitude": "-122.16153582049394", "name": "Philz Coffee"})
        );
        assert_eq!(
            with(
                "reaction",
                json!({"message_id": "wamid.X", "emoji": "\u{1F600}"})
            )["reaction"],
            json!({"message_id": "wamid.X", "emoji": "\u{1F600}"})
        );
        let contacts = with(
            "contacts",
            json!([{"name": {"formatted_name": "Barbara J. Johnson", "first_name": "Barbara"},
                    "phones": [{"phone": "+1 (940) 555-1234", "type": "Office", "wa_id": "19405551234"}]}]),
        );
        assert_eq!(
            contacts["contacts"],
            json!([{"name": {"formatted_name": "Barbara J. Johnson", "first_name": "Barbara"},
                    "phones": [{"phone": "+1 (940) 555-1234", "type": "Office", "wa_id": "19405551234"}]}])
        );
        let template = with(
            "template",
            json!({"name": "order_confirmation", "language": {"code": "en_US"},
                   "components": [{"type": "body", "parameters": [{"type": "text", "text": "Pablo"}]}]}),
        );
        assert_eq!(template["template"]["name"], "order_confirmation");
        assert_eq!(template["template"]["language"]["code"], "en_US");
        let buttons = with(
            "interactive",
            json!({"type": "button", "body": {"text": "Change?"},
                   "action": {"buttons": [{"type": "reply", "reply": {"id": "change", "title": "Change"}}]}}),
        );
        assert_eq!(
            buttons["interactive"],
            json!({"type": "button", "body": {"text": "Change?"},
                   "action": {"buttons": [{"type": "reply", "reply": {"id": "change", "title": "Change"}}]}})
        );
        let cta = with(
            "interactive",
            json!({"type": "cta_url", "header": {"type": "text", "text": "New dates"}, "body": {"text": "Tap"},
                   "footer": {"text": "Subject to change"},
                   "action": {"name": "cta_url", "parameters": {"display_text": "See Dates", "url": "https://www.luckyshrub.com"}}}),
        );
        assert_eq!(
            cta["interactive"]["action"]["parameters"]["url"],
            "https://www.luckyshrub.com"
        );
        assert_eq!(
            cta["interactive"]["header"],
            json!({"type": "text", "text": "New dates"})
        );
        let list = with(
            "interactive",
            json!({"type": "list", "body": {"text": "Which?"},
                   "action": {"button": "Shipping Options", "sections": [{"title": "I want it ASAP!",
                       "rows": [{"id": "priority_express", "title": "Priority Mail Express", "description": "Next Day to 2 Days"}]}]}}),
        );
        assert_eq!(list["interactive"]["action"]["button"], "Shipping Options");
    }

    /// Every field a request's objects carry reaches Meta's object, value
    /// for value, as the pages write it (`messages/contacts-messages`,
    /// `interactive-reply-buttons-messages`, `interactive-list-messages`,
    /// the media pages, `location-messages`): the conversion into the
    /// core's request drops none. (The compiler refuses a field missing
    /// from either side, and `-D warnings` one destructured but unused;
    /// this pins the values.) Decisive: each field of the `From` impls.
    #[test]
    fn every_field_a_request_carries_reaches_metas_object() {
        let to = json!({"phone": "+16505551234"});
        for (kind, object) in [
            (
                "contacts",
                json!([{
                    "addresses": [{"street": "1 Lucky Shrub Way", "city": "Menlo Park", "state": "CA",
                                   "zip": "94025", "country": "United States", "country_code": "US",
                                   "type": "Office"}],
                    "birthday": "1999-01-23",
                    "emails": [{"email": "bjohnson@luckyshrub.com", "type": "Work"}],
                    "name": {"formatted_name": "Barbara J. Johnson", "first_name": "Barbara",
                             "last_name": "Johnson", "middle_name": "Joana", "suffix": "Esq.",
                             "prefix": "Dr."},
                    "org": {"company": "Lucky Shrub", "department": "Legal", "title": "Lead Counsel"},
                    "phones": [{"phone": "+1 (940) 555-1234", "type": "Office", "wa_id": "19405551234"}],
                    "urls": [{"url": "https://www.luckyshrub.com", "type": "Company"}]
                }]),
            ),
            (
                "interactive",
                json!({"type": "button",
                       "header": {"type": "document",
                                  "document": {"link": "https://www.luckyshrub.com/a.pdf", "filename": "a.pdf"}},
                       "body": {"text": "Change?"}, "footer": {"text": "Lucky Shrub"},
                       "action": {"buttons": [{"type": "reply", "reply": {"id": "change", "title": "Change"}}]}}),
            ),
            (
                "interactive",
                json!({"type": "list", "header": {"type": "text", "text": "Shipping"},
                       "body": {"text": "Which?"}, "footer": {"text": "Lucky Shrub"},
                       "action": {"button": "Shipping Options", "sections": [{"title": "I want it ASAP!",
                           "rows": [{"id": "priority_express", "title": "Priority Mail Express",
                                     "description": "Next Day to 2 Days"}]}]}}),
            ),
            (
                "interactive",
                json!({"type": "cta_url", "header": {"type": "image", "image": {"id": "1479537139650973"}},
                       "body": {"text": "Tap"}, "footer": {"text": "Subject to change"},
                       "action": {"name": "cta_url",
                                  "parameters": {"display_text": "See Dates", "url": "https://www.luckyshrub.com"}}}),
            ),
            (
                "video",
                json!({"link": "https://www.luckyshrub.com/a.mp4", "caption": "A tour"}),
            ),
            (
                "document",
                json!({"id": "1479537139650973", "caption": "Invoice", "filename": "invoice.pdf"}),
            ),
            (
                "sticker",
                json!({"link": "https://www.luckyshrub.com/a.webp"}),
            ),
            (
                "location",
                json!({"latitude": "37.44216251868683", "longitude": "-122.16153582049394",
                       "name": "Philz Coffee", "address": "101 Forest Ave, Palo Alto, CA 94301"}),
            ),
        ] {
            let mut body = json!({"to": to, "type": kind});
            body[kind] = object.clone();
            let sent = built(body).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(sent[kind], object, "{kind}");
        }
    }

    /// The accepted message's every contact field reaches the answer
    /// (business-scoped-user-ids: a BSUID and its parent). Decisive: each
    /// field of `From<domain::Accepted>`.
    #[test]
    fn every_field_of_the_accepted_message_is_answered() {
        let response = serde_json::from_value(json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": "US.13491208655302741918", "wa_id": "16505551234",
                          "user_id": "US.13491208655302741918",
                          "parent_user_id": "US.ENT.11815799212886844830"}],
            "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
        }))
        .unwrap();
        let accepted = MessageAccepted::from(domain::Accepted::from_response(response).unwrap());
        assert_eq!(
            serde_json::to_value(&accepted).unwrap(),
            json!({
                "message_id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA",
                "contacts": [{"input": "US.13491208655302741918", "wa_id": "16505551234",
                              "user_id": "US.13491208655302741918",
                              "parent_user_id": "US.ENT.11815799212886844830"}]
            })
        );
    }

    #[test]
    fn content_is_checked_before_any_request() {
        let to = json!({"phone": "+16505551234"});
        for (body, error) in [
            (json!({"to": to, "type": "text"}), "invalid_request text"),
            (
                json!({"to": to, "type": "text", "text": {"body": "x"}, "image": {"id": "1"}}),
                "invalid_request image",
            ),
            (
                json!({"to": to, "type": "text", "text": {"body": "x".repeat(4097)}}),
                "invalid_request text.body",
            ),
            (
                json!({"to": to, "type": "image", "image": {"link": "http://x.example/a.png"}}),
                "invalid_request image.link",
            ),
            (
                json!({"to": to, "type": "image", "image": {"id": "1", "link": "https://x.example/a.png"}}),
                "invalid_request image",
            ),
            (
                json!({"to": to, "type": "location", "location": {"latitude": "north", "longitude": 1}}),
                "invalid_request location.latitude",
            ),
            (
                json!({"to": to, "type": "location", "location": {"latitude": 91, "longitude": 1}}),
                "invalid_request location.latitude",
            ),
            (
                json!({"to": to, "type": "reaction", "reaction": {"message_id": "wamid.X", "emoji": "x"}, "reply_to": "wamid.Y"}),
                "invalid_request reply_to",
            ),
            (
                json!({"to": to, "type": "template", "template": {"language": "en"}}),
                "invalid_request template",
            ),
            // A key the library would drop is refused, never left out.
            (
                json!({"to": to, "type": "template", "template": {"name": "order_confirmation",
                       "language": {"code": "en_US"}, "namespace": "a1b2c3"}}),
                "invalid_request template.namespace",
            ),
            (
                json!({"to": to, "type": "template", "template": {"name": "order_confirmation",
                       "language": {"code": "en_US"},
                       "components": [{"type": "body", "parameters": [{"type": "text", "text": "Pablo", "txet": "x"}]}]}}),
                "invalid_request template.components[0].parameters[0].txet",
            ),
            (
                json!({"to": to, "type": "template", "template": {"name": "order_confirmation",
                       "language": {"code": "en_US"}, "bad key!": 1}}),
                "invalid_request template",
            ),
            (
                json!({"to": to, "type": "text", "text": {"body": "x"}, "unknown": 1}),
                "invalid_request body",
            ),
        ] {
            assert_eq!(built(body.clone()).unwrap_err(), error, "{body}");
        }
    }
}
