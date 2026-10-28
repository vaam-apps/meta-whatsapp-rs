//! Messages (docs/design/server.md, sections 4.2 and 4.3), as services an
//! API adapter calls: building the library's message from a send request
//! ([`message`]: every limit Meta documents checked before any request),
//! the recipient and its E.164 rule ([`recipient`]), and, on a number the
//! caller's tenant owns, sending and marking read ([`Messages`]).
//!
//! The request is [`SendMessage`], a union the service owns, named after
//! Meta's Cloud API message object (so Meta's pages describe each type):
//! `text`; `image`, `video`, `audio`, `document`, `sticker` by uploaded
//! `id` or `https://` `link` (Meta fetches it, never the service);
//! `location`, `contacts`, `reaction`; `template` (Meta's object);
//! `interactive` `button`, `list` and `cta_url`. Any other type, Direct
//! Send included, is `422 unsupported_message_type`
//! ([`refuse_unsupported`], for an adapter whose request is JSON). It
//! carries the request's values as the request did, free of any API
//! toolkit: an adapter converts its own request type (the axum one's
//! carries its OpenAPI schema) into it, and every adapter's request is
//! checked alike, in the same order, on the same fields.
//!
//! Recipients: `phone` (E.164 **with** `+`: a digits-only number is
//! refused before any request, since Meta would read it as local to the
//! sender's country), `user_id` (a BSUID), both (Meta uses the phone
//! number), or `group_id`.
//!
//! A send answers the accepted message ([`Accepted`]), not a status: the
//! adapter renders it (the axum one: `202`). An idempotent send runs
//! under [`crate::idempotency::run`]. Meta enforces the 24-hour window:
//! outside it a free-form message is `409 customer_service_window_closed`.

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::messages::{
    Audio, Contact, ContactAddress, ContactEmail, ContactName, ContactOrg, ContactPhone,
    ContactUrl, CtaUrl, Document, Header, Image, Interactive, ListMessage, ListRow, ListSection,
    Location, MediaSource, MessageContent, OutboundMessage, Reaction, ReplyButton, ReplyButtons,
    SendResponse, SentContact, Sticker, Text, Video,
};
use meta_whatsapp_rs::client::templates::TemplateMessage;
use meta_whatsapp_rs::core::ids::{GroupId, MediaId, MessageId, UserId, WaId};
use meta_whatsapp_rs::core::recipient::Recipient;
use serde_json::Value;

use crate::authz::{Authorizer, OwnedNumber};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::meta_object;

// ─── The request ─────────────────────────────────────────────────────────

/// Who receives a message: a phone number, a business-scoped user id
/// (BSUID), both (Meta then uses the phone number), or a group.
#[derive(Debug, Clone, Default, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl MessageType {
    /// Every type, by its name (the request's `type`).
    pub const ALL: [(&'static str, Self); 11] = [
        ("text", Self::Text),
        ("image", Self::Image),
        ("video", Self::Video),
        ("audio", Self::Audio),
        ("document", Self::Document),
        ("sticker", Self::Sticker),
        ("location", Self::Location),
        ("contacts", Self::Contacts),
        ("reaction", Self::Reaction),
        ("template", Self::Template),
        ("interactive", Self::Interactive),
    ];

    /// Its name: the request's `type`, and the key of its content object.
    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, t)| *t == self)
            .map_or("", |(name, _)| name)
    }
}

/// `text` (`messages/text-messages`).
#[derive(Debug, Clone, PartialEq)]
pub struct TextObject {
    /// Body text, at most 4,096 characters.
    pub body: String,
    /// Ask WhatsApp to render a preview of the first URL.
    pub preview_url: Option<bool>,
}

/// `image` (`messages/image-messages`): an uploaded `id` or an `https://`
/// `link`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImageObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// Caption, at most 1,024 characters.
    pub caption: Option<String>,
}

/// `video` (`messages/video-messages`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// Caption, at most 1,024 characters.
    pub caption: Option<String>,
}

