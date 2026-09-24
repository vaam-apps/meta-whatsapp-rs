//! OTP service tests.
//!
//! The store is the real `MemoryKvStore` behind a wrapper that (1) records
//! every write, (2) yields after reads and compare-and-swaps so concurrent
//! callers interleave in the windows a non-atomic implementation would lose
//! in (the concurrency tests run on a multi-thread runtime), and (3) can
//! inject a concurrent writer's effect right before a given
//! compare-and-swap, so the same races are also tested deterministically.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use http::Method;
use pretty_assertions::assert_eq;
use serde_json::json;
use time::macros::datetime;
use wa_adapters::store::MemoryKvStore;
use wa_core::clock::ManualClock;
use wa_core::error::TransportError;
use wa_core::store::{StoreKey, Versioned};
use wa_core::testing::{RecordedRequest, ScriptedTransport};
use wa_core::{ErrorKind, GraphApiError};

use super::*;
use crate::RetryPolicy;

/// The docs' example number, in the E.164 form the service requires.
const PHONE: &str = "+12015553931";
const DIGITS: &str = "12015553931";
const PEPPER: &[u8] = b"0123456789abcdef0123456789abcdef-test-pepper";

/// What a concurrent writer does to the challenge record, injected right
/// before a compare-and-swap on it.
#[derive(Debug, Clone, Copy)]
enum Interference {
    /// Other guesses were counted, up to this many attempts.
    SetAttempts(u32),
    /// Another verifier consumed the challenge.
    Consume,
    /// A new code was issued: same key, another challenge.
    Replace,
    /// A concurrent issue wrote its challenge, sent at this UNIX ms.
    Reissue(i64),
}

#[derive(Debug)]
struct RecordingKv {
    inner: MemoryKvStore,
    writes: Mutex<Vec<(StoreKey, Vec<u8>)>>,
    /// `(challenge compare-and-swaps to let through first, what to do
    /// before the next one)`.
    armed: Mutex<Option<(usize, Interference)>>,
}

impl RecordingKv {
    fn record(&self, key: &StoreKey, value: &[u8]) {
        self.writes
            .lock()
            .unwrap()
            .push((key.clone(), value.to_vec()));
    }

    fn arm(&self, skip: usize, what: Interference) {
        *self.armed.lock().unwrap() = Some((skip, what));
    }

    /// The interference due before this compare-and-swap, if any.
    fn due(&self, key: &StoreKey) -> Option<Interference> {
        if key.namespace() != NAMESPACE {
            return None;
        }
        let mut armed = self.armed.lock().unwrap();
        match armed.as_mut() {
            Some((0, what)) => {
                let what = *what;
                *armed = None;
                Some(what)
            }
            Some((skip, _)) => {
                *skip -= 1;
                None
            }
            None => None,
        }
    }

    async fn interfere(&self, key: &StoreKey, what: Interference) {
        match what {
            Interference::SetAttempts(attempts) => {
                let current = self.inner.get(key).await.unwrap().unwrap();
                let mut record: StoredChallenge = serde_json::from_slice(&current.value).unwrap();
                record.attempts = attempts;
                let bytes = serde_json::to_vec(&record).unwrap();
                self.inner.put(key, bytes, Expiry::Keep).await.unwrap();
            }
            Interference::Consume => {
                assert!(self.inner.delete(key).await.unwrap());
            }
            Interference::Replace | Interference::Reissue(_) => {
                let current = self.inner.get(key).await.unwrap().unwrap();
                let mut record: StoredChallenge = serde_json::from_slice(&current.value).unwrap();
                record.id = "f".repeat(32);
                record.attempts = 0;
                if let Interference::Reissue(sent_at) = what {
                    record.sent_at = sent_at;
                }
                let bytes = serde_json::to_vec(&record).unwrap();
                self.inner.put(key, bytes, Expiry::Keep).await.unwrap();
            }
        }
    }
}

