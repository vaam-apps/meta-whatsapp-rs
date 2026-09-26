//! `meta-whatsapp-server`: the meta-whatsapp-rs HTTP service, for apps not
//! written in Rust (docs/design/server.md; how to run it:
//! docs/guides/server.md).
//!
//! A binary built on the `meta-whatsapp-rs` facade (axum and sqlx
//! through its re-exports) and on the service's framework-free core,
//! `meta-whatsapp-server-core` (the domain, the authorization order, the
//! error model as data, and the ports a backend implements). One
//! multi-tenant deployment per Meta app; a public listener serving only
//! Meta's webhook and `/livez`, an internal one serving the `/v1` API and
//! operations; Postgres for every record.
//!
//! | Module | What |
//! | --- | --- |
//! | [`config`] | the environment, refused at start when unsafe |
//! | [`store`] | the backends (Postgres, memory) of the core's ports, migrations |
//! | [`model`], [`keys`] | the core's domain types and API key format, re-exported |
//! | [`auth`] | the core's authorization order as middleware and extractors |
//! | [`api`] | both routers and the OpenAPI document |
//! | [`events`] | Meta's webhooks into the inbox and the event outbox (the core's routing and polling, re-exported) |
//! | [`idempotency`] | `Idempotency-Key` over HTTP (the core's engine: claim, replay, release) |
//! | [`ratelimit`] | the core's per-tenant token buckets by route class, re-exported |
//! | [`error`] | the error body, codes and statuses: the core's error model over HTTP |
//! | [`telemetry`], [`metrics`] | request logs and Prometheus metrics |
//! | [`serve`], [`listen`], [`cli`] | the process, its listeners' accept loop |

pub mod api;
pub mod auth;
pub mod cli;
pub mod config;
pub mod error;
pub mod events;
pub mod idempotency;
pub mod listen;
pub mod metrics;
pub mod ratelimit;
pub mod serve;
pub mod state;
pub mod store;
pub mod telemetry;

pub use meta_whatsapp_server_core::{keys, model};
