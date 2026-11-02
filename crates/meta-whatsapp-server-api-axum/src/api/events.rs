//! Events (scope `events`; docs/design/server.md, sections 2.3, 4.2 and
//! 4.5): polling the outbox, one event by id, and the live streams.
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET /v1/events` | the caller's tenant's events after a sequence of its own, `?after=&types=&phone_number_id=&limit=`; `410 cursor_expired` past retention, or past a tenant deleted and created again under the same id |
//! | `GET /v1/events/{id}` | one of the tenant's events, whole (roadmap M2b): another tenant's id, an operator-only row's, a purged or unknown one, and an id that is not an event's are `404`, alike |
//! | `GET /v1/events/stream`, `GET /v1/numbers/{pn}/events/stream` | Server-Sent Events: the HTTP crate's route group (`meta_whatsapp_server_http::api::events::stream_routes`), mounted here behind the same scope |
//!
//! Operator-only events (no tenant) are never answered. The logic is
//! [`crate::events::poll`] and the core's `events::get_event`; this module
//! parses the query, and the envelope is the HTTP crate's
//! ([`EventEnvelope`], shared with the streams).

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, Query, State};
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use serde::Serialize;
use utoipa::ToSchema;

use meta_whatsapp_server_core::events::get_event as get_event_of;
pub use meta_whatsapp_server_http::api::events::{
    EventEnvelope, EventType, KnownEventType, LAST_EVENT_ID, MAX_TYPES, StreamParams, stream_routes,
};
use meta_whatsapp_server_http::api::events::{
    envelope, parse_after, parse_phone_number_id, parse_types,
};

use crate::auth::Caller;
use crate::error::{ApiError, ErrorBody};
use crate::events::{MAX_PAGE_DATA_BYTES, poll};
use crate::model::{MAX_PAGE_SIZE, TenantId};
use crate::state::Grant;
use crate::store::events::EventQuery;

/// Default page size of `GET /v1/events`.
pub const DEFAULT_EVENTS_LIMIT: usize = 50;

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
    /// (`message_received,status_updated`) or repeated
    /// (`types=message_received&types=status_updated`), or both; every type
    /// when omitted.
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
    phone_number_id: Option<PhoneNumberId>,
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
            "after" => query.after = Some(parse_after(value, "after")?),
            "types" => parse_types(value, query.types.get_or_insert_with(Vec::new))?,
            "phone_number_id" => query.phone_number_id = Some(parse_phone_number_id(value)?),
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

/// A page of events.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventList {
    /// The events, in sequence order.
    pub data: Vec<EventEnvelope>,
    /// Pass as `after` to continue: the last event's sequence when more
    /// follow, else the tenant's newest sequence (so a poll never starts
    /// again from an old cursor). A filtered poll (`types`,
    /// `phone_number_id`) moves it past the events the filter left out:
    /// keep one cursor per filter set.
    pub next_after: i64,
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
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn list_events(
    State(state): State<Grant>,
    caller: Caller,
    query: EventsQuery,
) -> Result<Json<EventList>, ApiError> {
    let tenant = caller.tenant().clone();
    let polled = poll(state.outbox(), &query.for_tenant(tenant.clone())).await?;
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

/// `GET /v1/events/{id}`: one of the tenant's events, by id.
#[utoipa::path(
    get,
    path = "/v1/events/{id}",
    tag = "events",
    security(("api_key" = [])),
    params(
        ("id" = String, Path, description = "The event's id (`evt_` and 32 hex digits), as an envelope carries it"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The event, whole (`truncated` is false): what a delivery that left `data` out refers to", body = EventEnvelope),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden` (no `events` scope), `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no event of this tenant has this id (another tenant's event, one past retention, and an id that is no event's look alike)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget (`Retry-After`)", body = ErrorBody),
    )
)]
pub(crate) async fn get_event(
    State(state): State<Grant>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<EventEnvelope>, ApiError> {
    let tenant = caller.tenant().clone();
    let event = get_event_of(state.outbox(), &tenant, &id).await?;
    Ok(Json(envelope(&tenant, event)?))
}

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
        assert_eq!(
            ok.phone_number_id.as_ref().map(PhoneNumberId::as_str),
            Some("106540352242922")
        );
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
