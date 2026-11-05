//! `POST /webhooks/flows/{waba_id}` on the public listener (roadmap M5d):
//! the Flows data endpoint Meta calls while a user is in a Flow. Meta's
//! contract, not the integrators' (not in the OpenAPI document), as
//! `flows/guides/implementingyourflowendpoint` describes it. The rules are
//! the core's ([`meta_whatsapp_server_core::flows_endpoint`]: the order of
//! its checks, the health check, the error notification, the integrating
//! app's [`FlowHandler`](meta_whatsapp_server_core::flows_endpoint::FlowHandler));
//! this module reads a body and answers.
//!
//! - A deployment with no Flow key answers `404` to everything here.
//! - A missing or malformed `X-Hub-Signature-256` is `432` before a byte of
//!   the body is read; past 32 requests in flight on the replica, `503`
//!   before it is read too; a body over 256 KiB is `413`, one that takes
//!   over 15 s to arrive `408`.
//! - Then the core: the signature against every app secret (`432`), the
//!   WABA's tenant (`404`), decryption (`421`), the action; `200` with the
//!   sealed answer as `text/plain` (base64), or the refusal's bare status
//!   (`503` when the integrating app is unavailable or slow, `500` when it
//!   failed). Answers carry no body but the sealed one: Meta reads the
//!   status.

use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequest, Path, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::flows_endpoint::{RESPONSE_CONTENT_TYPE, Refusal};

use super::webhooks::signature_shaped;
use crate::events::{BODY_READ_TIMEOUT, Rejection};
use crate::state::AppState;

/// The target of this module's log lines.
const LOG_TARGET: &str = "meta_whatsapp_server::api::flows";

/// `POST /webhooks/flows/{waba_id}`: one of Meta's data endpoint requests
/// (see the module docs).
pub async fn receive(
    State(state): State<AppState>,
    Path(waba_id): Path<String>,
    request: Request,
) -> Response {
    let (status, outcome, body) = answer(&state, &waba_id, request).await;
    state.metrics().flow_endpoint_request(outcome);
    match body {
        Some(body) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(RESPONSE_CONTENT_TYPE),
            )],
            body,
        )
            .into_response(),
        None => status.into_response(),
    }
}

/// The status, the outcome's label and the sealed body, if any.
async fn answer(
    state: &AppState,
    waba_id: &str,
    request: Request,
) -> (StatusCode, &'static str, Option<String>) {
    let Some(endpoint) = state.flows_endpoint() else {
        return (StatusCode::NOT_FOUND, "not_found", None);
    };
    let rejections = state.events().rejections();
    // Before the body: an unsigned request is never buffered.
    let Some(signature) = signature_shaped(request.headers()).map(str::to_owned) else {
        if let Some(suppressed) = rejections.admit(Rejection::FlowForged) {
            tracing::warn!(
                target: LOG_TARGET,
                suppressed,
                "rejected a Flows endpoint request without a well-formed signature header"
            );
        }
        return refused(Refusal::BadSignature);
    };
    // A place among the requests answered at once, held to the end.
    let Some(_place) = endpoint.admit() else {
        if let Some(suppressed) = rejections.admit(Rejection::Busy) {
            tracing::warn!(
                target: LOG_TARGET,
                suppressed,
                "too many Flows endpoint requests at once: answering 503"
            );
        }
        return refused(Refusal::Unavailable);
    };
    // The route's body limit: 413 past 256 KiB; 408 when the body is too
    // slow to arrive.
    let read = tokio::time::timeout(BODY_READ_TIMEOUT, Bytes::from_request(request, state));
    let body = match read.await {
        Ok(Ok(body)) => body,
        Ok(Err(rejection)) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", None);
        }
        Ok(Err(rejection)) => {
            tracing::warn!(
                target: LOG_TARGET,
                status = %rejection.status(),
                "could not read a Flows endpoint body"
            );
            return (rejection.status(), "failed", None);
        }
        Err(_) => return (StatusCode::REQUEST_TIMEOUT, "slow_body", None),
    };
    match endpoint.handle(waba_id, Some(&signature), &body).await {
        Ok(sealed) => (StatusCode::OK, sealed.outcome, Some(sealed.body)),
        Err(refusal) => {
            let slot = match refusal {
                Refusal::BadSignature => Some(Rejection::FlowForged),
                Refusal::Undecryptable => Some(Rejection::FlowUndecryptable),
                _ => None,
            };
            if let Some(suppressed) = slot.and_then(|slot| rejections.admit(slot)) {
                tracing::warn!(
                    target: LOG_TARGET,
                    suppressed,
                    outcome = refusal.outcome(),
                    "refused a Flows endpoint request"
                );
            }
            refused(refusal)
        }
    }
}

/// A refusal's bare status and label.
fn refused(refusal: Refusal) -> (StatusCode, &'static str, Option<String>) {
    (
        StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        refusal.outcome(),
        None,
    )
}
