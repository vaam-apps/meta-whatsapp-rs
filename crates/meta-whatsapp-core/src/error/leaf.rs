//! Leaf error types. Each subsystem owns one; the root [`super::Error`]
//! aggregates them with `#[from]`.

/// The request never produced an HTTP response.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The request (or reading its body) exceeded its deadline.
    #[error("request timed out")]
    Timeout,
    /// Could not connect (DNS, TCP, TLS).
    #[error("connection failed: {0}")]
    Connect(#[source] anyhow::Error),
    /// The request could not be built (bad header value, bad multipart…).
    #[error("could not build request: {0}")]
    Build(String),
    /// Anything else the adapter reports.
    #[error("transport failure: {0}")]
    Backend(#[source] anyhow::Error),
    /// A body arrived but failed an integrity check (e.g. a media SHA-256
    /// mismatch). Retryable: a fresh download may be intact.
    #[error("integrity check failed: {0}")]
    Integrity(&'static str),
}

impl TransportError {
    /// Timeouts and connect failures are worth retrying (for idempotent
    /// requests); build errors are not.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout | Self::Connect(_) | Self::Integrity(_))
    }
}

/// A storage adapter failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// A stored value could not be (de)serialized.
    #[error("stored value for `{key}` is corrupt: {source}")]
    Corrupt {
        /// Key whose value is corrupt.
        key: String,
        /// Serde error.
        #[source]
        source: serde_json::Error,
    },
    /// The backend (database, cache) failed.
    #[error("storage backend failure: {0}")]
    Backend(#[source] anyhow::Error),
    /// Contention: the backend gave up waiting for another writer (a lock
    /// wait past its timeout, a write conflict, a transaction it had to
    /// abort) and did nothing. Trying again later may succeed
    /// ([`crate::Error::is_retryable`]). An adapter reports its own
    /// contention this way rather than as [`Self::Backend`], so that a
    /// caller can tell "busy, come back" from "broken" without knowing the
    /// adapter's error types.
    ///
    /// Retryable means the *storage call* may succeed later, not that the
    /// operation around it may be repeated. Inside an
    /// [`Error::Step`](crate::Error::Step) it stays retryable, and a step
    /// that stores after a send has sent: repeating the whole operation on
    /// `Busy` sends again. The library never returns a storage error after
    /// a send (it logs it: [`crate::Error::may_have_been_sent`]), and sends
    /// again automatically only when [`crate::Error::may_resend`] holds,
    /// which is never for an error in a step. A flow of your own that
    /// stores after a send must do the same: never repeat the send on
    /// `Busy` unless the send provably did nothing.
    #[error("storage backend busy: gave up waiting for another writer")]
    Busy,
}

impl StorageError {
    /// Whether this is contention ([`Self::Busy`]): nothing was done, and
    /// trying again later may succeed.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy)
    }
}

/// An [`crate::sink::EventSink`] failed to accept an event.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SinkError {
    /// The receiving side is gone (channel closed, subscriber dropped).
    #[error("sink closed")]
    Closed,
    /// The sink is full and configured not to wait.
    #[error("sink full")]
    Full,
    /// The sink's backend failed.
    #[error("sink delivery failed: {0}")]
    Delivery(#[source] anyhow::Error),
}

/// Input rejected locally, before any request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid `{field}`: {reason}")]
pub struct ValidationError {
    /// Offending field, dotted path for nested ones (`components[0].text`).
    pub field: String,
    /// Why it was rejected.
    pub reason: String,
}

impl ValidationError {
    /// The `field` of [`Self::customer_service_window_closed`]. Match on
    /// [`Self::is_customer_service_window_closed`] (or on
    /// [`Error::kind`](crate::Error::kind) being
    /// [`ErrorKind::CustomerServiceWindowClosed`](crate::ErrorKind::CustomerServiceWindowClosed))
    /// rather than comparing the string yourself.
    pub const CUSTOMER_SERVICE_WINDOW: &'static str = "customer_service_window";

    /// Build a validation error.
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// A free-form message refused locally because the 24-hour customer
    /// service window is closed (Meta would answer `131047`). Its
    /// [`Error::kind`](crate::Error::kind) is
    /// [`ErrorKind::CustomerServiceWindowClosed`](crate::ErrorKind::CustomerServiceWindowClosed),
    /// the same as Meta's.
    pub fn customer_service_window_closed() -> Self {
        Self::new(
            Self::CUSTOMER_SERVICE_WINDOW,
            "more than 24 hours since the customer's last message; send a template",
        )
    }

