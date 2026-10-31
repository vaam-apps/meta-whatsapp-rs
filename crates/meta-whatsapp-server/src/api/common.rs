//! What every route module shares: the JSON body extractor, list pages
//! and timestamps.

use std::collections::HashMap;

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequest, Query, Request};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{ApiError, ErrorCode};
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
                Err(ApiError::new(ErrorCode::PAYLOAD_TOO_LARGE))
            }
            Err(_) => Err(ApiError::invalid("body")),
        }
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
pub(crate) fn encode_cursor(after: &str) -> String {
    hex::encode(after)
}

fn decode_cursor(cursor: &str) -> Option<String> {
    String::from_utf8(hex::decode(cursor).ok()?).ok()
}

/// The `next_cursor` of a listing.
pub(crate) fn next_cursor<T>(listing: &Listing<T>) -> Option<String> {
    listing.next_after.as_deref().map(encode_cursor)
}

/// RFC 3339, UTC.
pub(crate) fn rfc3339(at: OffsetDateTime) -> String {
    at.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// `(StatusCode, Json)` shorthand.
pub(crate) fn json<T: serde::Serialize>(status: StatusCode, body: T) -> (StatusCode, Json<T>) {
    (status, Json(body))
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::ids::MediaId;

    use super::*;

    /// A Meta id is 1 to 64 digits, not starting with `0` (the core's
    /// rule, `meta_whatsapp_server_core::request::graph_id`'s tests, which
    /// the core's media services check a media id with since roadmap S5d),
    /// and its refusal names the caller's field. Decisive: each clause, the
    /// length cap included.
    #[test]
    fn a_graph_id_is_up_to_64_digits() {
        let id = |id: &str| {
            meta_whatsapp_server_core::request::typed_graph_id::<MediaId>("media_id", id)
                .map_err(ApiError::from)
        };
        assert!(id("1").is_ok());
        assert!(id(&"9".repeat(64)).is_ok());
        for bad in [
            String::new(),
            "9".repeat(65),
            "0123".to_owned(),
            "12a".to_owned(),
            "1/2".to_owned(),
        ] {
            let refused = id(&bad).unwrap_err();
            assert_eq!(refused.field(), Some("media_id"), "{bad:?}");
        }
    }
}
