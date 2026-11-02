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
//! object); `interactive` `button`, `list` and `cta_url`, and the payment
//! messages `order_details` and `order_status` (roadmap P2: their
//! `parameters` Meta's object, checked by the library's payment rules).
//! Since roadmap M5a also `pin`, the `interactive` types
//! `location_request_message`, `request_contact_info`,
//! `call_permission_request`, `voice_call`, `flow`, `address_message`,
//! `carousel`, `product`, `product_list` and `catalog_message` (a key the
//! library would drop refused on its path; a Flow's `flow_id` or a
//! product's `catalog_id` checked to be the number's WABA's, else `404`),
//! and Direct Send (`category`, `ttl_seconds`, `direct_send_config`) on
//! any type. Any other type is `422 unsupported_message_type`. Meta
//! enforces the 24-hour window: outside it a free-form message is `409
//! customer_service_window_closed` (a template and a Direct Send
//! `utility` or `authentication` message are not free-form).
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

use std::collections::HashMap;

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
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

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
    /// `interactive` (every `InteractiveType`).
    Interactive,
    /// `pin`: pin or unpin a group's message (`groups/groups-messaging`).
    Pin,
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
    /// An order to review and pay (`payments/payments-in/pg`,
    /// `payments/payments-br/orders`): scope `send`, like every message.
    OrderDetails,
    /// An order's new status (`payments/payments-in/pg`,
    /// `payments/payments-br/orders`).
    OrderStatus,
    /// A "send location" button (`messages/location-request-messages`).
    #[serde(rename = "location_request_message")]
    LocationRequest,
    /// Ask a user who writes by username for their phone number
    /// (`business-scoped-user-ids`).
    RequestContactInfo,
    /// Ask for permission to call (`calling/user-call-permissions`): only
    /// the body can be set.
    CallPermissionRequest,
    /// A WhatsApp call button (`calling/call-button-messages-deep-links`).
    VoiceCall,
    /// A button opening a WhatsApp Flow (`flows/guides/sendingaflow`).
    Flow,
    /// Ask for a shipping address, India only (`messages/address-messages`).
    #[serde(rename = "address_message")]
    Address,
    /// Media cards (`messages/interactive-media-carousel-messages`) or
    /// product cards (`catalogs/interactive-product-carousel-messages`).
    Carousel,
    /// One product (`catalogs/single-product-messages`).
    Product,
    /// Several products (`catalogs/multi-product-messages`).
    ProductList,
    /// The catalog's "View catalog" button (`catalogs/catalog-messages`).
    #[serde(rename = "catalog_message")]
    Catalog,
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

/// A payment message's `action.parameters` (`order_details`,
/// `order_status`), in Meta's JSON as the payments pages write it
/// (`payments/payments-in/pg`, `upi-intent`, `payment-links`,
/// `payments/payments-br/orders` and its pages): read into the library's
/// payment types with no key dropped, and checked by the order's
/// currency's rules (`INR` India's, `BRL` Brazil's) before any request.
#[derive(Debug, ToSchema)]
#[schema(as = OrderParameters, value_type = HashMap<String, Value>)]
pub struct OrderParameters(pub serde_json::Map<String, Value>);

/// A Flow message's `action.parameters` (`flows/guides/sendingaflow`,
/// "Interactive message parameters"): read into the library's
/// `FlowParameters`, a key it would drop refused on its path. Schema
/// only: the request carries Meta's object.
#[derive(ToSchema)]
#[allow(dead_code)] // read as Meta's object; the type documents it
pub struct FlowParameters {
    /// `3`, the only version Meta takes (the library writes it when
    /// absent).
    pub flow_message_version: Option<String>,
    /// Your identifier for this Flow's session (Meta's default:
    /// `unused`); kept out of every log.
    pub flow_token: Option<String>,
    /// The Flow, by id (digits): it must be one of the number's WABA's,
    /// else `404 not_found`, nothing sent. Not with `flow_name`.
    pub flow_id: Option<String>,
    /// The Flow, by name, which Meta finds in the number's WABA. Not with
    /// `flow_id`.
    pub flow_name: Option<String>,
    /// The button's text.
    pub flow_cta: String,
    /// `draft` or `published` (Meta's default).
    pub mode: Option<String>,
    /// `navigate` (Meta's default) or `data_exchange`.
    pub flow_action: Option<String>,
    /// The first screen, for `navigate`.
    pub flow_action_payload: Option<FlowActionPayload>,
}

