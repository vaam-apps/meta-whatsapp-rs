# Webhooks

**Goal:** a public endpoint that accepts Meta's deliveries for every WABA
your app serves, verifies them, records each event once, answers fast, and
alerts you when an account or template needs attention.

Example: [`cms_inbox.rs`](../../crates/wa-rs/examples/cms_inbox.rs) (its
`/webhook` routes). Agent skills:
[`wa-rs-webhook-endpoint`](../../skills/wa-rs-webhook-endpoint/SKILL.md),
[`wa-rs-webhook-events`](../../skills/wa-rs-webhook-events/SKILL.md) (the full event
table is its [references/events.md](../../skills/wa-rs-webhook-events/references/events.md)),
[`wa-rs-live-updates`](../../skills/wa-rs-live-updates/SKILL.md).

```text
POST ─► X-Hub-Signature-256 present and well-formed? (else 401 before the body is read)
     ─► body limit (3 MiB) ─► signature over the raw bytes (any of N app secrets)
     ─► parse ─► Vec<WebhookEvent> ─► per event: dedup claim ─► your EventSink ─► dedup done
     ─► 200 only when every event was delivered or was a duplicate
```

## 1. On Meta's side

- **Callback URL:** public, HTTPS, valid certificate (self-signed ones are
  not supported). Mutual TLS is available per app if you want to restrict
  who can connect.
- **Verify token:** a random string you choose. When you save the callback
  URL or the token in the App Dashboard (the WhatsApp use case's
  **Configuration** panel), Meta sends a `GET` with `hub.mode`,
  `hub.verify_token` and `hub.challenge`; the endpoint must echo the
  challenge with a `200`.
- **Fields:** subscribe to what you use (section 5). `messages` needs the
  `whatsapp_business_messaging` permission; every other field
  `whatsapp_business_management`.
- **Per WABA:** your app also has to be subscribed to each WABA
  (`POST /{waba}/subscribed_apps`). Embedded Signup does this for merchants.
  For your own WABA, check `client.waba(waba_id).subscribed_apps().await?`
  and call `subscribe_app(None)` if your app is missing.

Meta's pages:
[webhooks/overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/overview),
[webhooks/create-webhook-endpoint](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/create-webhook-endpoint),
[webhooks/override](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/override),
[throughput](https://developers.facebook.com/documentation/business-messaging/whatsapp/throughput).

## 2. The endpoint (axum)

```rust
use std::sync::Arc;
use wa_rs::prelude::*;

let handler = WebhookHandler::builder(
    SignatureVerifier::new(vec![AppSecret::new(app_secret)])?, // refuses an empty list or a blank secret
    VerifyToken::new(verify_token),
    Arc::new(sink), // Arc<dyn EventSink<WebhookEvent>>, section 3
)
.dedup(DedupGuard::new(kv.clone())) // a KvStore shared by every instance, section 4
.build();

// The axum the router is built with, re-exported: no axum dependency of your own.
let app = wa_rs::webhooks::axum::Router::new().nest("/webhooks/whatsapp", wa_rs::webhooks::router(Arc::new(handler)));
```

- `router` serves `GET` (verification) and `POST` (deliveries) and sets the
  body limit to the handler's (`DEFAULT_MAX_BODY_BYTES`, 3 MiB; Meta
  documents payloads up to 3 MB), replacing axum's 2 MiB default. Change it
  with `.max_body_bytes(n)` on the builder, not with a layer in front.
- The signature is over the **raw bytes**: nothing in front of this route
  may parse, decompress or re-serialize the body.
