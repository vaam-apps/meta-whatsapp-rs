//! `meta-whatsapp-server`: the meta-whatsapp-rs HTTP service, for apps not
//! written in Rust (docs/design/server.md; how to run it:
//! docs/guides/server.md).
//!
//! A binary built on the `meta-whatsapp-rs` facade and on the service's
//! crates, which it composes by its Cargo features (roadmap S7):
//!
//! | Crate | What | Feature |
//! | --- | --- | --- |
//! | `meta-whatsapp-server-core` | the domain, the authorization order, the error model as data, the ports a backend implements | always |
//! | `meta-whatsapp-server-http` | the listeners, authentication before the body, the §5 renderer, `/webhooks/meta`, media, operations | always |
//! | `meta-whatsapp-server-api-axum` | the `/v1` routes and `openapi/v1.json` | `api-axum` (default; the only API adapter, so required) |
//! | `meta-whatsapp-server-store-postgres` | the Postgres backend and its migrations | `store-postgres` (default) |
//! | `meta-whatsapp-server-store-memory` | the memory backend (development) | `store-memory` (default) |
//!
//! One multi-tenant deployment per Meta app; a public listener serving
//! only Meta's webhook and `/livez`, an internal one serving the `/v1` API
//! and operations; Postgres for every record.
//!
//! This crate holds the process: [`config`] (the environment, refused at
//! start when unsafe), [`serve`] (composing a backend, the HTTP crate and
//! the API adapter into a running service) and [`cli`] (the command line).
//! The composed crates' modules are re-exported under the paths they had
//! before the split, so `meta_whatsapp_server::…` names what it named:
//!
//! | Module | What |
//! | --- | --- |
//! | [`config`] | the environment, refused at start when unsafe |
//! | [`store`] | the core's storage ports and the backends this build has |
//! | [`model`], [`keys`] | the core's domain types and API key format |
//! | [`auth`] | the core's authorization order as middleware and extractors (the HTTP crate's) |
//! | [`api`] | both routers and the OpenAPI document (the API adapter's, with the HTTP crate's shared routes) |
//! | [`events`] | Meta's webhooks into the inbox and the event outbox (the HTTP crate's) |
//! | [`idempotency`] | `Idempotency-Key` over HTTP (the HTTP crate's) |
//! | [`ratelimit`] | the core's per-tenant token buckets by route class |
//! | [`error`] | the error body, codes and statuses (the HTTP crate's) |
//! | [`telemetry`], [`metrics`] | request logs and Prometheus metrics (the HTTP crate's) |
//! | [`state`] | what every route shares (the HTTP crate's) |
//! | [`serve`], [`listen`], [`cli`] | the process, its listeners' accept loop, the command line |

#[cfg(not(feature = "api-axum"))]
compile_error!(
    "meta-whatsapp-server needs an API adapter for its internal listener: enable the \
     `api-axum` feature (the only one today)"
);

pub mod cli;
pub mod config;
pub mod serve;
pub mod store;

#[cfg(feature = "api-axum")]
pub use meta_whatsapp_server_api_axum::api;
pub use meta_whatsapp_server_http::{
    auth, error, events, idempotency, keys, listen, metrics, model, ratelimit, state, telemetry,
};

/// Held by every unit test that binds a loopback port, and by the one that
/// starts a child process: some assert that a port they closed refuses
/// connections, and run at once another test's bind (on port 0, or on a
/// port it freed for `run`) can be handed that port, or a child forked in
/// the meantime can still hold its listener, before the refusal is
/// checked.
#[cfg(test)]
pub(crate) static LOOPBACK_PORTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
