//! Adapters for the `wa-core` ports.
//!
//! | Module | Port | Feature |
//! | --- | --- | --- |
//! | [`store::MemoryKvStore`] | `KvStore` | `memory` (default) |
//! | `store::MemoryConversationStore` | `ConversationStore` | `memory` (default) |
//! | `store::PostgresKvStore`, `store::PostgresConversationStore` | both | `postgres` |
//! | `store::RedisKvStore` | `KvStore` | `redis` |
//! | `http::ReqwestTransport` | `HttpTransport` | `reqwest` |
//! | `sink::*` | `EventSink` | `sinks` (default) |
//!
//! Every `KvStore` adapter must pass [`store::conformance`]; run it against
//! your own adapter too.

#[cfg(feature = "reqwest")]
pub mod http;
#[cfg(feature = "sinks")]
pub mod sink;
pub mod store;
