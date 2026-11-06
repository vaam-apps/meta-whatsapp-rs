//! The inbox (scope `inbox`; docs/design/server.md, section 4.2; roadmaps
//! M2a and M2g): a number's conversations, one conversation's history, its
//! window, a reply to its contact, marking it read, and a message by id.
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET /v1/numbers/{pn}/conversations` | the number's conversations, newest activity first, each with its window, `?limit=&cursor=` |
//! | `GET /v1/numbers/{pn}/conversations/{contact}/messages` | one conversation's history, newest first, `?limit=&cursor=` |
//! | `GET /v1/numbers/{pn}/conversations/{contact}/window` | the conversation's 24-hour window, as a free-form reply would meet it now |
//! | `POST /v1/numbers/{pn}/conversations/{contact}/messages` | a reply to the conversation's contact, with the tenant's WABA token: `POST /{pn}/messages` (`messages/send-messages`), after the window and the thread's owner are checked |
//! | `POST /v1/numbers/{pn}/conversations/{contact}/read` | the conversation's unread count reset; with `notify_customer`, its latest inbound message marked read on Meta first, with the tenant's WABA token: `POST /{pn}/messages` (`messages/mark-message-as-read`) |
//! | `GET /v1/numbers/{pn}/messages/{message_id}` | a message of the number's inbox, by id, with its conversation's contact |
//!
//! The rules are the core's ([`meta_whatsapp_server_core::inbox`]: the
//! `Conversations` service on an owned number): what a page shows of a
//! number that moved from another tenant (nothing dated before its
//! binding: decision D10), the cursors, the checks a reply makes per
//! number (`WA_SERVER_INBOX_REPLY_CHECKS`). This module keeps the routes,
//! the request and answer types with their OpenAPI schema, converts them
//! to and from the core's (every field named on both sides), and renders
//! the answers. A reply takes an `Idempotency-Key`, as a send does, and so
//! does marking a conversation read (its receipt is a request to Meta).

use meta_whatsapp_rs::core::store::{ConversationSummary, Direction, StoredMessage};
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::inbox::{Conversations, ListedConversation, ReplyWindow};
use meta_whatsapp_server_core::messages::{self as domain};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, PageParams, PageQuery, encode_cursor, rfc3339};
use super::messages::{
    AudioObject, ContactCard, DirectSendCategory, DirectSendConfig, DocumentObject, ImageObject,
    InteractiveObject, LocationObject, MessageAccepted, MessageType, OptionalJson, PinObject,
    ReactionObject, StickerObject, TextObject, VideoObject, each,
};
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::model::Listing;
use crate::state::Grant;

// ─── The answers ─────────────────────────────────────────────────────────

/// A conversation of the number: one contact.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Conversation)]
pub struct ConversationView {
    /// The contact: the customer's BSUID (`US.13491208655302741918`), else
    /// their `wa_id` (digits, no `+`), else a group's id. Percent-encode it
    /// in a path.
    pub contact: String,
    /// When its latest message was sent or received (RFC 3339).
    #[schema(format = DateTime)]
    pub last_message_at: String,
    /// When the customer last wrote (RFC 3339): what opens the 24-hour
    /// window, with the calls that reopen it. `null` when they never did
    /// since the number's binding began.
    #[schema(required = true, format = DateTime)]
    pub last_inbound_at: Option<String>,
    /// A text preview of the latest message, when it has one.
    #[schema(required = true)]
    pub last_text: Option<String>,
    /// The customer's messages since the conversation was last read.
    pub unread: u64,
    /// Its 24-hour window, as `GET …/{contact}/window` answers it.
    pub window: WindowView,
}

impl From<ListedConversation> for ConversationView {
    fn from(listed: ListedConversation) -> Self {
        let ListedConversation { summary, window } = listed;
        let ConversationSummary {
            key,
            last_message_at,
            last_inbound_at,
            last_text,
            unread,
        } = summary;
        Self {
            contact: key.contact,
            last_message_at: rfc3339(last_message_at),
            last_inbound_at: last_inbound_at.map(rfc3339),
            last_text,
            unread,
            window: window.into(),
        }
    }
}

/// A conversation's 24-hour window, as a free-form reply would meet it now.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ConversationWindow)]
pub struct WindowView {
    /// Whether a free-form reply would be sent to Meta now: the checks a
    /// reply makes (the window, which counts the customer's calls and their
    /// messages seen in standby; the thread's owner under Conversation
    /// Routing; on a number that moved to this tenant, a window opened
    /// since), with nothing sent. `false`: send a template. Meta still
    /// decides.
    pub open: bool,
    /// When the window the customer opened closes, or closed (RFC 3339),
    /// as the inbox recorded it; `null` when they opened none since the
    /// number's binding began (templates only). It may be past while
    /// `open` is true (a handover to this app, or the window check turned
    /// off for the number), or future while it is false (another app owns
    /// the thread).
    #[schema(required = true, format = DateTime)]
    pub closes_at: Option<String>,
}

impl From<ReplyWindow> for WindowView {
    fn from(window: ReplyWindow) -> Self {
        let ReplyWindow { open, closes_at } = window;
        Self {
            open,
            closes_at: closes_at.map(rfc3339),
        }
    }
}

/// A page of conversations.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationList {
    /// The conversations, most recent activity first.
    pub data: Vec<ConversationView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// Which way a message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = MessageDirection)]
pub enum DirectionName {
    /// From the customer to the business.
    Inbound,
    /// From the business to the customer.
    Outbound,
}

impl From<Direction> for DirectionName {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::Inbound => Self::Inbound,
            Direction::Outbound => Self::Outbound,
        }
    }
}

/// A message of a conversation, as the inbox recorded it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = InboxMessage)]
pub struct InboxMessageView {
    /// The message's id (`wamid.…`): the key of its status events.
    pub id: String,
    /// Which way it went.
    pub direction: DirectionName,
    /// Its type as Meta names it (`text`, `image`, `template`, …), or one
    /// of the library's own: `revoked` (deleted before it was recorded),
    /// `erased`, `media_placeholder` (a coexistence history's media, not
    /// yet filled in).
    pub kind: String,
    /// A plain-text rendering, when there is one.
    #[schema(required = true)]
    pub text: Option<String>,
    /// The message as Meta sent it (the webhook's message object) or as it
    /// was sent (the request body Meta received).
    #[schema(schema_with = payload_schema)]
    pub payload: Value,
    /// Where it stands: `received` (inbound), `accepted` (Meta accepted
    /// the send, no status yet), then `sent`, `delivered`, `read`,
    /// `played`, `failed` or `deleted`, as status events arrive. A value
    /// not listed here may appear: treat it as unknown.
    pub status: String,
    /// When it was sent or received (RFC 3339): Meta's time when known.
    #[schema(format = DateTime)]
    pub timestamp: String,
    /// When its status last changed (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub status_at: Option<String>,
    /// The error object of a `failed` status, as Meta sent it.
    #[schema(required = true, schema_with = error_schema)]
    pub error: Option<Value>,
}

impl From<StoredMessage> for InboxMessageView {
    fn from(message: StoredMessage) -> Self {
        let StoredMessage {
            id,
            conversation: _,
            direction,
            kind,
            text,
            payload,
            status,
            timestamp,
            status_at,
            error,
        } = message;
        Self {
            id: id.into_inner(),
            direction: direction.into(),
            kind,
            text,
            payload,
            // The library's own names (`snake_case`), the ones the store
            // keeps; an enum it adds is a string here too.
            status: serde_json::to_value(status)
                .ok()
                .and_then(|status| status.as_str().map(str::to_owned))
                .unwrap_or_default(),
            timestamp: rfc3339(timestamp),
            status_at: status_at.map(rfc3339),
            error,
        }
    }
}

/// A page of a conversation's history.
#[derive(Debug, Serialize, ToSchema)]
pub struct InboxMessageList {
    /// The messages, newest first.
    pub data: Vec<InboxMessageView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A message of the number's inbox, found by id, with its conversation.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = NumberInboxMessage)]
pub struct NumberMessageView {
    /// Its conversation's `contact` (a BSUID, a `wa_id` or a group id):
    /// where its history is.
    pub contact: String,
    /// The message, as the history answers it.
    pub message: InboxMessageView,
}

impl From<StoredMessage> for NumberMessageView {
    fn from(message: StoredMessage) -> Self {
        Self {
            contact: message.conversation.contact.clone(),
            message: message.into(),
        }
    }
}

/// The schema of a message's `payload`: an open object, so a generated
/// client reads its fields (a bare `object` becomes `Record<string,
/// never>` in TypeScript).
fn payload_schema() -> utoipa::openapi::schema::Object {
    use utoipa::openapi::schema::{AdditionalProperties, ObjectBuilder, Type};
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some(
            "The message as Meta sent it (the webhook's message object) or as it was sent (the \
             request body Meta received). Its fields are Meta's; new fields may appear.",
        ))
        .build()
}

