//! Events over HTTP, as far as every API adapter shares them
//! (docs/design/server.md, sections 4.2, 4.4 and 4.5): the envelope an
//! event is answered in (polled, read by id, streamed, and from M2c
//! delivered), its schemas, the filters' parameters, and the live streams
//! as Server-Sent Events (roadmap M2b), a documented route group an
//! adapter mounts behind its `events` scope ([`stream_routes`]).
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET /v1/events/stream` | the caller's tenant's events, live: `text/event-stream`, after `Last-Event-ID` (or `?after=`), with `?types=&phone_number_id=` |
//! | `GET /v1/numbers/{pn}/events/stream` | the same for one of the tenant's numbers (`404` for another's, before anything is read) |
//!
//! **Frames.** `event: whatsapp`, `id: <sequence>` (the tenant's own),
//! `data: <envelope>`, as `GET /v1/events` answers it; a `: keepalive`
//! comment every 15 s; and the last frame, when the service ends a stream:
//! `event: lagged` (the client took nothing for too long and was dropped,
//! or events after its cursor were purged: reload, or poll from `after`)
//! or `event: shutdown` (the replica is stopping: reconnect, on another),
//! each with `data: {"after": <the last sequence sent>}`. A stream whose
//! key or tenant would no longer be admitted ends with no last frame. A
//! stream without
//! a cursor starts at the tenant's newest sequence; `Last-Event-ID` (what
//! `EventSource` sends when it reconnects) wins over `?after=`. A cursor
//! below what was purged is `410 cursor_expired`, one past the tenant's
//! newest sequence `422`, before the stream starts. The rules are the
//! core's [`meta_whatsapp_server_core::live`]: the outbox is the source of
//! truth, a notification only wakes a stream, the streams are bounded per
//! tenant and replica (`429 too_many_streams`, with `Retry-After`), and a
//! slow client is dropped.

use futures::StreamExt as _;
use meta_whatsapp_rs::core::ids::PhoneNumberId;
// `routes!` names `axum`.
use meta_whatsapp_rs::webhooks::axum;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, Query, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderName, HeaderValue};
use meta_whatsapp_rs::webhooks::axum::response::sse::{Event as SseEvent, Sse};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::events::TENANT_EVENT_TYPES;
use meta_whatsapp_server_core::live::{Frame, LiveStream, StreamQuery};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::common::rfc3339;
use super::ops::API_VERSION;
use crate::auth::Caller;
use crate::error::{ApiError, ErrorBody};
use crate::model::TenantId;
use crate::state::{AppState, Grant};
use crate::store::events::StoredEvent;

/// The target of this module's log lines (the API adapter's events
/// module's before roadmap M2b moved the envelope here).
const LOG_TARGET: &str = "meta_whatsapp_server::api::events";

/// At most this many types in `types`.
pub const MAX_TYPES: usize = 32;

/// `Last-Event-ID`: the last sequence an `EventSource` received, which it
/// sends when it reconnects.
pub static LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");

/// The live streams' routes, each with its documentation (`routes!`), for
/// an API adapter's internal router and OpenAPI document: the adapter puts
/// them behind the tenant guard for scope `events`
/// (`meta_whatsapp_server_core::model::Scope::Events`).
pub fn stream_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(stream_events))
        .routes(routes!(stream_number_events))
}

/// `after`: a sequence, 0 or more, else `422 invalid_request` on `field`.
///
/// # Errors
///
/// Not a sequence.
pub fn parse_after(value: &str, field: &'static str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|after| *after >= 0)
        .ok_or_else(|| ApiError::invalid(field))
}

/// One `types` value (comma-separated `KnownEventType`s) added to
/// `types`, each once, at most [`MAX_TYPES`]; else `422 invalid_request`
/// on `types`.
///
/// # Errors
///
/// A type a tenant never receives, an empty one, too many.
pub fn parse_types(value: &str, types: &mut Vec<String>) -> Result<(), ApiError> {
    for kind in value.split(',').map(str::trim) {
        if !TENANT_EVENT_TYPES.contains(&kind) {
            return Err(ApiError::invalid("types"));
        }
        if !types.iter().any(|t| t == kind) {
            types.push(kind.to_owned());
        }
    }
    if types.len() > MAX_TYPES {
        return Err(ApiError::invalid("types"));
    }
    Ok(())
}

/// `phone_number_id`: the library's numeric id shape (1 to 64 ASCII
/// digits), else `422 invalid_request` on `phone_number_id`.
///
/// # Errors
///
/// Not a phone number id.
pub fn parse_phone_number_id(value: &str) -> Result<PhoneNumberId, ApiError> {
    PhoneNumberId::new(value).map_err(|_| ApiError::invalid("phone_number_id"))
}

/// One event (docs/design/server.md, section 4.4): the same envelope a
/// webhook endpoint (M2) receives.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Event)]
pub struct EventEnvelope {
    /// The event's id (`evt_…`): derived from the event, so the same event
    /// keeps it wherever it is recorded again (until the operator rotates
    /// the Meta app secret it is derived with); deduplicate on it. An
    /// error, or a body that is not a webhook, seen again over an hour
    /// later is a new event, with a new id.
    pub id: String,
    /// Its position among the tenant's events (each tenant has its own
    /// sequence): increasing (with gaps), never reused within one
    /// database's history (a point-in-time restore of the service's
    /// database rolls sequences back: resynchronise and poll without
    /// `after` when the operator says so); order on it.
    pub sequence: i64,
    /// Its type (`EventType`), the `event` tag of `data`.
    #[serde(rename = "type")]
    #[schema(value_type = EventType)]
    pub event_type: String,
    /// The envelope's version: `v1`.
    pub api_version: &'static str,
    /// The tenant it belongs to.
    pub tenant_id: String,
    /// The business phone number, when the event names one.
    #[schema(required = true)]
    pub phone_number_id: Option<String>,
    /// Its WhatsApp Business Account, when the event names one.
    #[schema(required = true)]
    pub waba_id: Option<String>,
    /// When the service received it (RFC 3339).
    #[schema(format = DateTime)]
    pub received_at: String,
    /// Whether `data` was left out for its size (webhook deliveries only;
    /// always `false` here).
    pub truncated: bool,
    /// The event: meta-whatsapp-rs's `WebhookEvent` JSON, an open object
    /// in the document (`event_data_schema`).
    #[schema(schema_with = event_data_schema)]
    pub data: serde_json::Value,
}

/// The schema of an envelope's `data`: an open object, so a generated
/// client reads its fields (a bare `object` becomes `Record<string, never>`
/// in TypeScript).
fn event_data_schema() -> utoipa::openapi::schema::Object {
    use utoipa::openapi::schema::{AdditionalProperties, ObjectBuilder, Type};
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some(
            "The event: meta-whatsapp-rs's WebhookEvent JSON, tagged by `event` (the envelope's \
             `type`). Its fields are Meta's, as the library normalizes them; new fields may \
             appear.",
        ))
        .build()
}

/// The envelope of a stored event of `tenant`.
///
/// # Errors
///
/// `500 internal` for a stored event whose data is not JSON (logged).
pub fn envelope(tenant: &TenantId, event: StoredEvent) -> Result<EventEnvelope, ApiError> {
    let data = serde_json::from_str(&event.data).map_err(|_| {
        tracing::error!(
            target: LOG_TARGET,
            sequence = event.sequence,
            "an outbox event is not JSON"
        );
        ApiError::internal()
    })?;
    Ok(EventEnvelope {
        id: event.id,
        sequence: event.sequence,
        event_type: event.event_type,
        api_version: API_VERSION,
        tenant_id: tenant.as_str().to_owned(),
        phone_number_id: event
            .phone_number_id
            .map(meta_whatsapp_rs::core::ids::PhoneNumberId::into_inner),
        waba_id: event
            .waba_id
            .map(meta_whatsapp_rs::core::ids::WabaId::into_inner),
        received_at: rfc3339(event.created_at),
        truncated: false,
        data,
    })
}

/// The `KnownEventType` schema: the types a tenant receives today.
pub struct KnownEventType;

impl utoipa::PartialSchema for KnownEventType {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .enum_values(Some(TENANT_EVENT_TYPES))
            .description(Some(
                "The event types a tenant receives today (switch on them and let any other \
                 fall to a default).",
            ))
            .into()
    }
}

impl ToSchema for KnownEventType {}

/// The `EventType` schema: a [`KnownEventType`], or any other string, since
/// types grow within `v1`.
pub struct EventType;

impl utoipa::PartialSchema for EventType {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::Ref;
        use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder, Type};
        AnyOfBuilder::new()
            .item(Ref::from_schema_name("KnownEventType"))
            .item(ObjectBuilder::new().schema_type(Type::String))
            .description(Some(
                "An event type: a KnownEventType today. Types only grow within v1: ignore an \
                 unknown one.",
            ))
            .into()
    }
}

impl ToSchema for EventType {}

/// The query of the stream routes, as the OpenAPI document declares it
/// ([`StreamParamsQuery`] reads it, with `Last-Event-ID`).
#[derive(Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StreamParams {
    /// Start after this sequence (`next_after` of a poll, or an event's
    /// `sequence`), when no `Last-Event-ID` header is sent. Omit both to
    /// start with the next event.
    #[param(minimum = 0)]
    pub after: Option<i64>,
    /// Only these types (`KnownEventType`), comma-separated or repeated;
    /// every type when omitted.
    pub types: Option<String>,
    /// Only this business phone number's events (on
    /// `/v1/events/stream`).
    pub phone_number_id: Option<String>,
}

/// The stream routes' query and `Last-Event-ID`, validated: `422
/// invalid_request` on the parameter at fault (`Last-Event-ID` for the
/// header).
#[derive(Debug)]
pub struct StreamParamsQuery(StreamQuery);