/// A Flow's first screen (`flow_action_payload`).
#[derive(ToSchema)]
#[allow(dead_code)] // schema only
pub struct FlowActionPayload {
    /// The first screen's id.
    pub screen: String,
    /// Its input data: a non-empty object, or one serialized as a string
    /// (Meta's example).
    #[schema(value_type = Option<Value>)]
    pub data: Option<Value>,
}

/// A call button's `action.parameters`
/// (`calling/call-button-messages-deep-links`). Schema only: the request
/// carries Meta's object.
#[derive(ToSchema)]
#[allow(dead_code)] // schema only
pub struct VoiceCallParameters {
    /// The button's label, at most 20 characters (Meta's default: `Call
    /// Now`).
    pub display_text: Option<String>,
    /// How long the button works, 1 to 43,200 minutes (Meta's default:
    /// 10,080, seven days).
    pub ttl_minutes: Option<u32>,
    /// Returned as `cta_payload` in the call's events, at most 512
    /// characters.
    pub payload: Option<String>,
}

/// An address message's `action.parameters` (`messages/address-messages`).
/// Schema only: the request carries Meta's object.
#[derive(ToSchema)]
#[allow(dead_code)] // schema only
pub struct AddressParameters {
    /// `IN`: India only. Required.
    pub country: String,
    /// Values filled in for the user (`name`, `phone_number`,
    /// `in_pin_code`, …).
    pub values: Option<HashMap<String, String>>,
    /// Addresses the user may pick instead.
    pub saved_addresses: Option<Vec<SavedAddress>>,
    /// Errors per field, shown until the user fixes them.
    pub validation_errors: Option<HashMap<String, String>>,
}

/// An address offered to the user.
#[derive(ToSchema)]
#[allow(dead_code)] // schema only
pub struct SavedAddress {
    /// Returned as `saved_address_id` in the reply.
    pub id: String,
    /// Its fields.
    pub value: HashMap<String, String>,
}

/// A catalog message's `action.parameters` (`catalogs/catalog-messages`).
/// Schema only: the request carries Meta's object.
#[derive(ToSchema)]
#[allow(dead_code)] // schema only
pub struct CatalogParameters {
    /// The product whose image is the thumbnail (Meta's default: the
    /// catalog's first).
    pub thumbnail_product_retailer_id: Option<String>,
}

/// An interactive message's `action.parameters`: a URL button's, or
/// another type's object in Meta's JSON (a payment message's, a Flow's, a
/// call button's, an address message's, a catalog message's). Its
/// `Debug` shows an object's keys, never its values (a Flow's
/// `flow_token` reaches no log).
#[derive(Deserialize)]
#[serde(untagged)]
pub enum InteractiveParameters {
    /// `cta_url`'s.
    CtaUrl(CtaUrlParameters),
    /// Any other type's (the name dates from roadmap P2).
    Order(serde_json::Map<String, Value>),
}

impl std::fmt::Debug for InteractiveParameters {
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

/// A `$ref` to the schema `name`, described.
fn described_ref(name: &str, description: &str) -> utoipa::openapi::Ref {
    let mut reference = utoipa::openapi::Ref::from_schema_name(name);
    description.clone_into(&mut reference.description);
    reference
}

/// The schema of `action.parameters`: a URL button's, another type's, or
/// none.
fn interactive_parameters_schema() -> utoipa::openapi::schema::Schema {
    use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder, Schema, Type};
    Schema::AnyOf(
        AnyOfBuilder::new()
            .item(described_ref("CtaUrlParameters", "The URL button."))
            .item(described_ref(
                "OrderParameters",
                "An `order_details` or `order_status` message's parameters.",
            ))
            .item(described_ref(
                "FlowParameters",
                "A `flow` message's parameters.",
            ))
            .item(described_ref(
                "VoiceCallParameters",
                "A `voice_call` message's parameters.",
            ))
            .item(described_ref(
                "AddressParameters",
                "An `address_message`'s parameters.",
            ))
            .item(described_ref(
                "CatalogParameters",
                "A `catalog_message`'s parameters.",
            ))
            .item(ObjectBuilder::new().schema_type(Type::Null))
            .build(),
    )
}

/// A product of a `product_list` section.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ProductItem)]
pub struct ProductItemObject {
    /// The product's retailer id (its SKU) in the catalog.
    pub product_retailer_id: String,
}

