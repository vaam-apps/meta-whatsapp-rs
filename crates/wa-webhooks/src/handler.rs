//! [`WebhookHandler`]: the framework-independent core of a webhook endpoint.
//!
//! `deliver` runs, in this order: size check → signature (before any
//! parsing) → parse → normalize → per event: dedup claim → sink. The `axum`
//! router is a thin HTTP mapping of it; any other framework can call it the
//! same way.

use std::sync::Arc;

use serde_json::Value;
use wa_core::error::ValidationError;
use wa_core::secret::VerifyToken;
use wa_core::sink::EventSink;

use crate::dedup::DedupGuard;
use crate::event::WebhookEvent;
use crate::payload::WebhookPayload;
use crate::signature::SignatureVerifier;
use crate::verify::{VerificationQuery, verify_subscription};

/// Default body size limit: 3 MiB.
///
/// `webhooks/overview` (§ Payload size) says payloads can be up to 3 MB, and
/// `history` webhooks can describe thousands of messages. A lower limit
/// would answer `413` to legitimate deliveries, which Meta retries for 7
/// days and then drops.
pub const DEFAULT_MAX_BODY_BYTES: usize = 3 * 1024 * 1024;

/// What one `deliver` call did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeliveryReport {
    /// Events handed to the sink (including an `Unparsed` one).
    pub delivered: usize,
    /// Events skipped because the dedup guard had seen them.
    pub duplicates: usize,
    /// Bodies that verified but did not parse (0 or 1).
    pub unparsed: usize,
}

/// Verifies, parses, deduplicates and dispatches webhook deliveries.
///
/// Build it with [`WebhookHandler::builder`]; share it behind an `Arc`.
#[derive(Debug)]
pub struct WebhookHandler {
    verifier: SignatureVerifier,
    verify_token: VerifyToken,
    sink: Arc<dyn EventSink<WebhookEvent>>,
    dedup: Option<DedupGuard>,
    max_body_bytes: usize,
}

/// Builder for [`WebhookHandler`]. The required parts are the arguments of
/// [`WebhookHandler::builder`], so `build` cannot fail.
#[derive(Debug)]
#[must_use]
pub struct WebhookHandlerBuilder {
    handler: WebhookHandler,
}

impl WebhookHandlerBuilder {
    /// Deduplicate Meta's retries with `guard` (recommended in production:
    /// Meta retries for 7 days and sends to every subscribed app).
    pub fn dedup(mut self, guard: DedupGuard) -> Self {
        self.handler.dedup = Some(guard);
        self
    }

    /// Reject bodies larger than `bytes` (default [`DEFAULT_MAX_BODY_BYTES`]).
    pub fn max_body_bytes(mut self, bytes: usize) -> Self {
        self.handler.max_body_bytes = bytes;
        self
    }

    /// Finish.
    pub fn build(self) -> WebhookHandler {
        self.handler
    }
}

impl WebhookHandler {
    /// Start building a handler.
    pub fn builder(
        verifier: SignatureVerifier,
        verify_token: VerifyToken,
        sink: Arc<dyn EventSink<WebhookEvent>>,
    ) -> WebhookHandlerBuilder {
        WebhookHandlerBuilder {
            handler: Self {
                verifier,
                verify_token,
                sink,
                dedup: None,
                max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            },
        }
    }

    /// The configured body size limit.
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// Answer a subscription verification `GET`: the challenge to echo with
    /// a `200`, or an error to answer `403` to.
    pub fn verify(&self, query: &VerificationQuery) -> wa_core::Result<String> {
        verify_subscription(query, &self.verify_token)
    }

    /// Handle one webhook `POST`.
    ///
    /// `Ok` means "answer `200`": every event was delivered or was a
    /// duplicate, or the body verified but was not a webhook payload (then
    /// it is delivered as [`WebhookEvent::Unparsed`] and logged with
    /// `tracing::error!` — retrying an unparseable body for 7 days helps no
    /// one).
    ///
    /// Errors, and the answer they call for:
    /// - [`wa_core::Error::Validation`] (`field == "body"`): too large → `413`.
    /// - [`wa_core::Error::Webhook`]: missing, malformed or wrong signature → `401`.
    /// - [`wa_core::Error::Sink`] / [`wa_core::Error::Storage`]: the sink or the
    ///   dedup store failed → `500`, so Meta redelivers.
    ///
    /// Dedup claims are taken one event at a time, just before that event
    /// is handed to the sink. When the sink fails, the failing event's claim
    /// is released before returning; the events after it were never
    /// claimed, and the ones before it were delivered and keep theirs. So
    /// Meta's redelivery of the batch skips exactly the delivered events.
    pub async fn deliver(
        &self,
        signature_header: Option<&str>,
        body: &[u8],
    ) -> wa_core::Result<DeliveryReport> {
        if body.len() > self.max_body_bytes {
            return Err(ValidationError::new(
                "body",
                format!(
                    "{} bytes exceeds the {}-byte limit",
                    body.len(),
                    self.max_body_bytes
                ),
            )
            .into());
        }
        if let Err(error) = self.verifier.verify(signature_header, body) {
            tracing::warn!(%error, body_len = body.len(), "rejected webhook delivery");
            return Err(error.into());
        }
        match WebhookPayload::from_slice(body) {
            Ok(payload) => self.dispatch(payload.into_events()).await,
            Err(error) => self.deliver_unparsed(body, &error).await,
        }
    }

    async fn deliver_unparsed(
        &self,
        body: &[u8],
        error: &serde_json::Error,
    ) -> wa_core::Result<DeliveryReport> {
        // The body is not logged: it may hold message content (personal data).
        tracing::error!(
            %error,
            body_len = body.len(),
            "signed webhook body is not a webhook payload; acknowledging it as `Unparsed`"
        );
        let raw = serde_json::from_slice::<Value>(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
        self.sink
            .deliver(WebhookEvent::Unparsed {
                raw,
                error: error.to_string(),
            })
            .await?;
        Ok(DeliveryReport {
            delivered: 1,
            duplicates: 0,
            unparsed: 1,
        })
    }

    async fn dispatch(&self, events: Vec<WebhookEvent>) -> wa_core::Result<DeliveryReport> {
        let mut report = DeliveryReport::default();
        for event in events {
            if let WebhookEvent::Unknown {
                field,
                parse_error: Some(error),
                ..
            } = &event
            {
                tracing::warn!(%field, %error, "webhook change kept untyped");
            }
            let claimed = match (&self.dedup, event.dedup_key()) {
                (Some(guard), Some(key)) => {
                    if !guard.claim_key(&key).await? {
                        report.duplicates += 1;
                        continue;
                    }
                    Some((guard, key))
                }
                _ => None,
            };
            if let Err(error) = self.sink.deliver(event).await {
                if let Some((guard, key)) = claimed
                    && let Err(release_error) = guard.release_key(&key).await
                {
                    tracing::error!(
                        error = %release_error,
                        "could not release a dedup claim after a sink failure; \
                         Meta's redelivery of that event will be skipped as a duplicate"
                    );
                }
                tracing::warn!(
                    %error,
                    delivered = report.delivered,
                    "event sink failed; answering non-200 so Meta redelivers"
                );
                return Err(error.into());
            }
            report.delivered += 1;
        }
        Ok(report)
    }
}
