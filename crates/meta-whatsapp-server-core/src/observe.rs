//! What an operation reports beside its answer, for the API adapter's
//! metrics: the core counts nothing itself, and names no metrics library.
//! The service's Prometheus registry (`meta_whatsapp_server::metrics`)
//! implements [`Observer`]; `()` observes nothing.

/// Told what an operation saw on its way, for the adapter's metrics.
pub trait Observer: Send + Sync {
    /// A Graph call failed, answered with the error code `code` (one of
    /// [`crate::error::CODES`]).
    fn graph_error(&self, code: &'static str);
}

/// Observes nothing.
impl Observer for () {
    fn graph_error(&self, _: &'static str) {}
}
