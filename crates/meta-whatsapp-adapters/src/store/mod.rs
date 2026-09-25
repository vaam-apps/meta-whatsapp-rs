//! Storage adapters.
//!
//! [`conformance`] is the executable definition of the `KvStore` contract and
//! [`conversation_conformance`] the one of the `ConversationStore` contract;
//! every adapter's tests run them, and so should yours.
//!
//! | Adapter | Port | Feature | Clock |
//! | --- | --- | --- | --- |
//! | `MemoryKvStore` | `KvStore` | `memory` | injectable `Clock` |
//! | `MemoryConversationStore` | `ConversationStore` | `memory` | — |
//! | `PostgresKvStore` | `KvStore` | `postgres` | the **database server's** `now()` |
//! | `PostgresConversationStore` | `ConversationStore` | `postgres` | — |
//! | `RedisKvStore` | `KvStore` | `redis` | the **Redis server's** `TIME` |

pub mod conformance;
pub mod conversation_conformance;
#[cfg(feature = "memory")]
mod memory_conversation;
#[cfg(feature = "memory")]
mod memory_kv;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
mod redis_kv;

/// The `redis` this adapter is built against — `RedisKvStore::new` takes its
/// connection types, so build connections with this re-export rather than
/// pinning redis yourself.
#[cfg(feature = "redis")]
pub use ::redis;
#[cfg(feature = "memory")]
pub use memory_conversation::MemoryConversationStore;
#[cfg(feature = "memory")]
pub use memory_kv::MemoryKvStore;
#[cfg(feature = "postgres")]
pub use postgres::{PostgresConversationStore, PostgresKvStore};
#[cfg(feature = "redis")]
pub use redis_kv::RedisKvStore;
