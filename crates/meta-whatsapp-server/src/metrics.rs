//! Prometheus metrics, served at `/metrics` on the internal listener
//! (docs/design/server.md, section 7.3).
//!
//! Labelled by listener, method, route **template**, status and error
//! code, never by an id, a number or a contact.
//!
//! | Metric | Labels |
//! | --- | --- |
//! | `wa_server_http_requests_total` | `listener`, `method`, `route`, `status`, `code` (empty on success) |
//! | `wa_server_http_request_duration_seconds` | `listener`, `method`, `route` |
//! | `wa_server_graph_errors_total` | `code` (the API error code of a failed Graph call) |

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;

use crate::telemetry::Listener;

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RequestLabels {
    listener: String,
    method: String,
    route: String,
    status: String,
    code: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RouteLabels {
    listener: String,
    method: String,
    route: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CodeLabels {
    code: String,
}

type DurationFamily = Family<RouteLabels, Histogram, fn() -> Histogram>;

/// The service's metrics. Cheap to clone.
#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Mutex<Registry>>,
    requests: Family<RequestLabels, Counter>,
    durations: DurationFamily,
    graph_errors: Family<CodeLabels, Counter>,
}

impl std::fmt::Debug for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metrics").finish_non_exhaustive()
    }
}

fn duration_histogram() -> Histogram {
    // 5 ms to about 10 s.
    Histogram::new(exponential_buckets(0.005, 2.0, 12))
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// A registry with every metric registered.
    pub fn new() -> Self {
        let mut registry = Registry::with_prefix("wa_server");
        let requests = Family::<RequestLabels, Counter>::default();
        let durations: DurationFamily = Family::new_with_constructor(duration_histogram);
        let graph_errors = Family::<CodeLabels, Counter>::default();
        registry.register(
            "http_requests",
            "HTTP requests answered, by route template, status and error code",
            requests.clone(),
        );
        registry.register(
            "http_request_duration_seconds",
            "HTTP request duration, by route template",
            durations.clone(),
        );
        registry.register(
            "graph_errors",
            "Failed Graph API calls, by the API error code they were answered with",
            graph_errors.clone(),
        );
        Self {
            registry: Arc::new(Mutex::new(registry)),
            requests,
            durations,
            graph_errors,
        }
    }

    /// Count an answered request.
    pub fn request(
        &self,
        listener: Listener,
        method: &str,
        route: &str,
        status: u16,
        code: Option<&str>,
        elapsed: Duration,
    ) {
        self.requests
            .get_or_create(&RequestLabels {
                listener: listener.as_str().to_owned(),
                method: method.to_owned(),
                route: route.to_owned(),
                status: status.to_string(),
                code: code.unwrap_or_default().to_owned(),
            })
            .inc();
        self.durations
            .get_or_create(&RouteLabels {
                listener: listener.as_str().to_owned(),
                method: method.to_owned(),
                route: route.to_owned(),
            })
            .observe(elapsed.as_secs_f64());
    }

    /// Count a failed Graph call.
    pub fn graph_error(&self, code: &str) {
        self.graph_errors
            .get_or_create(&CodeLabels {
                code: code.to_owned(),
            })
            .inc();
    }

    /// The Prometheus text exposition.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        // Writing into a String cannot fail.
        let _ = prometheus_client::encoding::text::encode(&mut out, &registry);
        out
    }
}