#[async_trait]
impl KvStore for RecordingKv {
    async fn get(&self, key: &StoreKey) -> std::result::Result<Option<Versioned>, StorageError> {
        let v = self.inner.get(key).await;
        tokio::task::yield_now().await;
        v
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> std::result::Result<u64, StorageError> {
        self.record(key, &value);
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> std::result::Result<Option<u64>, StorageError> {
        self.record(key, &value);
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> std::result::Result<Option<u64>, StorageError> {
        if let Some(what) = self.due(key) {
            self.interfere(key, what).await;
        }
        if let Some(v) = &new {
            self.record(key, v);
        }
        let written = self
            .inner
            .compare_and_swap(key, expected, new, expiry)
            .await;
        // Also yield between a write and the caller's next step, so a
        // concurrent caller can act on the new version first.
        tokio::task::yield_now().await;
        written
    }
    async fn delete(&self, key: &StoreKey) -> std::result::Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

struct Fixture {
    otp: OtpService,
    transport: ScriptedTransport,
    clock: ManualClock,
    kv: Arc<RecordingKv>,
}

fn client(transport: &ScriptedTransport, retry: RetryPolicy) -> Client {
    Client::builder()
        .transport(transport.clone())
        .access_token("TOKEN")
        .retry(retry)
        .build()
        .unwrap()
}

fn fixture(config: OtpConfig) -> Fixture {
    fixture_with(config, RetryPolicy::NONE, "105954558954427")
}

fn fixture_with(config: OtpConfig, retry: RetryPolicy, phone_number_id: &str) -> Fixture {
    let transport = ScriptedTransport::new();
    let clock = ManualClock::new(datetime!(2026-09-24 12:00 UTC));
    let kv = Arc::new(RecordingKv {
        inner: MemoryKvStore::with_clock(Arc::new(clock.clone())),
        writes: Mutex::new(Vec::new()),
        armed: Mutex::new(None),
    });
    let otp = OtpService::new(
        client(&transport, retry),
        phone_number_id,
        OtpTemplate::new("verification_code", "en_US"),
        kv.clone(),
        Arc::new(clock.clone()),
        OtpPepper::new(PEPPER).unwrap(),
        config,
    )
    .unwrap();
    Fixture {
        otp,
        transport,
        clock,
        kv,
    }
}

/// The send example response of the authentication pages.
fn accept(t: &ScriptedTransport) {
    t.push_json(
        200,
        json!({
          "messaging_product": "whatsapp",
          "contacts": [{"input": DIGITS, "wa_id": DIGITS}],
          "messages": [{"id": "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI4Qzc5QkNGNTc5NTMyMDU5QzEA"}]
        }),
    );
}

fn code_in(req: &RecordedRequest) -> String {
    let body = req.json().unwrap();
    body["template"]["components"][0]["parameters"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `code` with its first digit changed.
fn wrong(code: &str) -> String {
    let mut chars: Vec<char> = code.chars().collect();
    let d = chars[0].to_digit(10).unwrap();
    chars[0] = char::from_digit((d + 1) % 10, 10).unwrap();
    chars.into_iter().collect()
}

fn user() -> Recipient {
    Recipient::phone(PHONE)
}

async fn issue(f: &Fixture) -> (Challenge, String) {
    accept(&f.transport);
    let outcome = f.otp.issue(&user(), "login").await.unwrap();
    let IssueOutcome::Sent(challenge) = outcome else {
        panic!("expected a send, got {outcome:?}")
    };
    let code = code_in(&f.transport.last_request().unwrap());
    (challenge, code)
}

async fn stored(f: &Fixture) -> Option<StoredChallenge> {
    let key = f.otp.key(&Phone::of(&user()).unwrap(), "login").unwrap();
    f.otp.challenges.get(&key).await.unwrap().map(|(r, _, _)| r)
}

fn is_recipient_error(e: &Error) -> bool {
    matches!(e, Error::Validation(v) if v.field == "recipient")
}

#[tokio::test]
async fn issue_sends_the_documented_payload_then_verifies_once() {
    let f = fixture(OtpConfig::default());
    let (challenge, code) = issue(&f).await;

    let req = f.transport.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/105954558954427/messages");
    assert_eq!(req.bearer(), Some("TOKEN"));
    // copy-code/one-tap/zero-tap pages, send example request (`to` in the
    // E.164 form this service requires).
    assert_eq!(
        req.json(),
        Some(json!({
          "messaging_product": "whatsapp",
          "recipient_type": "individual",
          "to": PHONE,
          "type": "template",
          "template": {
            "name": "verification_code",
            "language": {"code": "en_US"},
            "components": [
              {"type": "body", "parameters": [{"type": "text", "text": code}]},
              {"type": "button", "sub_type": "url", "index": "0", "parameters": [{"type": "text", "text": code}]}
            ]
          }
        }))
    );
    assert_eq!(f.transport.remaining(), 0);
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(challenge.id.len(), 32);
    assert_eq!(challenge.expires_at, datetime!(2026-09-24 12:10 UTC));
    assert_eq!(
        challenge.message_id.as_str(),
        "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBI4Qzc5QkNGNTc5NTMyMDU5QzEA"
    );

    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Verified
    );
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::NotFound,
        "single use"
    );
    assert!(stored(&f).await.is_none());
}

#[tokio::test]
async fn the_code_goes_to_exactly_the_number_it_is_bound_to() {
    let f = fixture(OtpConfig::default());
    accept(&f.transport);
    let formatted = Recipient::phone(" +1 (201) 555-3931 ");
    let IssueOutcome::Sent(_) = f.otp.issue(&formatted, "login").await.unwrap() else {
        panic!("expected a send")
    };
    let req = f.transport.last_request().unwrap();
    let body = req.json().unwrap();
    assert_eq!(body["to"], json!(PHONE), "canonical E.164 is sent");
    let code = code_in(&req);
    // Separators do not change the destination, so they do not change the
    // key either; a BSUID next to the number is not sent.
    let both = Recipient::PhoneAndUser {
        phone: PHONE.into(),
        user: "US.13491208655302741918".into(),
    };
    assert_eq!(
        f.otp.verify(&both, "login", &code).await.unwrap(),
        VerifyOutcome::Verified
    );

    let f = fixture(OtpConfig::default());
    accept(&f.transport);
    let _ = f.otp.issue(&both, "login").await.unwrap();
    let body = f.transport.last_request().unwrap().json().unwrap();
    assert_eq!(body["to"], json!(PHONE));
    assert!(body.get("recipient").is_none(), "{body}");
}

#[tokio::test]
async fn numbers_without_their_plus_or_not_e164_are_refused() {
    let f = fixture(OtpConfig::default());
    // Without `+`, Meta prepends the sending number's country code: these
    // digits would reach +<business cc>12015553931, a different person.
    for raw in [
        DIGITS,
        "001 201 555 3931",
        "+0044 20 7946 0958",
        "+1 201 555 3931 ext 2",
        "+1.201.555.3931",
        "+",
        "+ () -",
        "+1234567890123456",
        "",
    ] {
        let r = Recipient::phone(raw);
        let e = f.otp.issue(&r, "login").await.unwrap_err();
        assert!(is_recipient_error(&e), "{raw}: {e}");
        assert!(
            !e.to_string().contains("201"),
            "error echoes the number: {e}"
        );
        let e = f.otp.verify(&r, "login", "123456").await.unwrap_err();
        assert!(is_recipient_error(&e), "{raw}: {e}");
    }
    assert!(f.transport.requests().is_empty());
    assert!(f.kv.writes.lock().unwrap().is_empty());
    // 15 digits is the E.164 maximum.
    accept(&f.transport);
    assert!(matches!(
        f.otp
            .issue(&Recipient::phone("+123456789012345"), "login")
            .await
            .unwrap(),
        IssueOutcome::Sent(_)
    ));
}

#[tokio::test]
async fn a_code_sent_without_plus_can_never_verify_the_plus_number() {
    // The draft keyed "12015553931" and "+12015553931" identically although
    // Meta delivers them to different people. Now the first is refused, so
    // no code can be bound to it.
    let f = fixture(OtpConfig::default());
    let e = f
        .otp
        .issue(&Recipient::phone(DIGITS), "login")
        .await
        .unwrap_err();
    assert!(is_recipient_error(&e), "{e}");
    assert_eq!(
        f.otp.verify(&user(), "login", "123456").await.unwrap(),
        VerifyOutcome::NotFound
    );
}

#[tokio::test]
async fn recipients_without_a_phone_number_are_refused_before_any_write() {
    let f = fixture(OtpConfig::default());
    for r in [
        Recipient::user("US.13491208655302741918"),
        Recipient::group("Y2FwaV9ncm91cDox"),
    ] {
        let e = f.otp.issue(&r, "login").await.unwrap_err();
        assert!(is_recipient_error(&e), "{e}");
        // The documented mapping: a local `InvalidParameter` standing for
        // Meta's 131062 (`RecipientNotSupported`).
        assert_eq!(e.kind(), ErrorKind::InvalidParameter);
        assert!(e.to_string().contains("131062"), "{e}");
        let e = f.otp.verify(&r, "login", "123456").await.unwrap_err();
        assert!(is_recipient_error(&e), "{e}");
    }
    assert!(f.transport.requests().is_empty());
    assert!(f.kv.writes.lock().unwrap().is_empty(), "no slot taken");
}

#[tokio::test]
async fn purposes_are_independent() {
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    assert_eq!(
        f.otp
            .verify(&user(), "reset_password", &code)
            .await
            .unwrap(),
        VerifyOutcome::NotFound
    );
    accept(&f.transport);
    assert!(matches!(
        f.otp.issue(&user(), "reset_password").await.unwrap(),
        IssueOutcome::Sent(_)
    ));
    let e = f.otp.issue(&user(), "").await.unwrap_err();
    assert!(
        matches!(e, Error::Validation(ref v) if v.field == "purpose"),
        "{e}"
    );
}

#[tokio::test]
async fn wrong_codes_count_down_then_lock_even_the_right_code() {
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    let bad = wrong(&code);
    for left in (0..5).rev() {
        assert_eq!(
            f.otp.verify(&user(), "login", &bad).await.unwrap(),
            VerifyOutcome::Invalid {
                attempts_left: left
            }
        );
    }
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::TooManyAttempts
    );
    assert_eq!(stored(&f).await.unwrap().attempts, 5);
}

#[tokio::test]
async fn the_last_allowed_attempt_can_still_succeed() {
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    for _ in 0..4 {
        let _ = f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap();
    }
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Verified
    );
}

#[tokio::test]
async fn codes_expire_at_the_ttl() {
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    f.clock.advance(Duration::from_secs(10 * 60 - 1));
    assert_eq!(
        f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap(),
        VerifyOutcome::Invalid { attempts_left: 4 }
    );
    f.clock.advance(Duration::from_secs(1));
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Expired
    );
    // Long after, the record is gone from the store.
    f.clock.advance(Duration::from_hours(1));
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::NotFound
    );
}

