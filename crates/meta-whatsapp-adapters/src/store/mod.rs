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
//! | `SqliteKvStore` | `KvStore` | `sqlite` | injectable `Clock` (the application's) |
//! | `SqliteConversationStore` | `ConversationStore` | `sqlite` | — |
//! | `RedisKvStore` | `KvStore` | `redis` | the **Redis server's** `TIME` |
//! | `RedisConversationStore` | `ConversationStore` | `redis` | — |
//! | `MongoKvStore` | `KvStore` | `mongodb` | the **database server's** `$$NOW` |
//! | `MongoConversationStore` | `ConversationStore` | `mongodb` | — |
//!
//! `RedisKvStore` and `RedisConversationStore` take any async redis
//! connection: `redis://` through redis-rs (`ConnectionManager`),
//! `rediss://` through `RedisTls`, which does the TLS handshake itself on an
//! explicit aws-lc-rs provider.
//!
//! **Message ids are stored once per store** in every adapter (today's rule,
//! `OPEN_QUESTIONS.md` #33): a second `append` of one id on another business
//! number returns `false`. The SQLite and Redis stores (roadmap L8) key a
//! message by business number and id and enforce that rule through a
//! separate index, so ids stored once per number would only drop the index.

#[cfg(any(feature = "sqlite", feature = "redis"))]
mod codec;
pub mod conformance;
pub mod conversation_conformance;
#[cfg(feature = "memory")]
mod memory_conversation;
#[cfg(feature = "memory")]
mod memory_kv;
#[cfg(feature = "mongodb")]
pub mod mongodb;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
mod redis_conversation;
#[cfg(feature = "redis")]
mod redis_kv;
#[cfg(feature = "redis")]
mod redis_tls;
#[cfg(feature = "sqlite")]
pub mod sqlite;
#[cfg(any(feature = "postgres", feature = "sqlite"))]
mod table_prefix;

#[cfg(feature = "mongodb")]
pub use self::mongodb::{MongoConversationStore, MongoKvStore};
/// The `redis` this adapter is built against — `RedisKvStore::new` takes its
/// connection types, so build connections with this re-export rather than
/// pinning redis yourself.
#[cfg(feature = "redis")]
pub use ::redis;
/// The `rustls` `RedisTls` is built on (feature `redis`):
/// `RedisTls::from_client_config` takes its `ClientConfig`, so build one
/// with this re-export rather than pinning rustls yourself.
#[cfg(feature = "redis")]
pub use ::rustls;
#[cfg(feature = "memory")]
pub use memory_conversation::MemoryConversationStore;
#[cfg(feature = "memory")]
pub use memory_kv::MemoryKvStore;
#[cfg(feature = "postgres")]
pub use postgres::{PostgresConversationStore, PostgresKvStore};
#[cfg(feature = "redis")]
pub use redis_conversation::RedisConversationStore;
#[cfg(feature = "redis")]
pub use redis_kv::RedisKvStore;
#[cfg(feature = "redis")]
pub use redis_tls::{RedisTls, RedisTlsBuilder, RedisTlsConnection};
#[cfg(feature = "sqlite")]
pub use sqlite::{SqliteConversationStore, SqliteKvStore};
#[cfg(any(feature = "postgres", feature = "sqlite"))]
pub use table_prefix::TablePrefix;
