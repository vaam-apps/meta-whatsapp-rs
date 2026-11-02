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
//! `interactive` `button`, `list` and `cta_url`, and the payment messages
//! `order_details` and `order_status` (roadmap P2: their `parameters`
//! Meta's object, read into the library's payment types with no key
//! dropped and checked by their currency's rules). Since roadmap M5a,
//! also `pin` (groups only), and the `interactive` types
//! `location_request_message`, `request_contact_info`,
//! `call_permission_request`, `voice_call`, `flow`, `address_message`,
//! `carousel` (media or product cards), `product`, `product_list` and
//! `catalog_message`, each read into the library's own type: what the
//! request carries is compared with what the library would send, and a
//! key it would drop is `422 invalid_request` on its path, before any
//! request ([`message`]). Direct Send (`category`, `ttl_seconds`,
//! `direct_send_config`) rides on any type, checked by the library's
//! rules; its `category` is one of Meta's three (an unknown one Meta
//! refuses). Any other type is `422 unsupported_message_type`
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
//! **Objects named by id** (roadmap M5a, the design's M5.2): a Flow's
//! `flow_id` and a product message's `catalog_id` name objects Meta
//! finds through any WABA the number's token reaches, so each is checked
//! against the number's own WABA before the send (`GET /{flow_id}`, its
//! `whatsapp_business_account`; `GET /{waba_id}/product_catalogs`): one
//! that is not the WABA's is `404 not_found` and nothing is sent. A
//! `flow_name` is Meta's to resolve within the sending number's WABA, a
//! group and a quoted or pinned message within the number's own
//! conversations.
//!
//! **A Flow's token** (`flow_token`) reaches no log: the request's `Debug`
//! shows a parameters object's keys, never its values
//! ([`ActionParameters`]).
//!
//! A send answers the accepted message ([`Accepted`]), not a status: the
//! adapter renders it (the axum one: `202`). An idempotent send runs
//! under [`crate::idempotency::run`]. Meta enforces the 24-hour window:
//! outside it a free-form message is `409 customer_service_window_closed`
//! (every type here but `template`, and a Direct Send `utility` or
//! `authentication` message, which Meta sends as a template).

use std::collections::BTreeMap;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::messages::{
    AddressMessage, AddressParameters, Audio, CallPermissionRequest, CardAction, CardHeader,
    CatalogMessage, Contact, ContactAddress, ContactEmail, ContactName, ContactOrg, ContactPhone,
    ContactUrl, CtaUrl, DirectSendCategory, Document, FlowAction, FlowActionPayload, FlowMessage,
    FlowMode, FlowParameters, FlowRef, Header, Image, Interactive, ListMessage, ListRow,
    ListSection, Location, LocationRequest, MediaCard, MediaCarousel, MediaSource, MessageContent,
    OutboundMessage, Pin, ProductCard, ProductCarousel, ProductList, ProductSection, QuickReply,
    Reaction, ReplyButton, ReplyButtons, RequestContactInfo, SavedAddress, SendResponse,
    SentContact, SingleProduct, Sticker, Text, Video, VoiceCall, VoiceCallParameters,
};
use meta_whatsapp_rs::client::payments::{
    OrderDetails, OrderDetailsParameters, OrderStatus, OrderUpdate, PaymentUpdate,
    PaymentUpdateStatus,
};
use meta_whatsapp_rs::client::templates::TemplateMessage;
use meta_whatsapp_rs::core::ids::{CatalogId, FlowId, GroupId, MediaId, MessageId, UserId, WaId};
use meta_whatsapp_rs::core::recipient::Recipient;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::authz::{Authorizer, OwnedNumber, object_failed};
use crate::error::{ErrorCode, ServiceError};
use crate::observe::Observer;
use crate::request::{dropped_key, graph_id, meta_object};

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
    /// `interactive` (every [`InteractiveType`]).
    Interactive,
    /// `pin`: pin or unpin a message in a group (`groups/groups-messaging`;
    /// roadmap M5a).
    Pin,
}

impl MessageType {
    /// Every type, by its name (the request's `type`).
    pub const ALL: [(&'static str, Self); 12] = [
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
        ("pin", Self::Pin),
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
    /// An order to review and pay (`payments/payments-in/pg`,
    /// `payments/payments-in/upi-intent`, `payments/payments-in/payment-links`,
    /// `payments/payments-br/orders`; roadmap P2).
    OrderDetails,
    /// An order's new status (`payments/payments-in/pg`, "Step 6",
    /// `payments/payments-br/orders`; roadmap P2).
    OrderStatus,
    /// `location_request_message`: a "send location" button
    /// (`messages/location-request-messages`; roadmap M5a).
    LocationRequest,
    /// `request_contact_info`: ask a user who writes by username for their
    /// phone number (`business-scoped-user-ids`; roadmap M5a).
    RequestContactInfo,
    /// `call_permission_request` (`calling/user-call-permissions`; roadmap
    /// M5a): only its body can be set.
    CallPermissionRequest,
    /// `voice_call`: a WhatsApp call button
    /// (`calling/call-button-messages-deep-links`; roadmap M5a).
    VoiceCall,
    /// `flow`: a button opening a WhatsApp Flow (`flows/guides/sendingaflow`;
    /// roadmap M5a).
    Flow,
    /// `address_message`: ask for a shipping address, India only
    /// (`messages/address-messages`; roadmap M5a).
    Address,
    /// `carousel`: media cards (`messages/interactive-media-carousel-messages`)
    /// or product cards (`catalogs/interactive-product-carousel-messages`;
    /// roadmap M5a).
    Carousel,
    /// `product`: one product (`catalogs/single-product-messages`; roadmap
    /// M5a).
    Product,
    /// `product_list`: several products (`catalogs/multi-product-messages`;
    /// roadmap M5a).
    ProductList,
    /// `catalog_message`: the catalog's "View catalog" button
    /// (`catalogs/catalog-messages`; roadmap M5a).
    Catalog,
}

impl InteractiveType {
    /// Every interactive type, by its name (`interactive.type`).
    pub const ALL: [(&'static str, Self); 15] = [
        ("button", Self::Button),
        ("list", Self::List),
        ("cta_url", Self::CtaUrl),
        ("order_details", Self::OrderDetails),
        ("order_status", Self::OrderStatus),
        ("location_request_message", Self::LocationRequest),
        ("request_contact_info", Self::RequestContactInfo),
        ("call_permission_request", Self::CallPermissionRequest),
        ("voice_call", Self::VoiceCall),
        ("flow", Self::Flow),
        ("address_message", Self::Address),
        ("carousel", Self::Carousel),
        ("product", Self::Product),
        ("product_list", Self::ProductList),
        ("catalog_message", Self::Catalog),
    ];

    /// Its name: the request's `interactive.type`.
    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, t)| *t == self)
            .map_or("", |(name, _)| name)
    }
}

impl Serialize for InteractiveType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// `{"text": …}`: an interactive message's body or footer.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InteractiveText {
    /// The text.
    pub text: String,
}

/// A media asset of an interactive header: an uploaded `id` or an
/// `https://` `link`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HeaderMedia {
    /// Uploaded media id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `https://` URL Meta fetches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// File name (documents only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// The kinds of interactive header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

/// An interactive message's header: `list` and `product_list` take
/// `text` only; a carousel's media card an `image` or `video` `link`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InteractiveHeader {
    /// Which kind (Meta's `type`).
    #[serde(rename = "type")]
    pub kind: HeaderType,
    /// The text, for `text`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The image, for `image`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<HeaderMedia>,
    /// The video, for `video`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<HeaderMedia>,
    /// The document, for `document`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<HeaderMedia>,
}

/// A reply button's `reply`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReplyObject {
    /// Returned in the reply's webhook; unique in the message.
    pub id: String,
    /// Label, at most 20 characters.
    pub title: String,
}

