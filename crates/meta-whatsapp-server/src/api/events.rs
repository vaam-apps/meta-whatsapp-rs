//! Events (scope `events`; docs/design/server.md, sections 2.3 and 4.2):
//! polling the outbox.
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET /v1/events` | the caller's tenant's events after a sequence of its own, `?after=&types=&phone_number_id=&limit=`; `410 cursor_expired` past retention |
//!
//! Operator-only events (no tenant) are never answered. The logic is
//! [`crate::events::poll`]; this module parses the query and shapes the
//! envelope.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Query, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use serde::Serialize;
use utoipa::ToSchema;

use super::common::rfc3339;
use super::ops::API_VERSION;
use crate::auth::Caller;
use crate::error::{ApiError, ErrorBody};
use crate::events::{MAX_PAGE_DATA_BYTES, TENANT_EVENT_TYPES, poll};
use crate::model::{MAX_PAGE_SIZE, TenantId};
use crate::state::AppState;
use crate::store::events::{EventQuery, StoredEvent};

/// Default page size of `GET /v1/events`.
pub const DEFAULT_EVENTS_LIMIT: usize = 50;

/// At most this many types in `types`.
pub const MAX_TYPES: usize = 32;

/// The query of `GET /v1/events`, as the OpenAPI document declares it
/// ([`EventsQuery`] reads it).
#[derive(Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsParams {
    /// Events after this sequence: `next_after` of the previous answer.
    /// Omit it to start at the oldest event still kept.
    #[param(minimum = 0)]
    pub after: Option<i64>,
    /// Only these types (`KnownEventType`), comma-separated
    /// (`message_received,status_updated`); every type when omitted.
    pub types: Option<String>,
    /// Only this business phone number's events.
    pub phone_number_id: Option<String>,
    /// At most this many events.
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub limit: Option<u32>,
}

/// `?after=&types=&phone_number_id=&limit=`, validated: `422
/// invalid_request` on the parameter at fault.
#[derive(Debug)]
pub struct EventsQuery {
    after: Option<i64>,
    types: Option<Vec<String>>,
    phone_number_id: Option<String>,
    limit: usize,
}

impl EventsQuery {
    /// The store query for `tenant`.
    fn for_tenant(self, tenant: TenantId) -> EventQuery {
        EventQuery {
            tenant,
            after: self.after,
            types: self.types,
            phone_number_id: self.phone_number_id,
            limit: self.limit,
            max_bytes: MAX_PAGE_DATA_BYTES,
        }
    }
}

/// Parse the query's pairs (`types` may also be repeated).
fn parse_query(pairs: &[(String, String)]) -> Result<EventsQuery, ApiError> {
    let mut query = EventsQuery {
        after: None,
        types: None,
        phone_number_id: None,
        limit: DEFAULT_EVENTS_LIMIT,
    };
    for (name, value) in pairs {
        match name.as_str() {
            "after" => {
                let after = value
                    .parse::<i64>()
                    .ok()
                    .filter(|a| *a >= 0)
                    .ok_or_else(|| ApiError::invalid("after"))?;
                query.after = Some(after);
            }
            "types" => {
                let types = query.types.get_or_insert_with(Vec::new);
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
            }
            "phone_number_id" => {
                let valid = !value.is_empty()
                    && value.len() <= 64
                    && value.bytes().all(|b| b.is_ascii_digit());
                if !valid {
                    return Err(ApiError::invalid("phone_number_id"));
                }
                query.phone_number_id = Some(value.clone());
            }
            "limit" => {
                query.limit = value
                    .parse::<usize>()
                    .ok()
                    .filter(|l| (1..=MAX_PAGE_SIZE).contains(l))
                    .ok_or_else(|| ApiError::invalid("limit"))?;
            }
            // Unknown parameters are ignored, as on every list.
            _ => {}
        }
    }
    Ok(query)
}

impl<S: Send + Sync> FromRequestParts<S> for EventsQuery {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::invalid("query"))?;
        parse_query(&pairs)
    }
}

