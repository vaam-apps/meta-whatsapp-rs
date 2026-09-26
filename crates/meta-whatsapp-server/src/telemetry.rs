//! Logs, request ids and the per-request span (docs/design/server.md,
//! section 7.3).
//!
//! Each request runs in a `request` span carrying its id, method, route
//! **template** (never the raw path: paths name numbers and, later,
//! contacts), tenant, the public id of the key that authenticated it and
//! status, and ends with one `request` event with the status and duration.
//! Every change an operator makes is also an `audit` event ([`audit`]). Nothing here logs a header, a query string or
//! a body: API keys, tokens and phone numbers never reach the logs.

use std::time::Instant;

use meta_whatsapp_rs::webhooks::axum::extract::{Request, State};
use meta_whatsapp_rs::webhooks::axum::http::{HeaderName, HeaderValue};
use meta_whatsapp_rs::webhooks::axum::middleware::Next;
use meta_whatsapp_rs::webhooks::axum::response::Response;
use tracing::Instrument;

use crate::config::LogFormat;
use crate::metrics::{Metrics, method_label};

/// `X-Request-Id`: echoed when the caller sends a well-formed one, else
/// generated.
pub static REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

tokio::task_local! {
    static CURRENT_REQUEST_ID: String;
}

/// The id of the request being served on this task, if any.
pub fn current_request_id() -> Option<String> {
    CURRENT_REQUEST_ID.try_with(Clone::clone).ok()
}

/// Install the global subscriber: JSON or text, filtered by `filter`
/// (`RUST_LOG` syntax; an unparsable filter falls back to `info`).
pub fn init(format: LogFormat, filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    // A second call (tests) keeps the first subscriber.
    let _ = match format {
        LogFormat::Json => builder.json().flatten_event(true).try_init(),
        LogFormat::Text => builder.try_init(),
    };
}

/// A caller's request id, if it is 1 to 128 characters of
/// `[A-Za-z0-9._:-]` (anything else could forge log lines), else `None`.
fn accepted_request_id(value: &HeaderValue) -> Option<String> {
    let value = value.to_str().ok()?;
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    valid.then(|| value.to_owned())
}

/// A new request id: `req_` and 16 random hex digits.
fn new_request_id() -> String {
    let mut bytes = [0u8; 8];
    // The id only correlates log lines: a failing generator is no reason
    // to refuse the request.
    let _ = getrandom::fill(&mut bytes);
    format!("req_{}", hex::encode(bytes))
}

/// Which listener a router serves, for logs and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    /// Meta's webhook and `/livez`.
    Public,
    /// The API and operations.
    Internal,
}

impl Listener {
    /// Label value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
        }
    }
}

/// What [`observe`] needs: the listener, its route templates and the
/// metrics.
#[derive(Clone)]
pub struct Observed {
    /// Which listener.
    pub listener: Listener,
    /// The route templates this listener serves.
    pub routes: std::sync::Arc<Vec<String>>,
    /// Where request metrics go.
    pub metrics: Metrics,
}

/// The template among `routes` that `path` matches (`{name}` matches one
/// segment), or `unmatched`.
pub fn route_template<'a>(routes: &'a [String], path: &str) -> &'a str {
    let segments: Vec<&str> = path.split('/').collect();
    routes
        .iter()
        .find(|template| {
            let parts: Vec<&str> = template.split('/').collect();
            parts.len() == segments.len()
                && parts.iter().zip(&segments).all(|(t, s)| {
                    (t.starts_with('{') && t.ends_with('}') && !s.is_empty()) || t == s
                })
        })
        .map_or("unmatched", String::as_str)
}