/// A reply button: `{"type": "reply", "reply": {…}}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReplyButtonObject {
    /// `reply` (Meta's `type`).
    #[serde(rename = "type")]
    pub kind: String,
    /// The button.
    pub reply: ReplyObject,
}

/// A list row.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ListRowObject {
    /// Returned in the reply's webhook.
    pub id: String,
    /// Title, at most 24 characters.
    pub title: String,
    /// Description, at most 72 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A list section.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ListSectionObject {
    /// Title, at most 24 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its rows.
    pub rows: Vec<ListRowObject>,
}

/// A product in a `product_list` section.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProductItemObject {
    /// The product's retailer id (its SKU) in the catalog.
    pub product_retailer_id: String,
}

/// A `product_list` section (`catalogs/multi-product-messages`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProductSectionObject {
    /// Title, at most 24 characters; required once there are several
    /// sections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its products.
    pub product_items: Vec<ProductItemObject>,
}

/// A section of `action.sections`: a list's rows, or a `product_list`'s
/// products.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SectionObject {
    /// A `list` section.
    List(ListSectionObject),
    /// A `product_list` section.
    Product(ProductSectionObject),
}

/// A URL button's `parameters`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CtaUrlParameters {
    /// Button label, at most 20 characters.
    pub display_text: String,
    /// The URL the button opens.
    pub url: String,
}

/// An interactive message's `action.parameters`: a URL button's, or
/// another type's object in Meta's JSON (`order_details`,
/// `order_status`, `flow`, `voice_call`, `address_message`,
/// `catalog_message`), read into the library's type with no key dropped.
///
/// Its `Debug` shows an object's keys, never its values: a Flow's
/// `flow_token`, a payment's details or an address never reach a log.
#[derive(Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ActionParameters {
    /// `cta_url`'s.
    CtaUrl(CtaUrlParameters),
    /// Any other type's, as Meta's pages write it (the name dates from
    /// roadmap P2, when only payment messages had one).
    Order(serde_json::Map<String, Value>),
}

impl std::fmt::Debug for ActionParameters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CtaUrl(parameters) => f.debug_tuple("CtaUrl").field(parameters).finish(),
            Self::Order(object) => f
                .debug_struct("Order")
                .field("keys", &object.keys().collect::<Vec<_>>())
                .finish_non_exhaustive(),
        }
    }
}

/// A carousel card's kind (Meta's card `type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CarouselCardType {
    /// A media card: an image or video header, a body, and a URL button
    /// or quick-reply buttons (both of Meta's examples name it
    /// `cta_url`).
    CtaUrl,
    /// A product card.
    Product,
}

/// A quick-reply button's `quick_reply`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QuickReplyObject {
    /// Returned in the reply's webhook, at most 256 characters.
    pub id: String,
    /// Label, at most 20 characters.
    pub title: String,
}

/// A media card's quick-reply button: `{"type": "quick_reply",
/// "quick_reply": {…}}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QuickReplyButtonObject {
    /// `quick_reply` (Meta's `type`).
    #[serde(rename = "type")]
    pub kind: String,
    /// The button.
    pub quick_reply: QuickReplyObject,
}

/// A carousel card's `action`: a media card's URL button (`name`
/// `cta_url`, `parameters`) or quick-reply `buttons`; a product card's
/// `product_retailer_id` and `catalog_id`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CardActionObject {
    /// `cta_url`, for a URL button.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The URL button.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<CtaUrlParameters>,
    /// Quick-reply buttons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buttons: Option<Vec<QuickReplyButtonObject>>,
    /// A product card's product.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_retailer_id: Option<String>,
    /// A product card's catalog, the same on every card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
}

/// A carousel card, as Meta writes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarouselCardObject {
    /// Its position, from `0`: the library sends each card's own, so it
    /// must be the card's place in the list.
    pub card_index: u32,
    /// Media or product (Meta's `type`); the same on every card.
    #[serde(rename = "type")]
    pub kind: CarouselCardType,
    /// A media card's image or video, by `link`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<InteractiveHeader>,
    /// A media card's body, at most 160 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<InteractiveText>,
    /// Its button(s), or its product.
    pub action: CardActionObject,
}

/// An interactive message's `action`: `buttons` for `button`; `button` and
/// `sections` for `list`; `name` and `parameters` for `cta_url`, the
/// payment messages, `flow`, `voice_call`, `address_message` and
/// `catalog_message`; `name` alone for `location_request_message`,
/// `request_contact_info` and `call_permission_request`; `cards` for
/// `carousel`; `catalog_id` with `product_retailer_id` for `product`, with
/// `sections` for `product_list`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InteractiveAction {
    /// Reply buttons, 1 to 3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buttons: Option<Vec<ReplyButtonObject>>,
    /// The list's button label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<String>,
    /// The list's or the product list's sections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<SectionObject>>,
    /// The action's name, for the types that have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The URL button, or another type's parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<ActionParameters>,
    /// A carousel's cards, 2 to 10.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cards: Option<Vec<CarouselCardObject>>,
    /// A product message's catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    /// A single product message's product.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_retailer_id: Option<String>,
}

/// `interactive`, as Meta writes it: any [`InteractiveType`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InteractiveObject {
    /// Which kind (Meta's `type`).
    #[serde(rename = "type")]
    pub kind: InteractiveType,
    /// Header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<InteractiveHeader>,
    /// Body: required by every type but `call_permission_request` and
    /// `product`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<InteractiveText>,
    /// Footer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<InteractiveText>,
    /// Buttons, list, URL button, cards, products, or another type's
    /// action.
    pub action: InteractiveAction,
}

/// `pin`'s operation (Meta's `type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinType {
    /// Pin the message.
    Pin,
    /// Unpin it.
    Unpin,
}

/// `pin` (`groups/groups-messaging`): pin or unpin a message of a group,
/// sent to the group (`to.group_id`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PinObject {
    /// `pin` or `unpin` (Meta's `type`).
    #[serde(rename = "type")]
    pub kind: PinType,
    /// The group message's id (`wamid.…`).
    pub message_id: String,
    /// For `pin` (required), 1 to 30 days; never for `unpin`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration_days: Option<u32>,
}

/// Direct Send's `category` (`direct-send/send-utility-and-authentication-messages`):
/// Meta's three values (Meta refuses any other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectSendCategoryObject {
    /// `utility`: sent as a template Meta generates, outside the
    /// 24-hour window too, at utility rates.
    Utility,
    /// `authentication` (access restricted by Meta): as `utility`, at
    /// authentication rates; never to a BSUID alone.
    Authentication,
    /// `service`: a normal service message (the same as no category),
    /// within the 24-hour window only.
    Service,
}

impl From<DirectSendCategoryObject> for DirectSendCategory {
    fn from(category: DirectSendCategoryObject) -> Self {
        match category {
            DirectSendCategoryObject::Utility => Self::Utility,
            DirectSendCategoryObject::Authentication => Self::Authentication,
            DirectSendCategoryObject::Service => Self::Service,
        }
    }
}

/// Direct Send's `direct_send_config` (`direct-send/business-named-templates`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectSendConfigObject {
    /// The template Meta creates or reuses under exactly this name:
    /// `utility` only, lowercase letters, digits and `_`, at most 512.
    pub template_name: String,
}

/// A raw Meta object (a send's `template`) in a `Debug`: its keys, never
/// its values, as [`ActionParameters`] shows an object (design D103): a
/// template's Flow button carries the Flow's `flow_token`, and its
/// parameters a customer's data or a one-time passcode.
pub struct KeysOnly<'a>(pub &'a Option<Value>);

