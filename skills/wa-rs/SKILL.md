---
name: wa-rs
description: "Map of wa-rs, the Rust toolkit for Meta's WhatsApp Business Platform: which crate and module does which job, Cargo features, building a Client (ReqwestTransport, per-merchant tokens with with_token), the error model (Error, ErrorKind, is_retryable, why sends are never replayed), BSUID-aware Recipient, and the segment-safe path rule for calling unwrapped endpoints. Load first for any code that depends on wa-rs; it routes to the other wa-rs-* skills."
---

# wa-rs

> **Verified against wa-rs 91431ae (2026-09-24).** On another revision, trust
> the code over this page (see `skills/README.md`).

wa-rs is a Cargo workspace. Depend on the facade crate **`wa-rs`** (lib name
`wa_rs`); it re-exports everything else.

```toml
[dependencies]
wa-rs = { git = "https://github.com/vaam-apps/wa-rs", rev = "91431ae033768fc8d849fa198ce734d785fb9c4a", features = ["postgres", "axum"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- **Not on crates.io** (`publish = false`; the name `wa-rs` is taken there by
  an unrelated crate). Pin a git `rev`. The repository is private: give Cargo
  credentials (e.g. `[net] git-fetch-with-cli = true` in `.cargo/config.toml`).
- **Toolchain ≥ 1.98.1, edition 2024** (`rust-version = "1.98.1"`).
- You bring the async runtime (tokio). Graph API version defaults to
  `ApiVersion::DEFAULT` = v25.0.

## Which crate / module for which job

| Job | Path | Skill |
| --- | --- | --- |
| Client, requests, retries | `wa_rs::Client`, `wa_rs::client::{ClientBuilder, GraphRequest, RetryPolicy}` | this one |
| Errors | `wa_rs::{Error, ErrorKind, GraphApiError, Result}`, leaves in `wa_rs::core::error` | this one |
| Ids, secrets, recipients | `wa_rs::core::{ids, secret, recipient}` | this one |
| Send messages | `wa_rs::client::messages` (`client.messages(pnid)`) | `wa-rs-messaging` |
| Media upload/download | `wa_rs::client::media` (`client.media(pnid)`) | `wa-rs-messaging` |
| Templates (create/list/send) | `wa_rs::client::templates` (`client.templates(waba)`) | `wa-rs-templates-otp` |
| OTP login | `wa_rs::client::authentication` (`OtpService`) | `wa-rs-templates-otp` |
| Marketing Messages API | `wa_rs::client::marketing` (`client.marketing(pnid)`) | `wa-rs-messaging` |
| In-App Signup opt-in links | `wa_rs::client::signups` | `wa-rs-messaging` |
| Merchant onboarding | `wa_rs::client::embedded_signup` | `wa-rs-embedded-signup` |
| WABA / phone numbers | `wa_rs::client::{waba, phone_numbers}` | `wa-rs-embedded-signup` |
| Webhooks | `wa_rs::webhooks` | `wa-rs-webhooks` |
| CMS inbox | `wa_rs::inbox::{Inbox, InboxSink}` | `wa-rs-cms-inbox` |
| Stores, sinks, HTTP transport | `wa_rs::adapters::{store, sink, http}` | `wa-rs-webhooks`, `wa-rs-cms-inbox` |
| PDF/PNG documents | `wa_rs::typst` (feature `typst`) | `wa-rs-documents` |

Also wrapped (read their rustdoc): `business_profile`, `commerce`, `flows`,
`groups`, `calling`, `analytics`, `qr_codes`, `block_users`. What is and is
not implemented: `docs/coverage.md` in the wa-rs repo. The design spec:
`docs/architecture.md`.

## Cargo features of `wa-rs`

| Feature | Default | Adds |
| --- | --- | --- |
| `reqwest` | yes | `wa_rs::adapters::http::ReqwestTransport` |
| `memory` | yes | `MemoryKvStore`, `MemoryConversationStore` (tests, dev, one instance) |
| `sinks` | yes | `wa_rs::adapters::sink::*` (channel, broadcast, fan-out, filter, fn, tracing) |
| `postgres` | no | `PostgresKvStore`, `PostgresConversationStore`, `store::postgres::migrate` (sqlx **0.9**) |
| `redis` | no | `RedisKvStore` (redis **1.x**; no TLS built in) |
| `axum` | no | `wa_rs::webhooks::{router, sse}` (axum **0.8**) |
| `typst` | no | `wa_rs::typst` |
| `flows-endpoint` | no | Flows data-endpoint crypto (builds aws-lc-rs) |
| `full` | no | all of the above |

Types from sqlx, redis and axum cross the API (`PgPool`, `ConnectionManager`,
`axum::Router`), so your own versions of those crates **must match** the
majors above or the types will not unify.

Which store: `KvStore` (token vault, OTP, webhook dedup, signup sessions) on
Postgres or Redis in production — **shared by every instance**; memory only
for tests or a single instance that may lose it all on restart. Postgres and
Redis decide expiry by *their* server clock (keep hosts on NTP). For
`rediss://`, build the connection yourself (enable `redis/tokio-rustls-comp`,
install a rustls crypto provider at startup) and pass it to
`RedisKvStore::new`; see its rustdoc.

