---
name: meta-whatsapp-rs-testing
description: "Testing code that uses meta-whatsapp-rs without Meta, a network or a database - ScriptedTransport (scripted Graph answers and failures, asserting method, path, bearer token and exact JSON, remaining() == 0), the memory stores, ManualClock for expiry and the 24-hour window, and signed webhook fixtures delivered through WebhookHandler. Load when writing unit or integration tests for WhatsApp sending, webhooks, OTP, the inbox or onboarding code built on meta-whatsapp-rs, or when a test needs to fake a Meta error or timeout."
---

# meta-whatsapp-rs-testing

> **Verified against meta-whatsapp-rs d9f4c05393be9b6b7ce688efe1ad309b026fbd37 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/integration.rs](examples/integration.rs) — four
tests meta-whatsapp-rs runs in its own gate. Every other skill's `examples/*.rs` ends
with tests in the same style.

## When to use

Any test of your code that sends, receives webhooks, stores tokens or
verifies codes. meta-whatsapp-rs's own tests use exactly these doubles.

## Set up

`ScriptedTransport` is behind meta-whatsapp-rs's `testing` feature. Enable it for
test builds only, on the same meta-whatsapp-rs dependency (no second pin to keep in
step), and `meta_whatsapp_rs::core::testing` exists in your tests:

```toml
[dependencies]
wa-rs = { git = "https://github.com/vaam-apps/wa-rs", rev = "<rev>" }

[dev-dependencies]
wa-rs = { git = "https://github.com/vaam-apps/wa-rs", rev = "<rev>", features = ["testing"] }
serde_json = "1"
time = { version = "0.3", features = ["macros"] } # ManualClock takes an OffsetDateTime
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## A client that talks to a script

```rust
Client::builder()
    .transport(transport.clone())
    .access_token("TEST-TOKEN")
    .retry(RetryPolicy::NONE) // one request per call: counts stay exact
    .build()
    .expect("a transport is set")
```

Queue answers in order with `push_json(status, body)`,
`push_bytes(status, content_type, bytes)` (media downloads) or
`push_error(|| TransportError::Timeout)`; an unscripted request fails.
Then assert what your code sent:

```rust
let request = transport.last_request().unwrap();
assert_eq!(request.method, "POST");
assert_eq!(request.path(), "/v25.0/106540352242922/messages");
assert_eq!(request.bearer(), Some("TEST-TOKEN"));
assert_eq!(
    request.json().unwrap(),
    json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "recipient": CUSTOMER,
        "type": "text",
        "text": {"body": "Your order has shipped."}
    })
);
assert_eq!(transport.remaining(), 0); // every scripted answer was used
```

`requests()` returns every request; `query(key)`, `header(name)`,
`multipart_field(name)` read the rest. A Graph error is a JSON body with
the documented shape, e.g. `{"error": {"code": 131047, "message": …}}`
with status 400: the client classifies it as usual.

## Webhooks: sign the fixture, deliver it

```rust
let body = text_webhook(NUMBER, CUSTOMER, "Does it come in navy?", 1_749_416_383);
let body = serde_json::to_vec(&body).unwrap();
let signature = meta_whatsapp_rs::webhooks::sign(&AppSecret::new(SECRET), &body);

let first = handler.deliver(Some(&signature), &body).await.unwrap();
assert_eq!(first.delivered, 1);
let retry = handler.deliver(Some(&signature), &body).await.unwrap();
assert_eq!(retry.duplicates, 1); // Meta's retry is recorded once
```

`sign` produces the `X-Hub-Signature-256` value Meta would send. Build
bodies from Meta's documented examples (the helper `text_webhook` in the
example file is one; copy the shapes of the webhook reference pages),
including BSUID-only customers without `wa_id`. A sink of
`meta_whatsapp_rs::adapters::sink::channel` lets the test read what was delivered;
`meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(..)?.into_events()` parses a
body without a handler. To drive an axum app, see the tests of
`wa-rs-webhook-endpoint` (`tower::ServiceExt::oneshot`).

## Time

`ManualClock::new(at)` only moves on `advance(d)` or `set(at)`. Pass the
same clock to everything that reads time: `MemoryKvStore::with_clock`,
`OtpService::new`, `Inbox::with_clock`, `InboxSink::with_clock` (it bounds
synced history timestamps), `TokenVault::with_clock`:

```rust
let transport = ScriptedTransport::new(); // nothing scripted: no request may leave
let clock = Arc::new(ManualClock::new(sent_at + Duration::from_hours(25)));
let inbox = Inbox::new(scripted_client(&transport), NUMBER, store).with_clock(clock);
let key = inbox.key(CUSTOMER);
```

## Pitfalls

- **Assert `remaining() == 0`.** Without it a test passes when your code
  skipped a request you scripted.
- Keep `RetryPolicy::NONE` unless the retry is what you test: the default
  policy replays idempotent requests and throttled sends, consuming
  scripted answers.
- A `MemoryKvStore` per test: shared stores leak dedup markers and OTP
  cooldowns between tests.
- Postgres and Redis expire records by their own clock: a `ManualClock`
  does not move them. Test expiry logic on the memory stores; test the
  adapters with their conformance suites (`wa-rs-storage`).

## What meta-whatsapp-rs does not do

- No mock HTTP server and no recorded fixtures of Meta's API: the scripts
  are yours, from Meta's documented examples.
- ~~wa-rs does not forward a `testing` feature: a wa-core dev-dependency
  is needed~~: true until 4eb93c9 (2026-09-24); use the feature above.

## Related skills

`wa-rs-setup` (the client), `wa-rs-webhook-endpoint` (testing the axum
route), `wa-rs-storage` (conformance suites for your own adapters),
`wa-rs-errors` (which errors to script).
