//! Rate limits (docs/design/server.md, section 6): the core's per-tenant
//! token buckets by route class, and its transfer slots
//! ([`meta_whatsapp_server_core::ratelimit`], re-exported here). The
//! tenant guard ([`crate::auth::tenant_guard`]) charges the bucket of the
//! request's operation (`RateLimiter::charge`: the class is the
//! operation's, roadmap S6) after the key, the tenant and the scope, and
//! answers `429 too_many_requests`, `retryable`, with `Retry-After`; each
//! limit is `WA_SERVER_RATE_<CLASS>` and `…_BURST` (the binary's
//! `config`). That `/v1`'s routes kept their class is the API adapter's
//! test (`meta-whatsapp-server-api-axum`, `api::tests`).

pub use meta_whatsapp_server_core::ratelimit::{
    Limited, Rate, RateLimiter, RateLimits, RouteClass, Slot, Slots, retry_after_secs,
};