/// Middleware around every request: request id, span, final log line and
/// request metrics.
pub async fn observe(
    State(observed): State<Observed>,
    mut request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let id = request
        .headers()
        .get(&REQUEST_ID)
        .and_then(accepted_request_id)
        .unwrap_or_else(new_request_id);
    if let Ok(value) = HeaderValue::from_str(&id) {
        request.headers_mut().insert(REQUEST_ID.clone(), value);
    }
    // A bounded label, never the request's own token (see `method_label`).
    let method = method_label(request.method());
    let route = route_template(&observed.routes, request.uri().path()).to_owned();
    // `tenant` and `key_id` are recorded by the authorization order (the
    // core's `Authorizer`, on the current span), once it knows them.
    let span = tracing::info_span!(
        "request",
        request_id = %id,
        method = %method,
        route = %route,
        tenant = tracing::field::Empty,
        key_id = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let mut response = CURRENT_REQUEST_ID
        .scope(id.clone(), next.run(request))
        .instrument(span.clone())
        .await;
    let status = response.status().as_u16();
    let elapsed = started.elapsed();
    span.record("status", status);
    span.in_scope(|| {
        let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        if route == "/livez" || route == "/readyz" || route == "/metrics" {
            tracing::debug!(status, duration_ms, "request");
        } else {
            tracing::info!(status, duration_ms, "request");
        }
    });
    let code = response
        .extensions()
        .get::<crate::error::ErrorCodeTag>()
        .map(|tag| tag.0);
    observed
        .metrics
        .request(observed.listener, method, &route, status, code, elapsed);
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID.clone(), value);
    }
    response
}

/// An operator's change, logged at `info` with the target `audit`:
/// `action`, the admin key's public id, and the ids it touched (tenant,
/// key, WABA; never a secret or a phone number).
pub fn audit(action: &'static str, admin_key_id: &str, subject: &Subject<'_>) {
    tracing::info!(
        target: "audit",
        action,
        admin_key_id,
        tenant = subject.tenant,
        key_id = subject.key_id,
        waba_id = subject.waba_id,
        "admin change"
    );
}

/// A tenant's change that destroys something at Meta (a template
/// deletion), logged like [`audit`] (target `audit`) with the public id of
/// the tenant or platform key that made it.
pub fn tenant_audit(action: &'static str, key_id: &str, subject: &Subject<'_>) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        "tenant change"
    );
}

/// What an [`audit`] event touched.
#[derive(Debug, Default, Clone, Copy)]
pub struct Subject<'a> {
    /// A tenant id.
    pub tenant: Option<&'a str>,
    /// A key's public id.
    pub key_id: Option<&'a str>,
    /// A WABA id.
    pub waba_id: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_match_segment_by_segment() {
        let routes: Vec<String> = [
            "/v1/numbers",
            "/v1/numbers/{pn}",
            "/v1/numbers/{pn}/profile",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(route_template(&routes, "/v1/numbers"), "/v1/numbers");
        assert_eq!(
            route_template(&routes, "/v1/numbers/123"),
            "/v1/numbers/{pn}"
        );
        assert_eq!(
            route_template(&routes, "/v1/numbers/123/profile"),
            "/v1/numbers/{pn}/profile"
        );
        assert_eq!(route_template(&routes, "/v1/numbers/"), "unmatched");
        assert_eq!(route_template(&routes, "/v1/numbers/1/2"), "unmatched");
        assert_eq!(route_template(&routes, "/elsewhere"), "unmatched");
    }

    #[test]
    fn only_well_formed_request_ids_are_echoed() {
        let ok = HeaderValue::from_static("abc-123_x.y:z");
        assert_eq!(accepted_request_id(&ok).as_deref(), Some("abc-123_x.y:z"));
        for bad in ["", "a b", "a\"b", "a/b"] {
            assert!(
                accepted_request_id(&HeaderValue::from_str(bad).unwrap()).is_none(),
                "{bad:?}"
            );
        }
        let long = "a".repeat(129);
        assert!(accepted_request_id(&HeaderValue::from_str(&long).unwrap()).is_none());
        assert!(new_request_id().starts_with("req_"));
    }
}
