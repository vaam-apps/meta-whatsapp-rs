---
name: wa-rs-webhooks
description: "Receiving WhatsApp webhooks with wa-rs - the endpoint (verify token, app secrets, 3 MiB body limit, axum router or any framework), WebhookHandler and the status codes Meta must get, the WebhookEvent variants that matter for e-commerce and a CMS, leased dedup (DedupGuard, ClaimInFlight answered 503), sinks (fan-out, broadcast to SSE, channel to a worker), forward-compatibility (Unknown, Unparsed, open enums) and keeping customer data out of logs. Load when writing or changing the webhook endpoint or anything that consumes webhook events."
---

# wa-rs-webhooks

> **Verified against wa-rs 7940d15 (2026-09-24).** On another revision, trust
> the code over this page (see `skills/README.md`).

Crate: `wa_rs::webhooks` (`router`/`sse` need the `axum` feature). One app =
one callback URL for **every** merchant's WABA (after `subscribe_app` in
onboarding, unless you set a per-WABA callback override); route events to
tenants by `event.phone_number_id()` (e.g. `TokenVault::get_by_phone_number`
or your own mapping).

```text
POST ─► X-Hub-Signature-256 present and well-formed? (else 401, body never read)
     ─► body, size-limited ─► signature over the raw bytes (any of N app secrets)
     ─► parse ─► Vec<WebhookEvent> ─► per event: dedup claim ─► your EventSink ─► dedup done
```

~~`POST ─► size limit ─► signature`~~: until 1529720 (2026-09-24) `router`
read the body (up to the limit) before looking at the header, so an unsigned
request made the server buffer it.

## The endpoint

```rust
use std::sync::Arc;
use wa_rs::core::secret::{AppSecret, VerifyToken};
use wa_rs::webhooks::{DedupGuard, SignatureVerifier, WebhookHandler, router};

let handler = WebhookHandler::builder(
    SignatureVerifier::new(vec![AppSecret::new(app_secret)])?, // add the old secret while rotating
    VerifyToken::new(verify_token),
    Arc::new(sink),                                            // Arc<dyn EventSink<WebhookEvent>>
)
.dedup(DedupGuard::new(kv.clone()))                            // shared KvStore in production
.build();

// The axum the router is built with, re-exported (feature `axum`).
let app = wa_rs::webhooks::axum::Router::new()
    .nest("/webhooks/whatsapp", router(Arc::new(handler)));
```

- **Fail closed on configuration.** `SignatureVerifier::new` refuses an empty
  list or a blank secret (an HMAC under `""` is forgeable by anyone). A blank
  `VerifyToken` makes every `GET` verification answer `403`. Read both from
  your secret store at startup and let a `Config` error stop the process.
- **Body limit**: `DEFAULT_MAX_BODY_BYTES` = 3 MiB (Meta documents payloads up
  to 3 MB); change with `.max_body_bytes(n)`. `router` also replaces axum's
  implicit 2 MiB limit. Do not put a smaller body-limit layer in front of
  this route, and do not put anything in front that parses or re-serializes
  the body: the signature is over the **raw bytes**.
- **Several app secrets** are accepted at once (each tried in constant time),
  so deliveries signed with the old secret still verify while a new one rolls
  out; drop the old one afterwards.

### Status codes (what `router` answers; do the same in another framework)

Not axum? Call `handler.verify(&VerificationQuery { .. })` for `GET` (returns
the challenge to echo as `text/plain`). For `POST`, do what `router` does:
read the `wa_rs::webhooks::SIGNATURE_HEADER` header (`x-hub-signature-256`;
available without the `axum` feature) first and answer `401` **before
reading the body** when it is missing; then read the raw body with a limit
of `handler.max_body_bytes()` (`413` over it) and call
`handler.deliver(Some(signature), &body)` (`deliver` takes
`Option<&str>`). Map its result:

| Outcome | Answer | Meta then |
| --- | --- | --- |
| `Ok(_)`: delivered, duplicate, or signed-but-unparseable (`Unparsed`) | `200` | stops |
| `Error::Webhook(MissingSignature \| MalformedSignature \| SignatureMismatch)` | `401` | retries |
| `Error::Webhook(PayloadTooLarge { .. })` | `413` | retries |
| `Error::Webhook(ClaimInFlight)` | `503` | retries later |
| `Error::Sink(..)` / `Error::Storage(..)` / anything else | `500` | retries (up to 7 days) |
| `GET`: `Error::Webhook(InvalidVerificationRequest(_) \| VerifyTokenMismatch)` or `Error::Config(_)` | `403` | — |

