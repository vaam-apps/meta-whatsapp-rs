//! The idempotency engine (docs/design/server.md, section 5.4): claiming
//! a caller's `Idempotency-Key`, settling it by what the answer proves,
//! and answering a repeat. The records are an [`IdempotencyRecords`]
//! port; an API adapter reads the key from its request and renders the
//! answers (the HTTP one: `meta_whatsapp_server::idempotency`).
//!
//! The key is **scoped to the tenant**: two tenants may use the same one.
//! A request carrying one:
//!
//! 1. is checked locally first (body, recipient, limits): a request that
//!    fails there never touches its key;
//! 2. claims the key, `in_progress`, with the SHA-256 of its method, path
//!    and body ([`Fingerprint`]), a lease and an expiry ([`claim`]);
//! 3. runs, then settles the key by what the answer proves
//!    ([`Claim::settle`]):
//!    - **nothing was sent** (`may_have_been_sent: false`: a 4xx from
//!      Meta, throttling, a refusal before the request): the key is
//!      **released**, so the caller may repeat the request with it;
//!    - anything else (a success, a timeout, a 5xx): the answer is
//!      **kept**, byte for byte, and a repeat gets it back without a
//!      second request to Meta.
//!
//! A repeat meeting the key finds ([`Repeat`]):
//!
//! | Record | Answer |
//! | --- | --- |
//! | another request's (another method, path or body) | `422 idempotency_key_reused` |
//! | `in_progress`, lease running | `409 idempotency_in_progress`, `retryable`, `may_have_been_sent: true` (the other request may be sending) |
//! | `in_progress`, lease over (the process died, or the request was cut without settling) | `409 outcome_unknown`, `may_have_been_sent: true`: never a new send |
//! | completed | the kept answer |
//!
//! A request cut before it settled its key (a [`Claim`] dropped: the
//! deadline, or the client leaving) settles it as a `504 timeout` that may
//! have been sent, from a task of its own; if that fails too, the lease
//! runs out and the key reads as unknown. The key is never logged: it
//! names the caller's records.

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::core::error::StorageError;
use sha2::{Digest, Sha256};

use crate::error::ServiceError;
use crate::model::{
    IdempotencyClaim, IdempotencyKey, IdempotencyRecord, IdempotencyState, TenantId,
};
use crate::store::IdempotencyRecords;

/// SHA-256 of what a request asks: its method, its path and its body.
/// The same request always has the same fingerprint (a JSON body's object
/// keys are sorted and its whitespace ignored); a request with another
/// number, another route or another body has another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Of a request with a JSON body. `method` is the HTTP method's name
    /// (`POST`): an HTTP adapter's method type, or a string.
    pub fn json<M: AsRef<str> + ?Sized>(method: &M, path: &str, body: &serde_json::Value) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"json");
        framed(&mut hasher, method.as_ref().as_bytes());
        framed(&mut hasher, path.as_bytes());
        canonical(&mut hasher, body);
        Self(hasher.finalize().into())
    }

    /// Of a request made of named parts (a multipart form), in the order
    /// given.
    pub fn parts<M: AsRef<str> + ?Sized>(method: &M, path: &str, parts: &[(&str, &[u8])]) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"parts");
        framed(&mut hasher, method.as_ref().as_bytes());
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

/// The body an API adapter answers `error` with, byte for byte: what a
/// key keeps for a failure that may have been sent.
pub type RenderError = fn(&ServiceError) -> Vec<u8>;

/// What claiming a key found.
#[derive(Debug)]
pub enum Admission {
    /// The key was free (or its record had expired): run the request,
    /// then [`Claim::settle`] it.
    Claimed(Claim),
    /// Another request holds, or held, the key: answer this, without
    /// running the request.
    Repeat(Repeat),
}

/// The answer to a request meeting another request's record. `Debug`
/// shows a kept answer's length (`body_len`), never its bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum Repeat {
    /// Another method, path or body: `422 idempotency_key_reused`.
    Reused,
    /// Still running: `409 idempotency_in_progress`, `retryable`,
    /// `may_have_been_sent: true`.
    InProgress,
    /// Its lease ended without an answer: `409 outcome_unknown`,
    /// `may_have_been_sent: true`. Never a new send.
    OutcomeUnknown,
    /// Completed: its kept answer, again.
    Replay {
        /// Its status.
        status: u16,
        /// Its body, byte for byte.
        body: Vec<u8>,
    },
}

