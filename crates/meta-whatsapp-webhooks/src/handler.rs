//! [`WebhookHandler`]: the framework-independent core of a webhook endpoint.
//!
//! `deliver` runs, in this order: size check → signature (before any
//! parsing, and before any allocation proportional to the body) → parse →
//! normalize → per event: dedup claim → sink (→ on a permanent sink error,
//! with a dead-letter store: the dead-letter write) → dedup complete. The `axum`
//! router is a thin HTTP mapping of it; any other framework can call it the
//! same way.
//!
//! Nothing derived from a body's content is logged: bodies carry personal
//! data, and serde's error messages quote it. Logs get sizes, SHA-256
//! digests, field names and redacted error text (see `crate::redact`).

use std::sync::Arc;

use meta_whatsapp_core::error::{ConfigError, SinkError, WebhookError};
use meta_whatsapp_core::secret::VerifyToken;
use meta_whatsapp_core::sink::EventSink;
use serde_json::Value;

use crate::deadletter::DeadLetters;
use crate::dedup::{Claim, ClaimTicket, DedupGuard};
use crate::event::WebhookEvent;
use crate::payload::WebhookPayload;
use crate::redact;
use crate::signature::SignatureVerifier;
use crate::verify::{VerificationQuery, check_verify_token, verify_subscription};

/// Default body size limit: 3 MiB (3 145 728 bytes).
///
/// `webhooks/overview` (§ Payload size) says payloads can be up to "3 MB"
/// without saying which megabyte. The binary reading is the larger one, so
/// it accepts every payload either reading allows; the smaller limit would
/// answer `413` to a legitimate delivery between 3 000 000 and 3 145 728
/// bytes, which Meta retries for 7 days and then drops. The extra 141 KiB
/// is only ever buffered and hashed, never parsed, before the signature
/// checks out.
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
    /// Events the sink refused permanently, written to the dead-letter
    /// store ([`WebhookHandlerBuilder::dead_letters`]) and acknowledged.
    /// Not counted in `delivered`. Alert on it.
    pub dead_lettered: usize,
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
    dead_letters: Option<DeadLetters>,
    max_body_bytes: usize,
}

