//! Rate limits (docs/design/server.md, section 6): the core's per-tenant
//! token buckets by route class, and its transfer slots
//! ([`meta_whatsapp_server_core::ratelimit`], re-exported here). The
//! tenant guard ([`crate::auth::tenant_guard`]) charges the bucket of the
//! request's operation (`RateLimiter::charge`: the class is the
//! operation's, roadmap S6) after the key, the tenant and the scope, and
//! answers `429 too_many_requests`, `retryable`, with `Retry-After`; each
//! limit is `WA_SERVER_RATE_<CLASS>` and `…_BURST` (see
//! [`crate::config`]).

pub use meta_whatsapp_server_core::ratelimit::{
    Limited, Rate, RateLimiter, RateLimits, RouteClass, Slot, Slots, retry_after_secs,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Scope;

    /// `/v1`'s budgets did not move when the class became the operation's
    /// (roadmap S6): each tenant route's operation is in the class its
    /// method and scope gave it before (template management for the
    /// templates scope, a read for `GET`, a send for any other method).
    #[test]
    fn v1s_routes_keep_their_class() {
        let operations = crate::api::tenant_operations();
        assert!(operations.routes().len() >= 16);
        for (method, template, operation) in operations.routes() {
            let before = if operation.scope() == Scope::Templates {
                RouteClass::Templates
            } else if method == "GET" {
                RouteClass::Read
            } else {
                RouteClass::Send
            };
            assert_eq!(RouteClass::of(*operation), before, "{method} {template}");
        }
    }
}