/// The schema of a message's `error`: an open object, or `null`.
fn error_schema() -> utoipa::openapi::schema::Object {
    use utoipa::openapi::schema::{AdditionalProperties, ObjectBuilder, SchemaType, Type};
    ObjectBuilder::new()
        .schema_type(SchemaType::from_iter([Type::Object, Type::Null]))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some(
            "The error object of a `failed` status, as Meta sent it (`code`, `title`, …); \
             `null` otherwise.",
        ))
        .build()
}

/// The `next_cursor` of a listing the core answered.
fn next_cursor<T>(listing: &Listing<T>) -> Option<String> {
    listing.next_after.as_deref().map(encode_cursor)
}

// ─── The request ─────────────────────────────────────────────────────────

/// A reply to a conversation: a message's content as `POST
/// /v1/numbers/{pn}/messages` takes it, without `to` (the conversation's
/// contact receives it). `type` names the one content object present.
// Its `Debug` shows `template`'s keys only, as a send's does.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyMessage {
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
    /// For `pin`, in a group's conversation.
    pub pin: Option<PinObject>,
    /// Reply to this message of the conversation (`wamid.…`); not with
    /// `reaction`.
    pub reply_to: Option<String>,
    /// Echoed in this message's status events: set it to your own
    /// reference (the `Idempotency-Key`, say) to reconcile.
    pub callback_data: Option<String>,
    /// Direct Send (`direct-send`, for eligible accounts): `utility` and
    /// `authentication` are sent as templates Meta generates, so the
    /// 24-hour window does not hold them; `service` is a free-form reply.
    pub category: Option<DirectSendCategory>,
    /// Direct Send's time-to-live in seconds.
    pub ttl_seconds: Option<u32>,
    /// Direct Send's business-named template (`utility` only).
    pub direct_send_config: Option<DirectSendConfig>,
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

