//! `meta-whatsapp-server-api-axum`: the meta-whatsapp-rs service's `/v1`
//! API on axum, and its OpenAPI document (docs/design/server.md, section
//! 8.3; roadmap S7): the resource routes of today's v1 (admin, numbers,
//! messages, templates, events) and `openapi/v1.json`, which is generated
//! from their annotations (utoipa) and committed beside the binary,
//! `crates/meta-whatsapp-server/openapi/v1.json`.
//!
//! Permanent (design D26): the swap target, where every new route lands,
//! through the core's services, which keep the adapter thin. It mounts its
//! routes, with the HTTP crate's media and operations routes, on the
//! internal listener ([`api::internal_router`]), served with the state's
//! grant (`meta_whatsapp_server_http::state::Grant`), which only the
//! state's composer holds. Its handlers are private to this crate: they
//! act on the capability they are given without asking the authorizer
//! again, so another crate reaches them only through the router, behind
//! its guards (`tests/visibility`). The binary, `meta-whatsapp-server`,
//! compiles it in with its `api-axum` feature.

pub mod api;

// The HTTP crate's modules, under the names this crate's handlers have
// always used (`crate::auth`, `crate::state`, …): not re-exported.
#[cfg(test)]
use meta_whatsapp_server_http::ratelimit;
use meta_whatsapp_server_http::{auth, error, events, idempotency, model, state, store};