#[tokio::test]
async fn resend_cooldown_then_the_new_code_replaces_the_old() {
    let f = fixture(OtpConfig::default());
    let (_, old) = issue(&f).await;
    f.clock.advance(Duration::from_secs(10));
    assert_eq!(
        f.otp.issue(&user(), "login").await.unwrap(),
        IssueOutcome::CoolingDown {
            retry_after: Duration::from_secs(20)
        }
    );
    assert_eq!(
        f.transport.requests().len(),
        1,
        "nothing sent while cooling down"
    );
    f.clock.advance(Duration::from_secs(20));
    let (_, new) = issue(&f).await;
    assert_eq!(f.transport.requests().len(), 2);
    if old != new {
        assert!(matches!(
            f.otp.verify(&user(), "login", &old).await.unwrap(),
            VerifyOutcome::Invalid { .. }
        ));
    }
    assert_eq!(
        f.otp.verify(&user(), "login", &new).await.unwrap(),
        VerifyOutcome::Verified
    );
}

#[tokio::test]
async fn the_sixth_code_in_an_hour_is_refused_and_the_window_slides() {
    // The default config: 30 s cooldown, 5 codes per rolling hour.
    assert_eq!(OtpConfig::default().issue_limit, Some(IssueLimit::DEFAULT));
    assert_eq!(
        IssueLimit::DEFAULT,
        IssueLimit {
            max_issues: 5,
            window: Duration::from_hours(1)
        }
    );
    let f = fixture(OtpConfig::default());
    // 12:00, 12:10, 12:20, 12:30, 12:40.
    let mut current = String::new();
    for i in 0..5 {
        if i > 0 {
            f.clock.advance(Duration::from_mins(10));
        }
        current = issue(&f).await.1;
    }
    // 12:45. The 12:40 code is still outstanding (it expires at 12:50).
    f.clock.advance(Duration::from_mins(5));
    assert_eq!(
        f.otp.issue(&user(), "login").await.unwrap(),
        IssueOutcome::RateLimited {
            retry_after: Duration::from_mins(15)
        },
        "the 12:00 code leaves the window at 13:00"
    );
    assert_eq!(f.transport.requests().len(), 5, "nothing sent");
    assert!(
        matches!(
            f.otp
                .verify(&user(), "login", &wrong(&current))
                .await
                .unwrap(),
            VerifyOutcome::Invalid { .. }
        ),
        "a refused issue leaves the outstanding code in place"
    );
    assert_eq!(
        f.otp.verify(&user(), "login", &current).await.unwrap(),
        VerifyOutcome::Verified
    );

    // 13:00: the 12:00 code has left the window, one slot is free.
    f.clock.advance(Duration::from_mins(15));
    let _ = issue(&f).await;
    // 13:01: 12:10…12:40 and 13:00 are in the window. A fixed window reset
    // at 13:00 would allow this one; the rolling window does not.
    f.clock.advance(Duration::from_mins(1));
    assert_eq!(
        f.otp.issue(&user(), "login").await.unwrap(),
        IssueOutcome::RateLimited {
            retry_after: Duration::from_mins(9)
        }
    );
    // 13:10: the 12:10 code leaves.
    f.clock.advance(Duration::from_mins(9));
    let _ = issue(&f).await;
    assert_eq!(f.transport.requests().len(), 7);
    assert_eq!(f.transport.remaining(), 0);
}