/// A `product_list` section (`catalogs/multi-product-messages`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ProductSection)]
pub struct ProductSectionObject {
    /// Title, at most 24 characters; required once there are several
    /// sections.
    pub title: Option<String>,
    /// Its products, 30 at most in all sections.
    pub product_items: Vec<ProductItemObject>,
}

/// A section of `action.sections`: a `list`'s rows, or a `product_list`'s
/// products.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SectionObject {
    /// A `list` section.
    List(ListSectionObject),
    /// A `product_list` section.
    Product(ProductSectionObject),
}

/// The schema of `action.sections`: a list's or a product list's, or none.
fn sections_schema() -> utoipa::openapi::schema::Array {
    use utoipa::openapi::Ref;
    use utoipa::openapi::schema::{AnyOfBuilder, ArrayBuilder, SchemaType, Type};
    ArrayBuilder::new()
        .schema_type(SchemaType::from_iter([Type::Array, Type::Null]))
        .items(
            AnyOfBuilder::new()
                .item(Ref::from_schema_name("ListSection"))
                .item(Ref::from_schema_name("ProductSection")),
        )
        .description(Some("The list's sections, or the product list's."))
        .build()
}

/// A carousel card's kind (Meta's card `type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CarouselCardType {
    /// A media card (Meta's examples name both of its kinds `cta_url`):
    /// an image or video header, a body, and a URL button or quick-reply
    /// buttons.
    CtaUrl,
    /// A product card.
    Product,
}

/// A quick-reply button's `quick_reply`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = QuickReply)]
pub struct QuickReplyObject {
    /// Returned in the reply's webhook, at most 256 characters.
    pub id: String,
    /// Label, at most 20 characters.
    pub title: String,
}

/// A media card's quick-reply button: `{"type": "quick_reply",
/// "quick_reply": {…}}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = QuickReplyButton)]
pub struct QuickReplyButtonObject {
    /// `quick_reply`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The button.
    pub quick_reply: QuickReplyObject,
}

/// A carousel card's `action`: a media card's URL button (`name`
/// `cta_url` and `parameters`) or its quick-reply `buttons`; a product
/// card's `product_retailer_id` and `catalog_id`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CarouselCardAction)]
pub struct CardActionObject {
    /// `cta_url`, for a URL button.
    pub name: Option<String>,
    /// The URL button.
    pub parameters: Option<CtaUrlParameters>,
    /// Quick-reply buttons.
    pub buttons: Option<Vec<QuickReplyButtonObject>>,
    /// A product card's product.
    pub product_retailer_id: Option<String>,
    /// A product card's catalog, the same on every card: one of the
    /// number's WABA's, else `404 not_found`, nothing sent.
    pub catalog_id: Option<String>,
}

/// A carousel card, as Meta writes it.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CarouselCard)]
pub struct CarouselCardObject {
    /// Its position, from `0`: the card's place in the list.
    pub card_index: u32,
    /// Media (`cta_url`) or `product`; the same on every card.
    #[serde(rename = "type")]
    pub kind: CarouselCardType,
    /// A media card's image or video, by `https://` `link`.
    pub header: Option<InteractiveHeader>,
    /// A media card's body, at most 160 characters and 2 line breaks.
    pub body: Option<InteractiveText>,
    /// Its button(s), or its product.
    pub action: CardActionObject,
}

/// An interactive message's `action`: `buttons` for `button`; `button` and
/// `sections` for `list`; `name` (`cta_url`) and `parameters` for
/// `cta_url`; `name` (`review_and_pay`, `review_order`) and `parameters`
/// for `order_details` and `order_status`; `name` (the type's own:
/// `flow`, `voice_call`, `address_message`, `catalog_message`) and
/// `parameters`, or `name` alone (`send_location`, `request_contact_info`,
/// `call_permission_request`); `cards` for `carousel`; `catalog_id` with
/// `product_retailer_id` for `product`, with `sections` for
/// `product_list`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InteractiveAction {
    /// Reply buttons, 1 to 3.
    pub buttons: Option<Vec<ReplyButtonObject>>,
    /// The list's button label.
    pub button: Option<String>,
    /// The list's or the product list's sections.
    #[schema(schema_with = sections_schema)]
    pub sections: Option<Vec<SectionObject>>,
    /// `cta_url`; `review_and_pay` for `order_details`, `review_order`
    /// for `order_status`; the type's own for the others that have one.
    pub name: Option<String>,
    /// The URL button, or another type's parameters.
    #[schema(schema_with = interactive_parameters_schema)]
    pub parameters: Option<InteractiveParameters>,
    /// A carousel's cards, 2 to 10.
    pub cards: Option<Vec<CarouselCardObject>>,
    /// A `product` or `product_list` message's catalog: one of the
    /// number's WABA's, else `404 not_found`, nothing sent.
    pub catalog_id: Option<String>,
    /// A `product` message's product.
    pub product_retailer_id: Option<String>,
}

