//! Idempotency keys over HTTP (docs/design/server.md, section 5.4): the
//! `Idempotency-Key` header, and the answers of the core's engine
//! ([`meta_whatsapp_server_core::idempotency`]: claim, settle, replay,
//! and running an operation under a key, `run`, whose rules its module
//! documents). This module reads the header, hands the core's `run` the
//! service's records, lease, expiry, error body and metrics, and renders
//! what it answers.
//!
//! Sends, media uploads and template creation take an optional
//! `Idempotency-Key` header (inbox replies and OTP issue will, with their
//! routes). The key is **scoped to the tenant**. A request carrying one
//! claims it with the SHA-256 of its operation and its canonical input
//! ([`Fingerprint`]: the number or WABA the path names, decoded, and the
//! body; never the path's spelling, since roadmap S6), a lease of twice
//! the Graph timeout and an expiry of
//! `WA_SERVER_IDEMPOTENCY_TTL` (24 hours by default); it runs, then keeps
//! its answer (a kept answer comes back with `Idempotent-Replayed: true`,
//! without a second request to Meta) or, when the answer proves nothing
//! was sent, releases the key.
//!
//! A request cut at its deadline (the deadline layer drops it) settles its
//! key as a `504 timeout` that may have been sent, from a task of its own;
//! if that fails too, the lease runs out and the key reads as unknown.
//! The key is never logged: it names the caller's records.

use std::future::Future;

use meta_whatsapp_rs::webhooks::axum::extract::FromRequestParts;
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderName, HeaderValue, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
pub use meta_whatsapp_server_core::idempotency::Fingerprint;
use meta_whatsapp_server_core::idempotency::{self as engine, Answer, Keeper};
use serde::Serialize;

use crate::auth::Caller;
use crate::error::{ApiError, CODES, ErrorCodeTag, error_body_bytes};
use crate::model::IdempotencyKey;
use crate::state::AppState;

/// `Idempotency-Key`.
pub static IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

/// `Idempotent-Replayed: true` marks an answer kept from an earlier
/// request with the same key.
pub static IDEMPOTENT_REPLAYED: HeaderName = HeaderName::from_static("idempotent-replayed");

/// The request's `Idempotency-Key`, if it sent one. A malformed key (see
/// [`IdempotencyKey::parse`]) or two of them is `422 invalid_request` on
/// `Idempotency-Key`.
#[derive(Debug)]
pub struct KeyHeader(pub Option<IdempotencyKey>);

impl<S: Send + Sync> FromRequestParts<S> for KeyHeader {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let mut values = parts.headers.get_all(&IDEMPOTENCY_KEY).iter();
        let key = match (values.next(), values.next()) {
            (None, _) => Ok(Self(None)),
            (Some(value), None) => value
                .to_str()
                .ok()
                .and_then(IdempotencyKey::parse)
                .map(|key| Self(Some(key)))
                .ok_or_else(|| ApiError::invalid("Idempotency-Key")),
            (Some(_), Some(_)) => Err(ApiError::invalid("Idempotency-Key")),
        };
        std::future::ready(key)
    }
}

/// A successful answer of an idempotent route, as it is sent and kept: a
/// status and a JSON body. Its `Debug` prints the body's length
/// (`body_len`), never its bytes: a kept answer names the tenant's
/// customers.
#[derive(Clone)]
pub struct Success {
    status: StatusCode,
    body: Vec<u8>,
}

impl std::fmt::Debug for Success {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Success")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Success {
    /// `status` with `value` as its JSON body.
    pub fn json<T: Serialize>(status: StatusCode, value: &T) -> Result<Self, ApiError> {
        serde_json::to_vec(value)
            .map(|body| Self { status, body })
            .map_err(|_| ApiError::internal())
    }

    /// The core's answer, for its `run`.
    fn into_answer(self) -> Answer {
        Answer::new(self.status.as_u16(), self.body)
    }
}