#[tokio::test]
async fn the_issue_limit_can_be_turned_off_explicitly() {
    let f = fixture(OtpConfig {
        issue_limit: None,
        ..OtpConfig::default()
    });
    for _ in 0..12 {
        let _ = issue(&f).await;
        f.clock.advance(Duration::from_secs(30));
    }
    assert_eq!(f.transport.requests().len(), 12);
    assert!(
        f.kv.writes
            .lock()
            .unwrap()
            .iter()
            .all(|(k, _)| k.namespace() == NAMESPACE),
        "no issue log without a limit"
    );
}

#[tokio::test]
async fn a_rejected_send_removes_the_challenge() {
    let f = fixture(OtpConfig::default());
    f.transport.push_json(
        400,
        json!({"error": {"message": "(#132001) Template name does not exist in the translation",
                          "type": "OAuthException", "code": 132001}}),
    );
    let err = f.otp.issue(&user(), "login").await.unwrap_err();
    assert_eq!(err.graph().map(|g| g.code), Some(132001));
    let code = code_in(&f.transport.last_request().unwrap());
    assert!(!err.to_string().contains(&code), "{err}");
    assert!(stored(&f).await.is_none());
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::NotFound
    );
    // No cooldown left behind by a message that never went out.
    let _ = issue(&f).await;

    // A connection that never opened sent nothing either.
    let f = fixture(OtpConfig::default());
    f.transport
        .push_error(|| TransportError::Connect(anyhow::anyhow!("refused")));
    assert!(f.otp.issue(&user(), "login").await.is_err());
    assert!(stored(&f).await.is_none());
}