/// `audio` (`messages/audio-messages`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// `true`: a voice message (an Ogg file with the OPUS codec).
    pub voice: Option<bool>,
}

/// `document` (`messages/document-messages`).
#[derive(Debug, Clone, Default, PartialEq)]
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StickerObject {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
}

/// A coordinate in decimal degrees: Meta's pages write it as a string
/// (`"37.44216251868683"`); a number is accepted too.
#[derive(Debug, Clone, PartialEq)]
pub enum Coordinate {
    /// A number.
    Number(f64),
    /// A string, as Meta's pages write it.
    Text(String),
}

/// `location` (`messages/location-messages`).
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, Default, PartialEq)]
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
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// Label, e.g. `Home` (Meta's `type`).
    pub label: Option<String>,
}

/// A contact card's email address.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContactEmailObject {
    /// Address.
    pub email: String,
    /// Label, e.g. `Work` (Meta's `type`).
    pub label: Option<String>,
}

/// A contact card's organization.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContactOrgObject {
    /// Company.
    pub company: Option<String>,
    /// Department.
    pub department: Option<String>,
    /// Job title.
    pub title: Option<String>,
}

/// A contact card's phone number (content: shown as written).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContactPhoneObject {
    /// The number, as shown.
    pub phone: String,
    /// Label, e.g. `Mobile` (Meta's `type`).
    pub label: Option<String>,
    /// The contact's WhatsApp id: WhatsApp then offers "Message".
    pub wa_id: Option<String>,
}

/// A contact card's website.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContactUrlObject {
    /// URL.
    pub url: String,
    /// Label, e.g. `Company` (Meta's `type`).
    pub label: Option<String>,
}

/// One card of a `contacts` message (`messages/contacts-messages`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContactCard {
    /// Addresses.
    pub addresses: Vec<ContactAddressObject>,
    /// Birthday, `YYYY-MM-DD`.
    pub birthday: Option<String>,
    /// Email addresses.
    pub emails: Vec<ContactEmailObject>,
    /// Name.
    pub name: ContactNameObject,
    /// Organization.
    pub org: Option<ContactOrgObject>,
    /// Phone numbers.
    pub phones: Vec<ContactPhoneObject>,
    /// Websites.
    pub urls: Vec<ContactUrlObject>,
}

/// `reaction` (`messages/reaction-messages`): an emoji on a received
/// message; an empty `emoji` removes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReactionObject {
    /// The received message's id (`wamid.…`).
    pub message_id: String,
    /// The emoji.
    pub emoji: String,
}

/// The interactive types the service sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveType {
    /// Reply buttons (`messages/interactive-reply-buttons-messages`).
    Button,
    /// A list (`messages/interactive-list-messages`).
    List,
    /// A URL button (`messages/interactive-cta-url-messages`).
    CtaUrl,
}

impl InteractiveType {
    /// Every interactive type, by its name (`interactive.type`).
    pub const ALL: [(&'static str, Self); 3] = [
        ("button", Self::Button),
        ("list", Self::List),
        ("cta_url", Self::CtaUrl),
    ];
}

/// `{"text": …}`: an interactive message's body or footer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InteractiveText {
    /// The text.
    pub text: String,
}

/// A media asset of an interactive header: an uploaded `id` or an
/// `https://` `link`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeaderMedia {
    /// Uploaded media id.
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    pub link: Option<String>,
    /// File name (documents only).
    pub filename: Option<String>,
}

/// The kinds of interactive header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct InteractiveHeader {
    /// Which kind (Meta's `type`).
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplyObject {
    /// Returned in the reply's webhook; unique in the message.
    pub id: String,
    /// Label, at most 20 characters.
    pub title: String,
}

/// A reply button: `{"type": "reply", "reply": {…}}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplyButtonObject {
    /// `reply` (Meta's `type`).
    pub kind: String,
    /// The button.
    pub reply: ReplyObject,
}