## Build a client

```rust
use wa_rs::Client;
use wa_rs::adapters::http::ReqwestTransport;

let client = Client::builder()
    .transport(ReqwestTransport::new()?)
    .access_token(std::env::var("WA_SYSTEM_USER_TOKEN")?) // optional
    .build()?;
```

`Client` is cheap to clone (one `Arc` + an optional token). Build **one** at
startup and derive per-tenant copies:

```rust
let merchant = client.with_token(stored.token); // AccessToken from the TokenVault
merchant.messages(phone_number_id).send(&msg).await?;
```

`with_token` shares the transport, endpoint and retry policy; only the token
differs. Never build a client per request. `ClientBuilder` also takes
`api_version`, `endpoint` (proxy/mock), `retry`, `timeout` (default 30 s,
`DEFAULT_TIMEOUT`), `user_agent`.

Every endpoint family hangs off the client: `messages(pnid)`, `media(pnid)`,
`templates(waba)`, `authentication(waba)`, `embedded_signup(app)`,
`signups(waba)`, `waba(waba)`, `phone_number(pnid)`, `marketing(pnid)`, …
Ids are newtypes (`PhoneNumberId`, `WabaId`, `MessageId`, `UserId`, …) with
`From<&str>`/`From<String>`, so `client.messages("1065…")` works, but a
`WabaId` cannot be passed where a `PhoneNumberId` is expected.

## Errors

Every fallible call returns `wa_rs::Result<T>` = `Result<T, wa_rs::Error>`.

```text
Error::Api(GraphApiError)   Meta's error object → .kind() classifies .code
Error::Http{status, ..}     non-Graph error body (e.g. HTML 502)
Error::Transport(..)        no response: timeout, connect, integrity (media hash)
Error::Decode{..}           2xx body of an unexpected shape
Error::Validation(..)       refused locally, nothing was sent; .field names the JSON path
Error::Webhook/Storage/Sink/Crypto/Config
Error::Step{step, source}   a multi-step flow (onboarding) failed at `step`
Error::Other(anyhow)        integrator code, typst RenderError
```

**Branch on `err.kind()` (an `ErrorKind`), never on message text or HTTP
status.** `err.graph()` gives the `GraphApiError` (also through `Step`), whose
`.code` distinguishes codes that share a kind. Full table:
[references/error-kinds.md](references/error-kinds.md).

```rust
use wa_rs::{Error, ErrorKind};

match client.messages(pnid).send(&msg).await {
    Ok(sent) => record(sent.message_id()),
    Err(e) => match e.kind() {
        ErrorKind::CustomerServiceWindowClosed => send_a_template_instead(),
        ErrorKind::MarketingOptedOut => mark_opted_out(),         // 131050: never retry
        ErrorKind::EcosystemEngagementLimit => wait_at_least_24h(), // 131049: never auto-retry
        ErrorKind::RecipientNotSupported => resend_by_phone_number(), // 131062: BSUID refused
        _ if matches!(e, Error::Validation(_)) => fix_input(),   // nothing was sent
        _ => return Err(e),
    },
}
```

### Retries: "could succeed later" is not "safe to repeat"

- `err.is_retryable()` answers *could the same request succeed later*.
- The client retries **idempotent** requests (GET, DELETE, and POSTs marked
  idempotent) on any retryable error, with jittered backoff
  (`RetryPolicy::default()`: 3 retries, 250 ms base, 8 s cap; `RetryPolicy::NONE`
  disables).
