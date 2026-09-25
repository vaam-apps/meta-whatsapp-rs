//! The `wa-rs` error tree.
//!
//! ```text
//! Error                      (root; every public fallible fn returns it)
//! ├── Api(GraphApiError)     Meta answered with a Graph error object → .kind() classifies it
//! ├── Http { status, .. }    Meta answered, but not with a Graph error object (e.g. an HTML 502)
//! ├── Transport(..)          no answer at all: DNS, TLS, timeout, reset
//! ├── Decode { .. }          an answer whose body did not match the expected shape
//! ├── Validation(..)         rejected locally, before any request was made
//! ├── Webhook(..)            bad signature, bad verify token, unparseable delivery
//! ├── Storage(..)            a `KvStore` / `ConversationStore` adapter failed
//! ├── Sink(..)               an `EventSink` adapter failed
//! ├── Crypto(..)             encryption/decryption (token vault, Flows endpoint)
//! ├── Config(..)             the client or a service was built with missing/invalid settings
//! ├── Credit(..)             a Solution Partner credit line step stopped: refused, busy, to reconcile, or a revocation part-way
//! ├── Step { step, source }  a multi-step flow (onboarding) failed part-way; `step` says where
//! └── Other(anyhow::Error)   anything an integrator raises that has no typed home
//! ```
//!
//! Leaf enums use `thiserror`; the opaque leaves (`Transport::Backend`,
//! `Storage::Backend`, `Sink::Delivery`, `Other`) carry an [`anyhow::Error`] so
//! adapters written outside this workspace can surface their own error types
//! without us having to know them.
//!
//! Decide on behaviour with [`Error::kind`] and [`Error::is_retryable`], never
//! by matching message strings.

mod credit;
mod graph;
mod leaf;

pub use credit::{CreditError, CreditRevocation, RevocationIncomplete};

pub use graph::{ErrorData, ErrorKind, GraphApiError, GraphErrorEnvelope};
pub use leaf::{
    ConfigError, CryptoError, SinkError, StorageError, TransportError, ValidationError,
    WebhookError,
};