impl std::fmt::Debug for KeysOnly<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            None => f.write_str("None"),
            Some(Value::Object(object)) => f
                .debug_struct("Object")
                .field("keys", &object.keys().collect::<Vec<_>>())
                .finish_non_exhaustive(),
            Some(_) => f.write_str("Some(..)"),
        }
    }
}

/// A message to send, as the request carried it: `kind` (the request's
/// `type`) names the one content object present.
///
/// Its `Debug` shows `template`'s keys only ([`KeysOnly`]): a template's
/// Flow button carries the Flow's `flow_token` (roadmap M5b, the M5a
/// review's finding).
#[derive(Clone, PartialEq)]
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
    /// For `pin`.
    pub pin: Option<PinObject>,
    /// Reply to this received message (`wamid.…`); not with `reaction`.
    pub reply_to: Option<String>,
    /// Echoed in this message's status events (Meta's
    /// `biz_opaque_callback_data`).
    pub callback_data: Option<String>,
    /// Direct Send's `category` (`direct-send`).
    pub category: Option<DirectSendCategoryObject>,
    /// Direct Send's time-to-live in seconds (`direct-send/configure-message-ttl`):
    /// `utility` 30 to 43,200, `authentication` 30 to 900.
    pub ttl_seconds: Option<u32>,
    /// Direct Send's business-named template.
    pub direct_send_config: Option<DirectSendConfigObject>,
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
            pin: None,
            reply_to: None,
            callback_data: None,
            category: None,
            ttl_seconds: None,
            direct_send_config: None,
        }
    }
}

impl std::fmt::Debug for SendMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Every field named, so that a new one fails to compile here.
        let Self {
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
            pin,
            reply_to,
            callback_data,
            category,
            ttl_seconds,
            direct_send_config,
        } = self;
        f.debug_struct("SendMessage")
            .field("to", to)
            .field("kind", kind)
            .field("text", text)
            .field("image", image)
            .field("video", video)
            .field("audio", audio)
            .field("document", document)
            .field("sticker", sticker)
            .field("location", location)
            .field("contacts", contacts)
            .field("reaction", reaction)
            .field("template", &KeysOnly(template))
            .field("interactive", interactive)
            .field("pin", pin)
            .field("reply_to", reply_to)
            .field("callback_data", callback_data)
            .field("category", category)
            .field("ttl_seconds", ttl_seconds)
            .field("direct_send_config", direct_send_config)
            .finish()
    }
}

/// A reply to a conversation, as the request carried it (roadmap M2a): a
/// [`SendMessage`] without `to`, as the conversation names its contact.
/// `kind` (the request's `type`) names the one content object present.
/// Its `Debug` shows `template`'s keys only, as [`SendMessage`]'s does.
#[derive(Clone, PartialEq)]
pub struct ReplyMessage {
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
    /// `components`).
    pub template: Option<Value>,
    /// For `interactive`.
    pub interactive: Option<InteractiveObject>,
    /// For `pin` (a group's conversation).
    pub pin: Option<PinObject>,
    /// Reply to this message of the conversation (`wamid.…`); not with
    /// `reaction`.
    pub reply_to: Option<String>,
    /// Echoed in this message's status events (Meta's
    /// `biz_opaque_callback_data`).
    pub callback_data: Option<String>,
    /// Direct Send's `category`: `utility` and `authentication` are sent
    /// as templates, so the window does not hold them.
    pub category: Option<DirectSendCategoryObject>,
    /// Direct Send's time-to-live in seconds.
    pub ttl_seconds: Option<u32>,
    /// Direct Send's business-named template.
    pub direct_send_config: Option<DirectSendConfigObject>,
}

impl ReplyMessage {
    /// A reply of `kind`, with no content object yet.
    pub fn new(kind: MessageType) -> Self {
        Self {
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
            pin: None,
            reply_to: None,
            callback_data: None,
            category: None,
            ttl_seconds: None,
            direct_send_config: None,
        }
    }

    /// The same content as a send with no recipient (every field named, so
    /// a field on one side only fails to compile).
    fn into_send(self) -> SendMessage {
        let Self {
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
            pin,
            reply_to,
            callback_data,
            category,
            ttl_seconds,
            direct_send_config,
        } = self;
        SendMessage {
            to: RecipientObject::default(),
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
            pin,
            reply_to,
            callback_data,
            category,
            ttl_seconds,
            direct_send_config,
        }
    }
}

impl std::fmt::Debug for ReplyMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Every field named, so that a new one fails to compile here.
        let Self {
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
            pin,
            reply_to,
            callback_data,
            category,
            ttl_seconds,
            direct_send_config,
        } = self;
        f.debug_struct("ReplyMessage")
            .field("kind", kind)
            .field("text", text)
            .field("image", image)
            .field("video", video)
            .field("audio", audio)
            .field("document", document)
            .field("sticker", sticker)
            .field("location", location)
            .field("contacts", contacts)
            .field("reaction", reaction)
            .field("template", &KeysOnly(template))
            .field("interactive", interactive)
            .field("pin", pin)
            .field("reply_to", reply_to)
            .field("callback_data", callback_data)
            .field("category", category)
            .field("ttl_seconds", ttl_seconds)
            .field("direct_send_config", direct_send_config)
            .finish()
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
            .ok_or_else(|| ServiceError::new(ErrorCode::UPSTREAM).with_may_have_been_sent(true))?;
        Ok(Self {
            message_id,
            contacts: response.contacts,
        })
    }
}

// ─── Building the library's message ──────────────────────────────────────

fn unsupported() -> ServiceError {
    ServiceError::new(ErrorCode::UNSUPPORTED_MESSAGE_TYPE)
}