/// A list row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListRowObject {
    /// Returned in the reply's webhook.
    pub id: String,
    /// Title, at most 24 characters.
    pub title: String,
    /// Description, at most 72 characters.
    pub description: Option<String>,
}

/// A list section.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListSectionObject {
    /// Title, at most 24 characters.
    pub title: Option<String>,
    /// Its rows.
    pub rows: Vec<ListRowObject>,
}

/// A URL button's `parameters`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CtaUrlParameters {
    /// Button label, at most 20 characters.
    pub display_text: String,
    /// The URL the button opens.
    pub url: String,
}

/// An interactive message's `action`: `buttons` for `button`; `button` and
/// `sections` for `list`; `name` (`cta_url`) and `parameters` for
/// `cta_url`.
#[derive(Debug, Clone, Default, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct InteractiveObject {
    /// Which kind (Meta's `type`).
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

/// A message to send, as the request carried it: `kind` (the request's
/// `type`) names the one content object present.
#[derive(Debug, Clone, PartialEq)]
pub struct SendMessage {
    /// Who receives it.
    pub to: RecipientObject,
    /// Which content object follows.
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
    pub template: Option<Value>,
    /// For `interactive`.
    pub interactive: Option<InteractiveObject>,
    /// Reply to this received message (`wamid.…`); not with `reaction`.
    pub reply_to: Option<String>,
    /// Echoed in this message's status events (Meta's
    /// `biz_opaque_callback_data`).
    pub callback_data: Option<String>,
}

impl SendMessage {
    /// A message of `kind` to `to`, with no content object yet.
    pub fn new(to: RecipientObject, kind: MessageType) -> Self {
        Self {
            to,
            kind,
            text: None,
            image: None,
            video: None,
            audio: None,
            document: None,
            sticker: None,
            location: None,
            contacts: None,
            reaction: None,
            template: None,
            interactive: None,
            reply_to: None,
            callback_data: None,
        }
    }
}

// ─── The answer ──────────────────────────────────────────────────────────

/// Meta accepted the message; delivery comes later, as status events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// The message's id (`wamid.…`): the key of its status events.
    pub message_id: MessageId,
    /// The addressee, as Meta resolved it.
    pub contacts: Vec<SentContact>,
}

impl Accepted {
    /// The accepted message of Meta's answer. Meta accepting without a
    /// message id (never documented) is `502 upstream`, with
    /// `may_have_been_sent`: the message may have gone out.
    ///
    /// # Errors
    ///
    /// No message id in `response`.
    pub fn from_response(response: SendResponse) -> Result<Self, ServiceError> {
        let message_id = response
            .message_id()
            .cloned()
            .ok_or_else(|| ServiceError::new("upstream").with_may_have_been_sent(true))?;
        Ok(Self {
            message_id,
            contacts: response.contacts,
        })
    }
}

// ─── Building the library's message ──────────────────────────────────────

/// Top-level keys of Direct Send, which the service does not offer.
const DIRECT_SEND_FIELDS: [&str; 3] = ["category", "ttl_seconds", "direct_send_config"];

fn unsupported() -> ServiceError {
    ServiceError::new("unsupported_message_type")
}

/// For an adapter whose request is a JSON body, before it reads it into
/// a [`SendMessage`]: a body naming a type the service does not send (a
/// `type` or an `interactive.type` outside [`MessageType::ALL`] and
/// [`InteractiveType::ALL`]), or carrying a key of Direct Send
/// (`category`, `ttl_seconds`, `direct_send_config`), is `422
/// unsupported_message_type`, not a malformed body.
///
/// # Errors
///
/// As above; anything else passes, to be read and checked.
pub fn refuse_unsupported(body: &Value) -> Result<(), ServiceError> {
    let Some(object) = body.as_object() else {
        return Ok(());
    };
    if DIRECT_SEND_FIELDS.iter().any(|f| object.contains_key(*f)) {
        return Err(unsupported());
    }
    if let Some(kind) = object.get("type").and_then(Value::as_str)
        && !MessageType::ALL.iter().any(|(name, _)| *name == kind)
    {
        return Err(unsupported());
    }
    if let Some(kind) = object
        .get("interactive")
        .and_then(|i| i.get("type"))
        .and_then(Value::as_str)
        && !InteractiveType::ALL.iter().any(|(name, _)| *name == kind)
    {
        return Err(unsupported());
    }
    Ok(())
}

