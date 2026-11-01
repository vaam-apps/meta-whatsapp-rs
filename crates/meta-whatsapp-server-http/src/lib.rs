//! `meta-whatsapp-server-http`: what every API adapter of the
//! meta-whatsapp-rs service shares over HTTP (docs/design/server.md,
//! section 8.3; roadmap S7): the listeners, telemetry, request ids,
//! authentication before the body is read, rate limits, the §5 error
//! renderer, and the routes an adapter does not make itself: Meta's
//! webhook (`/webhooks/meta`), media upload and download, and the
//! operations routes (health, metrics, version).
//!
//! On axum, through the facade (`meta_whatsapp_rs::webhooks::axum`), and
//! on the service's framework-free core, `meta-whatsapp-server-core`. An
//! API adapter (`meta-whatsapp-server-api-axum`, the `/v1` routes and their
//! OpenAPI document) mounts its routes on the internal listener with
//! [`api::internal_router_with`]; it reaches the records and the
//! authorization order only through the [`state::Grant`] that composing the
//! state ([`state::AppState::from_backend`]) hands its composer, never
//! through an [`state::AppState`] alone. The binary,
//! `meta-whatsapp-server`, composes this crate, an API adapter and a
//! backend.
//!
//! | Module | What |
//! | --- | --- |
//! | [`model`], [`keys`] | the core's domain types and API key format, re-exported |
//! | [`store`] | the core's storage ports, re-exported (the backends are crates of their own) |
//! | [`state`] | what every route shares, and the grant an API adapter's routes are served with |
//! | [`auth`] | the core's authorization order as middleware and extractors |
//! | [`api`] | the public listener's router, the internal listener's layers, and the routes shared by every adapter: Meta's webhook, media, operations |
//! | [`events`] | Meta's webhooks into the inbox and the event outbox: what the listener needs of a body, over the core's pipeline (its `Inbound`, routing and polling, re-exported) |
//! | [`idempotency`] | `Idempotency-Key` over HTTP (the core's engine: claim, replay, release) |
//! | [`ratelimit`] | the core's per-tenant token buckets by route class, re-exported |
//! | [`error`] | the error body, codes and statuses: the core's error model over HTTP |
//! | [`telemetry`], [`metrics`] | request logs and Prometheus metrics |
//! | [`listen`] | the listeners' accept loop |
//!
//! Every log line keeps the target it had before the crates split
//! (`meta_whatsapp_server::<module>`, named explicitly): an operator's
//! filters and alerts name them.

pub mod api;
pub mod auth;
pub mod error;
pub mod events;
pub mod idempotency;
pub mod listen;
pub mod metrics;
pub mod ratelimit;
pub mod state;
pub mod store;
pub mod telemetry;

pub use meta_whatsapp_server_core::{keys, model};

/// Held by every unit test that binds a loopback port: some assert that a
/// port they closed refuses connections, and run at once another test's
/// bind (on port 0) can be handed that port before the refusal is checked.
#[cfg(test)]
pub(crate) static LOOPBACK_PORTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
