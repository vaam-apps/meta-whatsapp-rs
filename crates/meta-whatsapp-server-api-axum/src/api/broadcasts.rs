//! Broadcasts (scope `broadcasts`; docs/design/server.md, section 4.2;
//! roadmap M5k, slice 1): a template sent to many people from a number,
//! queued, paced at the deployment's rate and read by id.
//!
//! | Route | Does |
//! | --- | --- |
//! | `POST /v1/numbers/{pn}/broadcasts` | queues a broadcast; `202` with its state; `Idempotency-Key` for the whole broadcast |
//! | `GET /v1/numbers/{pn}/broadcasts/{broadcast_id}` | its state and counts, never a message |
//! | `POST /v1/numbers/{pn}/broadcasts/{broadcast_id}/cancel` | stops it before its next send |
//!
//! The core's [`meta_whatsapp_server_core::broadcasts`] decides everything:
//! a broadcast is its tenant's or it does not exist (`404`), each message
//! is checked as a send is, and no recipient or parameter reaches a log,
//! an audit event or an answer.

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::broadcasts::{
    BroadcastRecipient, BroadcastStatus, Broadcasts, NewBroadcast,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::ToSchema;

use super::common::{ApiJson, rfc3339};
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// One person of a broadcast: a phone number, a business-scoped user id
/// (BSUID), or both (Meta then uses the phone number).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = BroadcastRecipient)]
pub struct RecipientBody {
    /// E.164 **with** its `+` (`+16505551234`).
    pub phone: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`).
    pub user_id: Option<String>,
    /// This person's own `components`, replacing the template's: their
    /// parameters (Meta's template components, as a send takes them).
    #[schema(value_type = Option<Vec<HashMap<String, Value>>>)]
    pub components: Option<Value>,
}

/// A template for many people.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateBroadcastRequest)]
pub struct CreateBody {
    /// Meta's template object: `name`, `language` and, for parameters
    /// every person shares, `components`. A template naming a catalog is
    /// not offered in a broadcast.
    #[schema(value_type = HashMap<String, Value>)]
    pub template: Value,
    /// The people: each named by `phone`, `user_id` or both; each once.
    pub recipients: Vec<RecipientBody>,
}

/// The bodies name people and their parameters: their `Debug` shows
/// neither.
macro_rules! redacted_debug {
    ($($body:ident),*) => {$(
        impl std::fmt::Debug for $body {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($body)).finish_non_exhaustive()
            }
        }
    )*};
}
redacted_debug!(RecipientBody, CreateBody);

/// A broadcast: where it stands and how many people are in each outcome.
/// No person, no parameter, no message.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Broadcast)]
pub struct BroadcastView {
    /// Its id.
    pub id: String,
    /// The number it sends from.
    pub phone_number_id: String,
    /// `queued`, `running`, `completed`, `cancelled` or `stopped` (given
    /// up after its retries: the people left are in `queued`).
    pub state: &'static str,
    /// When it was made (RFC 3339).
    pub created_at: String,
    /// How many people it is for (each once).
    pub recipients: usize,
    /// Not sent yet.
    pub queued: usize,
    /// Accepted by Meta.
    pub sent: usize,
    /// Refused by Meta.
    pub failed: usize,
    /// Sent or not, nobody knows (a replica died mid-send, a timeout):
    /// never sent again.
    pub unknown: usize,
    /// Skipped for Meta's per-user marketing limit.
    pub limited: usize,
    /// The failures by the error code of Meta's refusal (`marketing_opted_out`,
    /// `template_paused`, `reconnect_required`, …), never Meta's text.
    pub failures: BTreeMap<String, u64>,
}

impl From<BroadcastStatus> for BroadcastView {
    fn from(status: BroadcastStatus) -> Self {
        Self {
            id: status.id,
            phone_number_id: status.number.as_str().to_owned(),
            state: status.state.as_str(),
            created_at: rfc3339(status.created_at),
            recipients: status.recipients,
            queued: status.queued,
            sent: status.sent,
            failed: status.failed,
            unknown: status.unknown,
            limited: status.limited,
            failures: status.failures,
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, ApiError> {
    serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))
}

fn number(pn: String) -> Result<PhoneNumberId, ApiError> {
    // Only a numeric id can be bound: anything else does not exist.
    PhoneNumberId::new(pn).map_err(|_| ApiError::not_found())
}

/// `POST /v1/numbers/{pn}/broadcasts`: queue a broadcast.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/broadcasts",
    tag = "broadcasts",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never queue twice"),
    ),
    request_body = CreateBody,
    responses(
        (status = 202, description = "Queued: nothing is sent yet, at the number's rate it will be", body = BroadcastView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `recipients`, `recipients[i]`, `template` or its fields, or `body`; `idempotency_key_reused`: nothing queued", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget, or too many broadcasts at once (`Retry-After`)", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: nothing queued", body = ErrorBody),
    )
)]
pub(crate) async fn create_broadcast(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_broadcast(owned.phone_number_id(), &body);
    let checked = parse::<CreateBody>(body).and_then(|body| {
        let recipients = body
            .recipients
            .into_iter()
            .map(|r| BroadcastRecipient {
                phone: r.phone,
                user_id: r.user_id,
                components: r.components,
            })
            .collect();
        Ok(NewBroadcast::new(
            body.template,
            recipients,
            state.broadcasts().settings(),
        )?)
    });
    let request = match checked {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let act = async {
        let broadcasts = Broadcasts::new(state.authz(), &caller, state.broadcasts())?;
        let status = broadcasts.create(owned.capability(&state), request).await?;
        Success::json(StatusCode::ACCEPTED, &BroadcastView::from(status))
    };
    state.idempotent(&caller, key, fingerprint, act).await
}

/// `GET /v1/numbers/{pn}/broadcasts/{broadcast_id}`: a broadcast's state
/// and counts.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/broadcasts/{broadcast_id}",
    tag = "broadcasts",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("broadcast_id" = String, Path, description = "The broadcast's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The broadcast", body = BroadcastView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number or broadcast for this tenant", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn get_broadcast(
    State(state): State<Grant>,
    caller: Caller,
    Path((pn, id)): Path<(String, String)>,
) -> Result<Json<BroadcastView>, ApiError> {
    let broadcasts = Broadcasts::new(state.authz(), &caller, state.broadcasts())?;
    let status = broadcasts.get(&number(pn)?, &id).await?;
    Ok(Json(BroadcastView::from(status)))
}

/// `POST /v1/numbers/{pn}/broadcasts/{broadcast_id}/cancel`: stop a
/// broadcast.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/broadcasts/{broadcast_id}/cancel",
    tag = "broadcasts",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("broadcast_id" = String, Path, description = "The broadcast's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The broadcast as it stands: `cancelled` if it was queued or running (a send past its last check still goes out), else as it had settled", body = BroadcastView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number or broadcast for this tenant", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn cancel_broadcast(
    State(state): State<Grant>,
    caller: Caller,
    Path((pn, id)): Path<(String, String)>,
) -> Result<Json<BroadcastView>, ApiError> {
    let broadcasts = Broadcasts::new(state.authz(), &caller, state.broadcasts())?;
    let status = broadcasts.cancel(&number(pn)?, &id).await?;
    Ok(Json(BroadcastView::from(status)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A body is the documented object and no more; its `Debug` names
    /// nobody.
    #[test]
    fn a_body_is_the_documented_object_and_hides_its_people() {
        let body = json!({
            "template": {"name": "promo", "language": {"code": "en_US"}},
            "recipients": [{"phone": "+16505551234", "components": []}],
        });
        let parsed = parse::<CreateBody>(body.clone()).unwrap();
        let shown = format!("{parsed:?}");
        assert!(
            !shown.contains("16505551234") && !shown.contains("promo"),
            "{shown}"
        );
        for bad in [
            json!({"template": {}, "recipients": [], "to": "x"}),
            json!({"template": {}, "recipients": [{"phone": "+1", "group_id": "1"}]}),
            json!({"recipients": []}),
        ] {
            assert!(parse::<CreateBody>(bad).is_err());
        }
    }
}
