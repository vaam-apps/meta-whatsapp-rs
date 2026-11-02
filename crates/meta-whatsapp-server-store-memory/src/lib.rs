//! `meta-whatsapp-server-store-memory`: the meta-whatsapp-rs service's
//! memory backend (docs/design/server.md, section 8.2), one process's
//! records, emptied on restart: for `WA_SERVER_ENV=development` and tests.
//!
//! [`MemoryBackend`] is a `meta_whatsapp_server_core::backend::Backend`: the
//! core's ports ([`MemoryStore`] for the records and the idempotency
//! records, [`MemoryEventStore`] for the event outbox,
//! [`MemoryWebhookStore`] for webhooks out, [`MemoryLeaderLock`])
//! over one lock, with the library's memory `KvStore` and
//! `ConversationStore`. It passes the core's conformance suite
//! (`meta_whatsapp_server_core::conformance::run`, this crate's
//! `tests/conformance.rs`). The binary, `meta-whatsapp-server`, compiles it
//! in with its `store-memory` feature (roadmap S7).
//!
//! It depends on the facade and on the service's core, never on the
//! service's HTTP crates.

mod events;
mod memory;
mod webhooks;

pub use events::MemoryEventStore;
pub use memory::{MemoryBackend, MemoryLeaderLock, MemoryStore};
pub use webhooks::MemoryWebhookStore;
