//! `/webhooks/meta` on the public listener.
//!
//! M1a serves Meta's subscription check (`GET`) only: the verify token is
//! compared in constant time by the library's `verify_subscription`, and
//! the challenge is echoed as inert text, as the library's router does.
//! Deliveries (`POST`) arrive with milestone M1c; until then the route has
//! no `POST` and answers it `405`, so Meta keeps retrying instead of
//! believing the events were handled.

use meta_whatsapp_rs::webhooks::VerificationQuery;
use meta_whatsapp_rs::webhooks::axum::extract::rejection::QueryRejection;
use meta_whatsapp_rs::webhooks::axum::extract::{Query, State};
use meta_whatsapp_rs::webhooks::axum::http::{StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_rs::webhooks::verify_subscription;

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