/// The recipient, E.164 checked: a phone number without its `+` is
/// refused here, before any request.
///
/// # Errors
///
/// `422 invalid_request` on `to.phone` (not strict E.164 with its `+`),
/// `to.user_id` (blank), `to.group_id` (blank) or `to` (a group with a
/// phone number or a BSUID, or nobody).
pub fn recipient(to: RecipientObject) -> Result<Recipient, ServiceError> {
    let phone = to.phone.map(e164).transpose()?;
    let user = to
        .user_id
        .map(|id| {
            if id.trim().is_empty() {
                Err(ServiceError::invalid("to.user_id"))
            } else {
                Ok(UserId::new(id))
            }
        })
        .transpose()?;
    match (phone, user, to.group_id) {
        (None, None, Some(group)) if !group.trim().is_empty() => {
            Ok(Recipient::Group(GroupId::new(group)))
        }
        (None, None, Some(_)) => Err(ServiceError::invalid("to.group_id")),
        (Some(phone), None, None) => Ok(Recipient::Phone(phone)),
        (None, Some(user), None) => Ok(Recipient::User(user)),
        (Some(phone), Some(user), None) => Ok(Recipient::PhoneAndUser { phone, user }),
        // A group with a phone or a BSUID, or nobody.
        (_, _, Some(_)) | (None, None, None) => Err(ServiceError::invalid("to")),
    }
}

/// Longest E.164 number, in digits (ITU-T E.164).
pub const E164_MAX_DIGITS: usize = 15;

/// Whether `phone` is strict E.164 with its `+`: `+`, then 1 to
/// [`E164_MAX_DIGITS`] digits, the first not `0`.
pub fn is_e164(phone: &str) -> bool {
    phone.strip_prefix('+').is_some_and(|digits| {
        (1..=E164_MAX_DIGITS).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
            && !digits.starts_with('0')
    })
}

/// `phone` if it is strict E.164 with its `+` ([`is_e164`]), else `422`
/// on `to.phone`. Never quotes the value.
fn e164(phone: String) -> Result<String, ServiceError> {
    if is_e164(&phone) {
        Ok(phone)
    } else {
        Err(ServiceError::invalid("to.phone"))
    }
}

/// An uploaded `id` or an `https://` `link`, exactly one.
fn media_source(
    field: &str,
    id: Option<String>,
    link: Option<String>,
) -> Result<MediaSource, ServiceError> {
    match (id, link) {
        // A body value, not a path segment: sent as given, for Meta to
        // judge, as before ids were checked (v1's answers unchanged).
        (Some(id), None) => Ok(MediaSource::id(MediaId::new_unchecked(id))),
        (None, Some(link))
            if link
                .get(..8)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://")) =>
        {
            Ok(MediaSource::link(link))
        }
        (None, Some(_)) => Err(ServiceError::invalid(format!("{field}.link"))),
        _ => Err(ServiceError::invalid(field)),
    }
}

/// A decimal-degree coordinate.
fn coordinate(field: &str, value: Coordinate) -> Result<f64, ServiceError> {
    match value {
        Coordinate::Number(n) => Ok(n),
        Coordinate::Text(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| ServiceError::invalid(field.to_owned())),
    }
}

fn required<T>(field: &'static str, value: Option<T>) -> Result<T, ServiceError> {
    value.ok_or_else(|| ServiceError::invalid(field))
}

