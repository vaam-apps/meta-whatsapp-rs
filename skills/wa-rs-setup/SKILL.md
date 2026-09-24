---
name: wa-rs-setup
description: "Setting up wa-rs for WhatsApp - what to create on Meta's side first (app, WABA, phone number id, system user token, app secret, verify token), building the Client with wa_rs::client, wa_rs::client_builder or Client::builder (transport, timeout, retry policy, API version pinning, proxy endpoint), per-merchant clients with with_token, and calling a Graph endpoint wa-rs does not wrap without leaking the token. Load when creating the WhatsApp client, wiring tokens and configuration, upgrading the Graph API version, or calling an unwrapped endpoint."
---

# wa-rs-setup

> **Verified against wa-rs 3a3db05aa425c1737d8bb9239206036dbc81969f (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/client.rs](examples/client.rs), compiled and
tested by wa-rs's own gate.

## When to use

Before the first message: the Meta-side checklist, then one `Client` per
process. Also when tuning timeouts and retries, pinning the Graph API
version, or reaching an edge wa-rs does not wrap.

## On Meta's side, once

| # | Step | You end up with |
| --- | --- | --- |
| 1 | App Dashboard: an app with the **Connect with customers through WhatsApp** use case, in a business portfolio | app id |
| 2 | The use case's API Setup: connect or create a WhatsApp Business Account | WABA id, phone number **id** |
| 3 | Business Settings → System users: a system user with your app and WABA assigned; a token with `whatsapp_business_messaging` and `whatsapp_business_management` | a long-lived system user token |
| 4 | App settings → Basic | the app secret (server only) |
| 5 | A random verify token; your webhook endpoint deployed; callback URL and fields set in the use case's Configuration | webhooks arriving (`wa-rs-webhook-endpoint`) |
| 6 | Business verification, a payment method in WhatsApp Manager | production limits |

The token the API Setup page shows is a user token that expires within
hours: tests only. A merchant's own number goes through Embedded Signup
instead (`wa-rs-embedded-signup`). Meta's pages are the authority:
[get-started](https://developers.facebook.com/documentation/business-messaging/whatsapp/get-started),
[access-tokens](https://developers.facebook.com/documentation/business-messaging/whatsapp/access-tokens).

## Build the client

```rust
// One business, one system user token (feature `reqwest`, on by default):
let client = wa_rs::client(std::env::var("WA_SYSTEM_USER_TOKEN")?)?;

// Multi-tenant: no default token; every call runs as a merchant (below).
let platform = wa_rs::client_builder()?.build()?;
```

Both are `Client::builder()` with the production transport
(`ReqwestTransport`: rustls, HTTP/2, `HTTPS_PROXY`), Graph API v25.0
(`ApiVersion::DEFAULT`), a 30 s timeout (`DEFAULT_TIMEOUT`) and
`RetryPolicy::default()` (3 retries, 250 ms base, 8 s cap). Everything can
be set explicitly, once, at startup:

```rust
let transport = ReqwestTransport::builder()
    .connect_timeout(Duration::from_secs(5))
    .build()?;
Client::builder()
    .transport(transport) // required: `ReqwestTransport`, or your own `HttpTransport`
    .access_token(token) // optional: the default token
    .api_version(ApiVersion::new(25, 0)) // pinned: moves only when you change it
    .timeout(Duration::from_secs(15)) // per request; default 30 s
    .retry(RetryPolicy {
        max_retries: 2,
        base_delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(4),
    })
    .build()
```

A Graph proxy or mock server: pass
`GraphEndpoint::custom(base_url, ApiVersion::DEFAULT)?` to the builder's
`.endpoint(..)`; the token then goes to the proxy, never to
`graph.facebook.com`.

## Act as a merchant

```rust
let merchant = platform.with_token(merchant_token);
let text = OutboundMessage::text(to, "Your order has shipped.");
merchant.messages(phone_number_id).send(&text).await
```

`Client` is one `Arc` plus an optional token: clone it freely.
`with_token` shares the transport, connection pool and retry policy; only
the token differs. Each `wa_rs::client()` call builds a new pool, so never
build a client per request. Merchant tokens come from the vault
(`wa-rs-token-vault`).

Endpoint families hang off the client, scoped by id: `messages(pnid)`,
`media(pnid)`, `templates(waba)`, `authentication(waba)`,
`embedded_signup(app)`, `waba(waba)`, `phone_number(pnid)`,
`marketing(pnid)`, `signups(waba)`, `commerce(pnid)`, `flows(waba)`, …
Ids are distinct newtypes (`PhoneNumberId`, `WabaId`, `MessageId`, …)
built from `&str` or `String`: a WABA id cannot go where a phone number id
is expected. `messages()` takes the phone number **id**, not the number.

## An endpoint wa-rs does not wrap

```rust
client
    .get_at(&[waba_id.as_str()]) // one verbatim, percent-encoded segment per element
    .query("fields", "id,name,timezone_id")
    .send()
    .await
```

`get_at`/`post_at`/`delete_at`/`request_at` keep auth, retries, error
decoding and the credential host allowlist. Mark a POST
`.idempotent(true)` only when replaying it cannot duplicate an effect.

## Pitfalls

- **Never `client.get(&format!("{id}/…"))`.** The literal-path builders
  split on `/`: an id from a database or a webhook such as
  `123/subscribed_apps` would address another Graph object with your
  token. `get_at` keeps it one segment (the test proves it).
- The token is attached only to the configured Graph endpoint (scheme,
  host and port) and `https://lookaside.fbsbx.com` (media downloads); any
  other URL given to `client.request_url(method, url)` fails with
  `Error::Validation` on `url` before a byte is sent.
- An employee system user sees nothing until the WABA is assigned to it:
  Graph error `200`, `ErrorKind::Permission`, not an HTTP 403.
- `ClientBuilder::build` fails without a transport (`Error::Config`);
  `wa_rs::client_builder` fails only when TLS cannot initialise.

## What wa-rs does not do

- No token acquisition or refresh: system user tokens come from Business
  Settings, merchants' tokens from Embedded Signup, and an expired one
  (`ErrorKind::Authentication`, code 190) means a new one
  ([open question 8](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants)).
- Crate names are not settled; `wa_rs::client` is both a module and a
  function ([open questions 1–2](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#naming-and-publishing)).

## Related skills

`wa-rs` (install, features), `wa-rs-errors`, `wa-rs-testing`
(`ScriptedTransport` instead of the network), `wa-rs-token-vault`,
`wa-rs-production` (timeouts, retries and versions in production).