/// `interactive`, as Meta writes it: `button`, `list`, `cta_url`,
/// `order_details`, `order_status`, `location_request_message`,
/// `request_contact_info`, `call_permission_request`, `voice_call`,
/// `flow`, `address_message`, `carousel`, `product`, `product_list` or
/// `catalog_message`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = InteractiveContent)]
pub struct InteractiveObject {
    /// Which kind.
    #[serde(rename = "type")]
    pub kind: InteractiveType,
    /// Header.
    pub header: Option<InteractiveHeader>,
    /// Body: required by every type but `call_permission_request` and
    /// `product`.
    pub body: Option<InteractiveText>,
    /// Footer.
    pub footer: Option<InteractiveText>,
    /// Buttons, list, URL button, cards, products, or another type's
    /// action.
    pub action: InteractiveAction,
}

/// `pin`'s operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PinType {
    /// Pin the message.
    Pin,
    /// Unpin it.
    Unpin,
}

/// `pin` (`groups/groups-messaging`): pin or unpin a group's message, sent
/// to the group (`to.group_id`); only the group's admin may, and three
/// messages at most are pinned.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = PinContent)]
pub struct PinObject {
    /// `pin` or `unpin`.
    #[serde(rename = "type")]
    pub kind: PinType,
    /// The group message's id (`wamid.…`).
    pub message_id: String,
    /// For `pin` (required), 1 to 30 days; never for `unpin`.
    pub expiration_days: Option<u32>,
}

/// Direct Send's `category` (`direct-send/send-utility-and-authentication-messages`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DirectSendCategory {
    /// Sent as a template Meta generates, outside the 24-hour window too,
    /// at utility rates.
    Utility,
    /// As `utility`, at authentication rates (access restricted by Meta);
    /// never to a BSUID alone.
    Authentication,
    /// A normal service message (the same as none): within the 24-hour
    /// window only.
    Service,
}

/// Direct Send's `direct_send_config` (`direct-send/business-named-templates`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DirectSendConfig {
    /// The template Meta creates or reuses under exactly this name:
    /// `utility` only; lowercase letters, digits and `_`, at most 512.
    pub template_name: String,
}

/// A message to send: `type` names the one content object present.
// Its `Debug` shows `template`'s keys only (a template's Flow button
// carries the Flow's `flow_token`; roadmap M5b).
#[derive(Deserialize, ToSchema)]
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
    /// For `pin`, to a group.
    pub pin: Option<PinObject>,
    /// Reply to this received message (`wamid.…`); not with `reaction`.
    pub reply_to: Option<String>,
    /// Echoed in this message's status events: set it to your own
    /// reference (the `Idempotency-Key`, say) to reconcile.
    pub callback_data: Option<String>,
    /// Direct Send (`direct-send`, for eligible accounts): `utility` or
    /// `authentication` sends the message as a template Meta generates.
    pub category: Option<DirectSendCategory>,
    /// Direct Send's time-to-live in seconds: `utility` 30 to 43,200,
    /// `authentication` 30 to 900; not without one of them.
    pub ttl_seconds: Option<u32>,
    /// Direct Send's business-named template (`utility` only).
    pub direct_send_config: Option<DirectSendConfig>,
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
            .field("template", &domain::KeysOnly(template))
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
    ProductItemObject { product_retailer_id }
    QuickReplyObject { id, title }
}

impl From<ProductSectionObject> for domain::ProductSectionObject {
    fn from(value: ProductSectionObject) -> Self {
        let ProductSectionObject {
            title,
            product_items,
        } = value;
        Self {
            title,
            product_items: each(product_items),
        }
    }
}

impl From<SectionObject> for domain::SectionObject {
    fn from(value: SectionObject) -> Self {
        match value {
            SectionObject::List(section) => Self::List(section.into()),
            SectionObject::Product(section) => Self::Product(section.into()),
        }
    }
}

impl From<CarouselCardType> for domain::CarouselCardType {
    fn from(kind: CarouselCardType) -> Self {
        match kind {
            CarouselCardType::CtaUrl => Self::CtaUrl,
            CarouselCardType::Product => Self::Product,
        }
    }
}

