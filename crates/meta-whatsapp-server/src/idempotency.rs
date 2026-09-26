//! Idempotency keys over HTTP (docs/design/server.md, section 5.4): the
//! `Idempotency-Key` header, and the answers of the core's engine
//! ([`meta_whatsapp_server_core::idempotency`]: claim, settle, replay,
//! whose rules its module documents).
//!
//! Sends, media uploads and template creation take an optional
//! `Idempotency-Key` header (inbox replies and OTP issue will, with their
//! routes). The key is **scoped to the tenant**. A request carrying one
//! claims it with the SHA-256 of its method, path and body
//! ([`Fingerprint`]), a lease of twice the Graph timeout and an expiry of
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
use meta_whatsapp_server_core::idempotency::{self as engine, Admission, Outcome, Repeat};
use serde::Serialize;

use crate::error::{ApiError, CODES, ErrorCodeTag, error_body_bytes};
use crate::model::{IdempotencyKey, TenantId};
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

/// Run `operation` for `tenant` under `key` (see the module docs), or
/// plainly without one. `operation` must do nothing before it is polled:
/// a repeat of a kept or running request never polls it.
///
/// Crate-private: `tenant` is taken as given, so only this crate's
/// handlers, which have it from an admitted [`crate::auth::Caller`], call
/// it. Another crate could otherwise claim a tenant's keys, or replay its
/// kept answers, by naming it.
pub(crate) async fn run(
    state: &AppState,
    tenant: &TenantId,
    key: Option<IdempotencyKey>,
    fingerprint: Fingerprint,
    operation: impl Future<Output = Result<Success, ApiError>>,
) -> Response {
    let Some(key) = key else {
        return match operation.await {
            Ok(success) => success.into_response(),
            Err(error) => error.into_response(),
        };
    };
    let settings = state.settings();
    let admission = engine::claim(
        state.idempotency_records(),
        tenant,
        key,
        &fingerprint,
        settings.idempotency_lease,
        settings.idempotency_ttl,
        error_body_bytes,
    )
    .await;
    match admission {
        Err(error) => ApiError::from(error).into_response(),
        Ok(Admission::Repeat(repeat)) => existing(state, repeat),
        Ok(Admission::Claimed(claim)) => match operation.await {
            Ok(success) => {
                claim
                    .settle(Outcome::Answered {
                        status: success.status.as_u16(),
                        body: &success.body,
                    })
                    .await;
                success.into_response()
            }
            Err(error) => {
                claim
                    .settle(Outcome::Failed(error.as_service_error()))
                    .await;
                error.into_response()
            }
        },
    }
}

/// The answer to a repeat that met another request's record.
fn existing(state: &AppState, repeat: Repeat) -> Response {
    state.metrics().idempotency(repeat.outcome());
    match repeat.into_answer() {
        Ok((status, body)) => replay(status, body),
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
    use meta_whatsapp_rs::webhooks::axum::http::Method;
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

    /// Key order and whitespace do not change a fingerprint; a value, the
    /// path or the method does.
    #[test]
    fn fingerprints_follow_the_request_not_its_spelling() {
        let post = Method::POST;
        let path = "/v1/numbers/1/messages";
        let a: serde_json::Value = serde_json::from_str(
            r#"{"to": {"phone": "+1"}, "type": "text", "text": {"body": "hi"}}"#,
        )
        .unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"text":{"body":"hi"},"type":"text","to":{"phone":"+1"}}"#)
                .unwrap();
        assert_eq!(
            Fingerprint::json(&post, path, &a),
            Fingerprint::json(&post, path, &b)
        );
        let other_body = json!({"to": {"phone": "+1"}, "type": "text", "text": {"body": "ho"}});
        assert_ne!(
            Fingerprint::json(&post, path, &a),
            Fingerprint::json(&post, path, &other_body)
        );
        assert_ne!(
            Fingerprint::json(&post, path, &a),
            Fingerprint::json(&post, "/v1/numbers/2/messages", &a)
        );
        assert_ne!(
            Fingerprint::json(&post, path, &a),
            Fingerprint::json(&Method::PUT, path, &a)
        );
        // Framing: moving a byte between parts changes the digest.
        assert_ne!(
            Fingerprint::parts(&post, path, &[("type", b"ab"), ("file", b"c")]),
            Fingerprint::parts(&post, path, &[("type", b"a"), ("file", b"bc")])
        );
        // Strings and numbers never collide.
        assert_ne!(
            Fingerprint::json(&post, path, &json!("1")),
            Fingerprint::json(&post, path, &json!(1))
        );
    }

    /// A request dropped before it settled its key (the deadline cut it)
    /// leaves a kept `504 timeout` that may have been sent, not a free key.
    /// (In `tests/messages.rs` until `run` became crate-private: it is
    /// that test, on the unit tests' state.)
    #[tokio::test]
    async fn a_request_cut_before_settling_keeps_a_timeout() {
        use std::time::Duration;

        use crate::model::{IdempotencyClaim, IdempotencyState};

        let state = AppState::for_tests();
        let tenant = TenantId::parse("merchant-42").unwrap();
        let key = IdempotencyKey::parse("k-cut").unwrap();
        let fingerprint = Fingerprint::json(&Method::POST, "/v1/numbers/1/messages", &json!({}));
        let never = std::future::pending::<Result<Success, ApiError>>();
        let cut = tokio::time::timeout(
            Duration::from_millis(20),
            run(&state, &tenant, Some(key.clone()), fingerprint, never),
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
