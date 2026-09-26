//! Rate limits (docs/design/server.md, section 6): the core's per-tenant
//! token buckets by route class, and its transfer slots
//! ([`meta_whatsapp_server_core::ratelimit`], re-exported here). The
//! tenant guard ([`crate::auth::tenant_guard`]) checks the bucket after
//! the key, the tenant and the scope, and answers `429
//! too_many_requests`, `retryable`, with `Retry-After`; each limit is
//! `WA_SERVER_RATE_<CLASS>` and `…_BURST` (see [`crate::config`]).

pub use meta_whatsapp_server_core::ratelimit::{
    Rate, RateLimiter, RateLimits, RouteClass, Slot, Slots, retry_after_secs,
};

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::webhooks::axum::http::Method;

    use super::*;
    use crate::model::Scope;

    #[test]
    fn routes_fall_into_their_class() {
        for (scope, method, class) in [
            (Scope::Send, Method::POST, RouteClass::Send),
            (Scope::Media, Method::POST, RouteClass::Send),
            (Scope::Media, Method::DELETE, RouteClass::Send),
            (Scope::Media, Method::GET, RouteClass::Read),
            (Scope::Numbers, Method::GET, RouteClass::Read),
            (Scope::Numbers, Method::PATCH, RouteClass::Send),
            (Scope::Templates, Method::GET, RouteClass::Templates),
            (Scope::Templates, Method::POST, RouteClass::Templates),
            (Scope::Templates, Method::DELETE, RouteClass::Templates),
        ] {
            assert_eq!(RouteClass::of(scope, &method), class, "{scope:?} {method}");
        }
    }
}