Anything but `200` makes Meta redeliver the **whole batch** for up to 7 days,
then drop it: one event whose sink fails every time holds back the events
after it in the same body until all are lost (a dead-letter design is
`OPEN_QUESTIONS.md` #30). Make permanent sink failures impossible where you
can; `InboxSink` does this for content Postgres cannot store (see
`wa-rs-cms-inbox`).

## Events

`WebhookEvent` is `#[non_exhaustive]` and serializes with an `"event"` tag
(`"message_received"`, …, the same as `event.kind()`). Always keep a `_ =>`
arm. The ones a store and a CMS care about:

| Variant | Carries | Use |
| --- | --- | --- |
| `MessageReceived` | `phone_number_id`, `contact: Option<Contact>`, `message: Box<InboundMessage>` | customer wrote; opens the 24 h window |
| `StatusUpdated` | `status: Box<Status>` (`.status`, `.errors`, `.biz_opaque_callback_data`, `.pricing`) | sent/delivered/read/failed of *your* messages |
| `ErrorReported` | `error: Box<GraphApiError>` | system/app-level errors |
| `UserPreferenceChanged` | `preference` (`.category`, `.value`: `PreferenceValue::Stop`/`Resume`) | marketing opt-out / opt-in |
| `UserIdChanged` | `update.user_id: IdChange { previous, current }` | a customer's BSUID changed |
| `TemplateStatusUpdated` | `update.event: TemplateStatusEvent`, `.reason`, `.message_template_id` | template approved/rejected/paused |
| `TemplateQualityUpdated`, `TemplateCategoryUpdated` | … | template health |
| `AccountUpdated`, `PhoneNumberQualityUpdated`, `PhoneNumberNameUpdated` | … | merchant account health |
| `MessageEchoed`, `HistorySynced`, `AppStateSynced` | … | coexistence (WhatsApp Business app) |
| `Unknown` | `field`, `raw`, `parse_error` | a field or shape this version does not type |
| `Unparsed` | `raw`, `error` | a signed body that is not a webhook envelope |

Full list and helpers: [references/events.md](references/events.md).

```rust
use wa_rs::webhooks::WebhookEvent;
use wa_rs::webhooks::fields::{MessageContent as Inbound, InteractiveReply};

match &event {
    WebhookEvent::MessageReceived { phone_number_id, contact, message, .. } => {
        // Key the customer by BSUID; the phone number (wa_id) may be absent.
        let who = contact.as_ref().and_then(|c| c.user_id.as_ref());
        match &message.content {
            Inbound::Text(t) => handle_text(phone_number_id, who, &t.body),
            Inbound::Interactive(InteractiveReply::ButtonReply(b)) => handle_button(&b.id),
            Inbound::Order(order) => handle_cart(&order.product_items),
            _ => {} // many more types, and Invalid/Unknown for shapes Meta adds
        }
    }
    WebhookEvent::StatusUpdated { status, .. } => {
        for e in &status.errors { let _ = e.kind(); } // 131049/131050 can arrive here
    }
    _ => {}
}
```

Name clashes to watch: `webhooks::fields::MessageContent` (inbound) vs
`client::messages::MessageContent` (outbound); `webhooks::fields::MessageStatus`
(status webhooks) vs `client::messages::MessageStatus` (send response);
`webhooks::fields::TemplateCategory` vs `client::templates::TemplateCategory`.
Alias one side on import.

## Dedup: a lease, not a marker

Meta retries for 7 days and sends the same notification to every app
subscribed to the WABA. `DedupGuard::new(kv)`:

1. **claim**: `put_if_absent` a `pending` marker living 60 s
   (`DEFAULT_CLAIM_LEASE`);
2. deliver to the sink;
3. **complete**: compare-and-swap to `done`, kept 7 days + 1 h
   (`DEFAULT_DEDUP_TTL`) — or **release** the claim if the sink failed.

A retry that finds a live `pending` claim gets `WebhookError::ClaimInFlight`
→ `503`, so Meta tries again later; answering `200` there could lose the
event if the other request dies. If a request dies mid-delivery, the lease
expires and the next retry delivers — a marker written up front would have
swallowed the event for 7 days. Semantics: **at-least-once, deduplicated**.
Sinks must still be idempotent: an event is delivered twice if a sink call
outlasts the lease, and `ErrorReported`/`Unparsed` are never deduplicated.

- Use a **shared** `KvStore` across instances, or each instance dedups alone.
- Keep sink calls well under the lease (`.with_lease(d)` to change it).
- On Postgres, call `PostgresKvStore::purge_expired()` periodically: dedup
  markers add a row per event.
- Store keys are SHA-256 of the dedup key (they can contain group
  participants' phone numbers).

## Sinks (`wa_rs::adapters::sink`, feature `sinks`)

The handler answers `200` only after your sink returns `Ok`. Keep it fast.

| Sink | Behaviour | Watch out |
| --- | --- | --- |
| `FanoutSink::new().with(a).with(b)` | delivers to all, concurrently; returns the first error | the ones that succeeded see the event again on redelivery |
| `BroadcastSink::new(cap)` (or `from_sender(tx)`) | live fan-out to subscribers; **never fails**; `.receiver()` is a tokio receiver for `sse` | no subscriber = dropped; slow subscribers get `Lagged`; each subscriber clones every event |
| `channel(cap)` → `ChannelSink` + `mpsc::Receiver` | hands events to a worker task | `Ok` once **enqueued**: a crash loses what is queued. `ChannelMode::TryOrFail` answers `500` when full instead of waiting |
| `FilterSink::new(inner, pred)` | filtered-out events count as handled | — |
| `FnSink::new(\|e\| async move { … })` | a closure | — |
| `TracingSink::new()` | logs the event kind only | `.with_payload(true)` logs customer data |

Persist what must not be lost *in* the sink path (e.g. `InboxSink` into
Postgres, see `wa-rs-cms-inbox`), and fan out to a channel/broadcast for the
rest. Your own sink: `#[async_trait] impl EventSink<WebhookEvent> for MySink`
returning `Result<(), SinkError>` (`SinkError::Delivery(anyhow)` for your
errors).

## Live updates over SSE

`sse(receiver, filter)` takes a **tokio `broadcast::Receiver<WebhookEvent>`**,
not a `BroadcastSubscription`. `BroadcastSink::receiver()` hands out exactly
that:

```rust
let live = BroadcastSink::<WebhookEvent>::new(256); // a clone goes into the FanoutSink
// in an authenticated handler, after checking the caller owns `pnid`:
wa_rs::webhooks::sse(live.receiver(), move |e| e.phone_number_id() == Some(&pnid))
```

(Creating the channel yourself and passing `tx.subscribe()`, with
`BroadcastSink::from_sender(tx.clone())`, still works; the `cms_inbox`
example does that.)

- Mount it **behind your own auth**, and make the filter an **allow-list**.
  `Unknown` and `Unparsed` have no phone number id and carry raw bodies that
  may belong to any tenant: `e.phone_number_id().is_none_or(…)` leaks them.
- Each event is `event: whatsapp` with the event JSON as data; when the
  receiver falls behind, `event: lagged` with the count arrives — reload
  history from the store.
- **Cost:** a broadcast receiver clones every event before the filter sees
  it, so each open stream copies every merchant's events, including
  multi-megabyte `HistorySynced` bodies. Fine for a handful of open
  inboxes; with many, see `OPEN_QUESTIONS.md` #31 (`Arc` events or one
  channel per phone number id).

## Forward compatibility

Meta adds fields, message types and enum values without notice, and an
endpoint that fails on them loses the delivery after 7 days of retries. So:

- Unknown fields → `WebhookEvent::Unknown { field, raw, parse_error }`;
  unknown message types → `MessageContent::Unknown`, bad shapes →
  `MessageContent::Invalid`; enum values → `Other(String)`. Handle them (log
  the field name, keep `raw` if you need it later); never fail on them.
- `Unparsed` bodies are acknowledged (`200`) and logged by size and digest
  only. Alert on them; do not answer non-`200`.
- If you mirror payloads into your own types, never use
  `#[serde(deny_unknown_fields)]`.

## Customer data in logs

The library logs sizes, SHA-256 digests, field names and redacted error text
— never payload values. Keep it that way in your code:

- Do not log request bodies, the signature header, or `WebhookEvent`'s
  `Debug` (names, phone numbers, message text). `TracingSink` without
  payload is the safe default.
- A captured signed body can be **replayed**: Meta's signature has no
  timestamp. Dedup turns replays within 7 days into duplicates; after that,
  and for keyless events, a replay is delivered again. Not logging bodies is
  what keeps them from being captured. Meta's mutual-TLS option narrows who
  can reach the endpoint at all.
