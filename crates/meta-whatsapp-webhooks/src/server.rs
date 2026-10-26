//! axum integration (feature `axum`): the webhook route and an SSE helper
//! for live inboxes.
//!
//! | Request | Answer |
//! | --- | --- |
//! | `GET /` valid verification | `200 text/plain` with `hub.challenge` (`nosniff`, `no-store`) |
//! | `GET /` anything else, or a blank configured verify token | `403` |
//! | `POST /` delivered, duplicate or unparseable-but-signed | `200` |
//! | `POST /` missing or malformed signature header (refused before the body is read) | `401` |
//! | `POST /` wrong signature | `401` |
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
use axum::extract::{DefaultBodyLimit, FromRequest, Query, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::{Stream, StreamExt as _};
use meta_whatsapp_core::Error;
use meta_whatsapp_core::error::WebhookError;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::event::WebhookEvent;
use crate::handler::WebhookHandler;
use crate::signature;
use crate::verify::VerificationQuery;

/// Header Meta signs deliveries with; defined (and available without the
/// `axum` feature) in [`crate::signature`].
pub use crate::signature::SIGNATURE_HEADER;

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

async fn receive(State(handler): State<Arc<WebhookHandler>>, request: Request) -> StatusCode {
    // The header's shape needs neither a secret nor the body: a missing or
    // malformed one is refused before a byte of the body is read, so an
    // unsigned request cannot make us buffer up to the body limit.
    let signature = match signature_header(request.headers()) {
        Ok(signature) => signature,
        Err(error) => {
            tracing::warn!(%error, "rejected webhook delivery before reading its body");
            return StatusCode::UNAUTHORIZED;
        }
    };
    // Honours the router's `DefaultBodyLimit` (413 over it).
    let body = match Bytes::from_request(request, &()).await {
        Ok(body) => body,
        Err(rejection) => {
            tracing::warn!(status = %rejection.status(), "could not read webhook body");
            return rejection.status();
        }
    };
    match handler.deliver(Some(&signature), &body).await {
        Ok(_) => StatusCode::OK,
        Err(error) => status_for(&error),
    }
}

/// The `X-Hub-Signature-256` value, if it is present, ASCII and shaped like
/// a signature (`sha256=` + 64 hex); whether it matches is the handler's
/// check.
fn signature_header(headers: &HeaderMap) -> Result<String, WebhookError> {
    let value = headers
        .get(SIGNATURE_HEADER)
        .ok_or(WebhookError::MissingSignature)?
        .to_str()
        .map_err(|_| WebhookError::MalformedSignature)?;
    signature::parse_header(Some(value))?;
    Ok(value.to_owned())
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
/// so a rejected event is never encoded.
///
/// The channel carries `Arc<WebhookEvent>` (what
/// `meta_whatsapp_adapters::sink::BroadcastSink` publishes): every open
/// stream receives the same allocation of an event, a reference count
/// rather than a copy, so a multi-megabyte `HistorySynced` body is held
/// once however many inboxes are open. Each stream still runs `filter`
/// on every event of every merchant on the channel.
///
/// Each event that passes `filter` (typically "same phone number id as the
/// merchant watching") is sent as `event: whatsapp` with the event's JSON as
/// `data`. When the receiver falls behind and the channel drops events, an
/// `event: lagged` with the number of missed events is sent and the stream
/// continues, so the client knows to reload history. The stream ends when
/// the channel closes. Keep-alive comments are on.
pub fn sse<F>(
    receiver: broadcast::Receiver<Arc<WebhookEvent>>,
    filter: F,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    F: Fn(&WebhookEvent) -> bool + Send + Sync + 'static,
{
    let stream = live(receiver, filter).filter_map(|item| {
        futures::future::ready(match item {
            Live::Event(event) => match Event::default().event("whatsapp").json_data(&*event) {
                Ok(sse_event) => Some(Ok(sse_event)),
                Err(error) => {
                    tracing::error!(%error, kind = event.kind(), "could not encode event for SSE");
                    None
                }
            },
            Live::Lagged(missed) => Some(Ok(Event::default()
                .event("lagged")
                .data(missed.to_string()))),
        })
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// What [`sse`] reads from its receiver, before encoding.
#[derive(Debug)]
enum Live {
    /// An event `filter` accepted: the channel's own `Arc`, shared with
    /// every other subscriber.
    Event(Arc<WebhookEvent>),
    /// The receiver fell behind and missed this many events.
    Lagged(u64),
}

/// The events of `receiver` that pass `filter`, and the lags; ends when
/// the channel closes.
fn live<F>(receiver: broadcast::Receiver<Arc<WebhookEvent>>, filter: F) -> impl Stream<Item = Live>
where
    F: Fn(&WebhookEvent) -> bool + Send + Sync + 'static,
{
    futures::stream::unfold((receiver, filter), |(mut receiver, filter)| async move {
        loop {
            let item = match receiver.recv().await {
                Ok(event) if filter(&event) => Live::Event(event),
                Ok(_) => continue,
                Err(RecvError::Lagged(missed)) => Live::Lagged(missed),
                Err(RecvError::Closed) => return None,
            };
            return Some((item, (receiver, filter)));
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::payload::WebhookPayload;
    use meta_whatsapp_core::error::{SinkError, ValidationError};

    fn text_event() -> WebhookEvent {
        let body = include_bytes!("../tests/fixtures/messages/text.json");
        WebhookPayload::from_slice(body)
            .unwrap()
            .into_events()
            .remove(0)
    }

    /// Roadmap L21b's decisive test, on the stream `sse` is built on: ten
    /// subscribers receive one allocation of an event. A stream that
    /// copied the event per subscriber (the behaviour before L21b) gives
    /// each its own and fails `Arc::ptr_eq`.
    #[tokio::test]
    async fn ten_sse_subscribers_share_one_allocation_per_event() {
        let (tx, _) = broadcast::channel(4);
        let mut streams: Vec<_> = (0..10)
            .map(|_| Box::pin(live(tx.subscribe(), |e| e.phone_number_id().is_some())))
            .collect();
        let sent = Arc::new(text_event());
        tx.send(Arc::clone(&sent)).unwrap();

        let mut received = Vec::new();
        for stream in &mut streams {
            match stream.next().await {
                Some(Live::Event(event)) => received.push(event),
                other => panic!("expected the event, got {other:?}"),
            }
        }
        assert_eq!(received.len(), 10);
        for (i, event) in received.iter().enumerate() {
            assert!(
                Arc::ptr_eq(&received[0], event) && Arc::ptr_eq(&sent, event),
                "subscriber {i} got its own copy of the event, not the shared allocation"
            );
        }
    }

    /// The same through the public `sse`: every stream's filter is handed
    /// the one allocation that was sent.
    #[tokio::test]
    async fn sse_filters_see_the_shared_event() {
        let (tx, _) = broadcast::channel(4);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut bodies: Vec<_> = (0..10)
            .map(|_| {
                let seen = Arc::clone(&seen);
                let filter = move |e: &WebhookEvent| {
                    seen.lock().unwrap().push(std::ptr::from_ref(e).addr());
                    true
                };
                sse(tx.subscribe(), filter)
                    .into_response()
                    .into_body()
                    .into_data_stream()
            })
            .collect();
        let sent = Arc::new(text_event());
        tx.send(Arc::clone(&sent)).unwrap();

        for body in &mut bodies {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
                .await
                .expect("an SSE frame")
                .unwrap()
                .unwrap();
            assert!(chunk.starts_with(b"event: whatsapp\n"));
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 10);
        let shared = Arc::as_ptr(&sent).addr();
        assert!(seen.iter().all(|&at| at == shared), "{seen:?} vs {shared}");
    }

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