- A blank app secret fails at startup. A blank verify token makes every
  verification answer `403`, but only when one arrives
  ([open question](../../OPEN_QUESTIONS.md#webhooks) 16): read both from
  your secret store and check them at boot.

| Outcome | Answer | Meta then |
| --- | --- | --- |
| delivered, duplicate, or signed but unparseable (`Unparsed`) | `200` | stops |
| missing or malformed signature header (answered before the body is read) | `401` | retries |
| wrong signature | `401` | retries |
| body over the limit | `413` | retries |
| another request is delivering one of the events (`ClaimInFlight`) | `503` | retries later |
| your sink or the dedup store failed | `500` | retries for up to 7 days |
| `GET` with a wrong token or mode | `403` | shows an error in the dashboard |

Anything but `200` makes Meta redeliver the **whole batch**.

### Another framework

Call the handler yourself and answer the same way. `VerificationQuery`
deserializes from the query string (it uses Meta's `hub.*` names). For a
`POST`, look at the signature header before reading the body, as `router`
does: `wa_rs::webhooks::SIGNATURE_HEADER` (`x-hub-signature-256`) is
available without the `axum` feature. Without it, answer `401` at once;
otherwise read the raw body with a limit of `handler.max_body_bytes()`
(`413` over it) and pass both to `deliver`, which takes the header as an
`Option<&str>` and checks its shape and the signature.

```rust
use wa_rs::core::error::WebhookError;
use wa_rs::webhooks::{DeliveryReport, VerificationQuery};

fn answer_get(handler: &WebhookHandler, query: &VerificationQuery) -> (u16, String) {
    match handler.verify(query) {
        Ok(challenge) => (200, challenge), // send as text/plain
        Err(_) => (403, String::new()),
    }
}

fn status_of(result: &wa_rs::Result<DeliveryReport>) -> u16 {
    match result {
        Ok(_) => 200,
        Err(Error::Webhook(
            WebhookError::MissingSignature | WebhookError::MalformedSignature | WebhookError::SignatureMismatch,
        )) => 401,
        Err(Error::Webhook(WebhookError::PayloadTooLarge { .. })) => 413,
        Err(Error::Webhook(WebhookError::ClaimInFlight)) => 503,
        Err(_) => 500,
    }
}
// POST: no SIGNATURE_HEADER → 401 without reading the body; else read at most
// handler.max_body_bytes() and answer status_of(&handler.deliver(Some(signature), &raw_body).await)
```

## 3. Answer fast: what the sink does

Meta expects a median answer under 250 ms and fewer than 1 % over one
second, delivers concurrently, and asks you to size for about three status
webhooks per outbound message plus one per inbound message. The handler
answers only after your sink returns, so the sink must be quick and must
persist what cannot be lost.

```rust
use async_trait::async_trait;
use wa_rs::adapters::sink::{BroadcastSink, ChannelMode, FanoutSink, channel};
use wa_rs::core::error::SinkError;
use wa_rs::webhooks::fields::MessageContent as Inbound;

#[derive(Debug)]
struct Orders; // holds your pool

#[async_trait]
impl EventSink<WebhookEvent> for Orders {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        match &event {
            WebhookEvent::StatusUpdated { status, .. } => {
                // `callback_data` you set on the send comes back here.
                if let Some(tag) = status.biz_opaque_callback_data.as_deref() {
                    record_delivery(tag, status.status.as_str()).await?; // idempotent upsert
                }
            }
            WebhookEvent::MessageReceived { message, .. } => {
                if let Inbound::Order(cart) = &message.content {
                    record_cart(&message.id, cart).await?; // keyed by message id
                }
            }
            _ => {}
        }
        Ok(())
    }
}

let (worker, jobs) = channel::<WebhookEvent>(1024); // slow side effects, run by a task reading `jobs`
let sink = FanoutSink::new()
    .with(Orders)                                        // durable: its error → 500 → Meta retries
    .with(BroadcastSink::from_sender(live.clone()))     // live UI, never fails
    .with(worker.with_mode(ChannelMode::TryOrFail));     // full queue → 500 instead of waiting
```

- **A sink error that can never succeed** holds back its batch: Meta
  redelivers the whole body (the events after the failing one too) for 7
  days, then drops it. Keep permanent failures out of the sink path
  (`InboxSink` records content exactly, and the stores keep it, U+0000
  included, for that reason); a dead-letter design
  is an [open question](../../OPEN_QUESTIONS.md#webhooks-and-live-updates) (30).
- **At least once.** The dedup guard removes Meta's retries, but a sink call
  that outlasts the lease, or a batch redelivered after one fanned-out sink
  failed, reaches the others again. Make every sink idempotent (key by
  message id or callback data).
- `ChannelSink` answers `Ok` once the event is **queued**: a crash loses the
  queue. Put only work you can lose or rebuild behind it, or write to a
  durable queue (an outbox table) inside your own sink.
- The CMS inbox persists with `InboxSink`: see [cms-inbox.md](cms-inbox.md).

## 4. Deduplication

Meta retries failed deliveries for 7 days and sends the same notification
to every app subscribed to a WABA. `DedupGuard::new(kv)` leases a claim per
event (60 s, `DEFAULT_CLAIM_LEASE`) before the sink, marks it done after
(kept 7 days and 1 hour, `DEFAULT_DEDUP_TTL`), and releases it when the
sink fails. A retry that meets a live lease gets `503`; one that finds a
lease left by a crashed request delivers after the lease expires.

| Store | Use |
| --- | --- |
| `MemoryKvStore` | one instance; each instance deduplicates alone, and a restart forgets |
| `PostgresKvStore` | production; call `purge_expired()` every few minutes to hourly (one row per event) |
| `RedisKvStore` | production; persistence on, eviction policy `noeviction` only (a `volatile-*` policy silently evicts dedup markers, then Meta's retries are delivered twice) |

Keep sink calls well under the lease (`.with_lease(d)` to change it).
`ErrorReported` and `Unparsed` events have no dedup key and are delivered
every time.

## 5. Which events matter

`WebhookEvent` is non-exhaustive: keep a `_` arm.

| Event (field) | E-commerce | CMS |
| --- | --- | --- |
| `MessageReceived` (`messages`) | replies, button taps, carts (`Order`) | the inbox |
| `StatusUpdated` (`messages`) | delivery of order updates; `errors` carry 131049/131050 | ticks in the inbox |
| `UserPreferenceChanged` (`user_preferences`) | marketing stop/resume | — |
| `TemplateStatusUpdated`, `TemplateQualityUpdated`, `TemplateCategoryUpdated` | template health | merchants' templates |
| `AccountUpdated` (`account_update`) | restrictions, violations | also onboarding and offboarding; for `Partner*` events `waba_id` comes from `waba_info` (the entry id is a business portfolio, kept as `entry_id`) |
| `PhoneNumberQualityUpdated`, `BusinessCapabilityUpdated`, `AccountAlert` | limits and quality | per merchant |
| `UserIdChanged` (`user_id_update`) | a customer's BSUID changed | merge conversations yourself |
| `Unknown`, `Unparsed` | a field or shape this version does not type (an `account_update` that did not parse keeps its `waba_info.waba_id` as `waba_id`) | same |

Key customers by their business-scoped user id (`contact.user_id`): since
2026 the phone number (`wa_id`) may be absent.

## 6. Security: verification, signatures, replay

- The verify token and the signature are compared in constant time.
  `SignatureVerifier::new` takes several app secrets, each tried, so
  deliveries signed with the old secret still verify while you roll out a
  new one; drop the old one afterwards.
- `router` refuses a request whose signature header is missing or malformed
  with `401` before reading its body, so an unsigned request cannot make the
  server buffer up to the body limit.
- Meta's signature carries no timestamp: a captured body can be replayed.
  Dedup turns replays within 7 days into duplicates. Not logging bodies is
  what keeps them from being captured; mTLS narrows who can connect at all.
- The library logs sizes, digests and field names, never payload values.
  Do not log request bodies, the signature header, or `WebhookEvent`'s
  `Debug` (names, numbers, message text) yourself.
- In tests, `wa_rs::webhooks::sign(&secret, body)` produces the header
  value Meta would send.

## 7. Callback overrides

Precedence is phone number, then WABA, then the app's callback. Set one with
`waba(id).subscribe_app(Some(&CallbackOverride::new(url, token)))` or
`phone_number(id).set_webhook_override(&override_)`
(`clear_webhook_override()` removes it); Meta verifies the URL with the
token like the app's callback. Only some fields follow an override
(`messages`, calls, groups, coexistence sync, among others); template and
account webhooks always go to the app's callback, so that endpoint must
exist even when every merchant has an override.

## 8. Operational alerts

```rust
use wa_rs::adapters::sink::FnSink;
use wa_rs::webhooks::fields::{AccountUpdateEvent, TemplateStatusEvent};

let alerts = FnSink::new(|event: WebhookEvent| async move {
    match &event {
        WebhookEvent::TemplateStatusUpdated { update, .. }
            if matches!(update.event, TemplateStatusEvent::Rejected | TemplateStatusEvent::Paused | TemplateStatusEvent::Disabled) =>
        {
            tracing::warn!(template = %update.message_template_name, status = update.event.as_str(), "template needs attention");
        }
        WebhookEvent::AccountUpdated { waba_id, update, .. }
            if matches!(update.event, AccountUpdateEvent::AccountRestriction | AccountUpdateEvent::AccountViolation) =>
        {
            // `Option`: an update whose `waba_info` names no WABA has none.
            tracing::error!(waba = ?waba_id, "account restricted or in violation");
        }
        WebhookEvent::Unknown { field, .. } => tracing::warn!(%field, "field not typed by this wa-rs version"),
        WebhookEvent::Unparsed { .. } => tracing::error!("signed body that is not a webhook payload"),
        _ => {}
    }
    Ok(())
});
```

Add it to the `FanoutSink`. Also watch `TemplateQualityUpdated`
(`new_quality_score`), `TemplateCategoryUpdated` (a re-categorized
template is billed differently), `PhoneNumberQualityUpdated` and
`BusinessCapabilityUpdated` (messaging limits), and the rate of `Unknown`:
a rising count usually means Meta shipped a field worth typing.

## Not handled

Messaging handovers and standby, `message_echoes` and `consumer_profile`
(no documented payloads) arrive as `Unknown`. There is no API to fetch past
webhooks: what your sink did not persist is gone after Meta's 7 days.