impl std::fmt::Debug for Repeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reused => f.write_str("Reused"),
            Self::InProgress => f.write_str("InProgress"),
            Self::OutcomeUnknown => f.write_str("OutcomeUnknown"),
            Self::Replay { status, body } => f
                .debug_struct("Replay")
                .field("status", status)
                .field("body_len", &body.len())
                .finish(),
        }
    }
}

impl Repeat {
    /// The answer to a request with `fingerprint` meeting `record`.
    pub fn of(record: IdempotencyRecord, fingerprint: &Fingerprint) -> Self {
        if record.fingerprint != *fingerprint.as_bytes() {
            return Self::Reused;
        }
        match record.state {
            IdempotencyState::InProgress {
                lease_expired: false,
            } => Self::InProgress,
            IdempotencyState::InProgress {
                lease_expired: true,
            } => Self::OutcomeUnknown,
            IdempotencyState::Completed { status, body } => Self::Replay { status, body },
        }
    }

    /// Its name, for metrics: `reused`, `in_progress`, `outcome_unknown`
    /// or `replayed`.
    pub fn outcome(&self) -> &'static str {
        match self {
            Self::Reused => "reused",
            Self::InProgress => "in_progress",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Replay { .. } => "replayed",
        }
    }

    /// The kept answer, `(status, body)`, or the error to answer instead.
    pub fn into_answer(self) -> Result<(u16, Vec<u8>), ServiceError> {
        match self {
            Self::Reused => Err(ServiceError::new("idempotency_key_reused")),
            Self::InProgress => Err(ServiceError::new("idempotency_in_progress")
                .retryable(true)
                .with_may_have_been_sent(true)),
            Self::OutcomeUnknown => {
                Err(ServiceError::new("outcome_unknown").with_may_have_been_sent(true))
            }
            Self::Replay { status, body } => Ok((status, body)),
        }
    }
}

/// Claim `key` for `tenant`'s request with `fingerprint`: a lease of
/// `lease` (twice the Graph timeout, section 5.4) and an expiry after
/// `ttl`. `render` is how the adapter answers an error, for the answer a
/// failure keeps.
///
/// # Errors
///
/// No random claim id (`500 internal`), or the store failing (`503
/// storage_unavailable`).
pub async fn claim(
    records: &Arc<dyn IdempotencyRecords>,
    tenant: &TenantId,
    key: IdempotencyKey,
    fingerprint: &Fingerprint,
    lease: Duration,
    ttl: Duration,
    render: RenderError,
) -> Result<Admission, ServiceError> {
    let Some(claim) = claim_id() else {
        return Err(ServiceError::internal());
    };
    let claimed = records
        .claim_idempotency_key(tenant, &key, fingerprint.as_bytes(), &claim, lease, ttl)
        .await?;
    Ok(match claimed {
        IdempotencyClaim::Existing(record) => Admission::Repeat(Repeat::of(record, fingerprint)),
        IdempotencyClaim::Claimed => Admission::Claimed(Claim {
            records: records.clone(),
            tenant: tenant.clone(),
            key,
            id: claim,
            render,
            armed: true,
        }),
    })
}

/// A random claim id: which request holds a key.
fn claim_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(hex::encode(bytes))
}

/// How the request holding a key ended. `Debug` shows an answer's length
/// (`body_len`), never its bytes.
#[derive(Clone, Copy)]
pub enum Outcome<'a> {
    /// It answered this: kept, byte for byte.
    Answered {
        /// The status.
        status: u16,
        /// The body.
        body: &'a [u8],
    },
    /// It failed: kept (as the adapter renders it) when it may have been
    /// sent, else the key is released.
    Failed(&'a ServiceError),
}

impl std::fmt::Debug for Outcome<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Answered { status, body } => f
                .debug_struct("Answered")
                .field("status", status)
                .field("body_len", &body.len())
                .finish(),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

/// A key this request claimed, until it settles it. Dropped unsettled (the
/// request cut), it settles it as a `504 timeout` that may have been sent,
/// from a task of its own.
pub struct Claim {
    records: Arc<dyn IdempotencyRecords>,
    tenant: TenantId,
    key: IdempotencyKey,
    /// The claim id: which request holds the key.
    id: String,
    render: RenderError,
    /// Not settled yet: dropping it settles it as a timeout.
    armed: bool,
}

