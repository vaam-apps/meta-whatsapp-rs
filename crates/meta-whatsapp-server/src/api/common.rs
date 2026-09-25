//! What every route module shares: the JSON body extractor, list pages
//! and timestamps.

use std::collections::HashMap;

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequest, Query, Request};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::ApiError;
use crate::model::{Listing, MAX_PAGE_SIZE, PageRequest};

/// A JSON request body. A body that is not the documented JSON object
/// (unknown fields included) is `422 invalid_request` on `body`, with the
/// service's sentence: serde's message could quote the value (a token).
/// Over the body limit, it is `413 payload_too_large`.
#[derive(Debug)]
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                Err(ApiError::new("payload_too_large"))
            }
            Err(_) => Err(ApiError::invalid("body")),
        }
    }
}

/// Longest Meta id a path or a query may name.
const MAX_GRAPH_ID_LEN: usize = 64;

/// A Meta object id from a path or a query (a media id, a template id):
/// digits, not starting with `0`, else `422 invalid_request` on `field`.
/// Checked before any request: an id is a Graph path segment, and a
/// route for one kind of object must not reach another (`DELETE
/// /{id}` deletes whatever node the id names).
pub fn graph_id(field: &'static str, id: &str) -> Result<(), ApiError> {
    let valid = !id.is_empty()
        && id.len() <= MAX_GRAPH_ID_LEN
        && id.bytes().all(|b| b.is_ascii_digit())
        && !id.starts_with('0');
    if valid {
        Ok(())
    } else {
        Err(ApiError::invalid(field))
    }
}

/// Default page size.
pub const DEFAULT_PAGE_SIZE: usize = 50;

/// The paging parameters, as the OpenAPI document declares them
/// ([`PageQuery`] reads them).
#[derive(Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageParams {
    /// Page size.
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// `?limit=` (1 to 100, default 50) and `?cursor=` (from `next_cursor`).
#[derive(Debug)]
pub struct PageQuery(pub PageRequest);

impl<S: Send + Sync> meta_whatsapp_rs::webhooks::axum::extract::FromRequestParts<S> for PageQuery {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut meta_whatsapp_rs::webhooks::axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, ApiError> {
        let Query(query) = Query::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::invalid("query"))?;
        let limit = match query.get("limit") {
            None => DEFAULT_PAGE_SIZE,
            Some(limit) => limit
                .parse::<usize>()
                .ok()
                .filter(|l| (1..=MAX_PAGE_SIZE).contains(l))
                .ok_or_else(|| ApiError::invalid("limit"))?,
        };
        let after = match query.get("cursor") {
            None => None,
            Some(cursor) => Some(decode_cursor(cursor).ok_or_else(|| ApiError::invalid("cursor"))?),
        };
        Ok(Self(PageRequest { after, limit }))
    }
}

/// An opaque cursor: the hex of the last id of the page.
pub fn encode_cursor(after: &str) -> String {
    hex::encode(after)
}

fn decode_cursor(cursor: &str) -> Option<String> {
    String::from_utf8(hex::decode(cursor).ok()?).ok()
}

/// The `next_cursor` of a listing.
pub fn next_cursor<T>(listing: &Listing<T>) -> Option<String> {
    listing.next_after.as_deref().map(encode_cursor)
}

/// RFC 3339, UTC.
pub fn rfc3339(at: OffsetDateTime) -> String {
    at.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Parse an RFC 3339 time from the request's `field`.
pub fn parse_rfc3339(field: &'static str, value: &str) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ApiError::invalid(field))
}

/// `(StatusCode, Json)` shorthand.
pub fn json<T: serde::Serialize>(status: StatusCode, body: T) -> (StatusCode, Json<T>) {
    (status, Json(body))
}
