---
name: meta-whatsapp-rs-webhook-endpoint
description: "The WhatsApp webhook endpoint with meta-whatsapp-rs - WebhookHandler with the app secrets (X-Hub-Signature-256 over the raw body, several secrets while rotating), the verify token GET handshake, the 3 MiB body limit, DedupGuard leases against Meta's 7-day retries, the axum router, doing the same in any other framework, and the exact status code Meta must get for each outcome (200, 401, 413, 503, 500, 403). Load when creating, deploying or changing the HTTP endpoint Meta calls, porting it to another web framework, or debugging webhook deliveries that Meta keeps retrying."
---

# meta-whatsapp-rs-webhook-endpoint

> **Verified against meta-whatsapp-rs d9f4c05393be9b6b7ce688efe1ad309b026fbd37 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/endpoint.rs](examples/endpoint.rs), compiled and
tested by meta-whatsapp-rs's own gate (axum through `tower::ServiceExt::oneshot`,
and the framework-free functions).

## When to use

The one public HTTPS endpoint Meta posts every webhook to, for every
merchant's WABA. What to do with the events it yields is
`wa-rs-webhook-events` and `wa-rs-live-updates`.

```text
POST ─► X-Hub-Signature-256 present and well-formed? (else 401, body never read)
     ─► raw body, at most 3 MiB (else 413) ─► HMAC over the raw bytes (any of N app secrets)
     ─► parse ─► events ─► per event: dedup claim ─► your EventSink ─► dedup done ─► 200
```

## Build the handler once

```rust
let handler = WebhookHandler::builder(
    SignatureVerifier::new(app_secrets)?, // refuses an empty list or a blank secret
    VerifyToken::new(verify_token),
    sink,
)
.dedup(DedupGuard::new(kv)) // Meta retries for 7 days and sends to every subscribed app
.build(); // body limit: 3 MiB (`.max_body_bytes(n)` to change)
```

Read the app secret and verify token from your secret store at startup and
let a blank one stop the process: a blank verify token otherwise answers
403 to every verification, but only when one arrives
([open question 16](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#webhooks)).
List the old app secret next to the new one while rotating, then drop it.

## axum

```rust
// The axum the router is built with, re-exported: no axum pin of your own.
wa_rs::webhooks::axum::Router::new().nest(
    "/webhooks/whatsapp",
    wa_rs::webhooks::router(Arc::new(handler)),
)
```

`router` serves `GET` (verification) and `POST` (deliveries), refuses a
missing or malformed signature header before reading the body, and
replaces axum's 2 MiB default limit with the handler's. Serve it with the
same re-exported axum (in the example, `routes` returns the router above):

```rust
let listener = tokio::net::TcpListener::bind(addr).await?;
wa_rs::webhooks::axum::serve(listener, routes(handler)).await
```

## Any other framework

```rust
let Some(signature) = signature else {
    return 401; // before reading a byte of the body
};
let Some(body) = read_body(handler.max_body_bytes()).await else {
    return 413;
};
status_of(&handler.deliver(Some(signature), &body).await)
```

The header is `wa_rs::webhooks::SIGNATURE_HEADER` (`x-hub-signature-256`,
available without the `axum` feature). `GET`: deserialize the query into
`VerificationQuery` (Meta's `hub.*` names), `handler.verify(&query)` →
echo the challenge as `text/plain` with 200, else 403.

```rust
match result {
    Ok(_) => 200, // delivered, duplicate, or signed but unparseable (`Unparsed`)
    Err(Error::Webhook(
        WebhookError::MissingSignature
        | WebhookError::MalformedSignature
        | WebhookError::SignatureMismatch,
    )) => 401,
    Err(Error::Webhook(WebhookError::PayloadTooLarge { .. })) => 413,
    Err(Error::Webhook(WebhookError::ClaimInFlight)) => 503, // another request is delivering it
    Err(_) => 500, // your sink or the dedup store failed: Meta retries
}
```

Anything but 200 makes Meta redeliver **the whole batch** for up to 7
days, then drop it.

## Dedup is a lease, not a marker

`DedupGuard::new(kv)` claims each event for 60 s (`DEFAULT_CLAIM_LEASE`)
before your sink, marks it done after (kept 7 days + 1 h,
`DEFAULT_DEDUP_TTL`), and releases the claim when the sink fails. A retry
that finds a live claim gets `ClaimInFlight` → 503 and comes back later; a
claim left by a crashed request expires and the retry delivers. So:
**at-least-once, deduplicated**. The `KvStore` must be shared by every
instance; on Postgres, purge expired rows regularly (`wa-rs-storage`).
Keep sink calls well under the lease (`.with_lease(d)` to change it).

## Pitfalls

- **Nothing in front may parse, decompress or re-serialize the body**:
  the signature covers the raw bytes. No body-limit layer smaller than
  3 MiB in front either (Meta sends up to 3 MB).
- A sink error that can never succeed holds back every event after it in
  the same batch until Meta gives up. Make permanent failures impossible
  in the sink path (`wa-rs-live-updates`).
- Meta's signature has no timestamp: a captured body can be replayed.
  Dedup absorbs replays within 7 days; not logging bodies keeps them from
  being captured. Never log the body, the signature header or
  `WebhookEvent`'s `Debug`.
- `ErrorReported` and `Unparsed` events have no dedup key: delivered every
  time.
- The callback URL must be HTTPS with a valid certificate; in
  development, put a tunnel in front.

## What meta-whatsapp-rs does not do

- No dead-letter queue: one permanently failing event fails its batch
  ([open question 30](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#webhooks-and-live-updates)).
- No API to fetch past webhooks: what your sink did not persist is gone
  after Meta's 7 days.
- No mutual TLS setup (Meta offers it per app; configure it in front).

## Related skills

`wa-rs-webhook-events` (the events), `wa-rs-live-updates` (sinks),
`wa-rs-cms-inbox`, `wa-rs-storage` (the dedup store), `wa-rs-testing`
(signed fixtures), `wa-rs-phone-numbers` (subscriptions and overrides).