/// Builder for [`WebhookHandler`]. The required parts are the arguments of
/// [`WebhookHandler::builder`], so `build` cannot fail; what it cannot
/// refuse, a blank verify token, [`WebhookHandler::check`] reports.
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

    /// Dead-letter the events the sink refuses permanently
    /// ([`SinkError::Permanent`]) in `store`, and acknowledge them, instead
    /// of answering `500` for the whole batch: see [`crate::deadletter`].
    /// Use it with a [`Self::dedup`] guard, or a redelivery of the body
    /// dead-letters the event again.
    pub fn dead_letters(mut self, store: DeadLetters) -> Self {
        self.handler.dead_letters = Some(store);
        self
    }

    /// Reject bodies larger than `bytes` (default [`DEFAULT_MAX_BODY_BYTES`]).
    pub fn max_body_bytes(mut self, bytes: usize) -> Self {
        self.handler.max_body_bytes = bytes;
        self
    }

    /// Finish. Call [`WebhookHandler::check`] on the result at startup.
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
                dead_letters: None,
                max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            },
        }
    }

    /// Check what [`WebhookHandlerBuilder::build`] cannot refuse: today, a
    /// blank verify token (empty, or nothing but whitespace). Run it at
    /// startup, right after `build`, and stop there on an error; it is
    /// the caller's to run, and nothing else runs it.
    ///
    /// Without it, a blank token surfaces only when Meta's verification
    /// request arrives: [`WebhookHandler::verify`] then refuses every
    /// request, so the dashboard cannot verify the callback URL.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use meta_whatsapp_core::error::SinkError;
    /// # use meta_whatsapp_core::secret::{AppSecret, VerifyToken};
    /// # use meta_whatsapp_core::sink::EventSink;
    /// # use meta_whatsapp_webhooks::{SignatureVerifier, WebhookEvent, WebhookHandler};
    /// # #[derive(Debug)]
    /// # struct Discard;
    /// # #[async_trait::async_trait]
    /// # impl EventSink<WebhookEvent> for Discard {
    /// #     async fn deliver(&self, _: WebhookEvent) -> Result<(), SinkError> { Ok(()) }
    /// # }
    /// # fn main() -> meta_whatsapp_core::Result<()> {
    /// # let sink: Arc<dyn EventSink<WebhookEvent>> = Arc::new(Discard);
    /// let handler = WebhookHandler::builder(
    ///     SignatureVerifier::new(vec![AppSecret::new("app-secret")])?,
    ///     VerifyToken::new(" "), // read from a variable set to a space
    ///     sink.clone(),
    /// )
    /// .build();
    /// assert!(handler.check().is_err()); // at startup: stop here
    ///
    /// let handler = WebhookHandler::builder(
    ///     SignatureVerifier::new(vec![AppSecret::new("app-secret")])?,
    ///     VerifyToken::new("vibecoding"),
    ///     sink,
    /// )
    /// .build();
    /// handler.check()?;
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when the verify token is blank. Its text is fixed
    /// and names no part of the token.
    pub fn check(&self) -> Result<(), ConfigError> {
        check_verify_token(&self.verify_token)
    }

    /// The configured body size limit.
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// Answer a subscription verification `GET`: the challenge to echo with
    /// a `200`, or an error to answer `403` to. A blank configured verify
    /// token rejects every request ([`verify_subscription`]).
    pub fn verify(&self, query: &VerificationQuery) -> meta_whatsapp_core::Result<String> {
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
    /// Errors, and the answer they call for (anything but `200` makes Meta
    /// redeliver):
    /// - [`WebhookError::PayloadTooLarge`]: body over the limit → `413`.
    /// - [`WebhookError::MissingSignature`], [`WebhookError::MalformedSignature`],
    ///   [`WebhookError::SignatureMismatch`] → `401`.
    /// - [`meta_whatsapp_core::Error::Sink`] / [`meta_whatsapp_core::Error::Storage`]: the sink, the
    ///   dedup store or the dead-letter store failed → `500`.
    /// - [`meta_whatsapp_core::error::WebhookError::ClaimInFlight`]: another request
    ///   is delivering one of the events right now → `503`.
    ///
    /// With a [`DedupGuard`], each event is claimed just before it goes to
    /// the sink and marked done just after. When the sink fails, that
    /// event's claim is released; the events before it stay done and the
    /// events after it were never claimed, so Meta's redelivery of the
    /// batch delivers exactly the ones that did not get through. If this
    /// future is dropped mid-delivery (client gone, timeout, crash), the
    /// claim expires after the guard's lease instead of swallowing the
    /// event.
    ///
    /// With a dead-letter store ([`WebhookHandlerBuilder::dead_letters`]),
    /// an event the sink refuses permanently ([`SinkError::is_permanent`])
    /// is written there, its claim marked done, and the next event
    /// delivered: it counts in [`DeliveryReport::dead_lettered`], and the
    /// delivery is acknowledged. If that write fails, the claim is released
    /// and the store's error returned (`500`), so Meta redelivers. Without
    /// one, a permanent error is answered like any other.
    pub async fn deliver(
        &self,
        signature_header: Option<&str>,
        body: &[u8],
    ) -> meta_whatsapp_core::Result<DeliveryReport> {
        if body.len() > self.max_body_bytes {
            tracing::warn!(
                body_len = body.len(),
                limit = self.max_body_bytes,
                "rejected oversized webhook delivery"
            );
            return Err(WebhookError::PayloadTooLarge {
                size: body.len(),
                limit: self.max_body_bytes,
            }
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
    ) -> meta_whatsapp_core::Result<DeliveryReport> {
        // Size and digest, never content: the body may hold personal data,
        // and so may serde's error text, hence the redaction.
        tracing::error!(
            error = %redact::serde_message(&error.to_string()),
            category = ?error.classify(),
            body_len = body.len(),
            body_sha256 = %redact::body_digest(body),
            "signed webhook body is not a webhook payload; acknowledging it as `Unparsed`"
        );
        let raw = serde_json::from_slice::<Value>(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
        let mut report = DeliveryReport {
            unparsed: 1,
            ..DeliveryReport::default()
        };
        let event = WebhookEvent::Unparsed {
            raw,
            error: error.to_string(),
        };
        match self.hand_over(event).await? {
            Handed::Delivered => report.delivered = 1,
            Handed::DeadLettered => report.dead_lettered = 1,
        }
        Ok(report)
    }

    /// Hand `event` to the sink. A permanent sink error, with a dead-letter
    /// store, is dead-lettered; any other error is returned.
    async fn hand_over(&self, event: WebhookEvent) -> Result<Handed, HandOverError> {
        let Some(store) = &self.dead_letters else {
            self.sink
                .deliver(event)
                .await
                .map_err(HandOverError::Sink)?;
            return Ok(Handed::Delivered);
        };
        // The sink consumes the event; a dead letter needs it after.
        let kept = event.clone();
        match self.sink.deliver(event).await {
            Ok(()) => Ok(Handed::Delivered),
            Err(error) if error.is_permanent() => match store.record(&kept, &error).await {
                Ok(_) => Ok(Handed::DeadLettered),
                Err(store_error) => {
                    tracing::error!(
                        error = %store_error,
                        kind = kept.kind(),
                        "could not dead-letter an event the sink refused permanently; \
                         answering non-200 so Meta redelivers"
                    );
                    Err(HandOverError::Other(store_error))
                }
            },
            Err(error) => Err(HandOverError::Sink(error)),
        }
    }

    /// Release `ticket` after the event could not be handed over.
    async fn release(ticket: Option<&(&DedupGuard, ClaimTicket)>) {
        if let Some((guard, ticket)) = ticket
            && let Err(release_error) = guard.release(ticket).await
        {
            tracing::error!(
                error = %release_error,
                "could not release a dedup claim after a sink failure; \
                 Meta's redelivery of that event waits for the claim's lease to expire"
            );
        }
    }

    async fn dispatch(
        &self,
        events: Vec<WebhookEvent>,
    ) -> meta_whatsapp_core::Result<DeliveryReport> {
        let mut report = DeliveryReport::default();
        for event in events {
            if let WebhookEvent::Unknown {
                field,
                parse_error: Some(error),
                ..
            } = &event
            {
                tracing::warn!(
                    %field,
                    error = %redact::serde_message(error),
                    "webhook change kept untyped"
                );
            }
            let ticket = match &self.dedup {
                Some(guard) => match guard.claim(&event).await? {
                    Claim::Acquired(ticket) => Some((guard, ticket)),
                    Claim::Duplicate => {
                        report.duplicates += 1;
                        continue;
                    }
                    Claim::InFlight => {
                        tracing::warn!(
                            kind = event.kind(),
                            delivered = report.delivered,
                            "webhook event is being delivered by another request; \
                             answering non-200 so Meta retries"
                        );
                        return Err(meta_whatsapp_core::error::WebhookError::ClaimInFlight.into());
                    }
                    // `Untracked`, or a variant added later: deliver.
                    _ => None,
                },
                None => None,
            };
            match self.hand_over(event).await {
                Ok(Handed::Delivered) => report.delivered += 1,
                Ok(Handed::DeadLettered) => report.dead_lettered += 1,
                Err(error) => {
                    Self::release(ticket.as_ref()).await;
                    if let HandOverError::Sink(error) = &error {
                        tracing::warn!(
                            %error,
                            delivered = report.delivered,
                            "event sink failed; answering non-200 so Meta redelivers"
                        );
                    }
                    return Err(error.into());
                }
            }
            if let Some((guard, ticket)) = &ticket {
                // The sink has the event (or the dead-letter store does:
                // acknowledged, so a redelivery of the body is a duplicate
                // rather than a second dead letter); failing the request
                // now would only make Meta redeliver what we already have.
                // Log and go on: the claim's lease expiring is the worst
                // case (a duplicate delivery if Meta retries this body for
                // another reason).
                match guard.complete(ticket).await {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(
                        "dedup lease expired before the event was marked delivered; \
                         it may be delivered twice"
                    ),
                    Err(error) => tracing::error!(
                        %error,
                        "could not mark a delivered webhook event as done; \
                         it may be delivered twice"
                    ),
                }
            }
        }
        Ok(report)
    }
}

/// Where [`WebhookHandler::hand_over`] put an event.
enum Handed {
    Delivered,
    DeadLettered,
}

/// Why [`WebhookHandler::hand_over`] failed: the sink (logged by the
/// caller), or the dead-letter store (logged already).
enum HandOverError {
    Sink(SinkError),
    Other(meta_whatsapp_core::Error),
}

impl From<HandOverError> for meta_whatsapp_core::Error {
    fn from(error: HandOverError) -> Self {
        match error {
            HandOverError::Sink(error) => error.into(),
            HandOverError::Other(error) => error,
        }
    }
}
