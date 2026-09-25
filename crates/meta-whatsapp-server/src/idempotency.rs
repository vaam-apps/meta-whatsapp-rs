//! Idempotency keys (docs/design/server.md, section 5.4).
//!
//! Sends, media uploads and template creation take an optional
//! `Idempotency-Key` header (inbox replies and OTP issue will, with their
//! routes). The key is **scoped to the tenant**: two tenants may use the
//! same one. A request carrying one:
//!
//! 1. is checked locally first (body, recipient, limits): a request that
//!    fails there never touches its key;
//! 2. claims the key, `in_progress`, with the SHA-256 of its method, path
//!    and body ([`Fingerprint`]), a lease of twice the Graph timeout and
//!    an expiry of `WA_SERVER_IDEMPOTENCY_TTL` (24 hours by default);
//! 3. runs, then settles the key by what the answer proves:
//!    - **nothing was sent** (`may_have_been_sent: false`: a 4xx from
//!      Meta, throttling, a refusal before the request): the key is
//!      **released**, so the caller may repeat the request with it;
//!    - anything else (a success, a timeout, a 5xx): the answer is
//!      **kept**, byte for byte, and a repeat gets it back with
//!      `Idempotent-Replayed: true`, without a second request to Meta.
//!
//! A repeat meeting the key finds:
//!
//! | Record | Answer |
//! | --- | --- |
//! | another request's (another method, path or body) | `422 idempotency_key_reused` |
//! | `in_progress`, lease running | `409 idempotency_in_progress`, `retryable`, `may_have_been_sent: true` (the other request may be sending) |
//! | `in_progress`, lease over (the process died, or the request was cut without settling) | `409 outcome_unknown`, `may_have_been_sent: true`: never a new send |
//! | completed | the kept answer, `Idempotent-Replayed: true` |
//!
//! A request cut at its deadline (the deadline layer drops it) settles its
//! key as a `504 timeout` that may have been sent, from a task of its own;
//! if that fails too, the lease runs out and the key reads as unknown.
//! The key is never logged: it names the caller's records.

use std::future::Future;

use meta_whatsapp_rs::webhooks::axum::extract::FromRequestParts;
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{ApiError, CODES, ErrorCodeTag};
use crate::model::{
    IdempotencyClaim, IdempotencyKey, IdempotencyRecord, IdempotencyState, TenantId,
};
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

/// SHA-256 of what a request asks: its method, its path and its body.
/// The same request always has the same fingerprint (a JSON body's object
/// keys are sorted and its whitespace ignored); a request with another
/// number, another route or another body has another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Of a request with a JSON body.
    pub fn json(method: &Method, path: &str, body: &serde_json::Value) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"json");
        framed(&mut hasher, method.as_str().as_bytes());
        framed(&mut hasher, path.as_bytes());
        canonical(&mut hasher, body);
        Self(hasher.finalize().into())
    }

    /// Of a request made of named parts (a multipart form), in the order
    /// given.
    pub fn parts(method: &Method, path: &str, parts: &[(&str, &[u8])]) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"parts");
        framed(&mut hasher, method.as_str().as_bytes());
        framed(&mut hasher, path.as_bytes());
        for (name, value) in parts {
            framed(&mut hasher, name.as_bytes());
            framed(&mut hasher, value);
        }
        Self(hasher.finalize().into())
    }

    /// The digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// `bytes` with its length first, so that no two sequences of fields hash
/// alike.
fn framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// A JSON value, with object keys in sorted order whatever the parser
/// kept.
fn canonical(hasher: &mut Sha256, value: &serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Null => hasher.update(b"n"),
        Value::Bool(b) => hasher.update(if *b { b"t" } else { b"f" }),
        Value::Number(n) => {
            hasher.update(b"#");
            framed(hasher, n.to_string().as_bytes());
        }
        Value::String(s) => {
            hasher.update(b"s");
            framed(hasher, s.as_bytes());
        }
        Value::Array(items) => {
            hasher.update(b"[");
            hasher.update((items.len() as u64).to_be_bytes());
            for item in items {
                canonical(hasher, item);
            }
        }
        Value::Object(map) => {
            hasher.update(b"{");
            hasher.update((map.len() as u64).to_be_bytes());
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in entries {
                framed(hasher, key.as_bytes());
                canonical(hasher, value);
            }
        }
    }
}

