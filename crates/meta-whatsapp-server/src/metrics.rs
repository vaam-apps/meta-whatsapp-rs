//! Prometheus metrics, served at `/metrics` on the internal listener
//! (docs/design/server.md, section 7.3).
//!
//! Labelled by listener, method, route **template**, status and error
//! code, never by an id, a number or a contact. Every label takes a
//! bounded set of values: a method outside the standard ones is `other`
//! (a client, on the public listener anyone, may send any token as a
//! method), an unknown path `unmatched`.
//!
//! | Metric | Labels |
//! | --- | --- |
//! | `wa_server_http_requests_total` | `listener`, `method`, `route`, `status`, `code` (empty on success) |
//! | `wa_server_http_request_duration_seconds` | `listener`, `method`, `route` |
//! | `wa_server_graph_errors_total` | `code` (the API error code of a failed Graph call) |
//! | `wa_server_webhook_deliveries_total` | `outcome`: Meta's `POST /webhooks/meta` answered `delivered` (200), `unauthenticated` (401), `payload_too_large` (413), `slow_body` (408: the body took over 15 s), `busy` (503: 64 deliveries already in flight on the replica, no turn to record within 10 s, or the outbox locked for over 2 s), `in_flight` (503: another request holds an event's dedup lease; a run of them means sinks outlast the lease), `failed` (500) |
//! | `wa_server_webhook_events_total` | `event_type` (the event's type), `audience` (`tenant` or `operator`: an operator-only row) |
//! | `wa_server_webhook_duplicate_events_total` | `stage`: `dedup` (the dedup lease had seen it), `outbox` (the outbox had it) |
//! | `wa_server_webhook_sink_failures_total` | `stage`: `routing`, `serialization`, `inbox`, `outbox`, `outbox_busy` (a lock waited for over 2 s: `busy`) |
//! | `wa_server_idempotency_total` | `outcome`: `replayed`, `reused`, `in_progress`, `outcome_unknown` (a repeat that met a key's record) |
//! | `wa_server_rate_limited_total` | `class` (`send`, `read`, `templates`): requests refused `429` by the service's own limits |

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use meta_whatsapp_rs::webhooks::axum::http::Method;
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

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct OutcomeLabels {
    outcome: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct EventLabels {
    event_type: String,
    audience: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct StageLabels {
    stage: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ClassLabels {
    class: String,
}

type DurationFamily = Family<RouteLabels, Histogram, fn() -> Histogram>;

/// An HTTP method as a label or a log field: the methods an API client
/// sends as they are, anything else `other`. hyper accepts any token as a
/// method, hundreds of kilobytes long: kept as is, each new one would add
/// label sets that are never freed, and log lines of any size.
pub fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

/// The service's metrics. Cheap to clone.
#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Mutex<Registry>>,
    requests: Family<RequestLabels, Counter>,
    durations: DurationFamily,
    graph_errors: Family<CodeLabels, Counter>,
    webhook_deliveries: Family<OutcomeLabels, Counter>,
    webhook_events: Family<EventLabels, Counter>,
    webhook_duplicates: Family<StageLabels, Counter>,
    webhook_failures: Family<StageLabels, Counter>,
    idempotency: Family<OutcomeLabels, Counter>,
    rate_limited: Family<ClassLabels, Counter>,
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
        let webhook_deliveries = Family::<OutcomeLabels, Counter>::default();
        let webhook_events = Family::<EventLabels, Counter>::default();
        let webhook_duplicates = Family::<StageLabels, Counter>::default();
        let webhook_failures = Family::<StageLabels, Counter>::default();
        registry.register(
            "webhook_deliveries",
            "Meta's webhook deliveries, by outcome",
            webhook_deliveries.clone(),
        );
        registry.register(
            "webhook_events",
            "Webhook events recorded in the outbox, by type and audience (tenant or operator)",
            webhook_events.clone(),
        );
        registry.register(
            "webhook_duplicate_events",
            "Webhook events already recorded, by the stage that recognised them",
            webhook_duplicates.clone(),
        );
        registry.register(
            "webhook_sink_failures",
            "Webhook events that failed to be recorded, by stage (Meta redelivers them)",
            webhook_failures.clone(),
        );
        let idempotency = Family::<OutcomeLabels, Counter>::default();
        registry.register(
            "idempotency",
            "Requests whose Idempotency-Key met an earlier request's record, by outcome",
            idempotency.clone(),
        );
        let rate_limited = Family::<ClassLabels, Counter>::default();
        registry.register(
            "rate_limited",
            "Requests refused by the service's rate limits, by route class",
            rate_limited.clone(),
        );
        Self {
            registry: Arc::new(Mutex::new(registry)),
            requests,
            durations,
            graph_errors,
            webhook_deliveries,
            webhook_events,
            webhook_duplicates,
            webhook_failures,
            idempotency,
            rate_limited,
        }
    }

    /// Count an answered request.
    ///
    /// `method` is a [`method_label`]: the request's own token never
    /// becomes a label.
    pub fn request(
        &self,
        listener: Listener,
        method: &'static str,
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

    /// Count one of Meta's webhook deliveries by its outcome (a fixed set:
    /// see the module docs).
    pub fn webhook_delivery(&self, outcome: &'static str) {
        self.webhook_deliveries
            .get_or_create(&OutcomeLabels {
                outcome: outcome.to_owned(),
            })
            .inc();
    }

    /// Count a repeat that met an idempotency key's record (`outcome`:
    /// `replayed`, `reused`, `in_progress`, `outcome_unknown`).
    pub fn idempotency(&self, outcome: &'static str) {
        self.idempotency
            .get_or_create(&OutcomeLabels {
                outcome: outcome.to_owned(),
            })
            .inc();
    }

    /// Count an event recorded in the outbox. `event_type` is the library's
    /// `WebhookEvent::kind` (a fixed set), `audience` `tenant` or
    /// `operator`.
    pub fn webhook_event(&self, event_type: &'static str, audience: &'static str) {
        self.webhook_events
            .get_or_create(&EventLabels {
                event_type: event_type.to_owned(),
                audience: audience.to_owned(),
            })
            .inc();
    }

    /// Count `n` events already recorded, recognised at `stage` (`dedup`
    /// or `outbox`).
    pub fn webhook_duplicates(&self, stage: &'static str, n: u64) {
        if n > 0 {
            self.webhook_duplicates
                .get_or_create(&StageLabels {
                    stage: stage.to_owned(),
                })
                .inc_by(n);
        }
    }

    /// Count an event that failed to be recorded at `stage`.
    pub fn webhook_failure(&self, stage: &'static str) {
        self.webhook_failures
            .get_or_create(&StageLabels {
                stage: stage.to_owned(),
            })
            .inc();
    }

    /// Count a request refused by the rate limits of `class`.
    pub fn rate_limited(&self, class: &'static str) {
        self.rate_limited
            .get_or_create(&ClassLabels {
                class: class.to_owned(),
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
