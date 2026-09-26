---
name: meta-whatsapp-rs-live-updates
description: "Where WhatsApp webhook events go after meta-whatsapp-rs verifies them - writing your own EventSink, FanoutSink to several sinks, BroadcastSink plus the SSE stream for a live UI (allow-list filter, lagged events, per-subscriber cost), ChannelSink to a background worker (Wait vs TryOrFail), FilterSink, FnSink, TracingSink, idempotency and the at-least-once guarantee, and what is lost on a crash. Load when wiring what happens to webhook events, streaming live chat updates to a browser, or moving slow work off the webhook request."
---

# meta-whatsapp-rs-live-updates

> **Verified against meta-whatsapp-rs b7211bc1f282f873b605e7a3a1126ce4e45e5677 (2026-09-26).** On another revision, trust the code over this page.

Reference code: [examples/sinks.rs](examples/sinks.rs), compiled and
tested by meta-whatsapp-rs's own gate. Sinks are in `meta_whatsapp_rs::adapters::sink` (feature
`sinks`, on by default); `sse` is in `meta_whatsapp_rs::webhooks` (feature `axum`).

## When to use

Deciding what the webhook endpoint does with each event: persist it, show
it live, queue slow work. The handler answers Meta only after your sink
returns, so the sink decides both what is kept and what Meta retries.

## Your durable sink

```rust
#[async_trait]
impl EventSink<WebhookEvent> for Deliveries {
    async fn deliver(&self, event: WebhookEvent) -> Result<(), SinkError> {
        if let WebhookEvent::StatusUpdated { status, .. } = &event
            && let Some(tag) = status.biz_opaque_callback_data.as_deref()
        {
            // An upsert keyed by `tag` and `status.id`: a redelivery is harmless.
            tracing::debug!(tag, status = status.status.as_str(), "delivery update");
        }
        Ok(())
    }
}
```

An `Err` (wrap your error in `SinkError::Delivery(anyhow::Error)`) makes
the endpoint answer 500 and Meta redeliver the whole batch.

## Compose

```rust
let (worker, jobs) = channel::<WebhookEvent>(1024);
let sink = FanoutSink::new()
    .with(Deliveries) // durable: its error → 500 → Meta retries
    .with(BroadcastSink::from_sender(live)) // live views: never fails, drops when nobody listens
    .with(worker.with_mode(ChannelMode::TryOrFail)); // full queue → 500 instead of waiting
(sink, jobs)
```

| Sink | Behaviour | Watch out |
| --- | --- | --- |
| `FanoutSink` | every sink, concurrently; returns the first error | the sinks that succeeded see the event again on redelivery |
| `BroadcastSink` | live fan-out; never fails | nobody listening = dropped; slow subscribers lag |
| `ChannelSink` (`channel(cap)`) | hands events to a worker | `Ok` once **queued**: a crash loses the queue; `ChannelMode::Wait` (default) blocks the webhook when full |
| `FilterSink::new(inner, pred)` | filtered-out events count as handled | — |
| `FnSink::new(closure)` | an async closure | — |
| `TracingSink::new()` | logs the event kind | `.with_payload(true)` logs customer data |

The CMS inbox's `InboxSink` goes in the fan-out too (`meta-whatsapp-rs-cms-inbox`).

## Live view over SSE

```rust
// An allow-list: `Unknown` and `Unparsed` events carry no number and may
// belong to any merchant, so `is_none_or(…)` would leak them.
let only_this_number = move |e: &WebhookEvent| e.phone_number_id() == Some(&owned_number);
meta_whatsapp_rs::webhooks::sse(live.subscribe(), only_this_number) // `event: whatsapp`; `event: lagged` → reload
```

`sse` takes a tokio `broadcast::Receiver<WebhookEvent>`: `tx.subscribe()`
or `BroadcastSink::receiver()`. Each event goes out as `event: whatsapp`
with the event JSON; when the receiver falls behind, `event: lagged` with
the count arrives — reload history from your store. Mount it **behind
your own auth, after checking the caller owns the number**. A browser's
`EventSource` sends no `Authorization` header: use your cookie session.

## The worker

```rust
while let Some(event) = jobs.recv().await {
    tracing::info!(kind = event.kind(), "background job"); // kind only: no customer data in logs
}
```

## Pitfalls

- **At least once.** Dedup removes Meta's retries, but a sink call that
  outlasts the 60 s lease, or a batch redelivered after another sink
  failed, delivers again: make every sink idempotent.
- **Persist inside the sink path** what you cannot lose; a
  `ChannelSink` queue lives in memory and dies with the process.
- A sink error that can never succeed blocks its whole batch until Meta
  drops it after 7 days: turn permanent failures into logged successes.
- **SSE cost**: a broadcast receiver clones every event before the filter
  sees it, so each open stream copies every merchant's events (including
  multi-megabyte `HistorySynced` bodies). Fine for a handful of open
  inboxes; see OPEN_QUESTIONS.md #31 before hundreds (decided on
  2026-09-26: shared events instead of clones, roadmap L21b).
- The broadcast channel is per process: with several instances, relay
  events yourself (Postgres `LISTEN/NOTIFY`, Redis pub/sub) or pin a
  merchant's traffic to one instance.

## What meta-whatsapp-rs does not do

- No dead-letter store yet for events a sink cannot take
  ([OPEN_QUESTIONS.md #30](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#webhooks-and-live-updates),
  decided on 2026-09-26, roadmap L21a), no cross-instance relay, no
  per-number channels (#31).
- No durable queue: use your own outbox table inside a sink.

## Related skills

`meta-whatsapp-rs-webhook-endpoint`, `meta-whatsapp-rs-webhook-events`, `meta-whatsapp-rs-cms-inbox`,
`meta-whatsapp-rs-production` (several instances), `meta-whatsapp-rs-testing`.
