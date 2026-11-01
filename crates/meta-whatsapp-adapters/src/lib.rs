//! Adapters for the `meta-whatsapp-core` ports.
//!
//! # Features
//!
//! Items are named in code, not linked: every one of them sits behind its
//! feature, and these docs must build with any feature set.
//!
//! | Feature | Adds | Port | Pulls in |
//! | --- | --- | --- | --- |
//! | `memory` (default) | `store::MemoryKvStore`, `store::MemoryConversationStore` | `KvStore`, `ConversationStore` | tokio `sync` |
//! | `sinks` (default) | `sink`: channel, broadcast, fan-out, filter, fn, tracing sinks | `EventSink` | tokio `sync`, tokio-stream |
//! | `reqwest` | `http::ReqwestTransport` | `HttpTransport` | reqwest (rustls, aws-lc-rs, HTTP/2) |
//! | `postgres` | `store::PostgresKvStore`, `store::PostgresConversationStore`, `store::postgres::migrate` | `KvStore`, `ConversationStore` | sqlx (Postgres, tokio, rustls) |
//! | `redis` | `store::RedisKvStore`, `store::RedisTls` (`rediss://`) | `KvStore` | redis (tokio, connection manager; none of its TLS features), rustls and tokio-rustls (aws-lc-rs), rustls-platform-verifier |
//! | `ffmpeg` | `media::FfmpegConverter` | `MediaConverter` | tokio `process`; runs an `ffmpeg` you install, links none |
//!
//! Every feature compiles on its own; none is required by another.
//!
//! # Known limitations
//!
//! - **Redis over TLS (`rediss://`) goes through `store::RedisTls`, not
//!   redis-rs's TLS**, which configures rustls from the process-wide default
//!   crypto provider and panics when both `aws-lc-rs` and `ring` are linked
//!   and none was installed. `RedisTls` names aws-lc-rs explicitly; its
//!   connection reconnects on its own but is not redis-rs's
//!   `ConnectionManager`, and Redis Cluster and Sentinel over TLS are not
//!   covered.
//! - **Postgres refuses U+0000 in identifiers**: a message id, contact or
//!   phone number id, or a `StoreKey`, holding one is rejected with
//!   `StorageError::Backend`. Message content (kind, text, payload, status
//!   error, preview) keeps it, and key/value values are bytes; content
//!   columns are `BYTEA` and `json`, which costs SQL-side search and
//!   indexing (see `store::postgres`).
//! - **Proxies**: `ReqwestTransport` honours `HTTP(S)_PROXY`/`NO_PROXY`
//!   from the environment, not macOS/Windows system settings.
//!
//! # Which adapter when
//!
//! - **Key/value (`KvStore`)** backs the token vault, OTP challenges,
//!   webhook dedup and Embedded Signup sessions. Use **Postgres** when you
//!   already run it: tokens and OTP state survive restarts and are shared by
//!   every instance. Use **Redis** when you run several instances and want
//!   cheap expiring keys (dedup markers, OTPs); make it persistent if tokens
//!   live there. Use **memory** for tests, development, or one instance that
//!   can afford to lose OTPs and dedup markers on restart.
//! - **Conversation history (`ConversationStore`)** backs the in-app inbox.
//!   **Postgres** in production; **memory** for tests and demos. (No Redis
//!   adapter: history is not cache-shaped.)
//! - **Sinks** route webhook events: a `sink::ChannelSink` to a worker for
//!   anything slow, a `sink::BroadcastSink` for live inbox views, a
//!   `sink::FanoutSink` to do both.
//! - **Transport**: `ReqwestTransport` unless you need your own HTTP stack.
//! - **Media conversion** (a recording into a voice message's Ogg Opus,
//!   an image into a sticker's WebP or under the image limit):
//!   `media::FfmpegConverter` when the host has an `ffmpeg` with libopus
//!   (and libwebp, for stickers);
//!   the library ships none, and the service's image carries none.
//!
//! # Whose clock?
//!
//! Expiry is decided by the store's clock, and the stores do not share one:
//! `store::MemoryKvStore` uses an injectable `Clock` (the system clock by
//! default), **Postgres uses the database server's `now()`**, and Redis its
//! server's `TIME` and key TTLs. For the shared backends that is the point —
//! every application instance agrees on what has expired — but it means
//! `Expiry::At(t)` is compared against the *server's* time, and a test's
//! `ManualClock` cannot move a Postgres or Redis store forward. Keep
//! database hosts on NTP. Precision: Postgres keeps microseconds, Redis
//! milliseconds.
//!
//! # Wiring
//!
//! ```no_run
//! # #[cfg(all(feature = "postgres", feature = "sinks"))]
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use meta_whatsapp_adapters::sink::{BroadcastSink, FanoutSink, TracingSink, channel};
//! use meta_whatsapp_adapters::store::{PostgresConversationStore, PostgresKvStore, postgres};
//! use meta_whatsapp_core::store::{ConversationStore, KvStore};
//!
//! #[derive(Debug, Clone)]
//! struct Event; // in practice `meta_whatsapp_webhooks::WebhookEvent`
//!
//! let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
//! postgres::migrate(&pool).await?;
//! let kv: Arc<dyn KvStore> = Arc::new(PostgresKvStore::new(pool.clone()));
//! let inbox: Arc<dyn ConversationStore> = Arc::new(PostgresConversationStore::new(pool));
//!
//! let (to_worker, mut jobs) = channel::<Event>(1024);
//! let live = BroadcastSink::<Event>::new(256);
//! let sink = FanoutSink::new()
//!     .with(to_worker)
//!     .with(live.clone())
//!     .with(TracingSink::new());
//! // Hand `sink` to the webhook router, `live.subscribe()` to each SSE
//! // client, and drain `jobs` in a worker task that writes to `inbox`.
//! # let _ = (kv, inbox, sink, jobs.recv());
//! # Ok(()) }
//! ```
//!
//! Every `KvStore` adapter must pass [`store::conformance`] and every
//! `ConversationStore` adapter [`store::conversation_conformance`]; run them
//! against your own adapters too.

#[cfg(feature = "reqwest")]
pub mod http;
#[cfg(feature = "ffmpeg")]
pub mod media;
#[cfg(feature = "sinks")]
pub mod sink;
pub mod store;