/// One event (docs/design/server.md, section 4.4): the same envelope a
/// webhook endpoint (M2) receives.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Event)]
pub struct EventEnvelope {
    /// The event's id (`evt_…`): unique, stable; deduplicate on it.
    pub id: String,
    /// Its position among the tenant's events (each tenant has its own
    /// sequence): increasing, never reused; order on it.
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

/// A page of events.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventList {
    /// The events, in sequence order.
    pub data: Vec<EventEnvelope>,
    /// Pass as `after` to continue: the last event's sequence when more
    /// follow, else the tenant's newest sequence (so a poll never starts
    /// again from an old cursor).
    pub next_after: i64,
}

/// The envelope of a stored event of `tenant`.
fn envelope(tenant: &TenantId, event: StoredEvent) -> Result<EventEnvelope, ApiError> {
    let data = serde_json::from_str(&event.data).map_err(|_| {
        tracing::error!(sequence = event.sequence, "an outbox event is not JSON");
        ApiError::internal()
    })?;
    Ok(EventEnvelope {
        id: event.id,
        sequence: event.sequence,
        event_type: event.event_type,
        api_version: API_VERSION,
        tenant_id: tenant.as_str().to_owned(),
        phone_number_id: event.phone_number_id,
        waba_id: event.waba_id,
        received_at: rfc3339(event.created_at),
        truncated: false,
        data,
    })
}

/// `GET /v1/events`: the tenant's events after a sequence.
#[utoipa::path(
    get,
    path = "/v1/events",
    tag = "events",
    security(("api_key" = [])),
    params(
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        EventsParams,
    ),
    responses(
        (status = 200, description = "The tenant's events after `after`", body = EventList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden` (no `events` scope), `tenant_suspended`", body = ErrorBody),
        (status = 410, description = "`cursor_expired`: events after `after` were purged (past retention, or deleted with a tenant of the same id); start again without `after`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `after` (malformed, or past the tenant's newest sequence), `types` (not a `KnownEventType`), `phone_number_id` or `limit`", body = ErrorBody),
    )
)]
pub async fn list_events(
    State(state): State<AppState>,
    caller: Caller,
    query: EventsQuery,
) -> Result<Json<EventList>, ApiError> {
    let tenant = caller.tenant().clone();
    let polled = poll(state.events().outbox(), &query.for_tenant(tenant.clone())).await?;
    let data = polled
        .events
        .into_iter()
        .map(|event| envelope(&tenant, event))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(EventList {
        data,
        next_after: polled.next_after,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(query: &[(&str, &str)]) -> Vec<(String, String)> {
        query
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn field(query: &[(&str, &str)]) -> Option<String> {
        parse_query(&pairs(query))
            .err()
            .map(|e| e.body().error.field.unwrap_or_default())
    }

    #[test]
    fn the_query_is_validated_parameter_by_parameter() {
        let ok = parse_query(&pairs(&[
            ("after", "17"),
            ("types", "message_received, status_updated"),
            ("types", "message_received"),
            ("phone_number_id", "106540352242922"),
            ("limit", "100"),
            ("unknown", "ignored"),
        ]))
        .unwrap();
        assert_eq!(ok.after, Some(17));
        assert_eq!(
            ok.types.as_deref(),
            Some(&["message_received".to_owned(), "status_updated".to_owned()][..])
        );
        assert_eq!(ok.phone_number_id.as_deref(), Some("106540352242922"));
        assert_eq!(ok.limit, 100);
        let default = parse_query(&[]).unwrap();
        assert_eq!(
            (default.after, default.types, default.limit),
            (None, None, 50)
        );
        for (query, at) in [
            (&[("after", "-1")][..], "after"),
            (&[("after", "x")][..], "after"),
            (&[("types", "unknown")][..], "types"),
            (&[("types", "unparsed")][..], "types"),
            (&[("types", "partner_solution_updated")][..], "types"),
            (&[("types", "message_received,")][..], "types"),
            (
                &[("phone_number_id", "+15550783881")][..],
                "phone_number_id",
            ),
            (&[("phone_number_id", "")][..], "phone_number_id"),
            (&[("limit", "0")][..], "limit"),
            (&[("limit", "101")][..], "limit"),
        ] {
            assert_eq!(field(query).as_deref(), Some(at), "{query:?}");
        }
    }
}