/// For an adapter whose request is a JSON body, before it reads it into
/// a [`SendMessage`]: a body naming a type the service does not send (a
/// `type` or an `interactive.type` outside [`MessageType::ALL`] and
/// [`InteractiveType::ALL`]) is `422 unsupported_message_type`, not a
/// malformed body. Direct Send's keys are read since roadmap M5a.
///
/// # Errors
///
/// As above; anything else passes, to be read and checked.
pub fn refuse_unsupported(body: &Value) -> Result<(), ServiceError> {
    let Some(object) = body.as_object() else {
        return Ok(());
    };
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

/// The library's interactive message for `value`: the types before roadmap
/// M5a as they were built ([`interactive_before_m5a`]), the others read
/// into the library's type and compared with it ([`interactive_compared`]).
fn interactive(value: InteractiveObject) -> Result<Interactive, ServiceError> {
    match value.kind {
        InteractiveType::Button
        | InteractiveType::List
        | InteractiveType::CtaUrl
        | InteractiveType::OrderDetails
        | InteractiveType::OrderStatus => interactive_before_m5a(value),
        _ => interactive_compared(value),
    }
}

/// `interactive.body`'s text, which the type requires.
fn body_text(body: Option<InteractiveText>) -> Result<String, ServiceError> {
    required("interactive.body", body).map(|b| b.text)
}

/// The interactive types of roadmap P2 and before: built as they were
/// (the keys of the action they do not read are not refused, as before),
/// except that a key of the action they never had (`cards`, `catalog_id`,
/// `product_retailer_id`) or a product list's section is refused.
#[allow(clippy::too_many_lines)] // one arm per type, read top to bottom
fn interactive_before_m5a(value: InteractiveObject) -> Result<Interactive, ServiceError> {
    let InteractiveObject {
        kind,
        header: value_header,
        body,
        footer,
        action,
    } = value;
    let footer = footer.map(|f| f.text);
    Ok(match kind {
        InteractiveType::Button => {
            only_action_fields(&action, &ACTION_FIELDS_BEFORE_M5A)?;
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
            let mut message = ReplyButtons::new(body_text(body)?, buttons);
            message.header = value_header.map(header).transpose()?;
            message.footer = footer;
            Interactive::Buttons(message)
        }
        InteractiveType::List => {
            only_action_fields(&action, &ACTION_FIELDS_BEFORE_M5A)?;
            let sections = required("interactive.action.sections", action.sections)?
                .into_iter()
                .enumerate()
                .map(|(i, section)| match section {
                    SectionObject::List(s) => Ok(ListSection {
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
                    }),
                    SectionObject::Product(_) => Err(ServiceError::invalid(format!(
                        "interactive.action.sections[{i}]"
                    ))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let button = required("interactive.action.button", action.button)?;
            let mut message = ListMessage::new(body_text(body)?, button, sections);
            message.header = match value_header {
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
            only_action_fields(&action, &ACTION_FIELDS_BEFORE_M5A)?;
            if action.name.as_deref() != Some("cta_url") {
                return Err(ServiceError::invalid("interactive.action.name"));
            }
            let ActionParameters::CtaUrl(parameters) =
                required("interactive.action.parameters", action.parameters)?
            else {
                return Err(ServiceError::invalid("interactive.action.parameters"));
            };
            let mut message =
                CtaUrl::new(body_text(body)?, parameters.display_text, parameters.url);
            message.header = value_header.map(header).transpose()?;
            message.footer = footer;
            Interactive::CtaUrl(message)
        }
        InteractiveType::OrderDetails => {
            only_action_fields(&action, &["name", "parameters"])?;
            if action.name.as_deref() != Some("review_and_pay") {
                return Err(ServiceError::invalid("interactive.action.name"));
            }
            let parameters: OrderDetailsParameters =
                meta_object(PARAMETERS, &order_parameters(action.parameters)?)?;
            let mut message = OrderDetails::new(body_text(body)?, parameters);
            message.header = value_header.map(order_header).transpose()?;
            message.footer = footer;
            message.into()
        }
        InteractiveType::OrderStatus => {
            only_action_fields(&action, &["name", "parameters"])?;
            if action.name.as_deref() != Some("review_order") {
                return Err(ServiceError::invalid("interactive.action.name"));
            }
            // No page documents a header with fields for an order status
            // (Brazil's table lists one with none): not sent.
            if value_header.is_some() {
                return Err(ServiceError::invalid("interactive.header"));
            }
            let parameters: OrderStatusParameters =
                meta_object(PARAMETERS, &order_parameters(action.parameters)?)?;
            Interactive::OrderStatus(parameters.into_message(body_text(body)?, footer))
        }
        _ => return Err(ServiceError::internal()),
    })
}

/// The action's keys the types before roadmap M5a read (each its own;
/// the others they leave out, as before).
const ACTION_FIELDS_BEFORE_M5A: [&str; 5] = ["buttons", "button", "sections", "name", "parameters"];

/// Each key of an interactive message's `action`, and whether the request
/// carries it.
fn action_fields(action: &InteractiveAction) -> [(&'static str, bool); 8] {
    [
        ("buttons", action.buttons.is_some()),
        ("button", action.button.is_some()),
        ("sections", action.sections.is_some()),
        ("name", action.name.is_some()),
        ("parameters", action.parameters.is_some()),
        ("cards", action.cards.is_some()),
        ("catalog_id", action.catalog_id.is_some()),
        ("product_retailer_id", action.product_retailer_id.is_some()),
    ]
}

/// An `action` carrying only the keys `allowed`: another key would be
/// dropped, so it is refused on its path.
fn only_action_fields(action: &InteractiveAction, allowed: &[&str]) -> Result<(), ServiceError> {
    match action_fields(action)
        .into_iter()
        .find(|(field, present)| *present && !allowed.contains(field))
    {
        Some((field, _)) => Err(ServiceError::invalid(format!("interactive.action.{field}"))),
        None => Ok(()),
    }
}

/// The interactive types of roadmap M5a, each read into the library's own
/// type; then what the request carries is compared with what the library
/// would send: a key it would drop (a header or a footer the type has
/// not, a key of another type's action, a key of a parameters object the
/// library does not know, an `id` for a carousel card's media, …) is
/// `422 invalid_request` on its path, nothing sent. The values the
/// library writes itself (an action's `name`, a card's `card_index` and
/// `type`, a Flow's `flow_message_version`) are checked to be the ones it
/// writes.
#[allow(clippy::too_many_lines)] // one arm per type, read top to bottom
fn interactive_compared(value: InteractiveObject) -> Result<Interactive, ServiceError> {
    let requested = serde_json::to_value(&value).map_err(|_| ServiceError::internal())?;
    let InteractiveObject {
        kind,
        header: value_header,
        body,
        footer,
        action,
    } = value;
    let footer = footer.map(|f| f.text);
    let built: Interactive = match kind {
        InteractiveType::LocationRequest => {
            action_named(&action, "send_location")?;
            LocationRequest::new(body_text(body)?).into()
        }
        InteractiveType::RequestContactInfo => {
            action_named(&action, "request_contact_info")?;
            RequestContactInfo::new(body_text(body)?).into()
        }
        InteractiveType::CallPermissionRequest => {
            action_named(&action, "call_permission_request")?;
            CallPermissionRequest {
                body: body.map(|b| b.text),
            }
            .into()
        }
        InteractiveType::VoiceCall => {
            action_named(&action, "voice_call")?;
            let mut message = VoiceCall::new(body_text(body)?);
            message.parameters = action
                .parameters
                .as_ref()
                .map(parameters_as::<VoiceCallParametersObject>)
                .transpose()?
                .map(VoiceCallParametersObject::into_library);
            message.into()
        }
        InteractiveType::Flow => {
            action_named(&action, "flow")?;
            let parameters =
                parameters_as::<FlowParametersObject>(&required(PARAMETERS, action.parameters)?)?
                    .into_library()?;
            let mut message = FlowMessage::new(body_text(body)?, parameters);
            message.header = value_header.map(header).transpose()?;
            message.footer = footer;
            message.into()
        }
        InteractiveType::Address => {
            action_named(&action, "address_message")?;
            let parameters = parameters_as::<AddressParametersObject>(&required(
                PARAMETERS,
                action.parameters,
            )?)?
            .into_library();
            AddressMessage {
                header: value_header.map(header).transpose()?,
                body: body_text(body)?,
                footer,
                parameters,
            }
            .into()
        }
        InteractiveType::Carousel => carousel(
            body_text(body)?,
            required("interactive.action.cards", action.cards)?,
        )?,
        InteractiveType::Product => {
            let mut message = SingleProduct::new(
                CatalogId::new(required(
                    "interactive.action.catalog_id",
                    action.catalog_id,
                )?),
                required(
                    "interactive.action.product_retailer_id",
                    action.product_retailer_id,
                )?,
            );
            message.body = body.map(|b| b.text);
            message.footer = footer;
            message.into()
        }
        InteractiveType::ProductList => {
            let header = text_header(value_header)?;
            let sections =
                product_sections(required("interactive.action.sections", action.sections)?)?;
            let mut message = ProductList::new(
                header,
                body_text(body)?,
                CatalogId::new(required(
                    "interactive.action.catalog_id",
                    action.catalog_id,
                )?),
                sections,
            );
            message.footer = footer;
            message.into()
        }
        InteractiveType::Catalog => {
            action_named(&action, "catalog_message")?;
            let mut message = CatalogMessage::new(body_text(body)?);
            message.footer = footer;
            message.thumbnail_product_retailer_id = action
                .parameters
                .as_ref()
                .map(parameters_as::<CatalogParametersObject>)
                .transpose()?
                .and_then(|p| p.thumbnail_product_retailer_id);
            message.into()
        }
        InteractiveType::Button
        | InteractiveType::List
        | InteractiveType::CtaUrl
        | InteractiveType::OrderDetails
        | InteractiveType::OrderStatus => return Err(ServiceError::internal()),
    };
    let sent = serde_json::to_value(&built).map_err(|_| ServiceError::internal())?;
    match dropped_key(&requested, &sent, "interactive") {
        Some(path) => Err(ServiceError::invalid(path)),
        None => Ok(built),
    }
}

/// An `action` named `name`, the one the library writes for its type.
fn action_named(action: &InteractiveAction, name: &str) -> Result<(), ServiceError> {
    if action.name.as_deref() == Some(name) {
        Ok(())
    } else {
        Err(ServiceError::invalid("interactive.action.name"))
    }
}

/// A `product_list`'s header: text, which it requires.
fn text_header(value: Option<InteractiveHeader>) -> Result<String, ServiceError> {
    match value {
        Some(InteractiveHeader {
            kind: HeaderType::Text,
            text: Some(text),
            ..
        }) => Ok(text),
        _ => Err(ServiceError::invalid("interactive.header")),
    }
}

/// A `product_list`'s sections, each of products.
fn product_sections(sections: Vec<SectionObject>) -> Result<Vec<ProductSection>, ServiceError> {
    sections
        .into_iter()
        .enumerate()
        .map(|(i, section)| match section {
            SectionObject::Product(s) => Ok(ProductSection {
                title: s.title,
                product_retailer_ids: s
                    .product_items
                    .into_iter()
                    .map(|p| p.product_retailer_id)
                    .collect(),
            }),
            SectionObject::List(_) => Err(ServiceError::invalid(format!(
                "interactive.action.sections[{i}]"
            ))),
        })
        .collect()
}

/// A carousel: media cards, or product cards (the first card's `type`
/// decides, and every card must share it); each card's `card_index` its
/// place in the list, which the library writes.
fn carousel(body: String, cards: Vec<CarouselCardObject>) -> Result<Interactive, ServiceError> {
    let kind = cards.first().map(|card| card.kind);
    for (i, card) in cards.iter().enumerate() {
        if usize::try_from(card.card_index).ok() != Some(i) {
            return Err(ServiceError::invalid(format!(
                "interactive.action.cards[{i}].card_index"
            )));
        }
        if Some(card.kind) != kind {
            return Err(ServiceError::invalid(format!(
                "interactive.action.cards[{i}].type"
            )));
        }
    }
    Ok(if kind == Some(CarouselCardType::Product) {
        let cards = cards
            .into_iter()
            .enumerate()
            .map(|(i, card)| {
                let action = card.action;
                let path = format!("interactive.action.cards[{i}].action");
                let catalog = action
                    .catalog_id
                    .ok_or_else(|| ServiceError::invalid(format!("{path}.catalog_id")))?;
                let product = action
                    .product_retailer_id
                    .ok_or_else(|| ServiceError::invalid(format!("{path}.product_retailer_id")))?;
                Ok(ProductCard::new(CatalogId::new(catalog), product))
            })
            .collect::<Result<Vec<_>, ServiceError>>()?;
        ProductCarousel::new(body, cards).into()
    } else {
        let cards = cards
            .into_iter()
            .enumerate()
            .map(|(i, card)| media_card(i, card))
            .collect::<Result<Vec<_>, _>>()?;
        MediaCarousel::new(body, cards).into()
    })
}

/// Media card `i`: an image or video header by `https://` `link` (the
/// page documents no other), its body, and a URL button (`name`
/// `cta_url`) or quick-reply buttons.
fn media_card(i: usize, card: CarouselCardObject) -> Result<MediaCard, ServiceError> {
    let path = format!("interactive.action.cards[{i}]");
    let value_header = card
        .header
        .ok_or_else(|| ServiceError::invalid(format!("{path}.header")))?;
    let (kind, media) = match value_header.kind {
        HeaderType::Image => ("image", value_header.image),
        HeaderType::Video => ("video", value_header.video),
        HeaderType::Text | HeaderType::Document => {
            return Err(ServiceError::invalid(format!("{path}.header")));
        }
    };
    let link = media
        .and_then(|m| m.link)
        .filter(|link| {
            link.get(..8)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        })
        .ok_or_else(|| ServiceError::invalid(format!("{path}.header.{kind}.link")))?;
    let card_header = if kind == "image" {
        CardHeader::Image(link)
    } else {
        CardHeader::Video(link)
    };
    let action = card.action;
    let card_action = if let Some(buttons) = action.buttons {
        CardAction::QuickReplies(
            buttons
                .into_iter()
                .enumerate()
                .map(|(j, b)| {
                    if b.kind == "quick_reply" {
                        Ok(QuickReply::new(b.quick_reply.id, b.quick_reply.title))
                    } else {
                        Err(ServiceError::invalid(format!(
                            "{path}.action.buttons[{j}].type"
                        )))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    } else {
        if action.name.as_deref() != Some("cta_url") {
            return Err(ServiceError::invalid(format!("{path}.action.name")));
        }
        let parameters = action
            .parameters
            .ok_or_else(|| ServiceError::invalid(format!("{path}.action.parameters")))?;
        CardAction::Url {
            display_text: parameters.display_text,
            url: parameters.url,
        }
    };
    let mut media_card = MediaCard::new(card_header, card_action);
    media_card.body = card.body.map(|b| b.text);
    Ok(media_card)
}

/// Where an interactive message's parameters are, in the request.
const PARAMETERS: &str = "interactive.action.parameters";

/// A parameters object, read as `M` (an object it is not is `422` on
/// [`PARAMETERS`]); its keys are compared with what the library sends
/// afterwards ([`interactive_compared`]), so `M` reads Meta's names and
/// ignores the others.
fn parameters_as<M: DeserializeOwned>(parameters: &ActionParameters) -> Result<M, ServiceError> {
    let value = serde_json::to_value(parameters).map_err(|_| ServiceError::internal())?;
    M::deserialize(&value).map_err(|_| ServiceError::invalid(PARAMETERS))
}

/// A Flow message's `action.parameters`, by Meta's names
/// (`flows/guides/sendingaflow`, "Interactive message parameters").
#[derive(serde::Deserialize)]
struct FlowParametersObject {
    flow_message_version: Option<String>,
    flow_token: Option<String>,
    flow_id: Option<String>,
    flow_name: Option<String>,
    flow_cta: String,
    mode: Option<FlowModeObject>,
    flow_action: Option<FlowAction>,
    flow_action_payload: Option<FlowActionPayloadObject>,
}

/// `mode`: `draft` or `published`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum FlowModeObject {
    Draft,
    Published,
}

/// `flow_action_payload`.
#[derive(serde::Deserialize)]
struct FlowActionPayloadObject {
    screen: String,
    data: Option<Value>,
}

impl FlowParametersObject {
    /// The library's parameters: a `flow_message_version` other than the
    /// library's (`3`, the only one Meta takes), a Flow named by both id
    /// and name or by neither, or an id that is not a Meta id (it becomes
    /// a Graph path segment when it is checked against the number's WABA)
    /// is refused on its field.
    fn into_library(self) -> Result<FlowParameters, ServiceError> {
        let Self {
            flow_message_version,
            flow_token,
            flow_id,
            flow_name,
            flow_cta,
            mode,
            flow_action,
            flow_action_payload,
        } = self;
        if flow_message_version.is_some_and(|v| v != FlowParameters::MESSAGE_VERSION) {
            return Err(ServiceError::invalid(
                "interactive.action.parameters.flow_message_version",
            ));
        }
        let flow = match (flow_id, flow_name) {
            (Some(id), None) => {
                graph_id("interactive.action.parameters.flow_id", &id)?;
                FlowRef::Id(FlowId::new(id))
            }
            (None, Some(name)) => FlowRef::Name(name),
            // "Cannot be used with the `flow_id` parameter."
            (Some(_), Some(_)) => {
                return Err(ServiceError::invalid(
                    "interactive.action.parameters.flow_name",
                ));
            }
            (None, None) => {
                return Err(ServiceError::invalid(
                    "interactive.action.parameters.flow_id",
                ));
            }
        };
        let mut parameters = FlowParameters::new(flow, flow_cta);
        parameters.flow_token = flow_token;
        parameters.mode = mode.map(|m| match m {
            FlowModeObject::Draft => FlowMode::Draft,
            FlowModeObject::Published => FlowMode::Published,
        });
        parameters.flow_action = flow_action;
        parameters.flow_action_payload = flow_action_payload.map(|p| FlowActionPayload {
            screen: p.screen,
            data: p.data,
        });
        Ok(parameters)
    }
}

/// A call button's `action.parameters`
/// (`calling/call-button-messages-deep-links`).
#[derive(serde::Deserialize)]
struct VoiceCallParametersObject {
    display_text: Option<String>,
    ttl_minutes: Option<u32>,
    payload: Option<String>,
}

impl VoiceCallParametersObject {
    fn into_library(self) -> VoiceCallParameters {
        let Self {
            display_text,
            ttl_minutes,
            payload,
        } = self;
        VoiceCallParameters {
            display_text,
            ttl_minutes,
            payload,
        }
    }
}

/// An address message's `action.parameters` (`messages/address-messages`).
#[derive(serde::Deserialize)]
struct AddressParametersObject {
    country: String,
    #[serde(default)]
    values: BTreeMap<String, String>,
    #[serde(default)]
    saved_addresses: Vec<SavedAddressObject>,
    #[serde(default)]
    validation_errors: BTreeMap<String, String>,
}

/// A saved address offered.
#[derive(serde::Deserialize)]
struct SavedAddressObject {
    id: String,
    value: BTreeMap<String, String>,
}

impl AddressParametersObject {
    fn into_library(self) -> AddressParameters {
        let Self {
            country,
            values,
            saved_addresses,
            validation_errors,
        } = self;
        AddressParameters {
            country,
            values,
            saved_addresses: saved_addresses
                .into_iter()
                .map(|a| SavedAddress {
                    id: a.id,
                    value: a.value,
                })
                .collect(),
            validation_errors,
        }
    }
}

/// A catalog message's `action.parameters` (`catalogs/catalog-messages`).
#[derive(serde::Deserialize)]
struct CatalogParametersObject {
    thumbnail_product_retailer_id: Option<String>,
}

/// A payment message's `parameters`, Meta's object.
fn order_parameters(parameters: Option<ActionParameters>) -> Result<Value, ServiceError> {
    match required(PARAMETERS, parameters)? {
        ActionParameters::Order(object) => Ok(Value::Object(object)),
        ActionParameters::CtaUrl(_) => Err(ServiceError::invalid(PARAMETERS)),
    }
}

/// An `order_details` header: an image, by uploaded `id` or `https://`
/// `link` (the payments pages document no other; Brazil's takes a link
/// only, which the library checks).
fn order_header(value: InteractiveHeader) -> Result<MediaSource, ServiceError> {
    let InteractiveHeader {
        kind,
        text,
        image,
        video,
        document,
    } = value;
    if kind != HeaderType::Image || text.is_some() || video.is_some() || document.is_some() {
        return Err(ServiceError::invalid("interactive.header"));
    }
    let image = image.ok_or_else(|| ServiceError::invalid("interactive.header.image"))?;
    if image.filename.is_some() {
        return Err(ServiceError::invalid("interactive.header.image.filename"));
    }
    media_source("interactive.header.image", image.id, image.link)
}

/// An `order_status` message's `parameters`, as Meta's pages write them
/// (`payments/payments-in/pg`, "Step 6"; `payments/payments-br/orders`):
/// read with no key dropped.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OrderStatusParameters {
    reference_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<OrderUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payment: Option<PaymentUpdateObject>,
}

/// `parameters.payment` of a Brazil `order_status` message.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PaymentUpdateObject {
    status: PaymentUpdateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timestamp: Option<u64>,
}

impl OrderStatusParameters {
    /// The library's message. The wire names no currency, and the library
    /// checks an order status by its order's (`OrderStatus::currency`, not
    /// sent): it is checked by Brazil's rules, the wider (India's refuse a
    /// `payment` update, a `pending` status, a missing `order` and a
    /// `reference_id` longer than 35 characters, which Brazil's page
    /// allows), so an India order's status breaking only India's rules is
    /// left for Meta to refuse. The body's and footer's limits, the
    /// `reference_id`'s alphabet and its 60 characters, an order update's
    /// description, are checked either way.
    fn into_message(self, body: String, footer: Option<String>) -> OrderStatus {
        let Self {
            reference_id,
            order,
            payment,
        } = self;
        let mut message = OrderStatus::brl(body, reference_id);
        message.order = order;
        message.payment = payment.map(|p| {
            let update = PaymentUpdate::new(p.status);
            match p.timestamp {
                Some(timestamp) => update.timestamp(timestamp),
                None => update,
            }
        });
        message.footer = footer;
        message
    }
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
        ("pin", request.pin.is_some()),
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
/// recipient ([`recipient`]); the content object (a key the library would
/// drop refused on its path, for the types of roadmap M5a); then the
/// library's checks, Direct Send's among them, on the request's names for
/// its fields (`to`, `reply_to`, `callback_data`, `ttl_seconds`,
/// `direct_send_config`).
///
/// # Errors
///
/// `422 invalid_request` on the field at fault.
pub fn message(mut request: SendMessage) -> Result<OutboundMessage, ServiceError> {
    only_the_named_object(&request)?;
    let recipient = recipient(std::mem::take(&mut request.to))?;
    build(recipient, request)
}

/// The library's message for a reply's `request`, addressed to
/// `recipient` (the conversation's contact: the library's
/// `Inbox::recipient`, never the request's word), checked as [`message`]
/// checks a send: one content object, the one `type` names; the content
/// object; then the library's checks, on the request's names for its
/// fields (`reply_to`, `callback_data`, …). A refusal of the recipient
/// (a pin to a conversation that is not a group's, a Direct Send
/// `authentication` message to a contact known by BSUID alone) is on
/// `contact`, the path's: the reply has no `to`.
///
/// # Errors
///
/// `422 invalid_request` on the field at fault.
pub fn reply_message(
    request: ReplyMessage,
    recipient: Recipient,
) -> Result<OutboundMessage, ServiceError> {
    let request = request.into_send();
    only_the_named_object(&request)?;
    build(recipient, request).map_err(|error| {
        if error.field() == Some("to") {
            ServiceError::invalid("contact")
        } else {
            error
        }
    })
}

/// `pin`, read into the library's [`Pin`]: a `pin` needs its
/// `expiration_days`, and an `unpin`'s would be dropped (refused on its
/// path).
fn pin(value: PinObject) -> Result<Pin, ServiceError> {
    let requested = serde_json::to_value(&value).map_err(|_| ServiceError::internal())?;
    let PinObject {
        kind,
        message_id,
        expiration_days,
    } = value;
    let message_id = MessageId::new(message_id);
    let built = match kind {
        PinType::Pin => Pin::Pin {
            message_id,
            expiration_days: required("pin.expiration_days", expiration_days)?,
        },
        PinType::Unpin => Pin::Unpin { message_id },
    };
    let sent = serde_json::to_value(&built).map_err(|_| ServiceError::internal())?;
    match dropped_key(&requested, &sent, "pin") {
        Some(path) => Err(ServiceError::invalid(path)),
        None => Ok(built),
    }
}

/// The library's message of `request`'s content, to `recipient` (its `to`
/// is not read: the caller read it into `recipient`, or has none).
fn build(recipient: Recipient, request: SendMessage) -> Result<OutboundMessage, ServiceError> {
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
        MessageType::Pin => pin(required("pin", request.pin)?)?.into(),
    };
    let mut message = OutboundMessage::new(recipient, content);
    if let Some(id) = request.reply_to {
        message = message.reply_to(MessageId::new(id));
    }
    if let Some(data) = request.callback_data {
        message = message.callback_data(data);
    }
    // Direct Send: the library checks the category's rules (a TTL and a
    // business-named template only where Meta takes them, no
    // `authentication` message to a BSUID alone, the tighter limits).
    if let Some(category) = request.category {
        message = message.category(category.into());
    }
    if let Some(ttl) = request.ttl_seconds {
        message = message.ttl_seconds(ttl);
    }
    if let Some(config) = request.direct_send_config {
        message = message.direct_send_template_name(config.template_name);
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

// ─── Objects named by id (the design's M5.2) ─────────────────────────────

/// An object a message names by id, which Meta would find through any
/// WABA the number's token reaches (a token may reach other tenants').
#[derive(Debug, Clone, PartialEq, Eq)]
enum NamedObject<'m> {
    /// A Flow message's `flow_id`.
    Flow(&'m FlowId),
    /// A product message's `catalog_id` (one per message: a product
    /// carousel's cards share theirs, which the library checks).
    Catalog(&'m CatalogId),
    /// The catalogs a template's product parameters name (a single-product
    /// template's header, a product card carousel's cards; roadmap M5b,
    /// left to it by M5a): every `catalog_id` the template as sent would
    /// carry, at any depth, a parameter the library does not type
    /// included, each of which must be the WABA's.
    TemplateCatalogs(Vec<String>),
}

/// Every `catalog_id` in `value`, at any depth (a string, or a number
/// written as one).
fn catalog_ids(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                match (key.as_str(), value) {
                    ("catalog_id", Value::String(id)) => found.push(id.clone()),
                    ("catalog_id", Value::Number(id)) => found.push(id.to_string()),
                    _ => catalog_ids(value, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| catalog_ids(item, found)),
        _ => {}
    }
}

/// The object `message` names by id, if any. A Flow named by `flow_name`
/// is Meta's to find within the sending number's WABA; a
/// `catalog_message` names none (Meta shows the WABA's own catalog).
fn named_object(message: &OutboundMessage) -> Option<NamedObject<'_>> {
    if let MessageContent::Template(template) = &message.content {
        // What Meta would receive, so that a parameter in a catch-all is
        // read as well as a typed one.
        let mut found = Vec::new();
        catalog_ids(
            &serde_json::to_value(template).unwrap_or(Value::Null),
            &mut found,
        );
        found.sort_unstable();
        found.dedup();
        return (!found.is_empty()).then_some(NamedObject::TemplateCatalogs(found));
    }
    let MessageContent::Interactive(interactive) = &message.content else {
        return None;
    };
    match interactive {
        Interactive::Flow(flow) => match &flow.parameters.flow {
            FlowRef::Id(id) => Some(NamedObject::Flow(id)),
            FlowRef::Name(_) => None,
        },
        Interactive::Product(product) => Some(NamedObject::Catalog(&product.catalog_id)),
        Interactive::ProductList(list) => Some(NamedObject::Catalog(&list.catalog_id)),
        Interactive::ProductCarousel(carousel) => carousel
            .cards
            .first()
            .map(|card| NamedObject::Catalog(&card.catalog_id)),
        _ => None,
    }
}

/// Whether `message` names an object [`admit_named_object`] asks Meta
/// about.
pub(crate) fn names_an_object(message: &OutboundMessage) -> bool {
    named_object(message).is_some()
}

/// The object `message` names by id ([`named_object`]) is the number's
/// WABA's, asked of Meta with the number's token before anything is sent:
/// a Flow's `GET /{flow_id}` must answer that very id and name the
/// number's WABA as its `whatsapp_business_account`; a catalog must be
/// among `GET /{waba_id}/product_catalogs` (a WABA has one at most). A
/// message naming none asks nothing.
///
/// # Errors
///
/// `404 not_found`, without Meta's code or text: the object is not the
/// WABA's, or Meta refuses the id (none, or not for this token). Meta's
/// failure otherwise, with its `details`, counted in `observer`, a `190`
/// marking the WABA's numbers; never `may_have_been_sent`, as the message
/// was not sent (an `Idempotency-Key` is released).
pub(crate) async fn admit_named_object(
    authz: &Authorizer,
    number: &OwnedNumber,
    observer: &dyn Observer,
    message: &OutboundMessage,
) -> Result<(), ServiceError> {
    let checked = match named_object(message) {
        None => return Ok(()),
        Some(NamedObject::Flow(id)) => number.client().flow(id.clone()).get().await.map(|flow| {
            let waba = flow
                .whatsapp_business_account
                .as_ref()
                .and_then(|w| w.get("id"))
                .and_then(Value::as_str);
            flow.id == *id && waba == Some(number.waba_id().as_str())
        }),
        Some(NamedObject::Catalog(id)) => number
            .client()
            .waba(number.waba_id().clone())
            .product_catalogs(&[])
            .await
            .map(|page| page.data.iter().any(|catalog| catalog.id == *id)),
        Some(NamedObject::TemplateCatalogs(ids)) => number
            .client()
            .waba(number.waba_id().clone())
            .product_catalogs(&[])
            .await
            .map(|page| {
                ids.iter()
                    .all(|id| page.data.iter().any(|catalog| catalog.id.as_str() == id))
            }),
    };
    match checked {
        Ok(true) => Ok(()),
        Ok(false) => {
            tracing::info!("an object a message names is not the number's WABA's: not sent");
            Err(ServiceError::not_found())
        }
        Err(error) => {
            let api = number.failed(authz, &error).await;
            if error.graph().is_some() {
                observer.graph_error(api.code());
            }
            Err(object_failed(api, &error).with_may_have_been_sent(false))
        }
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
    /// accepted message, or the error, with Meta's `details`. A Flow or a
    /// catalog it names by id is checked first to be the number's WABA's
    /// (the [module](self)'s "Objects named by id"). A timeout, or a
    /// failure of Meta's that may have gone through, carries
    /// `may_have_been_sent`; this never sends twice (the client's
    /// `RetryPolicy` never replays a send that timed out).
    ///
    /// # Errors
    ///
    /// `404 not_found`: a named object that is not the number's WABA's,
    /// nothing sent. Meta's refusal or failure, as the error model maps
    /// it (section 5.2: `409 customer_service_window_closed` for 131047,
    /// `504 timeout`, …); `502 upstream` when Meta accepted without a
    /// message id.
    pub async fn send(&self, message: &OutboundMessage) -> Result<Accepted, ServiceError> {
        admit_named_object(self.authz, self.number, self.observer, message).await?;
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

    /// A payment interactive (roadmap P2) of `kind`, with Meta's
    /// `parameters` and `name`.
    fn order(kind: InteractiveType, name: &str, parameters: serde_json::Value) -> SendMessage {
        let serde_json::Value::Object(parameters) = parameters else {
            panic!("an object");
        };
        let mut message = SendMessage::new(phone("+919876543210"), MessageType::Interactive);
        message.interactive = Some(InteractiveObject {
            kind,
            header: None,
            body: Some(InteractiveText {
                text: "your-text-body-content".to_owned(),
            }),
            footer: None,
            action: InteractiveAction {
                name: Some(name.to_owned()),
                parameters: Some(ActionParameters::Order(parameters)),
                ..InteractiveAction::default()
            },
        });
        message
    }

    /// `payments-in/pg`, step 6, and `payments-br/offsite-pix`'s simplified
    /// order: read into the library's types and sent as the pages print
    /// them; a key the library would drop, a wrong action name, a header on
    /// an order status, a URL button's parameters, refused on their field.
    /// Decisive: the two arms of `interactive`, `meta_object` on their
    /// parameters, and their checks.
    #[test]
    fn payment_interactives_are_read_whole_and_checked() {
        use serde_json::json;
        let status = json!({"reference_id": "reference-id-value",
            "order": {"status": "processing", "description": "optional-text"}});
        let built = message(order(
            InteractiveType::OrderStatus,
            "review_order",
            status.clone(),
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&built).unwrap()["interactive"],
            json!({"type": "order_status", "body": {"text": "your-text-body-content"},
                   "action": {"name": "review_order", "parameters": status}})
        );
        let pix = json!({
          "reference_id": "unique-reference-id",
          "type": "digital-goods",
          "payment_type": "br",
          "payment_settings": [{"type": "pix_dynamic_code", "pix_dynamic_code": {
            "code": "00020101021226700014br.gov.bcb.pix2548pix.example.com...",
            "merchant_name": "Account holder name", "key": "39580525000189", "key_type": "CNPJ"}}],
          "currency": "BRL",
          "total_amount": {"value": 50000, "offset": 100}
        });
        let built = message(order(
            InteractiveType::OrderDetails,
            "review_and_pay",
            pix.clone(),
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&built).unwrap()["interactive"]["action"],
            json!({"name": "review_and_pay", "parameters": pix})
        );
        let mut dropped = pix.clone();
        dropped["refrence_id"] = json!("typo");
        let mut header = order(InteractiveType::OrderStatus, "review_order", status.clone());
        header.interactive.as_mut().unwrap().header = Some(InteractiveHeader {
            kind: HeaderType::Text,
            text: Some("h".to_owned()),
            image: None,
            video: None,
            document: None,
        });
        let mut cta = order(InteractiveType::OrderDetails, "review_and_pay", pix.clone());
        cta.interactive.as_mut().unwrap().action.parameters =
            Some(ActionParameters::CtaUrl(CtaUrlParameters {
                display_text: "Pay".to_owned(),
                url: "https://example.com".to_owned(),
            }));
        let mut india_pending = status.clone();
        india_pending["order"]["status"] = json!("pending");
        for (request, field) in [
            (
                order(InteractiveType::OrderDetails, "review_and_pay", dropped),
                "interactive.action.parameters.refrence_id",
            ),
            (
                order(InteractiveType::OrderDetails, "review_order", pix.clone()),
                "interactive.action.name",
            ),
            (
                order(
                    InteractiveType::OrderStatus,
                    "review_and_pay",
                    status.clone(),
                ),
                "interactive.action.name",
            ),
            (header, "interactive.header"),
            (cta, "interactive.action.parameters"),
            (
                order(
                    InteractiveType::OrderStatus,
                    "review_order",
                    json!({"reference_id": "x".repeat(61)}),
                ),
                "interactive.action.parameters.reference_id",
            ),
        ] {
            assert_eq!(
                message(request).unwrap_err().field(),
                Some(field),
                "{field}"
            );
        }
        // The wire names no currency: a `pending` order status passes, as
        // Brazil's rules allow it (India's refusal is Meta's).
        assert!(
            message(order(
                InteractiveType::OrderStatus,
                "review_order",
                india_pending
            ))
            .is_ok()
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

    /// The types the service does not send are `unsupported_message_type`;
    /// a malformed body passes, for the reader to refuse. Direct Send's
    /// keys, `pin` and the interactive types of roadmap M5a pass too.
    #[test]
    fn unsupported_types_are_refused() {
        use serde_json::json;
        for body in [
            json!({"type": "order_details"}),
            json!({"type": "interactive", "interactive": {"type": "nfm_reply"}}),
            json!({"type": "interactive", "interactive": {"type": "flows"}}),
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
            json!({"type": "pin"}),
            json!({"type": "interactive", "interactive": {"type": "flow"}}),
            json!({"type": "text", "category": "utility"}),
            json!({"ttl_seconds": 60}),
        ] {
            assert!(refuse_unsupported(&body).is_ok(), "{body}");
        }
    }

    /// A Flow's token never reaches a log (design D103): a parameters
    /// object's `Debug` shows its keys, never its values, through every
    /// request type that holds one, and the library's message built from
    /// it hides the token too. Decisive: `ActionParameters`' `Debug`, and
    /// the library's `FlowParameters`' `Debug`.
    #[test]
    fn a_flow_token_never_reaches_debug() {
        use serde_json::json;
        let token = "SECRET-FLOW-TOKEN-1";
        let serde_json::Value::Object(parameters) = json!({"flow_message_version": "3",
            "flow_token": token, "flow_name": "appointment_booking_v1", "flow_cta": "Book!"})
        else {
            panic!("an object");
        };
        let mut request = SendMessage::new(phone("+16505551234"), MessageType::Interactive);
        request.interactive = Some(InteractiveObject {
            kind: InteractiveType::Flow,
            header: None,
            body: Some(InteractiveText {
                text: "Book an appointment".to_owned(),
            }),
            footer: None,
            action: InteractiveAction {
                name: Some("flow".to_owned()),
                parameters: Some(ActionParameters::Order(parameters)),
                ..InteractiveAction::default()
            },
        });
        let shown = format!("{request:?}");
        assert!(!shown.contains(token), "{shown}");
        assert!(shown.contains("flow_token"), "the keys are shown: {shown}");
        // And the token reaches Meta, as given, while the library's
        // message hides it too (its `FlowParameters`' `Debug`).
        let built = message(request).unwrap();
        let shown = format!("{built:?}");
        assert!(!shown.contains(token), "{shown}");
        assert_eq!(
            serde_json::to_value(&built).unwrap()["interactive"]["action"]["parameters"]["flow_token"],
            token
        );
    }

    /// A template's Flow button carries the Flow's `flow_token` (Meta's
    /// `flows/guides/flows-templates`): the request's raw `template` shows
    /// its keys only in `Debug`, on a send and on a reply, and the library's
    /// message built from it hides the token too (its `ParameterAction`'s
    /// `Debug`), while Meta still receives it. Roadmap M5b fixes what the
    /// M5a review found. Decisive: `SendMessage`'s and `ReplyMessage`'s own
    /// `Debug` (a derived one prints the object), and the library's
    /// `ParameterAction`'s.
    #[test]
    fn a_templates_flow_token_never_reaches_debug() {
        use serde_json::json;
        let token = "SECRET-TEMPLATE-FLOW-TOKEN-1";
        let template = json!({"name": "book_table", "language": {"code": "en_US"},
            "components": [{"type": "button", "sub_type": "flow", "index": "0",
                "parameters": [{"type": "action", "action": {"flow_token": token,
                    "flow_action_data": {"guests": 2}}}]}]});
        let mut send = SendMessage::new(phone("+16505551234"), MessageType::Template);
        send.template = Some(template.clone());
        let mut reply = ReplyMessage::new(MessageType::Template);
        reply.template = Some(template);
        for shown in [
            format!("{send:?}"),
            format!("{send:#?}"),
            format!("{reply:?}"),
            format!("{reply:#?}"),
        ] {
            assert!(!shown.contains(token), "{shown}");
            assert!(
                shown.contains("components") && shown.contains("language"),
                "the keys are shown: {shown}"
            );
        }
        assert!(format!("{:?}", KeysOnly(&None)).contains("None"));
        assert!(!format!("{:?}", KeysOnly(&Some(json!(token)))).contains(token));
        let built = message(send).unwrap();
        for shown in [format!("{built:?}"), format!("{built:#?}")] {
            assert!(!shown.contains(token), "{shown}");
        }
        assert_eq!(
            serde_json::to_value(&built).unwrap()["template"]["components"][0]["parameters"][0]["action"]
                ["flow_token"],
            token
        );
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