impl From<QuickReplyButtonObject> for domain::QuickReplyButtonObject {
    fn from(value: QuickReplyButtonObject) -> Self {
        let QuickReplyButtonObject { kind, quick_reply } = value;
        Self {
            kind,
            quick_reply: quick_reply.into(),
        }
    }
}

impl From<CardActionObject> for domain::CardActionObject {
    fn from(value: CardActionObject) -> Self {
        let CardActionObject {
            name,
            parameters,
            buttons,
            product_retailer_id,
            catalog_id,
        } = value;
        Self {
            name,
            parameters: parameters.map(Into::into),
            buttons: buttons.map(each),
            product_retailer_id,
            catalog_id,
        }
    }
}

impl From<CarouselCardObject> for domain::CarouselCardObject {
    fn from(value: CarouselCardObject) -> Self {
        let CarouselCardObject {
            card_index,
            kind,
            header,
            body,
            action,
        } = value;
        Self {
            card_index,
            kind: kind.into(),
            header: header.map(Into::into),
            body: body.map(Into::into),
            action: action.into(),
        }
    }
}

impl From<PinType> for domain::PinType {
    fn from(kind: PinType) -> Self {
        match kind {
            PinType::Pin => Self::Pin,
            PinType::Unpin => Self::Unpin,
        }
    }
}

impl From<PinObject> for domain::PinObject {
    fn from(value: PinObject) -> Self {
        let PinObject {
            kind,
            message_id,
            expiration_days,
        } = value;
        Self {
            kind: kind.into(),
            message_id,
            expiration_days,
        }
    }
}

impl From<DirectSendCategory> for domain::DirectSendCategoryObject {
    fn from(category: DirectSendCategory) -> Self {
        match category {
            DirectSendCategory::Utility => Self::Utility,
            DirectSendCategory::Authentication => Self::Authentication,
            DirectSendCategory::Service => Self::Service,
        }
    }
}

impl From<DirectSendConfig> for domain::DirectSendConfigObject {
    fn from(value: DirectSendConfig) -> Self {
        let DirectSendConfig { template_name } = value;
        Self { template_name }
    }
}

impl From<InteractiveParameters> for domain::ActionParameters {
    fn from(value: InteractiveParameters) -> Self {
        match value {
            InteractiveParameters::CtaUrl(parameters) => Self::CtaUrl(parameters.into()),
            InteractiveParameters::Order(object) => Self::Order(object),
        }
    }
}