/// Result alias used by every `wa-rs` crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Root of the error tree. See the [module docs](self) for the shape.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Meta returned a Graph API error object. Boxed: it is the largest
    /// variant by far and would otherwise bloat every `Result`.
    #[error(transparent)]
    Api(Box<GraphApiError>),

    /// Meta returned a non-success status without a Graph error object.
    #[error("unexpected HTTP {status} from Graph API: {body_snippet}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// First bytes of the body, for diagnostics. Never contains a token.
        body_snippet: String,
    },

    /// The request produced no HTTP response.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// A response body did not match the expected shape.
    #[error("could not decode {context}: {source}")]
    Decode {
        /// What was being decoded, e.g. `"send message response"`.
        context: &'static str,
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
        /// First bytes of the body, for diagnostics.
        body_snippet: String,
    },

    /// Input rejected locally before any request was made.
    #[error(transparent)]
    Validation(#[from] ValidationError),

    /// Webhook verification or parsing failed.
    #[error(transparent)]
    Webhook(#[from] WebhookError),

    /// A storage adapter failed.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// An event sink failed.
    #[error(transparent)]
    Sink(#[from] SinkError),

    /// Encryption or decryption failed.
    #[error(transparent)]
    Crypto(#[from] CryptoError),

    /// Missing or invalid configuration.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// A Solution Partner credit line step stopped (see [`CreditError`]):
    /// it carries whether to retry, whether anything reached Meta, and a
    /// revocation's report.
    #[error(transparent)]
    Credit(#[from] CreditError),

    /// A multi-step flow failed part-way through. Earlier steps took effect.
    #[error("step `{step}` failed: {source}")]
    Step {
        /// Stable, machine-readable step name (e.g. `"exchange_code"`).
        step: &'static str,
        /// What went wrong in that step.
        #[source]
        source: Box<Error>,
    },

    /// Anything else, raised by integrator code.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<GraphApiError> for Error {
    fn from(e: GraphApiError) -> Self {
        Self::Api(Box::new(e))
    }
}

impl Error {
    /// Wrap `self` as the failure of a named step of a multi-step flow.
    #[must_use]
    pub fn in_step(self, step: &'static str) -> Self {
        Self::Step {
            step,
            source: Box::new(self),
        }
    }

    /// Build a [`Error::Decode`] from a serde error and the raw body.
    pub fn decode(context: &'static str, source: serde_json::Error, body: &[u8]) -> Self {
        Self::Decode {
            context,
            source,
            body_snippet: snippet(body),
        }
    }

    /// The Graph API error, if this is (or wraps, through [`Error::Step`]) one.
    pub fn graph(&self) -> Option<&GraphApiError> {
        match self {
            Self::Api(e) => Some(e),
            Self::Step { source, .. } => source.graph(),
            _ => None,
        }
    }

    /// The credit line error, if this is (or wraps, through [`Error::Step`])
    /// one.
    pub fn credit(&self) -> Option<&CreditError> {
        match self {
            Self::Credit(e) => Some(e),
            Self::Step { source, .. } => source.credit(),
            _ => None,
        }
    }

    /// Classification of the failure. Non-API errors map to the closest kind.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Api(e) => e.kind(),
            Self::Http { status, .. } if *status >= 500 => ErrorKind::ServiceUnavailable,
            Self::Http { status: 429, .. } => ErrorKind::RateLimited,
            Self::Transport(_) => ErrorKind::ServiceUnavailable,
            // The inbox refuses free-form replies outside the 24h window
            // locally; report it like Meta's own 131047 so one condition has
            // one kind.
            Self::Validation(v) if v.is_customer_service_window_closed() => {
                ErrorKind::CustomerServiceWindowClosed
            }
            Self::Validation(_) => ErrorKind::InvalidParameter,
            Self::Credit(e) => e.kind(),
            Self::Step { source, .. } => source.kind(),
            _ => ErrorKind::Unknown,
        }
    }

    /// Whether repeating the *same* request later may succeed.
    ///
    /// This says nothing about whether it is *safe* to repeat: a transport
    /// error on a send may mean the message went out. The client's retry
    /// policy only replays non-idempotent requests on errors that prove the
    /// request was not processed (throttling), see `wa_client::RetryPolicy`.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api(e) => e.is_retryable(),
            Self::Http { status, .. } => *status >= 500 || *status == 429,
            Self::Transport(e) => e.is_retryable(),
            Self::Credit(e) => e.is_retryable(),
            Self::Step { source, .. } => source.is_retryable(),
            _ => false,
        }
    }
}

impl Error {
    /// Whether a failed **send** (message, OTP, marketing message) may
    /// nonetheless have been delivered, so resending risks a duplicate.
    ///
    /// `false` means Meta provably did nothing — a Graph error on a 4xx
    /// response (or built without a status), a throttling error on any
    /// status (the kinds for which
    /// [`ErrorKind::is_rejected_before_processing`] holds, which is also
    /// when the client replays a send), a local
    /// validation/configuration/crypto error, or a request that was never
    /// built or never connected — so it is safe to fix and resend. Storage,
    /// sink and webhook errors are `false` too: wa-rs raises them before a
    /// send, and after one it logs them instead of returning them (the
    /// inbox records a sent reply without failing). A [`CreditError`]
    /// decides for itself ([`CreditError::may_have_been_sent`]: a raced
    /// share, a share to reconcile, a revocation's `DELETE`s). `true` for a timeout,
    /// any other 5xx or non-4xx status, an answer that arrived but was
    /// unreadable or failed an integrity check, or anything unknown:
    /// reconcile with status webhooks (match on `biz_opaque_callback_data`)
    /// before sending again.
    ///
    /// The OTP service uses it too: a challenge is removed only when this
    /// is `false`.
    pub fn may_have_been_sent(&self) -> bool {
        // Exhaustive on purpose: a new variant has to decide here.
        match self {
            // A Graph error on a 1xx-3xx answer is as unknown as one on a
            // 5xx: only a 4xx is a rejection.
            Self::Api(e) => {
                e.http_status.is_some_and(|s| !(400..500).contains(&s))
                    && !e.kind().is_rejected_before_processing()
            }
            Self::Http { status, .. } => !(400..500).contains(status),
            Self::Transport(e) => match e {
                TransportError::Connect(_) | TransportError::Build(_) => false,
                // An integrity failure means a body came back: the request
                // reached the server.
                TransportError::Timeout
                | TransportError::Backend(_)
                | TransportError::Integrity(_) => true,
            },
            Self::Decode { .. } | Self::Other(_) => true,
            Self::Credit(e) => e.may_have_been_sent(),
            Self::Step { source, .. } => source.may_have_been_sent(),
            Self::Validation(_)
            | Self::Config(_)
            | Self::Crypto(_)
            | Self::Storage(_)
            | Self::Sink(_)
            | Self::Webhook(_) => false,
        }
    }
}

