//! Conversation routing (scope `routing`; docs/design/server.md, section
//! 4.2; roadmap M5l): a number's routing roles and the thread control
//! actions on a user's thread.
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/numbers/{pn}/routing-roles` | `GET /{pn}/routing_roles` |
//! | `POST /v1/numbers/{pn}/thread-control/pass` | `POST /{pn}/thread_control`, `action: pass` |
//! | `POST /v1/numbers/{pn}/thread-control/take` | `POST /{pn}/thread_control`, `action: take` |
//! | `POST /v1/numbers/{pn}/thread-control/release` | `POST /{pn}/thread_control`, `action: release` |
//!
//! Pages: `conversation-routing/thread-control`,
//! `conversation-routing/routing-roles`. The three actions take an
//! `Idempotency-Key` (Meta replays none of them harmlessly) and are
//! **audited** (the core's [`meta_whatsapp_server_core::routing`]): by the
//! number and the role passed to, never by user or `metadata`. Meta's
//! texts are not in the answer of an action, only its code and kind.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::routing::{self as domain, Person, Routing};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use crate::auth::{Caller, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// The user whose thread an action acts on: exactly one of a phone
/// number, a `wa_id` and a business-scoped user id (BSUID, Meta's
/// preferred identifier).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ThreadUser)]
pub struct UserBody {
    /// E.164 **with** its `+` (`+16505551234`).
    pub phone: Option<String>,
    /// The user's `wa_id`: digits, no `+` (`16505551234`).
    pub wa_id: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`); a parent
    /// BSUID (`US.ENT.…`) is refused.
    pub user_id: Option<String>,
}

impl From<UserBody> for Person {
    fn from(user: UserBody) -> Self {
        Self {
            phone: user.phone,
            wa_id: user.wa_id,
            user_id: user.user_id,
        }
    }
}

/// Pass the thread with a user to another responder.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = PassThreadRequest)]
pub struct PassBody {
    /// The user whose thread it is.
    pub user: UserBody,
    /// The role receiving the thread: `ai_agent`, `ctwa`,
    /// `customer_service`, `escalation`, `marketing` or `utility`. Absent:
    /// the account's escalation partner (the same as `escalation`). A
    /// role, never an app: the business that owns the number configures
    /// who holds it.
    pub target_role: Option<String>,
    /// Free text, at most 2,000 characters, forwarded verbatim to the new
    /// owner on its `control_passed` event. Never logged by the service.
    pub metadata: Option<String>,
}

/// Take the thread with a user (the escalation partner only).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = TakeThreadRequest)]
pub struct TakeBody {
    /// The user whose thread it is.
    pub user: UserBody,
    /// Free text, at most 2,000 characters, forwarded verbatim to the
    /// previous owner on its `control_taken` event. Never logged by the
    /// service.
    pub metadata: Option<String>,
}

/// Release the thread with a user to idle.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ReleaseThreadRequest)]
pub struct ReleaseBody {
    /// The user whose thread it is.
    pub user: UserBody,
}

/// What Meta answered to a thread action it accepted.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ThreadControlResult)]
pub struct ActedView {
    /// Meta's opaque id of the request: quote it to Meta's support.
    #[schema(required = true)]
    pub request_id: Option<String>,
}

impl From<domain::Acted> for ActedView {
    fn from(acted: domain::Acted) -> Self {
        Self {
            request_id: acted.request_id,
        }
    }
}

/// The routing roles a number's business holds.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = RoutingRoles)]
pub struct RolesView {
    /// Meta's names for the roles (`calls`, `ctwa`, `customer_service`,
    /// `escalation`, `marketing`, `utility`; a later one as Meta adds it),
    /// in no order. Empty: routing is on, and no entry point routes to
    /// the business.
    pub roles: Vec<String>,
    /// Whether the business has standby visibility (copies of activity on
    /// threads it does not own).
    pub is_standby: bool,
}

impl From<domain::Roles> for RolesView {
    fn from(roles: domain::Roles) -> Self {
        Self {
            roles: roles.roles,
            is_standby: roles.is_standby,
        }
    }
}

/// The bodies name a person and free text: their `Debug` shows neither
/// (as `Person`'s does not).
macro_rules! redacted_debug {
    ($($body:ident),*) => {$(
        impl std::fmt::Debug for $body {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($body)).finish_non_exhaustive()
            }
        }
    )*};
}
redacted_debug!(UserBody, PassBody, TakeBody, ReleaseBody);

fn routing<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<Routing<'a>, ApiError> {
    Ok(Routing::new(
        state.authz(),
        caller,
        owned.capability(state),
        state.conversations(),
        state.clock(),
    )?
    .observed_by(state.metrics()))
}

fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, ApiError> {
    serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))
}

