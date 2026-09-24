//! axum integration (feature `axum`): the webhook route and an SSE helper
//! for live inboxes.
//!
//! | Request | Answer |
//! | --- | --- |
//! | `GET /` valid verification | `200 text/plain` with `hub.challenge` (`nosniff`, `no-store`) |
//! | `GET /` anything else, or a blank configured verify token | `403` |
//! | `POST /` delivered, duplicate or unparseable-but-signed | `200` |
//! | `POST /` missing, malformed or wrong signature | `401` |
//! | `POST /` body over the limit | `413` |
//! | `POST /` sink or dedup store failure | `500` (Meta redelivers) |
//! | `POST /` an event is being delivered by another request ([`WebhookError::ClaimInFlight`]) | `503` (Meta redelivers) |
//!
//! Mount the router wherever your callback URL points
//! (`Router::new().nest("/webhooks/whatsapp", router(handler))`).

use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::Stream;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use wa_core::Error;
use wa_core::error::WebhookError;

use crate::event::WebhookEvent;
use crate::handler::WebhookHandler;
use crate::verify::VerificationQuery;

/// Header Meta signs deliveries with.
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// `GET /` (verification) and `POST /` (deliveries), backed by `handler`.
///
/// The body limit is the handler's [`WebhookHandler::max_body_bytes`],
/// enforced by axum before the body is buffered. Setting it explicitly also
/// replaces axum's implicit 2 MiB default, which is below the 3 MB Meta
/// documents for webhook payloads.
pub fn router(handler: Arc<WebhookHandler>) -> Router {
    let limit = handler.max_body_bytes();
    Router::new()
        .route("/", get(verify).post(receive))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(handler)
}

async fn verify(
    State(handler): State<Arc<WebhookHandler>>,
    query: Result<Query<VerificationQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return StatusCode::FORBIDDEN.into_response();
    };
    match handler.verify(&query) {
        // The challenge is reflected input. It is only echoed to someone who
        // knows the verify token, but still as inert text: `text/plain` and
        // `nosniff` so no browser renders it as HTML, `no-store` so no cache
        // keeps a response that only proves the token once.
        Ok(challenge) => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, "text/plain; charset=utf-8"),
                (X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (CACHE_CONTROL, "no-store"),
            ],
            challenge,
        )
            .into_response(),
        Err(error @ Error::Config(_)) => {
            tracing::error!(%error, "webhook verification refused: handler misconfigured");
            StatusCode::FORBIDDEN.into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "rejected webhook verification request");
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

async fn receive(
    State(handler): State<Arc<WebhookHandler>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let signature = match headers.get(SIGNATURE_HEADER).map(|v| v.to_str()) {
        None => None,
        Some(Ok(value)) => Some(value),
        Some(Err(_)) => {
            tracing::warn!(error = %WebhookError::MalformedSignature, "rejected webhook delivery");
            return StatusCode::UNAUTHORIZED;
        }
    };
    match handler.deliver(signature, &body).await {
        Ok(_) => StatusCode::OK,
        Err(error) => status_for(&error),
    }
}

fn status_for(error: &Error) -> StatusCode {
    match error {
        Error::Webhook(
            WebhookError::MissingSignature
            | WebhookError::MalformedSignature
            | WebhookError::SignatureMismatch,
        ) => StatusCode::UNAUTHORIZED,
        Error::Webhook(
            WebhookError::InvalidVerificationRequest(_) | WebhookError::VerifyTokenMismatch,
        ) => StatusCode::FORBIDDEN,
        // Behind the router's body limit layer this is a backstop, not the
        // usual path; both answer 413.
        Error::Webhook(WebhookError::PayloadTooLarge { .. }) => StatusCode::PAYLOAD_TOO_LARGE,
        Error::Webhook(WebhookError::ClaimInFlight) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Stream events from a broadcast channel as Server-Sent Events, for a live
/// inbox: subscribe a receiver to the channel your sink publishes to and
/// return this from a handler.
///
/// The events carry your customers' messages. Mount the route behind your
/// own authentication, and make `filter` an allow-list of what the viewer
/// may see (`|e| e.phone_number_id() == Some(&merchant_number)`), never a
/// deny-list: [`WebhookEvent::Unparsed`] and [`WebhookEvent::Unknown`] have
/// no phone number id and carry raw bodies that can belong to any tenant on
/// the app, so a filter such as `|e| e.phone_number_id().is_none_or(…)`
/// leaks them to everyone. The filter runs before an event is serialized,
/// so rejected events cost nothing.
///
/// Each event that passes `filter` (typically "same phone number id as the
/// merchant watching") is sent as `event: whatsapp` with the event's JSON as
/// `data`. When the receiver falls behind and the channel drops events, an
/// `event: lagged` with the number of missed events is sent and the stream
/// continues, so the client knows to reload history. The stream ends when
/// the channel closes. Keep-alive comments are on.
pub fn sse<F>(
    receiver: broadcast::Receiver<WebhookEvent>,
    filter: F,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    F: Fn(&WebhookEvent) -> bool + Send + Sync + 'static,
{
    let stream = futures::stream::unfold((receiver, filter), |(mut receiver, filter)| async move {
        loop {
            let event = match receiver.recv().await {
                Ok(event) if filter(&event) => {
                    match Event::default().event("whatsapp").json_data(&event) {
                        Ok(sse_event) => sse_event,
                        Err(error) => {
                            tracing::error!(%error, kind = event.kind(), "could not encode event for SSE");
                            continue;
                        }
                    }
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(missed)) => {
                    Event::default().event("lagged").data(missed.to_string())
                }
                Err(RecvError::Closed) => return None,
            };
            return Some((Ok(event), (receiver, filter)));
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wa_core::error::{SinkError, ValidationError};

    #[test]
    fn errors_map_to_the_documented_statuses() {
        let cases: [(Error, StatusCode); 9] = [
            (
                WebhookError::MissingSignature.into(),
                StatusCode::UNAUTHORIZED,
            ),
            (
                WebhookError::MalformedSignature.into(),
                StatusCode::UNAUTHORIZED,
            ),
            (
                WebhookError::SignatureMismatch.into(),
                StatusCode::UNAUTHORIZED,
            ),
            (
                WebhookError::VerifyTokenMismatch.into(),
                StatusCode::FORBIDDEN,
            ),
            (
                WebhookError::PayloadTooLarge { size: 2, limit: 1 }.into(),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
            // No longer produced for the body size; must not turn into 413.
            (
                ValidationError::new("body", "too big").into(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                WebhookError::ClaimInFlight.into(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                Error::Other(std::io::Error::other("integrator").into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (SinkError::Closed.into(), StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (error, status) in cases {
            assert_eq!(status_for(&error), status, "{error}");
        }
    }
}