impl From<ReplyMessage> for domain::ReplyMessage {
    fn from(value: ReplyMessage) -> Self {
        let ReplyMessage {
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

/// `POST /v1/numbers/{pn}/conversations/{contact}/read`: an empty body, or
/// this object.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = MarkConversationRead)]
pub struct MarkConversationReadRequest {
    /// Also mark the conversation's latest inbound message read on Meta
    /// (the blue ticks the customer sees), with one request. None is sent
    /// for a message older than 30 days (Meta's limit) or dated before the
    /// number's binding began.
    #[serde(default)]
    pub notify_customer: bool,
}

/// The reply a request body asks for, in the core's order: a type the
/// service does not send ([`domain::refuse_unsupported`]), then the body
/// read into [`ReplyMessage`] (`422 invalid_request` on `body`, unknown
/// fields included, `to` among them).
fn parse_reply(body: Value) -> Result<domain::ReplyMessage, ApiError> {
    domain::refuse_unsupported(&body)?;
    let request: ReplyMessage =
        serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))?;
    Ok(request.into())
}

// ─── The routes ──────────────────────────────────────────────────────────

/// The core's inbox operations on `owned`, with the number's reply checks,
/// counted in `state`'s metrics (`owned` is the state's own: the extractor
/// made it with `state.authz()`, which admits it).
fn conversations<'a>(
    state: &'a Grant,
    owned: &'a OwnedNumber,
) -> Result<Conversations<'a>, ApiError> {
    let checks = state
        .settings()
        .inbox_reply_checks
        .of(owned.phone_number_id());
    Ok(Conversations::new(
        state.authz(),
        owned.capability(state),
        state.conversations(),
        checks,
        state.clock(),
    )?
    .observed_by(state.metrics())
    .naming_the_account(state.settings().send_messaging_account_id))
}