- A **send** (`POST …/messages`, marketing sends, uploads) is replayed only
  when the error proves Meta did nothing: throttling (`RateLimited`,
  `PairRateLimited`, HTTP 429). **A timeout or 5xx on a send is returned, never
  replayed** — the message may already be on its way, and a duplicate OTP or
  order confirmation is worse than an error.
- So do not wrap sends in your own blind retry loop either. If you must retry
  from a job queue, follow the library's own rule (used by `OtpService`): a
  Graph error on a **4xx** response (`GraphApiError::http_status`) is a
  rejection; a **5xx**, a timeout or an unreadable 2xx means "may have been
  sent" — reconcile with status webhooks (`OutboundMessage::callback_data`
  comes back as `biz_opaque_callback_data`) before sending again.
- Never auto-retry `EcosystemEngagementLimit` (131049) or `SpamRateLimited`
  (131048): `is_retryable()` is `false` for them on purpose.
- `Registration` includes 133016 (too many (de)registrations: 72 h lock); it
  is deliberately not a rate limit and is never retried.

## Recipients (BSUID rules, mandatory in 2026)

Every messages webhook carries a business-scoped user id (`user_id`, e.g.
`US.13491208655302741918`); the phone number (`wa_id`) **may be absent**.
Never key a customer by phone number alone.

```rust
use wa_rs::core::recipient::Recipient;

Recipient::phone("+16505551234");             // `to`
Recipient::user("US.13491208655302741918");   // `recipient` (BSUID or parent BSUID)
Recipient::PhoneAndUser { phone: "+16505551234".into(), user: "US.1349…".into() }; // Meta uses the phone
Recipient::group("Y2FwaV9ncm91cDox");         // Groups API
```

- `&str` is **not** `Into<Recipient>`: always build one of the above.
- **Always include the `+` and country code.** Meta prepends *your business
  number's* country code to a number without `+` (`messages/send-messages`),
  so `"16505551234"` sent from an Indian number goes to `+9116505551234`. A
  `wa_id` from a webhook is digits only: prepend `+` before sending to it.
- Some sends refuse a BSUID (`ErrorKind::RecipientNotSupported`, 131062):
  authentication templates (OTP), marketing sends with a bid multiplier or max
  price. Use the phone number there.

## Calling an endpoint wa-rs does not wrap

Use the client's request builders so auth, retries, error decoding and the
credential host allowlist still apply:

```rust
// Any path containing an id: one verbatim segment per element.
let info: serde_json::Value = client
    .get_at(&[waba_id.as_str(), "some_edge"])
    .query("fields", "id,name")
    .send()
    .await?;
```

**Never `client.get(&format!("{id}/…"))`.** `get`/`post`/`delete`/`request`
split their path on `/`, so an id such as `123/subscribed_apps` (from a
database, a webhook, a form) would address a different Graph object *with
your token*. `get_at`/`post_at`/`delete_at`/`request_at` percent-encode `/`,
`?`, `#` inside each segment and refuse empty, `.` and `..` segments. The
literal-path builders are for literal paths only (`client.get("debug_token")`).
Mark a POST `.idempotent(true)` only when replaying it cannot duplicate an
effect. The token is only ever attached to the Graph endpoint or Meta's media
CDN over HTTPS; other URLs fail with a validation error before sending.

## Secrets

`AccessToken`, `AppSecret`, `VerifyToken`, `SecretBytes`, `SignupCode`,
`TwoStepPin`, `OtpPepper` print `[REDACTED]` in `Debug` and expose their value
only through `expose_secret()`. Keep it that way: never log
`expose_secret()`, never put a token in a URL you log, never render an OTP
into media.

## What wa-rs does not do

- No tenant model: it knows WABAs and phone number ids, not your merchants.
  Keep your own tenant → WABA / phone number mapping and check it on every
  request.
- No job queue, no outbox, no retry scheduler for sends.
- No payments, credit lines, Solution Partner APIs, conversation routing
  (`docs/coverage.md` rows 28–32).

Next: onboarding merchants → `wa-rs-embedded-signup`; receiving →
`wa-rs-webhooks`; inbox → `wa-rs-cms-inbox`; sending → `wa-rs-messaging`;
templates and OTP → `wa-rs-templates-otp`; PDFs → `wa-rs-documents`.