/// Each of `items`, converted.
pub(crate) fn each<T: Into<U>, U>(items: Vec<T>) -> Vec<U> {
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
            MessageType::Pin => Self::Pin,
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
            InteractiveType::OrderDetails => Self::OrderDetails,
            InteractiveType::OrderStatus => Self::OrderStatus,
            InteractiveType::LocationRequest => Self::LocationRequest,
            InteractiveType::RequestContactInfo => Self::RequestContactInfo,
            InteractiveType::CallPermissionRequest => Self::CallPermissionRequest,
            InteractiveType::VoiceCall => Self::VoiceCall,
            InteractiveType::Flow => Self::Flow,
            InteractiveType::Address => Self::Address,
            InteractiveType::Carousel => Self::Carousel,
            InteractiveType::Product => Self::Product,
            InteractiveType::ProductList => Self::ProductList,
            InteractiveType::Catalog => Self::Catalog,
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
            cards,
            catalog_id,
            product_retailer_id,
        } = value;
        Self {
            buttons: buttons.map(each),
            button,
            sections: sections.map(each),
            name,
            parameters: parameters.map(Into::into),
            cards: cards.map(each),
            catalog_id,
            product_retailer_id,
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
            body: body.map(Into::into),
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
            pin,
            reply_to,
            callback_data,
            category,
            ttl_seconds,
            direct_send_config,
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
            pin: pin.map(Into::into),
            reply_to,
            callback_data,
            category: category.map(Into::into),
            ttl_seconds,
            direct_send_config: direct_send_config.map(Into::into),
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
fn messages<'a>(state: &'a Grant, owned: &'a OwnedNumber) -> Result<Messages<'a>, ApiError> {
    Ok(Messages::new(state.authz(), owned.capability(state))?.observed_by(state.metrics()))
}

/// Send `message` with the number's token: `202` with its id, or the
/// error, with Meta's `details` (the core's [`Messages::send`]).
pub(crate) async fn send(
    state: &Grant,
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
        (status = 404, description = "`not_found`: no such number for this tenant, or a Flow (`flow_id`) or catalog (`catalog_id`) the message names that is not the number's WABA's; nothing was sent", body = ErrorBody),
        (status = 409, description = "`customer_service_window_closed`, `marketing_opted_out`, `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`, …", body = ErrorBody),
        (status = 422, description = "`invalid_request` (with `field`: `to.phone` without `+`, a `template` key the service would not send to Meta, …), `unsupported_message_type`, `idempotency_key_reused`, Meta's `template_*`, …: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests` (the service's limits, `Retry-After`) or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the message may have been sent (`may_have_been_sent: true`), unless the Flow or catalog check before it timed out (`may_have_been_sent: false`)", body = ErrorBody),
    )
)]
pub(crate) async fn send_message(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    uri: Uri,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    // The operation and its input (the number, as the path named it
    // decoded, and the body as it was sent), never the path's spelling.
    // A key claimed before roadmap S6 was stored under the request's
    // method, its path as sent and its body: matched too, for one TTL
    // after the upgrade (the legacy fingerprint goes after the first
    // release). A claim stores the new one.
    let fingerprint = Fingerprint::send_message(owned.phone_number_id(), &body).or_legacy(
        Fingerprint::legacy_v1_json(&Method::POST, uri.path(), &body),
    );
    let message = match parse_message(body) {
        Ok(message) => message,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, send(&state, &owned, &message))
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
    State(state): State<Grant>,
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
        assert!(serde_json::from_value::<MessageType>(json!("order_details")).is_err());
        assert!(serde_json::from_value::<InteractiveType>(json!("nfm_reply")).is_err());
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

    /// A Flow's token never reaches a log (design D103): the request's
    /// `Debug` shows a parameters object's keys, never its values.
    /// Decisive: `InteractiveParameters`' `Debug`.
    #[test]
    fn a_flow_token_never_reaches_the_requests_debug() {
        let token = "SECRET-FLOW-TOKEN-2";
        let request: SendMessage = serde_json::from_value(json!({
            "to": {"phone": "+16505551234"}, "type": "interactive",
            "interactive": {"type": "flow", "body": {"text": "Book"},
                "action": {"name": "flow", "parameters": {"flow_message_version": "3",
                    "flow_token": token, "flow_name": "appointment_booking_v1",
                    "flow_cta": "Book!"}}}
        }))
        .unwrap();
        let shown = format!("{request:?}");
        assert!(!shown.contains(token), "{shown}");
        assert!(shown.contains("flow_token"), "{shown}");
    }

    /// A template's Flow button carries the Flow's token too
    /// (`flows/guides/flows-templates`): the request's raw `template`
    /// shows its keys only, on a send and on an inbox reply (roadmap M5b,
    /// the M5a review's finding). Decisive: `SendMessage`'s and
    /// `ReplyMessage`'s own `Debug` (a derived one prints the object).
    #[test]
    fn a_templates_flow_token_never_reaches_the_requests_debug() {
        let token = "SECRET-TEMPLATE-FLOW-TOKEN-2";
        let template = json!({"name": "book_table", "language": {"code": "en_US"},
            "components": [{"type": "button", "sub_type": "flow", "index": "0",
                "parameters": [{"type": "action", "action": {"flow_token": token}}]}]});
        let send: SendMessage = serde_json::from_value(json!({
            "to": {"phone": "+16505551234"}, "type": "template", "template": template
        }))
        .unwrap();
        let reply: super::super::inbox::ReplyMessage =
            serde_json::from_value(json!({"type": "template", "template": template})).unwrap();
        for shown in [
            format!("{send:?}"),
            format!("{send:#?}"),
            format!("{reply:?}"),
            format!("{reply:#?}"),
        ] {
            assert!(!shown.contains(token), "{shown}");
            assert!(shown.contains("components"), "the keys are shown: {shown}");
        }
    }

    /// A type the service does not send is `unsupported_message_type`;
    /// Direct Send's keys are read since roadmap M5a.
    #[test]
    fn unsupported_types_are_refused() {
        let to = json!({"phone": "+16505551234"});
        for body in [
            json!({"to": to, "type": "order_details"}),
            json!({"to": to, "type": "interactive", "interactive": {"type": "nfm_reply"}}),
            json!({"to": to, "type": "system", "system": {}}),
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
