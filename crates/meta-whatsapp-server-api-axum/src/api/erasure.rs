//! Erasing a customer on a number (scope `erasure`; docs/design/server.md,
//! section 4.2, decisions D10, D30, D31; roadmap M2f).
//!
//! | Route | Does |
//! | --- | --- |
//! | `POST /v1/numbers/{pn}/erasures` | erase a person on the number: every identity the inbox connects to the ones named, from the inbox and from the tenant's own outbox rows of the number (never the operator-only ones), journaled and audited first and after; refused on a number moved from another tenant (`409 erasure_crosses_binding`); asks Meta nothing |
//!
//! The rules are the core's ([`meta_whatsapp_server_core::erasure`]: the
//! `Erasures` service on an owned number, the journal, the order of the
//! steps). This module keeps the route and its request and answer types
//! with their OpenAPI schema. The identities travel in the body, never
//! in the path (an ingress logs paths); the body has no `Debug` and is
//! read by [`ApiJson`], whose refusal never quotes it; the answer names
//! the erasure and the number, nothing of the person. No `Idempotency-Key`:
//! a repeat is harmless (it erases nothing more), and an idempotency
//! record would keep a fingerprint of the request.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_server_core::erasure::Erasures;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::common::{ApiJson, rfc3339};
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::state::Grant;

/// `POST /v1/numbers/{pn}/erasures`. No `Debug`: it names a customer.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ErasureRequest {
    /// The path's phone number id, again: an erasure cannot be undone.
    pub confirm: String,
    /// The person's identities on the number, as the inbox's routes name a
    /// conversation's contact: their BSUIDs (`US.13491208655302741918`),
    /// their `wa_id`s (digits, no `+`), the ones you hold yourself. 1 to
    /// 16; the service adds every identity its inbox connects to them on
    /// the number (the address book's contacts, the identity links).
    /// Never answered, stored or logged in clear.
    #[schema(min_items = 1, max_items = 16, write_only)]
    pub contacts: Vec<String>,
}

/// What an erasure answers: its id and time, nothing of what it found
/// (whether the person was known, how many records went: the operator's
/// audit event has the counts).
#[derive(Debug, Serialize, ToSchema)]
pub struct ErasureAnswer {
    /// The erasure's id (`era_` and 32 hex digits), in the deployment's
    /// erasure journal and the operator's audit event.
    pub erasure_id: String,
    /// Phone number id.
    pub phone_number_id: String,
    /// When it was made (RFC 3339).
    #[schema(format = DateTime)]
    pub erased_at: String,
}

/// `POST /v1/numbers/{pn}/erasures`: erase a customer on the number, from
/// the service's inbox and the tenant's own rows of its event outbox (not
/// the operator's). Refused on a number moved from another tenant, nothing
/// erased (`409 erasure_crosses_binding`). Irreversible; only when you call
/// it (which requests to honour is yours to decide). Erase on each of the
/// merchant's numbers, and again once Meta's 7-day redelivery window has
/// passed; Meta's contact book is the library's
/// `delete_contact_book_entry`.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/erasures",
    tag = "erasure",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = ErasureRequest,
    responses(
        (status = 200, description = "Erased (or nothing was stored of them): the same answer either way", body = ErasureAnswer),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden` (a key without the scope `erasure`), `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`erasure_not_configured` (no `WA_SERVER_ERASURE_KEY`), `erasure_crosses_binding` (the number came from another tenant, whose hidden records of the person an erasure would reach: nothing erased), `number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `confirm` (not the path's number), `contacts` (none, more than 16, or one the inbox's routes refuse: never quoted) or `body`: nothing was read or erased", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget (`Retry-After`)", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`: the number moved while it was erased, or the journal or a store failed; repeat it", body = ErrorBody),
    )
)]
pub(crate) async fn erase_customer(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiJson(body): ApiJson<ErasureRequest>,
) -> Result<Json<ErasureAnswer>, ApiError> {
    let ErasureRequest { confirm, contacts } = body;
    let outbox = state.outbox();
    let erasure = Erasures::new(
        state.authz(),
        &caller,
        owned.capability(&state),
        state.conversations(),
        outbox,
        state.erasure(),
        state.clock(),
    )?
    .erase(&confirm, &contacts)
    .await?;
    Ok(Json(ErasureAnswer {
        erasure_id: erasure.erasure_id,
        phone_number_id: owned.phone_number_id().as_str().to_owned(),
        erased_at: rfc3339(erasure.erased_at),
    }))
}