/// `GET /v1/numbers/{pn}/conversations`: the number's conversations.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/conversations",
    tag = "inbox",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PageParams,
    ),
    responses(
        (status = 200, description = "The conversations, most recent activity first; of a number whose WABA moved from another tenant, only what is dated from its binding's start on", body = ConversationList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`limit`, `cursor`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: retry", body = ErrorBody),
    )
)]
pub(crate) async fn list_conversations(
    State(state): State<Grant>,
    owned: OwnedNumber,
    PageQuery(page): PageQuery,
) -> Result<Json<ConversationList>, ApiError> {
    let listing = conversations(&state, &owned)?
        .list_with_windows(&page)
        .await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(ConversationList {
        data: listing.items.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

/// `GET /v1/numbers/{pn}/conversations/{contact}/messages`: a
/// conversation's history.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/conversations/{contact}/messages",
    tag = "inbox",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("contact" = String, Path, description = "The conversation's `contact` (a BSUID, a `wa_id` or a group id), percent-encoded"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PageParams,
    ),
    responses(
        (status = 200, description = "The messages, newest first; of a number whose WABA moved from another tenant, only those dated from its binding's start on. A contact with no conversation has an empty history", body = InboxMessageList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`contact`, `limit`, `cursor`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: retry", body = ErrorBody),
    )
)]
pub(crate) async fn list_conversation_messages(
    State(state): State<Grant>,
    owned: OwnedNumber,
    Path((_, contact)): Path<(String, String)>,
    PageQuery(page): PageQuery,
) -> Result<Json<InboxMessageList>, ApiError> {
    let listing = conversations(&state, &owned)?
        .history(&contact, &page)
        .await?;
    let next_cursor = next_cursor(&listing);
    Ok(Json(InboxMessageList {
        data: listing.items.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

/// Reply `request` to `contact` with the number's token: `202` with the
/// accepted message, or the error, with Meta's `details` (the core's
/// [`Conversations::reply`]).
async fn reply(
    state: &Grant,
    owned: &OwnedNumber,
    contact: &str,
    request: domain::ReplyMessage,
) -> Result<Success, ApiError> {
    let accepted = conversations(state, owned)?.reply(contact, request).await?;
    Success::json(StatusCode::ACCEPTED, &MessageAccepted::from(accepted))
}

/// `POST /v1/numbers/{pn}/conversations/{contact}/messages`: reply to the
/// conversation's contact.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/conversations/{contact}/messages",
    tag = "inbox",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("contact" = String, Path, description = "The conversation's `contact` (a BSUID, a `wa_id` or a group id), percent-encoded: the reply goes to it"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never send twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = ReplyMessage,
    responses(
        (status = 202, description = "Accepted by Meta and recorded `accepted` in the conversation; delivery arrives as status events", body = MessageAccepted),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or a Flow (`flow_id`) or catalog (`catalog_id`) the reply names that is not the number's WABA's; nothing was sent", body = ErrorBody),
        (status = 409, description = "`customer_service_window_closed` (a free-form reply outside the 24-hour window: send a template) or `thread_owned_elsewhere` (another app owns the thread), both refused before any request; `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`, …", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`contact`, `body`, a field of the content), `unsupported_message_type`, `idempotency_key_reused`, Meta's `template_*`, …: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests` (the service's limits, `Retry-After`) or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the reply may have been sent (`may_have_been_sent: true`), unless the Flow or catalog check before it timed out (`may_have_been_sent: false`)", body = ErrorBody),
    )
)]
pub(crate) async fn reply_to_conversation(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    Path((_, contact)): Path<(String, String)>,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    // The operation and its input (the number and the contact, as the
    // path named them decoded, and the body as it was sent), never the
    // path's spelling.
    let fingerprint = Fingerprint::reply_to_conversation(owned.phone_number_id(), &contact, &body);
    let request = match parse_reply(body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    // Boxed: the library's inbox checks and records around the send, which
    // makes the future large (clippy's `large_futures`).
    state
        .idempotent(
            &caller,
            key,
            fingerprint,
            Box::pin(reply(&state, &owned, &contact, request)),
        )
        .await
}

/// `GET /v1/numbers/{pn}/conversations/{contact}/window`: the
/// conversation's 24-hour window.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/conversations/{contact}/window",
    tag = "inbox",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("contact" = String, Path, description = "The conversation's `contact` (a BSUID, a `wa_id` or a group id), percent-encoded"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The window, as a free-form reply would meet it now (nothing is sent); of a number whose WABA moved from another tenant, only a window opened since its binding's start. A contact with no conversation has a closed one", body = WindowView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`contact`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: retry", body = ErrorBody),
    )
)]
pub(crate) async fn get_conversation_window(
    State(state): State<Grant>,
    owned: OwnedNumber,
    Path((_, contact)): Path<(String, String)>,
) -> Result<Json<WindowView>, ApiError> {
    let window = conversations(&state, &owned)?.window(&contact).await?;
    Ok(Json(window.into()))
}

/// Mark `contact`'s conversation read: `204`, or the error, with Meta's
/// `details` (the core's [`Conversations::mark_read`]).
async fn mark_read(
    state: &Grant,
    owned: &OwnedNumber,
    contact: &str,
    notify_customer: bool,
) -> Result<Success, ApiError> {
    conversations(state, owned)?
        .mark_read(contact, notify_customer)
        .await?;
    Ok(Success::no_content())
}

/// `POST /v1/numbers/{pn}/conversations/{contact}/read`: the merchant read
/// the conversation.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/conversations/{contact}/read",
    tag = "inbox",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("contact" = String, Path, description = "The conversation's `contact` (a BSUID, a `wa_id` or a group id), percent-encoded"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body send no second read receipt once answered (a repeat gets the kept answer, with `Idempotent-Replayed: true`; a `503` releases the key)"),
    ),
    request_body(content = Option<MarkConversationReadRequest>, description = "Optional; an empty body resets the unread count only"),
    responses(
        (status = 204, description = "The unread count is 0; with `notify_customer`, Meta marked the latest inbound message read first (none is sent for a message older than 30 days, or dated before the number's binding began)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`contact`, `body`), `idempotency_key_reused`, or Meta's `invalid_parameter`: the unread count is unchanged", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: the unread count is unchanged", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: retry (also when the number moved while Meta answered the receipt: the unread count is unchanged)", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn mark_conversation_read(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    Path((_, contact)): Path<(String, String)>,
    OptionalJson(body): OptionalJson<Value>,
) -> Response {
    // The operation and its input (the number and the contact, as the
    // path named them decoded, and the body as it was sent, `null` when
    // empty), never the path's spelling.
    let fingerprint = Fingerprint::mark_conversation_read(owned.phone_number_id(), &contact, &body);
    let request = match parse_mark_read(body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    // Boxed: the library's inbox reads around the receipt make the future
    // large (clippy's `large_futures`), as a reply's.
    state
        .idempotent(
            &caller,
            key,
            fingerprint,
            Box::pin(mark_read(&state, &owned, &contact, request.notify_customer)),
        )
        .await
}

/// The request an optional body asks for: none (`null`) is the default,
/// anything else must be [`MarkConversationReadRequest`] (`422
/// invalid_request` on `body`, unknown fields included).
fn parse_mark_read(body: Value) -> Result<MarkConversationReadRequest, ApiError> {
    if body.is_null() {
        return Ok(MarkConversationReadRequest::default());
    }
    serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))
}

/// `GET /v1/numbers/{pn}/messages/{message_id}`: a message of the
/// number's inbox.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/messages/{message_id}",
    tag = "inbox",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("message_id" = String, Path, description = "A message's id (`wamid.…`), sent or received on the number, percent-encoded"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The message, as the inbox recorded it, and its conversation's contact", body = NumberMessageView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such message of the number (of a number whose WABA moved from another tenant, none dated before its binding's start)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`message_id`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: retry", body = ErrorBody),
    )
)]
pub(crate) async fn get_inbox_message(
    State(state): State<Grant>,
    owned: OwnedNumber,
    Path((_, message_id)): Path<(String, String)>,
) -> Result<Json<NumberMessageView>, ApiError> {
    let message = conversations(&state, &owned)?.message(&message_id).await?;
    Ok(Json(message.into()))
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::MessageId;
    use meta_whatsapp_rs::core::store::{ConversationKey, DeliveryStatus};
    use serde_json::json;
    use time::macros::datetime;

    use super::*;

    /// A reply reads every content field the core's request has, and no
    /// `to`: the conversation names its contact.
    #[test]
    fn a_reply_carries_every_field_but_the_recipient() {
        let reply = parse_reply(json!({
            "type": "text",
            "text": {"body": "On its way", "preview_url": true},
            "reply_to": "wamid.HBgL",
            "callback_data": "order:1234"
        }))
        .unwrap();
        let mut expected = domain::ReplyMessage::new(domain::MessageType::Text);
        expected.text = Some(domain::TextObject {
            body: "On its way".to_owned(),
            preview_url: Some(true),
        });
        expected.reply_to = Some("wamid.HBgL".to_owned());
        expected.callback_data = Some("order:1234".to_owned());
        assert_eq!(reply, expected);
        let to = parse_reply(json!({
            "to": {"phone": "+16505551234"},
            "type": "text",
            "text": {"body": "hi"}
        }))
        .unwrap_err();
        assert_eq!(to.field(), Some("body"));
        // Direct Send rides on a reply (roadmap M5a), with Meta's three
        // categories only.
        let direct_send = parse_reply(json!({
            "type": "text", "text": {"body": "Your order shipped"},
            "category": "utility", "ttl_seconds": 600,
            "direct_send_config": {"template_name": "order_shipped"}
        }))
        .unwrap();
        assert_eq!(
            direct_send.category,
            Some(domain::DirectSendCategoryObject::Utility)
        );
        assert_eq!(direct_send.ttl_seconds, Some(600));
        assert_eq!(
            direct_send.direct_send_config,
            Some(domain::DirectSendConfigObject {
                template_name: "order_shipped".to_owned()
            })
        );
        let marketing = parse_reply(json!({"type": "text", "text": {"body": "x"},
                                           "category": "marketing"}))
        .unwrap_err();
        assert_eq!(marketing.field(), Some("body"));
        let unsupported = parse_reply(json!({"type": "order_details"})).unwrap_err();
        assert_eq!(unsupported.code(), "unsupported_message_type");
    }

    /// Every field of a stored message is answered, its status by the
    /// library's name.
    #[test]
    fn every_field_of_a_stored_message_is_answered() {
        let at = datetime!(2026-09-24 10:00:01 UTC);
        let view = InboxMessageView::from(StoredMessage {
            id: MessageId::new("wamid.1"),
            conversation: ConversationKey::new(
                meta_whatsapp_rs::core::ids::PhoneNumberId::new("1").unwrap(),
                "US.1",
            ),
            direction: Direction::Outbound,
            kind: "text".to_owned(),
            text: Some("hi".to_owned()),
            payload: json!({"text": {"body": "hi"}}),
            status: DeliveryStatus::Delivered,
            timestamp: at,
            status_at: Some(at),
            error: Some(json!({"code": 131026})),
        });
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            json!({
                "id": "wamid.1",
                "direction": "outbound",
                "kind": "text",
                "text": "hi",
                "payload": {"text": {"body": "hi"}},
                "status": "delivered",
                "timestamp": "2026-09-24T10:00:01Z",
                "status_at": "2026-09-24T10:00:01Z",
                "error": {"code": 131026}
            })
        );
        for (status, name) in [
            (DeliveryStatus::Received, "received"),
            (DeliveryStatus::Accepted, "accepted"),
            (DeliveryStatus::Sent, "sent"),
            (DeliveryStatus::Read, "read"),
            (DeliveryStatus::Played, "played"),
            (DeliveryStatus::Failed, "failed"),
            (DeliveryStatus::Deleted, "deleted"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), json!(name));
        }
    }
}