impl IntoResponse for Success {
    fn into_response(self) -> Response {
        json_response(self.status, self.body)
    }
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response {
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response()
}

/// Run `operation` for `caller`'s tenant under `key` (see the module
/// docs), or plainly without one: the core's
/// [`engine::run`](meta_whatsapp_server_core::idempotency::run), over the
/// service's idempotency records, its lease and expiry (`Settings`), the
/// section 5.1 error body (what a failure that may have been sent keeps)
/// and its metrics (`wa_server_idempotency_total`). `operation` must do
/// nothing before it is polled: a repeat of a kept or running request
/// never polls it.
///
/// A kept answer comes back with `Idempotent-Replayed: true`, a kept
/// error's metrics label its code; anything else is answered as the
/// operation or the engine answered it.
///
/// Crate-private, as the handlers are: they have `caller` from the
/// [`Caller`] extractor, which admitted it (the core's `run` admits it
/// again). The state's records are the service's own.
pub(crate) async fn run(
    state: &AppState,
    caller: &Caller,
    key: Option<IdempotencyKey>,
    fingerprint: Fingerprint,
    operation: impl Future<Output = Result<Success, ApiError>>,
) -> Response {
    let settings = state.settings();
    let keeper = Keeper::new(
        state.idempotency_records(),
        settings.idempotency_lease,
        settings.idempotency_ttl,
        error_body_bytes,
    )
    .observed_by(state.metrics());
    let ran = engine::run(&keeper, state.authz(), caller, key, &fingerprint, async {
        operation
            .await
            .map(Success::into_answer)
            .map_err(ApiError::into_service_error)
    })
    .await;
    match ran {
        Ok(answer) if answer.replayed() => {
            let status = answer.status();
            replay(status, answer.into_body())
        }
        Ok(answer) => {
            let status =
                StatusCode::from_u16(answer.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            json_response(status, answer.into_body())
        }
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// A kept answer, again.
fn replay(status: u16, body: Vec<u8>) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let code = (status.is_client_error() || status.is_server_error())
        .then(|| kept_code(&body))
        .flatten();
    let mut response = json_response(status, body);
    response.headers_mut().insert(
        IDEMPOTENT_REPLAYED.clone(),
        HeaderValue::from_static("true"),
    );
    if let Some(code) = code {
        response.extensions_mut().insert(ErrorCodeTag(code));
    }
    response
}

/// The code of a kept error body, as the metrics label it.
fn kept_code(body: &[u8]) -> Option<&'static str> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let code = value["error"]["code"].as_str()?;
    CODES
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(c, _, _)| *c)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn keys_are_visible_ascii_up_to_255_bytes() {
        assert!(IdempotencyKey::parse("order:1234:shipped").is_some());
        assert!(IdempotencyKey::parse(&"k".repeat(255)).is_some());
        for bad in ["", "with space", "tab\t", "é", "new\nline"] {
            assert!(IdempotencyKey::parse(bad).is_none(), "{bad:?}");
        }
        assert!(IdempotencyKey::parse(&"k".repeat(256)).is_none());
        let key = IdempotencyKey::parse("order:1234").unwrap();
        assert_eq!(format!("{key:?}"), "IdempotencyKey(10 bytes)");
    }

    /// A request dropped before it settled its key (the deadline cut it)
    /// leaves a kept `504 timeout` that may have been sent, not a free key.
    /// (In `tests/messages.rs` until `run` became crate-private: it is
    /// that test, on the unit tests' state.)
    #[tokio::test]
    async fn a_request_cut_before_settling_keeps_a_timeout() {
        use std::time::Duration;

        use crate::model::{IdempotencyClaim, IdempotencyState, KeyOwner, Scope, TenantId};

        let state = AppState::for_tests();
        let tenant = TenantId::parse("merchant-42").unwrap();
        // The tenant's caller, as the guard makes it (run takes a Caller
        // since roadmap S5b, not a tenant).
        state.store().create_tenant(&tenant, "").await.unwrap();
        let (minted, _) = crate::api::admin::mint(
            state.store(),
            KeyOwner::Tenant(tenant.clone()),
            vec![Scope::Send],
            String::new(),
            None,
        )
        .await
        .unwrap();
        let caller = state
            .authz()
            .tenant_caller(
                Some(&format!("Bearer {}", minted.expose_key())),
                None,
                Scope::Send,
            )
            .await
            .unwrap();
        let key = IdempotencyKey::parse("k-cut").unwrap();
        let fingerprint = Fingerprint::send_message(
            &meta_whatsapp_rs::core::ids::PhoneNumberId::new("1").unwrap(),
            &json!({}),
        );
        let never = std::future::pending::<Result<Success, ApiError>>();
        let cut = tokio::time::timeout(
            Duration::from_millis(20),
            run(&state, &caller, Some(key.clone()), fingerprint, never),
        )
        .await;
        assert!(cut.is_err(), "the request was cut");
        // The settlement runs on a task of its own.
        let mut kept = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let claim = state
                .idempotency_records()
                .claim_idempotency_key(
                    &tenant,
                    &key,
                    fingerprint.as_bytes(),
                    "probe",
                    Duration::from_secs(60),
                    Duration::from_secs(3600),
                )
                .await
                .unwrap();
            if let IdempotencyClaim::Existing(record) = claim
                && let IdempotencyState::Completed { status, body } = record.state
            {
                kept = Some((status, body));
                break;
            }
        }
        let (status, body) = kept.expect("the cut request's key was settled");
        assert_eq!(status, 504);
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "timeout");
        assert_eq!(body["error"]["may_have_been_sent"], true);
    }

    /// `Debug` of a successful answer shows its status and its length,
    /// never its bytes: the body is the caller's data (a recipient, a
    /// message id), and a `{:?}` in a log line or a panic would otherwise
    /// print it. Decisive: `Success`'s hand-written `Debug`.
    #[test]
    fn debug_shows_a_successs_length_never_its_bytes() {
        let value = json!({"messages": [{"id": "wamid.SECRET-BODY-7Q"}], "to": "+15551234567"});
        let success = Success::json(StatusCode::CREATED, &value).unwrap();
        let body = serde_json::to_vec(&value).unwrap();
        let text = String::from_utf8(body.clone()).unwrap();
        let bytes = format!("{body:?}");
        let bytes = &bytes[1..bytes.len() - 1];
        let debug = format!("{success:?}");
        assert!(debug.contains("201"), "{debug}");
        assert!(
            debug.contains(&format!("body_len: {}", body.len())),
            "{debug}"
        );
        assert!(!debug.contains("SECRET-BODY"), "the text: {debug}");
        assert!(!debug.contains("+15551234567"), "the text: {debug}");
        assert!(!debug.contains(&text), "the text: {debug}");
        assert!(!debug.contains(bytes), "the bytes: {debug}");
    }
}
