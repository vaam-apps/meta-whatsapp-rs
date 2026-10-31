// A request is charged by its operation (`RateLimiter::charge`), never a
// class of the adapter's choosing, such as one read from the HTTP method
// (roadmap S6).
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::ratelimit::{RateLimiter, RouteClass};

fn by_method(limiter: &RateLimiter, tenant: &TenantId) {
    let _ = limiter.check(tenant, RouteClass::Read);
}

fn main() {}