#[tokio::test]
async fn a_send_that_may_have_arrived_keeps_the_challenge() {
    // Even with retries on, a timed-out send is not replayed (it may have
    // been delivered), and the code stays verifiable for the same reason.
    let f = fixture_with(
        OtpConfig::default(),
        RetryPolicy::default(),
        "105954558954427",
    );
    f.transport.push_error(|| TransportError::Timeout);
    let err = f.otp.issue(&user(), "login").await.unwrap_err();
    assert!(
        matches!(err, Error::Transport(TransportError::Timeout)),
        "{err}"
    );
    assert_eq!(f.transport.requests().len(), 1, "a send is never replayed");
    let code = code_in(&f.transport.last_request().unwrap());
    assert!(matches!(
        f.otp.issue(&user(), "login").await.unwrap(),
        IssueOutcome::CoolingDown { .. }
    ));
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Verified
    );

    // A 2xx whose body cannot be read: Meta accepted the message.
    let f = fixture(OtpConfig::default());
    f.transport
        .push_json(200, json!({"messaging_product": "whatsapp"}));
    let err = f.otp.issue(&user(), "login").await.unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
    let code = code_in(&f.transport.last_request().unwrap());
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Verified
    );

    // An unparseable 2xx: kept too, and the error does not quote the body,
    // which names the recipient.
    let f = fixture(OtpConfig::default());
    f.transport.push_bytes(
        200,
        "application/json",
        format!(r#"{{"contacts":[{{"input":"{PHONE}","wa_id":"{DIGITS}"}}],"messages":"#),
    );
    let err = f.otp.issue(&user(), "login").await.unwrap_err();
    let Error::Decode { body_snippet, .. } = &err else {
        panic!("{err}")
    };
    assert!(!body_snippet.contains(DIGITS), "{body_snippet}");
    assert!(!format!("{err} {err:?}").contains(DIGITS), "{err:?}");
    assert!(stored(&f).await.is_some());

    // A gateway 5xx without a Graph error: unknown, kept.
    let f = fixture(OtpConfig::default());
    f.transport
        .push_bytes(502, "text/html", "<html>bad gateway</html>");
    let err = f.otp.issue(&user(), "login").await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 502, .. }), "{err}");
    assert!(stored(&f).await.is_some());
}