fn contact(card: ContactCard) -> Contact {
    Contact {
        addresses: card
            .addresses
            .into_iter()
            .map(|a| ContactAddress {
                street: a.street,
                city: a.city,
                state: a.state,
                zip: a.zip,
                country: a.country,
                country_code: a.country_code,
                kind: a.label,
            })
            .collect(),
        birthday: card.birthday,
        emails: card
            .emails
            .into_iter()
            .map(|e| ContactEmail {
                email: e.email,
                kind: e.label,
            })
            .collect(),
        name: ContactName {
            formatted_name: card.name.formatted_name,
            first_name: card.name.first_name,
            last_name: card.name.last_name,
            middle_name: card.name.middle_name,
            suffix: card.name.suffix,
            prefix: card.name.prefix,
        },
        org: card.org.map(|o| ContactOrg {
            company: o.company,
            department: o.department,
            title: o.title,
        }),
        phones: card
            .phones
            .into_iter()
            .map(|p| ContactPhone {
                phone: p.phone,
                kind: p.label,
                wa_id: p.wa_id.map(WaId::new),
            })
            .collect(),
        urls: card
            .urls
            .into_iter()
            .map(|u| ContactUrl {
                url: u.url,
                kind: u.label,
            })
            .collect(),
    }
}

fn header(value: InteractiveHeader) -> Result<Header, ServiceError> {
    let media = |field: &str, media: Option<HeaderMedia>| {
        let media =
            media.ok_or_else(|| ServiceError::invalid(format!("interactive.header.{field}")))?;
        Ok::<_, ServiceError>((
            media_source(&format!("interactive.header.{field}"), media.id, media.link)?,
            media.filename,
        ))
    };
    Ok(match value.kind {
        HeaderType::Text => Header::text(required("interactive.header.text", value.text)?),
        HeaderType::Image => Header::Image(media("image", value.image)?.0),
        HeaderType::Video => Header::Video(media("video", value.video)?.0),
        HeaderType::Document => {
            let (source, filename) = media("document", value.document)?;
            Header::Document { source, filename }
        }
    })
}

