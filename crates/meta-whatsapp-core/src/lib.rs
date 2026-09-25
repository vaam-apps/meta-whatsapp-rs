//! Core of `meta-whatsapp-rs`: the error tree, identifiers, and the **ports** every other
//! crate is written against.
//!
//! Nothing in here talks to the network, a database or a runtime. The ports
//! are traits; the adapters that implement them live in `meta-whatsapp-adapters` (and in
//! your own code, if none of those fit):
//!
//! | Port | Purpose | Shipped adapters |
//! | --- | --- | --- |
//! | [`transport::HttpTransport`] | Send a Graph API request | `reqwest` |
//! | [`store::KvStore`] | Tokens, OTPs, dedup keys, signup state | memory, Postgres, Redis |
//! | [`store::ConversationStore`] | Inbox history for in-app chat | memory, Postgres |
//! | [`sink::EventSink`] | Where webhook events go | channel, broadcast, fan-out, fn, tracing |
//! | [`clock::Clock`] | "Now", injectable for tests | system, manual |
//!
//! See `docs/architecture.md` at the repository root for how they compose.

pub mod clock;
pub mod config;
pub mod error;
pub mod ids;
pub mod paging;
pub mod recipient;
pub mod secret;
pub mod sink;
pub mod store;
pub mod timestamp;
pub mod transport;

#[cfg(feature = "testing")]
pub mod testing;

pub use error::{Error, ErrorKind, GraphApiError, Result};