#[test]
fn only_provable_rejections_count_as_not_sent() {
    let http = |status| Error::Http {
        status,
        body_snippet: String::new(),
    };
    let graph = |code, status| {
        let mut e = GraphApiError::new(code, "x");
        e.http_status = Some(status);
        Error::from(e)
    };
    for (error, sent) in [
        (graph(131026, 400), false),
        (graph(132001, 404), false),
        // Throttled before processing, whatever the status.
        (graph(130429, 400), false),
        (graph(130429, 503), false),
        // A 5xx Graph error proves nothing (also never replayed).
        (graph(131000, 500), true),
        (graph(2, 503), true),
        (GraphApiError::new(131026, "no status").into(), false),
        (ValidationError::new("path", "bad").into(), false),
        (TransportError::Build("bad header".into()).into(), false),
        (
            TransportError::Connect(anyhow::anyhow!("refused")).into(),
            false,
        ),
        (http(404), false),
        (http(429), false),
        (http(502), true),
        (TransportError::Timeout.into(), true),
        (
            TransportError::Backend(anyhow::anyhow!("reset")).into(),
            true,
        ),
        (
            Error::decode(
                "x",
                <serde_json::Error as serde::de::Error>::custom("x"),
                b"",
            ),
            true,
        ),
    ] {
        assert_eq!(may_have_been_sent(&error), sent, "{error}");
    }
}

#[tokio::test]
async fn ids_cannot_escape_their_path_segment() {
    // An id with `/` stays one segment: it cannot address another object.
    let f = fixture_with(
        OtpConfig::default(),
        RetryPolicy::NONE,
        "105954558954427/subscribed_apps",
    );
    accept(&f.transport);
    let _ = f.otp.issue(&user(), "login").await.unwrap();
    assert_eq!(
        f.transport.last_request().unwrap().path(),
        "/v25.0/105954558954427%2Fsubscribed_apps/messages"
    );
    // Ids that would be dropped or popped by URL normalization are refused
    // when the service is built.
    for bad in ["", ".", ".."] {
        let t = ScriptedTransport::new();
        let clock = ManualClock::new(datetime!(2026-09-24 12:00 UTC));
        let e = OtpService::new(
            client(&t, RetryPolicy::NONE),
            bad,
            OtpTemplate::new("verification_code", "en_US"),
            Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone()))),
            Arc::new(clock),
            OtpPepper::new(PEPPER).unwrap(),
            OtpConfig::default(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Config(_)), "{bad:?}: {e}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_wrong_guesses_never_exceed_max_attempts() {
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    let bad = wrong(&code);
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let otp = f.otp.clone();
            let bad = bad.clone();
            tokio::spawn(async move { otp.verify(&user(), "login", &bad).await.unwrap() })
        })
        .collect();
    let mut invalid = 0;
    let mut locked = 0;
    for t in tasks {
        match t.await.unwrap() {
            VerifyOutcome::Invalid { .. } => invalid += 1,
            VerifyOutcome::TooManyAttempts => locked += 1,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!((invalid, locked), (5, 15));
    assert_eq!(stored(&f).await.unwrap().attempts, 5);
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::TooManyAttempts
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_issues_send_once() {
    let f = fixture(OtpConfig::default());
    accept(&f.transport);
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let otp = f.otp.clone();
            tokio::spawn(async move { otp.issue(&user(), "login").await.unwrap() })
        })
        .collect();
    let mut sent = 0;
    for t in tasks {
        match t.await.unwrap() {
            IssueOutcome::Sent(_) => sent += 1,
            IssueOutcome::CoolingDown { .. } | IssueOutcome::RateLimited { .. } => {}
        }
    }
    assert_eq!(sent, 1);
    assert_eq!(f.transport.requests().len(), 1);
    let code = code_in(&f.transport.last_request().unwrap());
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Verified
    );
}

