//! The block list (scope `contacts`; docs/design/server.md, section 4.2;
//! roadmap M5g): blocking and unblocking people on a number, and the list.
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/numbers/{pn}/blocked-users` | `GET /{pn}/block_users` |
//! | `POST /v1/numbers/{pn}/blocked-users` | `POST /{pn}/block_users` |
//! | `POST /v1/numbers/{pn}/blocked-users/unblock` | `DELETE /{pn}/block_users` (a body on a `DELETE` is what Meta's page documents; the service takes a `POST` so that proxies do not drop it) |
//!
//! Pages: `block-users`, `reference/whatsapp-business-phone-number/block-api`.
//! A block and an unblock take an `Idempotency-Key` and are **audited**
//! (the core's [`meta_whatsapp_server_core::blocked_users`]): by how many
//! people they named, blocked and Meta refused, never by who. Meta
//! answers a person it refused with an error code, and the service
//! answers that code and its kind, never Meta's text.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::blocked_users::{self as domain, BlockedUsers, Person};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, encode_cursor};
use super::listing;
use super::templates::ApiQuery;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// A person to block or unblock: a phone number, a business-scoped user
/// id (BSUID), or both (Meta then uses the phone number).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = BlockUser)]
pub struct PersonBody {
    /// E.164 **with** its `+` (`+16505551234`).
    pub phone: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`); a parent
    /// BSUID (`US.ENT.…`) is refused.
    pub user_id: Option<String>,
}

/// The people to block or unblock.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = BlockUsersRequest)]
pub struct BlockBody {
    /// 1 to 1,000 people. Meta blocks only people who messaged the number
    /// in the last 24 hours.
    pub users: Vec<PersonBody>,
}

impl From<PersonBody> for Person {
    fn from(person: PersonBody) -> Self {
        Self {
            phone: person.phone,
            user_id: person.user_id,
        }
    }
}

/// A person the operation succeeded for.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BlockUserDone)]
pub struct DoneView {
    /// What the request sent: the phone number, or the BSUID if only that.
    #[schema(required = true)]
    pub input: Option<String>,
    /// The phone number as WhatsApp knows it (absent for a BSUID).
    #[schema(required = true)]
    pub wa_id: Option<String>,
    /// The BSUID, when one was sent.
    #[schema(required = true)]
    pub user_id: Option<String>,
}

/// Why Meta refused a person.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BlockUserReason)]
pub struct ReasonView {
    /// Meta's error code (`131047`: the person has not messaged the number
    /// in the last 24 hours; `131021`: the number itself).
    pub code: i64,
    /// The service's error code for it, to branch on (`ErrorCode`'s).
    pub kind: String,
}

/// A person Meta refused.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BlockUserRefused)]
pub struct RefusedView {
    /// What the request sent.
    #[schema(required = true)]
    pub input: Option<String>,
    /// The phone number as WhatsApp knows it (may be absent).
    #[schema(required = true)]
    pub wa_id: Option<String>,
    /// The BSUID, when one was sent.
    #[schema(required = true)]
    pub user_id: Option<String>,
    /// Why: Meta's codes, never its texts.
    pub reasons: Vec<ReasonView>,
}

/// What a block or an unblock came to, person by person.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BlockUsersResult)]
pub struct OutcomesView {
    /// The people blocked (or unblocked).
    pub done: Vec<DoneView>,
    /// The people Meta refused.
    pub refused: Vec<RefusedView>,
    /// Meta's code when it answered a partial failure (`139100`) next to
    /// the lists.
    #[schema(required = true)]
    pub error_code: Option<i64>,
}

impl From<domain::Outcomes> for OutcomesView {
    fn from(outcomes: domain::Outcomes) -> Self {
        Self {
            done: outcomes
                .done
                .into_iter()
                .map(|d| DoneView {
                    input: d.input,
                    wa_id: d.wa_id,
                    user_id: d.user_id,
                })
                .collect(),
            refused: outcomes
                .refused
                .into_iter()
                .map(|r| RefusedView {
                    input: r.input,
                    wa_id: r.wa_id,
                    user_id: r.user_id,
                    reasons: r
                        .reasons
                        .into_iter()
                        .map(|reason| ReasonView {
                            code: reason.code,
                            kind: reason.kind.to_owned(),
                        })
                        .collect(),
                })
                .collect(),
            error_code: outcomes.error_code,
        }
    }
}

/// A person on the block list.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BlockedUser)]
pub struct BlockedView {
    /// The phone number (absent when the person has a username).
    #[schema(required = true)]
    pub wa_id: Option<String>,
    /// The BSUID.
    #[schema(required = true)]
    pub user_id: Option<String>,
    /// The parent BSUID, when the portfolio is enrolled for them.
    #[schema(required = true)]
    pub parent_user_id: Option<String>,
}

