//! `meta-whatsapp-server`: the meta-whatsapp-rs HTTP service, for apps not
//! written in Rust (docs/design/server.md; how to run it:
//! docs/guides/server.md).
//!
//! A binary built on the `meta-whatsapp-rs` facade alone (axum and sqlx
//! through its re-exports). One multi-tenant deployment per Meta app; a
//! public listener serving only Meta's webhook and `/livez`, an internal
//! one serving the `/v1` API and operations; Postgres for every record.
//!
//! | Module | What |
//! | --- | --- |
//! | [`config`] | the environment, refused at start when unsafe |
//! | [`store`] | tenants, keys and bindings (Postgres, memory), migrations |
//! | [`keys`] | the API key format, digests, constant-time comparison |
//! | [`auth`] | the authorization order: key, tenant, scope, ownership, vault |
//! | [`api`] | both routers and the OpenAPI document |
//! | [`error`] | the error body, codes and statuses |
//! | [`telemetry`], [`metrics`] | request logs and Prometheus metrics |
//! | [`serve`], [`cli`] | the process |

pub mod api;
pub mod auth;
pub mod cli;
pub mod config;
pub mod error;
pub mod keys;
pub mod metrics;
pub mod model;
pub mod serve;
pub mod state;
pub mod store;
pub mod telemetry;