#[tokio::test]
async fn a_reissue_that_loses_the_race_cools_down_instead_of_overwriting() {
    // A code exists and its cooldown is over. Between this issue's read and
    // its write, a concurrent issue writes a fresh challenge: this one must
    // back off, not overwrite it and send a second code.
    let f = fixture(OtpConfig::default());
    let _ = issue(&f).await;
    f.clock.advance(Duration::from_secs(30));
    f.kv.arm(0, Interference::Reissue(to_ms(f.clock.now())));
    assert_eq!(
        f.otp.issue(&user(), "login").await.unwrap(),
        IssueOutcome::CoolingDown {
            retry_after: Duration::from_secs(30)
        }
    );
    assert_eq!(f.transport.requests().len(), 1, "no second message");
    assert_eq!(stored(&f).await.unwrap().id, "f".repeat(32));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reissues_send_once() {
    let f = fixture(OtpConfig::default());
    let _ = issue(&f).await;
    f.clock.advance(Duration::from_secs(30));
    accept(&f.transport);
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let otp = f.otp.clone();
            tokio::spawn(async move { otp.issue(&user(), "login").await.unwrap() })
        })
        .collect();
    let mut sent = 0;
    for t in tasks {
        match t.await.unwrap() {
            IssueOutcome::Sent(_) => sent += 1,
            IssueOutcome::CoolingDown { .. } | IssueOutcome::RateLimited { .. } => {}
        }
    }
    assert_eq!(sent, 1);
    assert_eq!(f.transport.requests().len(), 2);
    let code = code_in(&f.transport.last_request().unwrap());
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Verified
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_issues_never_exceed_the_issue_limit() {
    // No cooldown, so only the issue limit stands between 20 concurrent
    // requests and 20 messages.
    let f = fixture(OtpConfig {
        resend_cooldown: Duration::ZERO,
        ..OtpConfig::default()
    });
    for _ in 0..5 {
        accept(&f.transport);
    }
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let otp = f.otp.clone();
            tokio::spawn(async move { otp.issue(&user(), "login").await.unwrap() })
        })
        .collect();
    let (mut sent, mut limited) = (0, 0);
    for t in tasks {
        match t.await.unwrap() {
            IssueOutcome::Sent(_) => sent += 1,
            IssueOutcome::RateLimited { .. } => limited += 1,
            other @ IssueOutcome::CoolingDown { .. } => panic!("{other:?}"),
        }
    }
    assert_eq!((sent, limited), (5, 15));
    assert_eq!(f.transport.remaining(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_right_guesses_verify_exactly_once() {
    let f = fixture(OtpConfig {
        max_attempts: 50,
        ..OtpConfig::default()
    });
    let (_, code) = issue(&f).await;
    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let otp = f.otp.clone();
            let code = code.clone();
            tokio::spawn(async move { otp.verify(&user(), "login", &code).await.unwrap() })
        })
        .collect();
    let mut verified = 0;
    for t in tasks {
        match t.await.unwrap() {
            VerifyOutcome::Verified => verified += 1,
            VerifyOutcome::NotFound => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(verified, 1);
}

#[tokio::test]
async fn a_guess_that_loses_the_count_race_is_rechecked_not_compared() {
    // Between this guess's read and its count, other guesses use up every
    // attempt. The right code must not be compared for free.
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    f.kv.arm(0, Interference::SetAttempts(5));
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::TooManyAttempts
    );
    assert_eq!(stored(&f).await.unwrap().attempts, 5);
}

#[tokio::test]
async fn a_code_consumed_by_a_concurrent_guess_does_not_verify_again() {
    // This guess is counted, then another correct guess consumes the
    // challenge before this one does.
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    f.kv.arm(1, Interference::Consume);
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::NotFound
    );
}

#[tokio::test]
async fn a_code_replaced_while_it_is_consumed_does_not_verify() {
    // This guess is counted, then a new code replaces the challenge before
    // the consume: the old code must not verify, nor delete the new one.
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    f.kv.arm(1, Interference::Replace);
    assert_eq!(
        f.otp.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::NotFound
    );
    let left = stored(&f).await.expect("the new challenge survives");
    assert_eq!(left.id, "f".repeat(32));
    assert_eq!(left.attempts, 0);
}

#[tokio::test]
async fn the_store_never_sees_the_code_or_the_phone_number() {
    let f = fixture(OtpConfig {
        code_length: 8,
        ..OtpConfig::default()
    });
    let (_, code) = issue(&f).await;
    let _ = f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap();
    let writes = f.kv.writes.lock().unwrap().clone();
    let namespaces: std::collections::BTreeSet<&str> =
        writes.iter().map(|(k, _)| k.namespace()).collect();
    assert_eq!(
        namespaces,
        [NAMESPACE, ISSUE_LOG_NAMESPACE].into_iter().collect()
    );
    for (key, value) in &writes {
        assert!(!key.key().contains(DIGITS), "{key}");
        assert!(!key.key().contains("login"), "{key}");
        assert_eq!(key.key().len(), 64, "hex HMAC-SHA256 key: {key}");
        assert!(key.key().bytes().all(|b| b.is_ascii_hexdigit()), "{key}");
        let text = String::from_utf8(value.clone()).unwrap();
        assert!(
            !text.contains(&code),
            "stored value contains the code: {text}"
        );
        assert!(!text.contains(DIGITS), "{text}");
    }
}