impl<S: Send + Sync> FromRequestParts<S> for StreamParamsQuery {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::invalid("query"))?;
        let mut query = StreamQuery {
            after_field: "after",
            ..StreamQuery::default()
        };
        for (name, value) in &pairs {
            match name.as_str() {
                "after" => query.after = Some(parse_after(value, "after")?),
                "types" => parse_types(value, query.types.get_or_insert_with(Vec::new))?,
                "phone_number_id" => {
                    query.phone_number_id = Some(parse_phone_number_id(value)?);
                }
                // Unknown parameters are ignored, as on every list.
                _ => {}
            }
        }
        if let Some(header) = parts.headers.get(&LAST_EVENT_ID) {
            let value = header
                .to_str()
                .map_err(|_| ApiError::invalid("Last-Event-ID"))?;
            query.after = Some(parse_after(value, "Last-Event-ID")?);
            query.after_field = "Last-Event-ID";
        }
        Ok(Self(query))
    }
}

/// `{"after": n}`: what `lagged` and `shutdown` carry.
#[derive(Debug, Serialize)]
struct Ended {
    after: i64,
}

/// A stream's frames as Server-Sent Events. An event whose stored data is
/// not JSON ends the stream with an error (the connection is cut: the
/// client resumes after the last event it got).
fn sse(tenant: TenantId, stream: LiveStream) -> Response {
    let events = stream.map(move |frame| -> Result<SseEvent, StreamCut> {
        Ok(match frame {
            Frame::Event(event) => {
                let sequence = event.sequence;
                let envelope = envelope(&tenant, event).map_err(|_| StreamCut)?;
                SseEvent::default()
                    .event("whatsapp")
                    .id(sequence.to_string())
                    .json_data(&envelope)
                    .map_err(|_| StreamCut)?
            }
            Frame::Heartbeat => SseEvent::default().comment("keepalive"),
            Frame::Lagged { after } => SseEvent::default()
                .event("lagged")
                .json_data(Ended { after })
                .map_err(|_| StreamCut)?,
            Frame::Shutdown { after } => SseEvent::default()
                .event("shutdown")
                .json_data(Ended { after })
                .map_err(|_| StreamCut)?,
        })
    });
    let mut response = Sse::new(events).into_response();
    // Proxies that buffer (nginx) pass the stream on as it comes.
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

/// A stream cut on an event it could not encode (logged by
/// [`envelope`]).
#[derive(Debug, thiserror::Error)]
#[error("the event stream was cut on an event it could not encode")]
struct StreamCut;

/// `GET /v1/events/stream`: the tenant's events, live.
#[utoipa::path(
    get,
    path = "/v1/events/stream",
    tag = "events",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Last-Event-ID" = Option<String>, Header, description = "Resume after this sequence (the `id` of the last `whatsapp` frame received): what `EventSource` sends when it reconnects; wins over `after`"),
        StreamParams,
    ),
    responses(
        (status = 200, description = "Server-Sent Events: `event: whatsapp` frames (`id` the event's sequence, `data` an `Event`), a `: keepalive` comment every 15 s, and, when the service ends the stream, `event: lagged` (dropped as too slow, or events purged before they were sent) or `event: shutdown` (the replica is stopping), each with `data: {\"after\": <the last sequence sent>}`; the stream ends with no last frame once the key or the tenant would no longer be admitted (revoked, expired, suspended, deleted: reconnecting answers `401` or `403`)", content_type = "text/event-stream", body = String),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden` (no `events` scope), `tenant_suspended`", body = ErrorBody),
        (status = 410, description = "`cursor_expired`: events after the cursor were purged (past retention, or deleted with a tenant of the same id); start again without one", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `Last-Event-ID` or `after` (malformed, or past the tenant's newest sequence), `types` (not a `KnownEventType`) or `phone_number_id`", body = ErrorBody),
        (status = 429, description = "`too_many_streams`: the tenant's streams open on this replica (20 by default, `WA_SERVER_STREAMS_PER_TENANT`) or the replica's (1,000, `WA_SERVER_STREAMS_PER_REPLICA`), `Retry-After` 5 s (a place frees when any stream ends); `too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
        (status = 503, description = "`shutting_down`: the replica is stopping, open the stream on another; `storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn stream_events(
    State(state): State<AppState>,
    caller: Caller,
    StreamParamsQuery(query): StreamParamsQuery,
) -> Result<Response, ApiError> {
    let stream = state
        .streams()
        .open_stream(state.authz(), &caller, query)
        .await?;
    Ok(sse(caller.tenant().clone(), stream))
}

/// `GET /v1/numbers/{pn}/events/stream`: one of the tenant's numbers'
/// events, live.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/events/stream",
    tag = "events",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Last-Event-ID" = Option<String>, Header, description = "Resume after this sequence (the `id` of the last `whatsapp` frame received): what `EventSource` sends when it reconnects; wins over `after`"),
        StreamParams,
    ),
    responses(
        (status = 200, description = "Server-Sent Events of the number's events, as `/v1/events/stream` sends them (its `phone_number_id` parameter is the path's)", content_type = "text/event-stream", body = String),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden` (no `events` scope), `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant (nothing read; the number need not be connected)", body = ErrorBody),
        (status = 410, description = "`cursor_expired`: events after the cursor were purged; start again without one", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `Last-Event-ID` or `after` (malformed, or past the tenant's newest sequence) or `types`", body = ErrorBody),
        (status = 429, description = "`too_many_streams`, `too_many_requests`, as for `/v1/events/stream`", body = ErrorBody),
        (status = 503, description = "`shutting_down`, `storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn stream_number_events(
    State(state): State<AppState>,
    caller: Caller,
    Path(pn): Path<String>,
    StreamParamsQuery(query): StreamParamsQuery,
) -> Result<Response, ApiError> {
    // Only a numeric id can be bound (step 4): anything else does not exist.
    let pn = PhoneNumberId::new(pn).map_err(|_| ApiError::not_found())?;
    let stream = state
        .streams()
        .open_number_stream(state.authz(), &caller, pn, query)
        .await?;
    Ok(sse(caller.tenant().clone(), stream))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use super::*;

    #[test]
    fn the_filters_are_validated_parameter_by_parameter() {
        assert_eq!(parse_after("17", "after").unwrap(), 17);
        for bad in ["-1", "x", "", " 17"] {
            let error = parse_after(bad, "Last-Event-ID").unwrap_err();
            assert_eq!(error.body().error.field.as_deref(), Some("Last-Event-ID"));
        }
        let mut types = Vec::new();
        parse_types("message_received, status_updated", &mut types).unwrap();
        parse_types("message_received", &mut types).unwrap();
        assert_eq!(types, ["message_received", "status_updated"]);
        for bad in ["unknown", "unparsed", "message_received,"] {
            assert!(parse_types(bad, &mut Vec::new()).is_err(), "{bad}");
        }
        assert!(parse_phone_number_id("+15550783881").is_err());
        assert!(parse_phone_number_id("106540352242922").is_ok());
    }
}