/// Truncate a body for inclusion in an error message (512 bytes, lossy UTF-8).
pub fn snippet(body: &[u8]) -> String {
    const MAX: usize = 512;
    let text = String::from_utf8_lossy(&body[..body.len().min(MAX)]);
    if body.len() > MAX {
        format!("{text}…")
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_wrapping_preserves_graph_error_and_kind() {
        let api = GraphApiError::new(131047, "Re-engagement message");
        let err = Error::from(api).in_step("send_welcome");
        assert_eq!(err.graph().map(|g| g.code), Some(131047));
        assert_eq!(err.kind(), ErrorKind::CustomerServiceWindowClosed);
        assert!(err.to_string().starts_with("step `send_welcome` failed"));
    }

    #[test]
    fn a_local_window_refusal_has_the_same_kind_as_131047() {
        let local = Error::from(ValidationError::customer_service_window_closed());
        let remote = Error::from(GraphApiError::new(131047, "Re-engagement message"));
        assert_eq!(local.kind(), remote.kind());
        assert_eq!(
            local.kind(),
            ErrorKind::CustomerServiceWindowClosed,
            "{local}"
        );
        // One definition: the field the constructor sets is the one kind()
        // and the predicate recognise.
        let Error::Validation(v) = &local else {
            panic!("{local}")
        };
        assert_eq!(v.field, ValidationError::CUSTOMER_SERVICE_WINDOW);
        assert!(v.is_customer_service_window_closed());
        assert!(!ValidationError::new("to", "x").is_customer_service_window_closed());
        assert_eq!(
            Error::from(ValidationError::new("body", "x")).kind(),
            ErrorKind::InvalidParameter
        );
    }

    #[test]
    fn may_have_been_sent_only_when_meta_could_have_acted() {
        let api = |status: Option<u16>| {
            let mut g = GraphApiError::new(131047, "x");
            g.http_status = status;
            Error::from(g)
        };
        let http = |status| Error::Http {
            status,
            body_snippet: String::new(),
        };
        let decode = Error::decode("send", serde_json::from_str::<u8>("x").unwrap_err(), b"x");
        let throttled = |code, status| {
            let mut g = GraphApiError::new(code, "x");
            g.http_status = Some(status);
            Error::from(g)
        };
        for (err, sent) in [
            (api(Some(400)), false),
            (api(Some(429)), false),
            (api(Some(500)), true),
            (api(Some(499)), false),
            (api(Some(302)), true),
            (api(Some(200)), true),
            (api(None), false),
            // Throttling proves Meta did nothing, whatever the status: the
            // retry policy replays a send on it (and the OTP service drops
            // the challenge).
            (throttled(130_429, 503), false),
            (throttled(131_056, 500), false),
            (throttled(131_000, 503), true),
            (throttled(130_429, 302), false),
            (http(400), false),
            (http(404), false),
            (http(499), false),
            (http(500), true),
            (http(502), true),
            (Error::Transport(TransportError::Timeout), true),
            (
                Error::Transport(TransportError::Connect(anyhow::anyhow!("refused"))),
                false,
            ),
            (
                Error::Transport(TransportError::Backend(anyhow::anyhow!("reset"))),
                true,
            ),
            (decode, true),
            (ValidationError::new("to", "bad").into(), false),
            (ConfigError::new("no transport").into(), false),
            (Error::Other(anyhow::anyhow!("?")), true),
            (
                Error::Transport(TransportError::Timeout).in_step("send_code"),
                true,
            ),
            (api(Some(400)).in_step("send_code"), false),
            // Every row below pins one arm a mutation could flip unseen.
            (http(302), true),
            (
                Error::Transport(TransportError::Build("header".into())),
                false,
            ),
            (Error::Transport(TransportError::Integrity("sha256")), true),
            (CryptoError::Decrypt.into(), false),
            (
                StorageError::Backend(anyhow::anyhow!("db down")).into(),
                false,
            ),
            (SinkError::Closed.into(), false),
            (WebhookError::SignatureMismatch.into(), false),
        ] {
            assert_eq!(err.may_have_been_sent(), sent, "{err}");
        }
    }

    /// Credit line refusals: only a raced share, a share to reconcile, a
    /// share whose attach failed and a revocation's DELETEs reached Meta.
    #[test]
    fn credit_errors_say_whether_meta_could_have_acted() {
        let api = |status: Option<u16>| {
            let mut g = GraphApiError::new(131047, "x");
            g.http_status = status;
            Error::from(g)
        };
        for (err, sent) in [
            (credit_revoked(false).into(), false),
            (credit_revoked(true).into(), true),
            (busy(false).into(), false),
            (busy(true).into(), true),
            (CreditError::OwnerUnknown("x".into()).into(), false),
            (
                CreditError::StatusUnknown {
                    allocation_config_id: "A".into(),
                    status: "NEW".into(),
                }
                .into(),
                false,
            ),
            (CreditError::ApprovalRequired("x".into()).into(), false),
            (CreditError::Reconcile("x".into()).into(), true),
            // The two-call method's share went out; its attach did not.
            (
                CreditError::AttachFailed {
                    allocation_config_id: "A".into(),
                    source: Box::new(api(Some(400))),
                }
                .into(),
                true,
            ),
            (incomplete(false, None).into(), false),
            (incomplete(true, None).into(), true),
            (
                incomplete(false, Some(Error::Transport(TransportError::Timeout))).into(),
                true,
            ),
            (
                incomplete(false, Some(ValidationError::new("x", "y").into())).into(),
                false,
            ),
            (
                Error::from(incomplete(true, None)).in_step("revoke_credit_line"),
                true,
            ),
            // A ledger write, or a pending share nothing found, sends nothing.
            (
                {
                    let mut r = incomplete(false, None);
                    r.share_pending = true;
                    r.ledger = Some(Box::new(
                        StorageError::Backend(anyhow::anyhow!("db down")).into(),
                    ));
                    r.into()
                },
                false,
            ),
        ] {
            assert_eq!(err.may_have_been_sent(), sent, "{err}");
        }
    }

    fn credit_revoked(posted: bool) -> CreditError {
        CreditError::Revoked {
            business_id: None,
            reason: "revoked".into(),
            posted,
        }
    }

    fn busy(posted: bool) -> CreditError {
        CreditError::Busy {
            reason: "x".into(),
            posted,
        }
    }

    fn incomplete(deletes_sent: bool, source: Option<Error>) -> RevocationIncomplete {
        let mut r = RevocationIncomplete::new(CreditRevocation::new(None));
        r.deletes_sent = deletes_sent;
        r.source = source.map(Box::new);
        r
    }

    /// Busy and an unconfirmed revocation are worth retrying; a refusal is
    /// not; a revocation stopped by a record naming no business needs a
    /// person, whatever else happened.
    #[test]
    fn credit_errors_decide_retry_and_kind() {
        let step_busy = Error::from(busy(false)).in_step("share_credit_line");
        assert!(step_busy.is_retryable());
        assert_eq!(step_busy.kind(), ErrorKind::ServiceUnavailable);
        assert!(matches!(step_busy.credit(), Some(CreditError::Busy { .. })));
        assert!(busy(true).is_retryable(), "resume attaches what was shared");
        let revoked = Error::from(credit_revoked(false));
        assert!(!revoked.is_retryable());
        assert_eq!(revoked.kind(), ErrorKind::InvalidParameter);
        for refusal in [
            CreditError::OwnerUnknown("x".into()),
            CreditError::ApprovalRequired("x".into()),
        ] {
            assert!(!refusal.is_retryable(), "{refusal}");
            assert_eq!(refusal.kind(), ErrorKind::InvalidParameter);
        }
        // Only a person can settle a share to reconcile, or records naming
        // no business: one kind for both.
        let reconcile = CreditError::Reconcile("x".into());
        assert!(!reconcile.is_retryable());
        assert_eq!(reconcile.kind(), ErrorKind::Unknown);
        let attach = |source: Error| CreditError::AttachFailed {
            allocation_config_id: "A".into(),
            source: Box::new(source),
        };
        let refused = attach(ValidationError::new("waba_currency", "x").into());
        assert!(!refused.is_retryable());
        assert_eq!(refused.kind(), ErrorKind::InvalidParameter);
        let busy_attach = attach(Error::Http {
            status: 503,
            body_snippet: String::new(),
        });
        assert!(busy_attach.is_retryable());
        assert_eq!(busy_attach.kind(), ErrorKind::ServiceUnavailable);

        let mut unconfirmed = incomplete(true, None);
        unconfirmed.unconfirmed.push("A1".into());
        let err = Error::from(unconfirmed);
        assert!(err.is_retryable(), "a DELETE Meta has not confirmed yet");
        assert_eq!(err.kind(), ErrorKind::ServiceUnavailable);
        let mut unattributed = incomplete(false, None);
        unattributed.unattributed.push("U1".into());
        assert!(!unattributed.is_retryable());
        assert_eq!(CreditError::from(unattributed).kind(), ErrorKind::Unknown);
        let mut mixed = incomplete(true, None);
        mixed.unconfirmed.push("A1".into());
        mixed.unattributed.push("U1".into());
        assert!(!mixed.is_retryable());
        assert_eq!(
            CreditError::from(mixed).kind(),
            ErrorKind::Unknown,
            "not retryable, so not ServiceUnavailable"
        );
        // A pending share not found yet, or a ledger write that failed:
        // call again.
        let mut pending = incomplete(false, None);
        pending.share_pending = true;
        assert!(pending.is_incomplete() && pending.is_retryable());
        assert!(
            pending.to_string().contains("no recorded outcome"),
            "{pending}"
        );
        assert_eq!(
            CreditError::from(pending).kind(),
            ErrorKind::ServiceUnavailable
        );
        let mut ledger = incomplete(true, None);
        ledger.ledger = Some(Box::new(
            StorageError::Backend(anyhow::anyhow!("db down")).into(),
        ));
        assert!(ledger.is_incomplete());
        assert!(ledger.is_retryable(), "a storage failure is written again");
        assert!(ledger.to_string().contains("ledger"), "{ledger}");
        assert_eq!(
            CreditError::from(ledger).kind(),
            ErrorKind::ServiceUnavailable
        );
        assert!(!incomplete(true, None).is_incomplete());
        let transient = incomplete(false, Some(Error::Transport(TransportError::Timeout)));
        assert!(transient.is_retryable());
        let mut both = incomplete(false, Some(Error::Transport(TransportError::Timeout)));
        both.unattributed.push("U1".into());
        assert!(!both.is_retryable());
        let permanent = Error::from(incomplete(
            false,
            Some(ValidationError::new("x", "y").into()),
        ));
        assert!(!permanent.is_retryable());
        assert_eq!(permanent.kind(), ErrorKind::InvalidParameter);
        assert!(
            permanent
                .credit()
                .and_then(CreditError::revocation)
                .is_some()
        );
        assert!(
            Error::from(ValidationError::new("x", "y"))
                .credit()
                .is_none()
        );
    }

    #[test]
    fn snippet_truncates_long_bodies() {
        let body = vec![b'a'; 2000];
        let s = snippet(&body);
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 513);
    }

    #[test]
    fn http_5xx_is_retryable_4xx_is_not() {
        let e5 = Error::Http {
            status: 502,
            body_snippet: String::new(),
        };
        let e4 = Error::Http {
            status: 404,
            body_snippet: String::new(),
        };
        assert!(e5.is_retryable());
        assert!(!e4.is_retryable());
    }
}