/// A successful answer of an idempotent route, as it is sent and kept: a
/// status and a JSON body.
#[derive(Debug, Clone)]
pub struct Success {
    status: StatusCode,
    body: Vec<u8>,
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
pub async fn run(
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
    let Some(claim) = claim_id() else {
        return ApiError::internal().into_response();
    };
    let settings = state.settings();
    let claimed = state
        .store()
        .claim_idempotency_key(
            tenant,
            &key,
            fingerprint.as_bytes(),
            &claim,
            settings.idempotency_lease,
            settings.idempotency_ttl,
        )
        .await;
    match claimed {
        Err(error) => ApiError::from(error).into_response(),
        Ok(IdempotencyClaim::Existing(record)) => existing(state, record, fingerprint),
        Ok(IdempotencyClaim::Claimed) => {
            let held = Held {
                state: state.clone(),
                tenant: tenant.clone(),
                key,
                claim,
                armed: true,
            };
            let outcome = operation.await;
            held.settle(outcome).await
        }
    }
}

/// A random claim id: which request holds a key.
fn claim_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(hex::encode(bytes))
}

/// The answer to a repeat that met another request's record.
fn existing(state: &AppState, record: IdempotencyRecord, fingerprint: Fingerprint) -> Response {
    if record.fingerprint != *fingerprint.as_bytes() {
        state.metrics().idempotency("reused");
        return ApiError::new("idempotency_key_reused").into_response();
    }
    match record.state {
        IdempotencyState::InProgress {
            lease_expired: false,
        } => {
            state.metrics().idempotency("in_progress");
            ApiError::new("idempotency_in_progress")
                .retryable(true)
                .with_may_have_been_sent(true)
                .into_response()
        }
        IdempotencyState::InProgress {
            lease_expired: true,
        } => {
            state.metrics().idempotency("outcome_unknown");
            ApiError::new("outcome_unknown")
                .with_may_have_been_sent(true)
                .into_response()
        }
        IdempotencyState::Completed { status, body } => {
            state.metrics().idempotency("replayed");
            replay(status, body)
        }
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

/// A key this request claimed, until it settles it.
struct Held {
    state: AppState,
    tenant: TenantId,
    key: IdempotencyKey,
    claim: String,
    /// Not settled yet: dropping it settles it as a timeout.
    armed: bool,
}

impl Held {
    /// Keep or release the key by what `outcome` proves, and answer it.
    async fn settle(mut self, outcome: Result<Success, ApiError>) -> Response {
        let store = self.state.store();
        let response = match outcome {
            Ok(success) => {
                let kept = store
                    .complete_idempotency_key(
                        &self.tenant,
                        &self.key,
                        &self.claim,
                        success.status.as_u16(),
                        &success.body,
                    )
                    .await;
                note(kept, "kept");
                success.into_response()
            }
            Err(error) if error.may_have_been_sent() => {
                let body = serde_json::to_vec(&error.body()).unwrap_or_default();
                let kept = store
                    .complete_idempotency_key(
                        &self.tenant,
                        &self.key,
                        &self.claim,
                        error.status().as_u16(),
                        &body,
                    )
                    .await;
                note(kept, "kept");
                error.into_response()
            }
            Err(error) => {
                let released = store
                    .release_idempotency_key(&self.tenant, &self.key, &self.claim)
                    .await;
                note(released, "released");
                error.into_response()
            }
        };
        self.armed = false;
        response
    }
}

/// Log a key that could not be settled: it stays `in_progress` until its
/// lease ends, then reads as unknown (never a second send).
fn note(settled: Result<bool, meta_whatsapp_rs::core::error::StorageError>, what: &str) {
    match settled {
        Ok(true) => {}
        Ok(false) => tracing::warn!(outcome = what, "an idempotency key was no longer held"),
        Err(error) => {
            tracing::warn!(outcome = what, error = %error, "an idempotency key could not be settled");
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Cut before it settled (the request deadline, or the client
        // leaving): whatever was under way may have been sent.
        let answer = ApiError::new("timeout")
            .retryable(true)
            .with_may_have_been_sent(true);
        let status = answer.status().as_u16();
        let body = serde_json::to_vec(&answer.body()).unwrap_or_default();
        let (state, tenant, key, claim) = (
            self.state.clone(),
            self.tenant.clone(),
            self.key.clone(),
            std::mem::take(&mut self.claim),
        );
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let kept = state
                    .store()
                    .complete_idempotency_key(&tenant, &key, &claim, status, &body)
                    .await;
                note(kept, "kept after a cut");
            });
        }
    }
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
}
