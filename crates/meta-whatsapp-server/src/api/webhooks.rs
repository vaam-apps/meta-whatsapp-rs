//! `/webhooks/meta` on the public listener: Meta's contract, not the
//! integrators' (not in the OpenAPI document). Answers are bare statuses,
//! as the library's router answers them: Meta reads nothing but the
//! status.
//!
//! - `GET`: Meta's subscription check. The verify token is compared in
//!   constant time by the library's `verify_subscription`, and the
//!   challenge is echoed as inert text, as the library's router does.
//! - `POST`: a delivery. A missing or malformed `X-Hub-Signature-256` is
//!   `401` before a byte of the body is read; a body over 3 MiB is `413`;
//!   then the library's `WebhookHandler` checks the signature against every
//!   app secret (`401`), parses, and hands each event, under its dedup
//!   lease, to the service's sink ([`crate::events`]). `200` once every
//!   event is recorded (or was already); `503` while another request
//!   delivers one of them (Meta retries); `500` when recording failed
//!   (Meta redelivers the batch).

use meta_whatsapp_rs::core::error::WebhookError;
use meta_whatsapp_rs::webhooks::axum::body::Bytes;
use meta_whatsapp_rs::webhooks::axum::extract::rejection::QueryRejection;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequest, Query, Request, State};
use meta_whatsapp_rs::webhooks::axum::http::{HeaderMap, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_rs::webhooks::{SIGNATURE_HEADER, VerificationQuery, verify_subscription};

use crate::state::AppState;

/// `GET /webhooks/meta`: Meta's subscription check. `200` with the
/// challenge when the verify token matches, else `403` with no body.
pub async fn verify(
    State(state): State<AppState>,
    query: Result<Query<VerificationQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return StatusCode::FORBIDDEN.into_response();
    };
    match verify_subscription(&query, state.verify_token()) {
        Ok(challenge) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            challenge,
        )
            .into_response(),
        Err(meta_whatsapp_rs::Error::Config(_)) => {
            tracing::error!("webhook verification refused: the verify token is blank");
            StatusCode::FORBIDDEN.into_response()
        }
        Err(error) => {
            tracing::warn!(
                kind = error.kind().as_str(),
                "rejected a webhook verification request"
            );
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

/// `POST /webhooks/meta`: one of Meta's deliveries (see the module docs).
pub async fn receive(State(state): State<AppState>, request: Request) -> Response {
    let (status, outcome) = deliver(&state, request).await;
    state.metrics().webhook_delivery(outcome);
    status.into_response()
}

/// Whether a signature header is present and shaped like one (`sha256=`
/// and 64 hex digits, either case, surrounding whitespace ignored: the
/// library's rules). Needs neither the secret nor the body.
fn signature_shaped(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(SIGNATURE_HEADER)?.to_str().ok()?;
    let hex = value.trim().strip_prefix("sha256=")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
}

/// The answer to a delivery and its outcome label.
async fn deliver(state: &AppState, request: Request) -> (StatusCode, &'static str) {
    // Before the body: an unsigned request is never buffered.
    let Some(signature) = signature_shaped(request.headers()).map(str::to_owned) else {
        tracing::warn!("rejected a webhook delivery without a well-formed signature header");
        return (StatusCode::UNAUTHORIZED, "unauthenticated");
    };
    // Honours the public router's body limit: 413 past 3 MiB.
    let body = match Bytes::from_request(request, state).await {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            tracing::warn!("rejected a webhook delivery over the body limit");
            return (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
        }
        Err(rejection) => {
            tracing::warn!(status = %rejection.status(), "could not read a webhook body");
            return (rejection.status(), "failed");
        }
    };
    let handler = state.events().handler();
    match handler.deliver(Some(&signature), &body).await {
        Ok(report) => {
            state.metrics().webhook_duplicates(
                "dedup",
                u64::try_from(report.duplicates).unwrap_or(u64::MAX),
            );
            (StatusCode::OK, "delivered")
        }
        Err(meta_whatsapp_rs::Error::Webhook(
            WebhookError::MissingSignature
            | WebhookError::MalformedSignature
            | WebhookError::SignatureMismatch,
        )) => (StatusCode::UNAUTHORIZED, "unauthenticated"),
        Err(meta_whatsapp_rs::Error::Webhook(WebhookError::PayloadTooLarge { .. })) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
        }
        Err(meta_whatsapp_rs::Error::Webhook(WebhookError::ClaimInFlight)) => {
            (StatusCode::SERVICE_UNAVAILABLE, "in_flight")
        }
        Err(error) => {
            tracing::warn!(
                kind = error.kind().as_str(),
                "a webhook delivery failed: Meta redelivers it"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::webhooks::axum::http::HeaderValue;

    use super::*;

    #[test]
    fn a_signature_header_is_shaped_as_the_library_reads_it() {
        let hex = "ab".repeat(32);
        for (value, ok) in [
            (format!("sha256={hex}"), true),
            (format!("  sha256={}  ", hex.to_uppercase()), true),
            (format!("sha256={}", &hex[1..]), false),
            (format!("sha256={hex}0"), false),
            (format!("sha1={hex}"), false),
            (format!("sha256={}", "zz".repeat(32)), false),
            (String::new(), false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(SIGNATURE_HEADER, HeaderValue::from_str(&value).unwrap());
            assert_eq!(signature_shaped(&headers).is_some(), ok, "{value:?}");
        }
        assert!(signature_shaped(&HeaderMap::new()).is_none());
    }
}