#[tokio::test]
async fn every_hmac_is_keyed_by_the_pepper() {
    // Same store, same clock, another pepper: the record is not found (the
    // key is keyed), and with the other pepper's key it still does not
    // match (the code hash is keyed).
    let f = fixture(OtpConfig::default());
    let (_, code) = issue(&f).await;
    let other = OtpService {
        pepper: OtpPepper::new(vec![7u8; 32]).unwrap(),
        ..f.otp.clone()
    };
    assert_eq!(
        other.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::NotFound
    );
    let phone = Phone::of(&user()).unwrap();
    let (mine, theirs) = (
        f.otp.key(&phone, "login").unwrap(),
        other.key(&phone, "login").unwrap(),
    );
    assert_ne!(mine, theirs);
    let (record, _, expiry) = f.otp.challenges.get(&mine).await.unwrap().unwrap();
    f.otp
        .challenges
        .put(&theirs, &record, expiry.map_or(Expiry::Never, Expiry::At))
        .await
        .unwrap();
    assert_eq!(
        other.verify(&user(), "login", &code).await.unwrap(),
        VerifyOutcome::Invalid { attempts_left: 4 }
    );
}

#[tokio::test]
async fn debug_output_never_contains_the_code_or_the_pepper() {
    let f = fixture(OtpConfig::default());
    let (challenge, code) = issue(&f).await;
    let outcome = f.otp.verify(&user(), "login", &wrong(&code)).await.unwrap();
    let pepper = String::from_utf8_lossy(PEPPER).into_owned();
    for text in [
        format!("{:?}", f.otp),
        format!("{challenge:?}"),
        format!("{outcome:?}"),
        format!("{:?}", IssueOutcome::Sent(challenge.clone())),
        format!(
            "{:?}",
            otp_template_message("verification_code", "en_US", &code)
        ),
        format!("{:?}", OtpPepper::new(PEPPER).unwrap()),
    ] {
        assert!(!text.contains(&code), "{text}");
        assert!(!text.contains(&pepper), "{text}");
        assert!(!text.contains(DIGITS), "{text}");
    }
}

#[tokio::test]
async fn huge_cooldowns_and_windows_saturate_instead_of_panicking() {
    let f = fixture(OtpConfig {
        resend_cooldown: Duration::MAX,
        issue_limit: Some(IssueLimit {
            max_issues: 1,
            window: Duration::MAX,
        }),
        ..OtpConfig::default()
    });
    let _ = issue(&f).await;
    assert!(matches!(
        f.otp.issue(&user(), "login").await.unwrap(),
        IssueOutcome::CoolingDown { .. }
    ));
}

#[test]
fn rejection_sampling_skips_the_biased_bytes() {
    let mut code = String::new();
    push_digits(
        &mut code,
        6,
        &[250, 0, 255, 9, 251, 10, 249, 252, 253, 254, 123, 7, 8],
    );
    assert_eq!(code, "090937");
    let mut all = String::new();
    push_digits(&mut all, 256, &(0..=255).collect::<Vec<u8>>());
    assert_eq!(all.len(), 250, "exactly the 250 unbiased bytes are used");
    for d in '0'..='9' {
        assert_eq!(all.chars().filter(|c| *c == d).count(), 25, "digit {d}");
    }
}

#[test]
fn random_codes_have_the_configured_length() {
    for len in 4..=8 {
        let code = random_code(len).unwrap();
        assert_eq!(code.len(), usize::from(len));
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
    }
}

#[test]
fn an_rng_failure_is_a_crypto_error() {
    assert!(matches!(
        rng_error(getrandom::Error::UNSUPPORTED),
        Error::Crypto(CryptoError::Rng)
    ));
}

#[test]
fn config_and_pepper_are_checked() {
    for bad in [
        OtpConfig {
            code_length: 3,
            ..OtpConfig::default()
        },
        OtpConfig {
            code_length: 9,
            ..OtpConfig::default()
        },
        OtpConfig {
            ttl: Duration::ZERO,
            ..OtpConfig::default()
        },
        OtpConfig {
            ttl: Duration::from_mins(91),
            ..OtpConfig::default()
        },
        OtpConfig {
            max_attempts: 0,
            ..OtpConfig::default()
        },
        OtpConfig {
            issue_limit: Some(IssueLimit {
                max_issues: 0,
                window: Duration::from_secs(1),
            }),
            ..OtpConfig::default()
        },
        OtpConfig {
            issue_limit: Some(IssueLimit {
                max_issues: 1,
                window: Duration::ZERO,
            }),
            ..OtpConfig::default()
        },
    ] {
        assert!(matches!(bad.validate(), Err(Error::Config(_))), "{bad:?}");
    }
    assert!(OtpConfig::default().validate().is_ok());
    assert!(OtpPepper::new(vec![7u8; 31]).is_err());
    assert!(OtpPepper::new(vec![7u8; 32]).is_ok());
    assert!(OtpPepper::from_secret(SecretBytes::new(vec![7u8; 31])).is_err());
    assert!(OtpPepper::from_secret(SecretBytes::new(vec![7u8; 32])).is_ok());
}
