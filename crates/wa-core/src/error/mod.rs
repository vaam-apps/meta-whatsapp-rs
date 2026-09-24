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

mod graph;
mod leaf;

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
            Self::Validation(v) if v.field == "customer_service_window" => {
                ErrorKind::CustomerServiceWindowClosed
            }
            Self::Validation(_) => ErrorKind::InvalidParameter,
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
            Self::Step { source, .. } => source.is_retryable(),
            _ => false,
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
        let local = Error::from(ValidationError::new("customer_service_window", "closed"));
        let remote = Error::from(GraphApiError::new(131047, "Re-engagement message"));
        assert_eq!(local.kind(), remote.kind());
        assert_eq!(
            Error::from(ValidationError::new("body", "x")).kind(),
            ErrorKind::InvalidParameter
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