/// `GET /v1/numbers/{pn}/routing-roles`: the roles the number's business
/// holds.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/routing-roles",
    tag = "routing",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The roles the business holds on the number", body = RolesView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (the endpoint is rate limited per number and per app)", body = ErrorBody),
        (status = 502, description = "Meta failed or refused (code `100`: Conversation Routing is not enabled for the number)", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_routing_roles(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
) -> Result<Json<RolesView>, ApiError> {
    let roles = routing(&state, &caller, &owned)?.roles().await?;
    Ok(Json(RolesView::from(roles)))
}

/// `POST /v1/numbers/{pn}/thread-control/pass`: pass the thread.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/thread-control/pass",
    tag = "routing",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never pass twice"),
    ),
    request_body = PassBody,
    responses(
        (status = 200, description = "Meta accepted the pass; the new owner is told by its `control_passed` event, the caller by nothing", body = ActedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `user`, `user.phone`, `user.wa_id`, `user.user_id`, `target_role`, `metadata` or `body`; `idempotency_key_reused`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed or refused (its code, never its text)", body = ErrorBody),
        (status = 504, description = "`timeout`: the thread may have been passed", body = ErrorBody),
    )
)]
pub(crate) async fn pass_thread(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::pass_thread(owned.phone_number_id(), &body);
    let checked = parse::<PassBody>(body).and_then(|body| {
        let user = domain::user(body.user.into())?;
        let pass = domain::pass_of(body.target_role, body.metadata)?;
        Ok((user, pass))
    });
    let (user, pass) = match checked {
        Ok(checked) => checked,
        Err(error) => return error.into_response(),
    };
    let service = match routing(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    let act = async {
        let acted = service.pass(&user, &pass).await?;
        Success::json(StatusCode::OK, &ActedView::from(acted))
    };
    state.idempotent(&caller, key, fingerprint, act).await
}

/// `POST /v1/numbers/{pn}/thread-control/take`: take the thread.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/thread-control/take",
    tag = "routing",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never take twice"),
    ),
    request_body = TakeBody,
    responses(
        (status = 200, description = "Meta accepted the take; the previous owner is told by its `control_taken` event", body = ActedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `user`, `user.phone`, `user.wa_id`, `user.user_id`, `metadata` or `body`; `idempotency_key_reused`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed or refused (code `2494191`: not the escalation partner)", body = ErrorBody),
        (status = 504, description = "`timeout`: the thread may have been taken", body = ErrorBody),
    )
)]
pub(crate) async fn take_thread(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::take_thread(owned.phone_number_id(), &body);
    let checked = parse::<TakeBody>(body).and_then(|body| {
        let user = domain::user(body.user.into())?;
        let note = domain::metadata(body.metadata)?;
        Ok((user, note))
    });
    let (user, note) = match checked {
        Ok(checked) => checked,
        Err(error) => return error.into_response(),
    };
    let service = match routing(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    let act = async {
        let acted = service.take(&user, note.as_deref()).await?;
        Success::json(StatusCode::OK, &ActedView::from(acted))
    };
    state.idempotent(&caller, key, fingerprint, act).await
}

/// `POST /v1/numbers/{pn}/thread-control/release`: release the thread.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/thread-control/release",
    tag = "routing",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never release twice"),
    ),
    request_body = ReleaseBody,
    responses(
        (status = 200, description = "Meta accepted the release (an idle thread's too); no event follows", body = ActedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `user`, `user.phone`, `user.wa_id`, `user.user_id` or `body`; `idempotency_key_reused`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed or refused (its code, never its text)", body = ErrorBody),
        (status = 504, description = "`timeout`: the thread may have been released", body = ErrorBody),
    )
)]
pub(crate) async fn release_thread(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::release_thread(owned.phone_number_id(), &body);
    let checked = parse::<ReleaseBody>(body).and_then(|body| Ok(domain::user(body.user.into())?));
    let user = match checked {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    let service = match routing(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    let act = async {
        let acted = service.release(&user).await?;
        Success::json(StatusCode::OK, &ActedView::from(acted))
    };
    state.idempotent(&caller, key, fingerprint, act).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A body is the documented object and no more: a key outside it, a
    /// second way to name the user and a role Meta does not list are
    /// `422`, the field named. Decisive: each form.
    #[test]
    fn a_body_names_a_user_a_role_and_nothing_else() {
        let pass = |body: Value| {
            parse::<PassBody>(body).and_then(|b| {
                domain::user(b.user.into())?;
                domain::pass_of(b.target_role, b.metadata)?;
                Ok(())
            })
        };
        assert!(pass(json!({"user": {"user_id": "US.13491208655302741918"}})).is_ok());
        assert!(
            pass(
                json!({"user": {"phone": "+16505551234"}, "target_role": "ai_agent",
                        "metadata": "needs a human"})
            )
            .is_ok()
        );
        for (body, field) in [
            (json!({}), "body"),
            (json!({"user": {"phone": "+1650"}, "to": "x"}), "body"),
            (json!({"user": {"group_id": "1"}}), "body"),
            (json!({"user": {}}), "user"),
            (
                json!({"user": {"phone": "+16505551234", "wa_id": "16505551234"}}),
                "user",
            ),
            (json!({"user": {"phone": "16505551234"}}), "user.phone"),
            (
                json!({"user": {"phone": "+16505551234"}, "target_role": "billing"}),
                "target_role",
            ),
            (
                json!({"user": {"phone": "+16505551234"}, "metadata": "x".repeat(2001)}),
                "metadata",
            ),
        ] {
            assert_eq!(pass(body.clone()).unwrap_err().field(), Some(field));
        }
        assert!(parse::<ReleaseBody>(json!({"user": {"wa_id": "1"}, "metadata": "m"})).is_err());
        let shown = format!(
            "{:?}",
            parse::<PassBody>(
                json!({"user": {"phone": "+16505551234"}, "metadata": "SECRET-NOTE"})
            )
            .unwrap()
        );
        assert!(
            !shown.contains("16505551234") && !shown.contains("SECRET-NOTE"),
            "{shown}"
        );
    }
}