/// A page of the block list.
#[derive(Debug, Serialize, ToSchema)]
pub struct BlockedUserList {
    /// The people.
    pub data: Vec<BlockedView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// `?limit=` and `?cursor=`, as the OpenAPI document declares them.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct BlockedQuery {
    /// Page size, 1 to 100 (the service's bound; Meta's default without).
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

fn blocked_users<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<BlockedUsers<'a>, ApiError> {
    Ok(
        BlockedUsers::new(state.authz(), caller, owned.capability(state))?
            .observed_by(state.metrics()),
    )
}

/// The people of a request body, checked: the library's recipients.
fn recipients(body: Value) -> Result<Vec<meta_whatsapp_rs::core::recipient::Recipient>, ApiError> {
    let body = serde_json::from_value::<BlockBody>(body).map_err(|_| ApiError::invalid("body"))?;
    Ok(domain::people(
        body.users.into_iter().map(Person::from).collect(),
    )?)
}

/// Block `users`: `200` with the outcomes, or Meta's refusal.
async fn block(
    service: &BlockedUsers<'_>,
    users: &[meta_whatsapp_rs::core::recipient::Recipient],
) -> Result<Success, ApiError> {
    let outcomes = service.block(users).await?;
    Success::json(StatusCode::OK, &OutcomesView::from(outcomes))
}

/// Unblock `users`: `200` with the outcomes, or Meta's refusal.
async fn unblock(
    service: &BlockedUsers<'_>,
    users: &[meta_whatsapp_rs::core::recipient::Recipient],
) -> Result<Success, ApiError> {
    let outcomes = service.unblock(users).await?;
    Success::json(StatusCode::OK, &OutcomesView::from(outcomes))
}

/// `GET /v1/numbers/{pn}/blocked-users`: a page of the block list.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/blocked-users",
    tag = "contacts",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        BlockedQuery,
    ),
    responses(
        (status = 200, description = "A page of the block list", body = BlockedUserList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit`, `cursor` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_blocked_users(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiQuery(query): ApiQuery<BlockedQuery>,
) -> Result<Json<BlockedUserList>, ApiError> {
    let after = listing::after(query.cursor.as_deref())?;
    let page = blocked_users(&state, &caller, &owned)?
        .list(query.limit, after)
        .await?;
    Ok(Json(BlockedUserList {
        data: page
            .users
            .into_iter()
            .map(|u| BlockedView {
                wa_id: u.wa_id,
                user_id: u.user_id,
                parent_user_id: u.parent_user_id,
            })
            .collect(),
        next_cursor: page.next_after.as_deref().map(encode_cursor),
    }))
}

/// `POST /v1/numbers/{pn}/blocked-users`: block people.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/blocked-users",
    tag = "contacts",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never block twice"),
    ),
    request_body = BlockBody,
    responses(
        (status = 200, description = "Person by person: who was blocked, who Meta refused and why (a refusal of some is still a `200`)", body = OutcomesView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `users`, `users[i]`, `users[i].phone`, `users[i].user_id` or `body`; `idempotency_key_reused`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the people may have been blocked", body = ErrorBody),
    )
)]
pub(crate) async fn block_users(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::block_users(owned.phone_number_id(), &body);
    let users = match recipients(body) {
        Ok(users) => users,
        Err(error) => return error.into_response(),
    };
    let service = match blocked_users(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, block(&service, &users))
        .await
}

/// `POST /v1/numbers/{pn}/blocked-users/unblock`: unblock people.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/blocked-users/unblock",
    tag = "contacts",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never unblock twice"),
    ),
    request_body = BlockBody,
    responses(
        (status = 200, description = "Person by person: who was unblocked, who Meta refused and why", body = OutcomesView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `users`, `users[i]`, `users[i].phone`, `users[i].user_id` or `body`; `idempotency_key_reused`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the people may have been unblocked", body = ErrorBody),
    )
)]
pub(crate) async fn unblock_users(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::unblock_users(owned.phone_number_id(), &body);
    let users = match recipients(body) {
        Ok(users) => users,
        Err(error) => return error.into_response(),
    };
    let service = match blocked_users(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, unblock(&service, &users))
        .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A body is the documented object and no more: a key outside it, a
    /// group, and a value Meta would refuse are `422`, the field named.
    /// Decisive: each form.
    #[test]
    fn a_body_names_people_by_phone_or_bsuid_and_nothing_else() {
        assert!(recipients(json!({"users": [{"phone": "+16505551234"}]})).is_ok());
        assert!(recipients(json!({"users": [{"user_id": "US.13491208655302741918"}]})).is_ok());
        for (body, field) in [
            (json!({"users": [{"group_id": "1"}]}), "body"),
            (json!({"users": [], "extra": 1}), "body"),
            (json!({}), "body"),
            (json!({"users": []}), "users"),
            (json!({"users": [{}]}), "users[0]"),
            (
                json!({"users": [{"phone": "16505551234"}]}),
                "users[0].phone",
            ),
            (
                json!({"users": [{"user_id": "US.ENT.11815799212886844830"}]}),
                "users[0].user_id",
            ),
        ] {
            assert_eq!(
                recipients(body.clone()).unwrap_err().field(),
                Some(field),
                "{body}"
            );
        }
    }
}