impl std::fmt::Debug for Claim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key names the caller's records.
        f.debug_struct("Claim")
            .field("tenant", &self.tenant)
            .finish_non_exhaustive()
    }
}

impl Claim {
    /// Keep or release the key by what `outcome` proves. A key that
    /// cannot be settled is logged, and stays `in_progress` until its
    /// lease ends, then reads as unknown (never a second send).
    pub async fn settle(mut self, outcome: Outcome<'_>) {
        let records = self.records.clone();
        match outcome {
            Outcome::Answered { status, body } => {
                let kept = records
                    .complete_idempotency_key(&self.tenant, &self.key, &self.id, status, body)
                    .await;
                note(kept, "kept");
            }
            Outcome::Failed(error) if error.may_have_been_sent() => {
                let body = (self.render)(error);
                let kept = records
                    .complete_idempotency_key(
                        &self.tenant,
                        &self.key,
                        &self.id,
                        error.status(),
                        &body,
                    )
                    .await;
                note(kept, "kept");
            }
            Outcome::Failed(_) => {
                let released = records
                    .release_idempotency_key(&self.tenant, &self.key, &self.id)
                    .await;
                note(released, "released");
            }
        }
        self.armed = false;
    }
}

/// Log a key that could not be settled: it stays `in_progress` until its
/// lease ends, then reads as unknown (never a second send).
fn note(settled: Result<bool, StorageError>, what: &str) {
    match settled {
        Ok(true) => {}
        Ok(false) => tracing::warn!(outcome = what, "an idempotency key was no longer held"),
        Err(error) => {
            tracing::warn!(outcome = what, error = %error, "an idempotency key could not be settled");
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Cut before it settled (the request deadline, or the client
        // leaving): whatever was under way may have been sent.
        let answer = ServiceError::new("timeout")
            .retryable(true)
            .with_may_have_been_sent(true);
        let status = answer.status();
        let body = (self.render)(&answer);
        let (records, tenant, key, claim) = (
            self.records.clone(),
            self.tenant.clone(),
            self.key.clone(),
            std::mem::take(&mut self.id),
        );
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let kept = records
                    .complete_idempotency_key(&tenant, &key, &claim, status, &body)
                    .await;
                note(kept, "kept after a cut");
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::store::StoreResult;

    /// What a settle did to the records.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Complete(u16, Vec<u8>),
        Release,
    }

    /// Records that claim every key and remember how each was settled.
    #[derive(Debug, Default)]
    struct Recorded(Mutex<Vec<Call>>);

    #[async_trait]
    impl IdempotencyRecords for Recorded {
        async fn claim_idempotency_key(
            &self,
            _: &TenantId,
            _: &IdempotencyKey,
            _: &[u8; 32],
            _: &str,
            _: Duration,
            _: Duration,
        ) -> StoreResult<IdempotencyClaim> {
            Ok(IdempotencyClaim::Claimed)
        }

        async fn complete_idempotency_key(
            &self,
            _: &TenantId,
            _: &IdempotencyKey,
            _: &str,
            status: u16,
            body: &[u8],
        ) -> StoreResult<bool> {
            self.0
                .lock()
                .unwrap()
                .push(Call::Complete(status, body.to_vec()));
            Ok(true)
        }

        async fn release_idempotency_key(
            &self,
            _: &TenantId,
            _: &IdempotencyKey,
            _: &str,
        ) -> StoreResult<bool> {
            self.0.lock().unwrap().push(Call::Release);
            Ok(true)
        }

        async fn purge_idempotency_keys(&self) -> StoreResult<u64> {
            Ok(0)
        }
    }

    fn render(error: &ServiceError) -> Vec<u8> {
        error.code().as_bytes().to_vec()
    }

    async fn claimed(records: &Arc<Recorded>) -> Claim {
        let records: Arc<dyn IdempotencyRecords> = records.clone();
        let admission = claim(
            &records,
            &TenantId::parse("tenant-a").unwrap(),
            IdempotencyKey::parse("order:1").unwrap(),
            &Fingerprint::json("POST", "/v1/numbers/1/messages", &serde_json::json!({})),
            Duration::from_secs(60),
            Duration::from_secs(3600),
            render,
        )
        .await
        .unwrap();
        let Admission::Claimed(claim) = admission else {
            panic!("not claimed")
        };
        claim
    }

    /// A success and a failure that may have been sent are kept; a failure
    /// that proves nothing was sent releases the key. Decisive: the
    /// `may_have_been_sent` guard.
    #[tokio::test]
    async fn a_claim_settles_by_what_the_outcome_proves() {
        let records = Arc::new(Recorded::default());
        claimed(&records)
            .await
            .settle(Outcome::Answered {
                status: 202,
                body: b"{}",
            })
            .await;
        let sent = ServiceError::new("timeout").with_may_have_been_sent(true);
        claimed(&records).await.settle(Outcome::Failed(&sent)).await;
        let refused = ServiceError::new("customer_service_window_closed");
        claimed(&records)
            .await
            .settle(Outcome::Failed(&refused))
            .await;
        assert_eq!(
            *records.0.lock().unwrap(),
            [
                Call::Complete(202, b"{}".to_vec()),
                Call::Complete(504, b"timeout".to_vec()),
                Call::Release,
            ]
        );
    }

    /// A claim dropped before it settled keeps a `504 timeout`, from a task
    /// of its own. Decisive: the `Drop`.
    #[tokio::test]
    async fn a_claim_cut_before_it_settled_is_kept_as_a_timeout() {
        let records = Arc::new(Recorded::default());
        drop(claimed(&records).await);
        for _ in 0..100 {
            if !records.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            *records.0.lock().unwrap(),
            [Call::Complete(504, b"timeout".to_vec())]
        );
    }

    /// A repeat's answer follows the record: another request's fingerprint
    /// first, then its state.
    #[test]
    fn a_repeat_is_answered_by_the_record_it_met() {
        let mine = Fingerprint::json("POST", "/v1/numbers/1/messages", &serde_json::json!({}));
        let record = |fingerprint: &Fingerprint, state| IdempotencyRecord {
            fingerprint: *fingerprint.as_bytes(),
            state,
        };
        let other = Fingerprint::json("POST", "/v1/numbers/2/messages", &serde_json::json!({}));
        let done = IdempotencyState::Completed {
            status: 202,
            body: b"{}".to_vec(),
        };
        assert_eq!(
            Repeat::of(record(&other, done.clone()), &mine),
            Repeat::Reused
        );
        assert_eq!(
            Repeat::of(
                record(
                    &mine,
                    IdempotencyState::InProgress {
                        lease_expired: false
                    }
                ),
                &mine
            ),
            Repeat::InProgress
        );
        assert_eq!(
            Repeat::of(
                record(
                    &mine,
                    IdempotencyState::InProgress {
                        lease_expired: true
                    }
                ),
                &mine
            ),
            Repeat::OutcomeUnknown
        );
        let replay = Repeat::of(record(&mine, done), &mine);
        assert_eq!(replay.outcome(), "replayed");
        assert_eq!(replay.into_answer().unwrap(), (202, b"{}".to_vec()));
        let in_progress = Repeat::InProgress.into_answer().unwrap_err();
        assert_eq!(in_progress.code(), "idempotency_in_progress");
        assert!(in_progress.is_retryable() && in_progress.may_have_been_sent());
        let unknown = Repeat::OutcomeUnknown.into_answer().unwrap_err();
        assert_eq!(unknown.code(), "outcome_unknown");
        assert!(!unknown.is_retryable() && unknown.may_have_been_sent());
        assert_eq!(
            Repeat::Reused.into_answer().unwrap_err().code(),
            "idempotency_key_reused"
        );
    }

    /// Fingerprints are stored with their key (`wa_server_idempotency`,
    /// kept `WA_SERVER_IDEMPOTENCY_TTL`): derived otherwise after an
    /// upgrade, a caller repeating a request across it with the same key
    /// gets `422 idempotency_key_reused`. Known answers, computed apart
    /// from this code:
    //
    // python3 - <<'EOF'
    // import hashlib, struct
    // def framed(h, b): h.update(struct.pack('>Q', len(b))); h.update(b)
    // def canonical(h, v):
    //     if v is None: h.update(b'n')
    //     elif isinstance(v, bool): h.update(b't' if v else b'f')
    //     elif isinstance(v, int): h.update(b'#'); framed(h, str(v).encode())
    //     elif isinstance(v, str): h.update(b's'); framed(h, v.encode())
    //     elif isinstance(v, list):
    //         h.update(b'[' + struct.pack('>Q', len(v)))
    //         for x in v: canonical(h, x)
    //     else:
    //         h.update(b'{' + struct.pack('>Q', len(v)))
    //         for k in sorted(v, key=str.encode): framed(h, k.encode()); canonical(h, v[k])
    // h = hashlib.sha256()
    // for b in (b'json', b'POST', b'/v1/numbers/106540352242922/messages'): framed(h, b)
    // canonical(h, {"to": {"phone": "+15551234567"}, "type": "text",
    //     "text": {"body": "hi", "preview_url": False}, "tags": [None, True, 1, "1"]})
    // print(h.hexdigest())
    // h = hashlib.sha256()
    // for b in (b'parts', b'POST', b'/v1/numbers/106540352242922/media',
    //           b'type', b'image/png', b'file', b'\x89PNG'): framed(h, b)
    // print(h.hexdigest())
    // EOF
    //
    // Decisive: each tag (`json`, `parts`, `n`, `t`, `#`, `s`, `[`, `{`),
    // the length framing, and the order of an object's keys.
    #[test]
    fn fingerprints_are_pinned() {
        let body = serde_json::json!({
            "to": {"phone": "+15551234567"},
            "type": "text",
            "text": {"body": "hi", "preview_url": false},
            "tags": [null, true, 1, "1"],
        });
        let json = Fingerprint::json("POST", "/v1/numbers/106540352242922/messages", &body);
        assert_eq!(
            hex::encode(json.as_bytes()),
            "e83cc62715cb87890e03ddaa5d57b655ced695738930fc8c2ca164af6139f7d4"
        );
        let parts = Fingerprint::parts(
            "POST",
            "/v1/numbers/106540352242922/media",
            &[("type", b"image/png"), ("file", b"\x89PNG")],
        );
        assert_eq!(
            hex::encode(parts.as_bytes()),
            "04edb1c222d03832376711c1ba9450a54e08c14793501b90e48af0fc13788db9"
        );
    }

    /// The method's name is hashed, whatever type carries it.
    #[test]
    fn a_fingerprint_hashes_the_methods_name() {
        let body = serde_json::json!({"a": 1});
        assert_eq!(
            Fingerprint::json("POST", "/p", &body),
            Fingerprint::json(&String::from("POST"), "/p", &body)
        );
        assert_ne!(
            Fingerprint::json("POST", "/p", &body),
            Fingerprint::json("PUT", "/p", &body)
        );
    }

    /// `Debug` of a kept answer shows its length, never its bytes: the
    /// body is the caller's data (a recipient, a message), and a `{:?}`
    /// in a log line or a panic would otherwise print it. Decisive: each
    /// hand-written `Debug`.
    #[test]
    fn debug_shows_a_kept_bodys_length_never_its_bytes() {
        let body = br#"{"messages":[{"id":"wamid.SECRET-BODY-7Q"}],"to":"+15551234567"}"#.to_vec();
        let text = String::from_utf8(body.clone()).unwrap();
        let bytes = format!("{body:?}");
        let bytes = &bytes[1..bytes.len() - 1];
        let state = IdempotencyState::Completed {
            status: 200,
            body: body.clone(),
        };
        let record = IdempotencyRecord {
            fingerprint: [7; 32],
            state: state.clone(),
        };
        let printed = [
            format!("{state:?}"),
            format!("{record:?}"),
            format!("{:?}", IdempotencyClaim::Existing(record.clone())),
            format!("{:?}", Repeat::of(record, &Fingerprint([7; 32]))),
            format!(
                "{:?}",
                Outcome::Answered {
                    status: 200,
                    body: &body,
                }
            ),
        ];
        for debug in &printed {
            assert!(debug.contains("status: 200"), "{debug}");
            assert!(
                debug.contains(&format!("body_len: {}", body.len())),
                "{debug}"
            );
            assert!(!debug.contains("SECRET-BODY"), "the text: {debug}");
            assert!(!debug.contains(&text), "the text: {debug}");
            assert!(!debug.contains(bytes), "the bytes: {debug}");
            assert!(!debug.contains("7, 7, 7"), "the fingerprint: {debug}");
        }
        assert!(printed[3].starts_with("Replay"), "{}", printed[3]);
    }
}