fn interactive(value: InteractiveObject) -> Result<Interactive, ServiceError> {
    let body = value.body.text;
    let footer = value.footer.map(|f| f.text);
    let action = value.action;
    Ok(match value.kind {
        InteractiveType::Button => {
            let buttons = required("interactive.action.buttons", action.buttons)?
                .into_iter()
                .enumerate()
                .map(|(i, b)| {
                    if b.kind == "reply" {
                        Ok(ReplyButton::new(b.reply.id, b.reply.title))
                    } else {
                        Err(ServiceError::invalid(format!(
                            "interactive.action.buttons[{i}].type"
                        )))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut message = ReplyButtons::new(body, buttons);
            message.header = value.header.map(header).transpose()?;
            message.footer = footer;
            Interactive::Buttons(message)
        }
        InteractiveType::List => {
            let sections = required("interactive.action.sections", action.sections)?
                .into_iter()
                .map(|s| ListSection {
                    title: s.title,
                    rows: s
                        .rows
                        .into_iter()
                        .map(|r| ListRow {
                            id: r.id,
                            title: r.title,
                            description: r.description,
                        })
                        .collect(),
                })
                .collect::<Vec<_>>();
            let button = required("interactive.action.button", action.button)?;
            let mut message = ListMessage::new(body, button, sections);
            message.header = match value.header {
                None => None,
                Some(InteractiveHeader {
                    kind: HeaderType::Text,
                    text: Some(text),
                    ..
                }) => Some(text),
                // interactive-list-messages: a text header only.
                Some(_) => return Err(ServiceError::invalid("interactive.header")),
            };
            message.footer = footer;
            Interactive::List(message)
        }
        InteractiveType::CtaUrl => {
            if action.name.as_deref() != Some("cta_url") {
                return Err(ServiceError::invalid("interactive.action.name"));
            }
            let parameters = required("interactive.action.parameters", action.parameters)?;
            let mut message = CtaUrl::new(body, parameters.display_text, parameters.url);
            message.header = value.header.map(header).transpose()?;
            message.footer = footer;
            Interactive::CtaUrl(message)
        }
    })
}

/// The one content object `type` names, never another.
fn only_the_named_object(request: &SendMessage) -> Result<(), ServiceError> {
    let present = [
        ("text", request.text.is_some()),
        ("image", request.image.is_some()),
        ("video", request.video.is_some()),
        ("audio", request.audio.is_some()),
        ("document", request.document.is_some()),
        ("sticker", request.sticker.is_some()),
        ("location", request.location.is_some()),
        ("contacts", request.contacts.is_some()),
        ("reaction", request.reaction.is_some()),
        ("template", request.template.is_some()),
        ("interactive", request.interactive.is_some()),
    ];
    let named = request.kind.name();
    for (name, is_present) in present {
        if is_present && name != named {
            return Err(ServiceError::invalid(name));
        }
    }
    Ok(())
}

/// The library's message for `request`, checked against every limit Meta
/// documents (`OutboundMessage::validate`): nothing is sent when this
/// fails. In this order: one content object, the one `type` names; the
/// recipient ([`recipient`]); the content object; then the library's
/// checks, on the request's names for its fields (`to`, `reply_to`,
/// `callback_data`).
///
/// # Errors
///
/// `422 invalid_request` on the field at fault.
pub fn message(request: SendMessage) -> Result<OutboundMessage, ServiceError> {
    only_the_named_object(&request)?;
    let recipient = recipient(request.to)?;
    let content: MessageContent = match request.kind {
        MessageType::Text => {
            let text = required("text", request.text)?;
            let mut content = Text::new(text.body);
            content.preview_url = text.preview_url;
            content.into()
        }
        MessageType::Image => {
            let image = required("image", request.image)?;
            let mut content = Image::new(media_source("image", image.id, image.link)?);
            content.caption = image.caption;
            content.into()
        }
        MessageType::Video => {
            let video = required("video", request.video)?;
            let mut content = Video::new(media_source("video", video.id, video.link)?);
            content.caption = video.caption;
            content.into()
        }
        MessageType::Audio => {
            let audio = required("audio", request.audio)?;
            let mut content = Audio::new(media_source("audio", audio.id, audio.link)?);
            content.voice = audio.voice;
            content.into()
        }
        MessageType::Document => {
            let document = required("document", request.document)?;
            let mut content = Document::new(media_source("document", document.id, document.link)?);
            content.caption = document.caption;
            content.filename = document.filename;
            content.into()
        }
        MessageType::Sticker => {
            let sticker = required("sticker", request.sticker)?;
            Sticker::new(media_source("sticker", sticker.id, sticker.link)?).into()
        }
        MessageType::Location => {
            let location = required("location", request.location)?;
            let mut content = Location::new(
                coordinate("location.latitude", location.latitude)?,
                coordinate("location.longitude", location.longitude)?,
            );
            content.name = location.name;
            content.address = location.address;
            content.into()
        }
        MessageType::Contacts => MessageContent::Contacts(
            required("contacts", request.contacts)?
                .into_iter()
                .map(contact)
                .collect(),
        ),
        MessageType::Reaction => {
            let reaction = required("reaction", request.reaction)?;
            Reaction::new(reaction.message_id, reaction.emoji).into()
        }
        MessageType::Template => {
            let template: TemplateMessage =
                meta_object("template", &required("template", request.template)?)?;
            template.into()
        }
        MessageType::Interactive => {
            interactive(required("interactive", request.interactive)?)?.into()
        }
    };
    let mut message = OutboundMessage::new(recipient, content);
    if let Some(id) = request.reply_to {
        message = message.reply_to(MessageId::new(id));
    }
    if let Some(data) = request.callback_data {
        message = message.callback_data(data);
    }
    message
        .validate()
        .map_err(|error| ServiceError::invalid(request_field(&error.field)))?;
    Ok(message)
}

/// The request's name for a field of the library's message.
fn request_field(library: &str) -> String {
    match library {
        "to" | "recipient" | "recipient_type" => "to".to_owned(),
        "context" | "context.message_id" => "reply_to".to_owned(),
        "biz_opaque_callback_data" => "callback_data".to_owned(),
        other => other.to_owned(),
    }
}

// ─── The operations ──────────────────────────────────────────────────────

/// The message operations on a number the caller's tenant owns: an
/// [`OwnedNumber`] its [`Authorizer`] made, and admits (ownership and the
/// vault already checked: steps 4 and 5 of [`crate::authz`]), with that
/// number's token. A failed Graph call is told to the [`Observer`] (the
/// adapter's metrics), and a `190` marks the WABA's numbers
/// `reconnect_required` ([`OwnedNumber::failed`]). Cheap: a few
/// references.
pub struct Messages<'a> {
    authz: &'a Authorizer,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Messages<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Messages")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Messages<'a> {
    /// The operations on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]): a failed call marks through `authz`
    /// ([`OwnedNumber::failed`]). Failed Graph calls are reported to nobody
    /// until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, logged at
    /// `warn`, and nothing is sent.
    pub fn new(authz: &'a Authorizer, number: &'a OwnedNumber) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        Ok(Self {
            authz,
            number,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The error of a failed Graph call: the number's `failed` (a `190`
    /// marks its WABA's numbers), counted when Meta answered with a Graph
    /// error, with Meta's `details`.
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    /// Send `message` (built by [`message`]) with the number's token: the
    /// accepted message, or the error, with Meta's `details`. A timeout,
    /// or a failure of Meta's that may have gone through, carries
    /// `may_have_been_sent`; this never sends twice (the client's
    /// `RetryPolicy` never replays a send that timed out).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, as the error model maps it (section
    /// 5.2: `409 customer_service_window_closed` for 131047, `504
    /// timeout`, …); `502 upstream` when Meta accepted without a message
    /// id.
    pub async fn send(&self, message: &OutboundMessage) -> Result<Accepted, ServiceError> {
        let sent = self
            .number
            .client()
            .messages(self.number.phone_number_id().clone())
            .send(message)
            .await;
        match sent {
            Ok(response) => Accepted::from_response(response),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Show the blue ticks on the received message `message_id` (and every
    /// earlier one, `messages/mark-message-as-read`); with
    /// `typing_indicator`, "typing…" too (`typing-indicators`: until the
    /// reply, or 25 seconds). Never replayed.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `message_id` (blank), before any request;
    /// Meta's refusal or failure, with its `details` (an unknown or too old
    /// message is Meta's `invalid_parameter`).
    pub async fn mark_read(
        &self,
        message_id: &str,
        typing_indicator: bool,
    ) -> Result<(), ServiceError> {
        if message_id.trim().is_empty() {
            return Err(ServiceError::invalid("message_id"));
        }
        let messages = self
            .number
            .client()
            .messages(self.number.phone_number_id().clone());
        let message_id = MessageId::new(message_id);
        let marked = if typing_indicator {
            messages.mark_read_with_typing_indicator(&message_id).await
        } else {
            messages.mark_read(&message_id).await
        };
        match marked {
            Ok(()) => Ok(()),
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_to(to: RecipientObject) -> SendMessage {
        let mut message = SendMessage::new(to, MessageType::Text);
        message.text = Some(TextObject {
            body: "hi".to_owned(),
            preview_url: None,
        });
        message
    }

    fn phone(phone: &str) -> RecipientObject {
        RecipientObject {
            phone: Some(phone.to_owned()),
            ..RecipientObject::default()
        }
    }

    /// E.164 with its `+`, strictly: `+`, 1 to 15 digits, the first not
    /// `0`. Decisive: each clause.
    #[test]
    fn e164_is_strict() {
        for good in ["+16505551234", "+1", &format!("+{}", "9".repeat(15))] {
            assert!(is_e164(good), "{good}");
        }
        for bad in [
            "16505551234",
            "+1 650 555 1234",
            "+06505551234",
            "+1234567890123456",
            "+",
            "++1",
            "+1a",
            "",
        ] {
            assert!(!is_e164(bad), "{bad:?}");
        }
    }

    /// The recipient's fields are checked before the content, and a phone
    /// number without its `+` is refused on `to.phone`, quoting nothing.
    #[test]
    fn a_digits_only_phone_is_refused_on_to_phone() {
        let refused = message(text_to(phone("16505551234"))).unwrap_err();
        assert_eq!(
            (refused.status(), refused.code(), refused.field()),
            (422, "invalid_request", Some("to.phone"))
        );
        assert!(!format!("{refused:?}").contains("16505551234"));
        // The content object is checked after the recipient.
        let mut both = SendMessage::new(phone("1"), MessageType::Text);
        both.text = None;
        assert_eq!(message(both).unwrap_err().field(), Some("to.phone"));
        let ok = message(text_to(phone("+16505551234"))).unwrap();
        assert_eq!(
            serde_json::to_value(&ok).unwrap()["to"],
            serde_json::json!("+16505551234")
        );
    }

    /// Recipient shapes: a group alone, a phone and a BSUID together,
    /// nobody refused.
    #[test]
    fn recipients_are_phones_bsuids_or_groups() {
        let group = RecipientObject {
            group_id: Some("Y2FwaV9ncm91cDox".to_owned()),
            ..RecipientObject::default()
        };
        assert!(matches!(recipient(group), Ok(Recipient::Group(_))));
        let both = RecipientObject {
            phone: Some("+16505551234".to_owned()),
            user_id: Some("US.1".to_owned()),
            group_id: None,
        };
        assert!(matches!(
            recipient(both),
            Ok(Recipient::PhoneAndUser { .. })
        ));
        for (to, field) in [
            (RecipientObject::default(), "to"),
            (
                RecipientObject {
                    user_id: Some(" ".to_owned()),
                    ..RecipientObject::default()
                },
                "to.user_id",
            ),
            (
                RecipientObject {
                    group_id: Some(" ".to_owned()),
                    ..RecipientObject::default()
                },
                "to.group_id",
            ),
            (
                RecipientObject {
                    phone: Some("+16505551234".to_owned()),
                    group_id: Some("g".to_owned()),
                    ..RecipientObject::default()
                },
                "to",
            ),
        ] {
            assert_eq!(recipient(to).unwrap_err().field(), Some(field));
        }
    }

    /// Direct Send and the types the service does not send are
    /// `unsupported_message_type`; a malformed body passes, for the
    /// reader to refuse.
    #[test]
    fn unsupported_types_and_direct_send_are_refused() {
        use serde_json::json;
        for body in [
            json!({"type": "pin"}),
            json!({"type": "interactive", "interactive": {"type": "flow"}}),
            json!({"type": "text", "category": "utility"}),
            json!({"ttl_seconds": 60}),
        ] {
            assert_eq!(
                refuse_unsupported(&body).unwrap_err().code(),
                "unsupported_message_type",
                "{body}"
            );
        }
        for body in [
            json!({"type": "text"}),
            json!({"type": 1}),
            json!([]),
            json!({"interactive": {"type": "cta_url"}}),
        ] {
            assert!(refuse_unsupported(&body).is_ok(), "{body}");
        }
    }

    /// Meta accepting without a message id may have sent it: `502
    /// upstream`, `may_have_been_sent`.
    #[test]
    fn an_answer_without_a_message_id_may_have_been_sent() {
        let response: SendResponse = serde_json::from_value(serde_json::json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
            "messages": []
        }))
        .unwrap();
        let error = Accepted::from_response(response).unwrap_err();
        assert_eq!((error.status(), error.code()), (502, "upstream"));
        assert!(error.may_have_been_sent());
    }
}