    /// Whether this is [`Self::customer_service_window_closed`].
    pub fn is_customer_service_window_closed(&self) -> bool {
        self.field == Self::CUSTOMER_SERVICE_WINDOW
    }

    /// The `field` of [`Self::thread_owned_elsewhere`]. Match on
    /// [`Self::is_thread_owned_elsewhere`] (or on
    /// [`Error::kind`](crate::Error::kind) being
    /// [`ErrorKind::ThreadOwnedElsewhere`](crate::ErrorKind::ThreadOwnedElsewhere))
    /// rather than comparing the string yourself.
    pub const THREAD_OWNER: &'static str = "thread_owner";

    /// A service message refused locally because another app owns the
    /// thread under Conversation Routing (Meta would reject it too:
    /// `conversation-routing/thread-lifecycle`, "Sending without
    /// ownership"). Its [`Error::kind`](crate::Error::kind) is
    /// [`ErrorKind::ThreadOwnedElsewhere`](crate::ErrorKind::ThreadOwnedElsewhere).
    pub fn thread_owned_elsewhere() -> Self {
        Self::new(
            Self::THREAD_OWNER,
            "another app owns this thread under Conversation Routing; a template needs no ownership",
        )
    }

    /// Whether this is [`Self::thread_owned_elsewhere`].
    pub fn is_thread_owned_elsewhere(&self) -> bool {
        self.field == Self::THREAD_OWNER
    }
}

/// A webhook request could not be accepted.
///
/// The variants map onto HTTP responses: signature and token failures are
/// `401`/`403` (Meta will retry, which is right if *your* secret is wrong);
/// a body that verified but does not parse is a bug on one side and is
/// surfaced so it can be acknowledged and logged rather than retried for 7
/// days.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WebhookError {
    /// `X-Hub-Signature-256` header absent.
    #[error("missing X-Hub-Signature-256 header")]
    MissingSignature,
    /// Header present but not `sha256=<64 hex chars>`.
    #[error("malformed X-Hub-Signature-256 header")]
    MalformedSignature,
    /// No configured app secret produced this signature.
    #[error("webhook signature does not match any configured app secret")]
    SignatureMismatch,
    /// `hub.mode` was not `subscribe`, or a parameter was missing.
    #[error("invalid verification request: {0}")]
    InvalidVerificationRequest(&'static str),
    /// `hub.verify_token` did not match.
    #[error("verify token mismatch")]
    VerifyTokenMismatch,
    /// Signed body is not a payload we understand.
    #[error("webhook payload could not be parsed: {0}")]
    Parse(#[source] serde_json::Error),
    /// An event of this delivery is being delivered by another request right
    /// now (webhook dedup lease). Answer non-`200` (`503`) so Meta retries;
    /// answering `200` could lose the event if that other request dies.
    #[error("a webhook event in this delivery is being delivered by another request")]
    ClaimInFlight,
    /// Body exceeds the configured size limit (answer `413`).
    #[error("webhook body of {size} bytes exceeds the {limit}-byte limit")]
    PayloadTooLarge {
        /// Body size in bytes.
        size: usize,
        /// Configured limit in bytes.
        limit: usize,
    },
}

/// Encryption or decryption failed. Deliberately carries no detail that
/// could act as an oracle (no "bad padding" vs "bad tag").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CryptoError {
    /// Key material is missing, the wrong length, or unparseable.
    #[error("invalid key: {0}")]
    InvalidKey(&'static str),
    /// Ciphertext failed authentication or could not be decrypted.
    #[error("decryption failed")]
    Decrypt,
    /// Encryption failed.
    #[error("encryption failed")]
    Encrypt,
    /// Encoded input (base64, envelope framing) is malformed.
    #[error("malformed ciphertext: {0}")]
    Malformed(&'static str),
    /// The operating system's random number generator failed.
    #[error("random number generator failed")]
    Rng,
}

/// Missing or invalid configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("configuration error: {0}")]
pub struct ConfigError(pub String);

impl ConfigError {
    /// Build a configuration error.
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}
