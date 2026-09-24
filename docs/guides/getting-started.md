# Getting started

**Goal:** from an empty Meta account to a first WhatsApp message sent from
Rust, with errors handled the way the library expects.

Example: [`send_message.rs`](../../crates/wa-rs/examples/send_message.rs).
Agent skills: [`wa-rs-setup`](../../skills/wa-rs-setup/SKILL.md),
[`wa-rs-errors`](../../skills/wa-rs-errors/SKILL.md).

## 1. On Meta's side

Do these once. Paths in the App Dashboard move from time to time; the linked
pages are the authority.

| # | Step | You end up with |
| --- | --- | --- |
| 1 | Register as a Meta developer, then in the [App Dashboard](https://developers.facebook.com/apps) create an app with the **Connect with customers through WhatsApp** use case, attached to a business portfolio | an **app id** |
| 2 | In the use case's **API Setup**, connect an existing WhatsApp Business Account (WABA) or create one; Meta gives you a test business number | a **WABA id** and a **phone number id** |
| 3 | Send the sample `hello_world` template from that page to your own WhatsApp, then reply to it from your phone | an open 24-hour customer service window for tests |
| 4 | In Business Settings → **System users**, add a system user, assign it your app (manage app) and your WABA (full control), and generate a token with `whatsapp_business_messaging` and `whatsapp_business_management` (`business_management` only if your code manages the portfolio itself) | a long-lived **system user token** |
| 5 | App Dashboard → App settings → **Basic** | the **app secret** (server-side only) |
| 6 | Pick a random **verify token**, deploy your webhook endpoint ([webhooks.md](webhooks.md)), then in the use case's **Configuration** panel enter the callback URL and verify token and subscribe to at least `messages` | webhooks arriving |
| 7 | Before real traffic: verify your business (it raises messaging limits) and add a payment method in WhatsApp Manager | production readiness |

Meta's pages:
[get-started](https://developers.facebook.com/documentation/business-messaging/whatsapp/get-started),
[access-tokens](https://developers.facebook.com/documentation/business-messaging/whatsapp/access-tokens),
[permissions](https://developers.facebook.com/documentation/business-messaging/whatsapp/permissions),
[webhooks/create-webhook-endpoint](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/create-webhook-endpoint),
[messaging-limits](https://developers.facebook.com/documentation/business-messaging/whatsapp/messaging-limits).

Notes that save a day of debugging:

- The token the API Setup page generates is a *user* token that expires
  within hours. Use it for the first test only.
- An *employee* system user sees nothing until you give it access to each
  WABA; an *admin* sees every WABA of the portfolio. Missing access shows up
  as Graph error `200` (`ErrorKind::Permission`), not HTTP 403.
- The webhook callback must be HTTPS with a valid certificate; self-signed
  ones are not supported. In development, put a tunnel in front.
- A CMS that onboards **other businesses** is a Tech Provider (or a
  Solution Partner paying for them with its credit line) and needs more
  (business verification, App Review for Advanced access, a Facebook Login
  for Business configuration): see [embedded-signup.md](embedded-signup.md).

Keep the token, app secret and verify token in your secret manager, never
in the repository or the database.

## 2. Add the dependency

wa-rs is not on crates.io yet (the name is taken; see
[OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md#naming-and-publishing)). Depend
on it by git revision:

```toml
[dependencies]
wa-rs = { git = "https://github.com/vaam-apps/wa-rs", rev = "<commit>", features = ["axum", "postgres"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
```

- The repository is public: Cargo fetches it without credentials.
- Rust **1.98.1** or newer, edition 2024.
- Defaults are `reqwest` (the HTTP transport), `memory` (in-process stores)
  and `sinks`. Add `postgres`, `redis`, `axum` (webhook router and SSE),
  `typst` (documents) or `flows-endpoint` as needed; `full` enables all. The
  table is in the [README](../../README.md#feature-flags).
- Types from sqlx, axum and redis cross the API (`PgPool`, `axum::Router`,
  a redis connection). Use the versions wa-rs was built with, re-exported:
  `wa_rs::adapters::store::postgres::sqlx` (feature `postgres`),
  `wa_rs::webhooks::axum` (feature `axum`) and
  `wa_rs::adapters::store::redis` (feature `redis`), as the examples do;
  then there is nothing to pin. If you need axum features wa-rs does not
  turn on, add `axum = "0.8"` with them yourself: Cargo builds one axum 0.8
  for both (likewise `redis = "1"` with `tokio-rustls-comp` for
  `rediss://`).

## 3. Send a first message

```rust
use wa_rs::prelude::*;

let client = wa_rs::client(std::env::var("WA_TOKEN")?)?;
let messages = client.messages(std::env::var("WA_PHONE_NUMBER_ID")?);
let to = Recipient::phone("+16505551234"); // E.164, with the `+`

let hello = TemplateMessage::new("hello_world", "en_US");
let sent = messages.send(&OutboundMessage::template(to, hello)).await?;
println!("accepted as {:?}", sent.message_id());
```

Run the full program with
`WA_TOKEN=… WA_PHONE_NUMBER_ID=… WA_TO=+16505551234 cargo run -p wa-rs --example send_message`.

- `wa_rs::client(token)` builds a client on the production transport
  (rustls, HTTP/2, `HTTPS_PROXY` honoured), Graph API v25.0, a 30 s timeout
  and the default retry policy. Build **one** at startup and clone it:
  cloning is cheap and shares the connection pool.
- `messages(…)` takes the **phone number id**, not the phone number. Ids are
  distinct types (`PhoneNumberId`, `WabaId`, …) built from `&str`/`String`,
  so a WABA id cannot be passed where a phone number id is expected.
- Always write the `+`. Meta reads a number without it as local to *your
  business number's* country, so the message reaches someone else.
  A `&str` is not a `Recipient`: build one with `Recipient::phone`,
  `Recipient::user` (a business-scoped user id, BSUID) or `Recipient::group`.
- A successful `send` means Meta **accepted** the message. Delivery, reads
  and failures arrive later as status webhooks keyed by the returned id.
- Free-form messages (text, media, interactive) only reach a customer who
  wrote to you in the last 24 hours; templates reach anyone who opted in.

## 4. Handle errors

Every fallible call returns `wa_rs::Result<T>`. Branch on `err.kind()`, an
`ErrorKind` classified from Meta's error code, never on the message text or
the HTTP status. `ErrorKind` is non-exhaustive: keep a `_` arm.

| Variant | Means | Typically |
| --- | --- | --- |
| `Error::Validation(v)` | refused locally; **nothing was sent**; `v.field` names the JSON path | fix the input |
| `Error::Api(_)` | Meta answered with a Graph error; `err.graph()` gives it | branch on `kind()` |
| `Error::Transport(_)`, `Error::Http { .. }` 5xx | no usable answer | a send *may* have gone out |
| `Error::Decode { .. }` | a 2xx body of an unexpected shape (for sends, without the body: it names the recipient) | Meta accepted it: treat a send as sent |
| `Error::Step { step, .. }` | a multi-step flow (onboarding) stopped at `step` | see [embedded-signup.md](embedded-signup.md) |

```rust
use wa_rs::client::messages::Messages;
use wa_rs::prelude::*;

/// What the order service does next.
enum Next {
    Accepted(MessageId), // wait for status webhooks
    FixInput(String),    // refused locally, nothing was sent
    FixTemplate,         // not approved in that language, or wrong parameters
    Rejected(Error),     // Meta refused it: nothing went out
    Reconcile,           // may have been sent: check status webhooks first
}

async fn notify_shipped(messages: &Messages, to: Recipient, order_no: &str) -> Next {
    let template = TemplateMessage::new("order_shipped", "en_US").body([Parameter::text(order_no)]);
    let msg = OutboundMessage::template(to, template)
        .callback_data(format!("order:{order_no}")); // echoed on status webhooks
    match messages.send(&msg).await {
        Ok(sent) => sent.message_id().cloned().map_or(Next::Reconcile, Next::Accepted),
        Err(Error::Validation(v)) => Next::FixInput(v.field),
        Err(e) => match e.kind() {
            ErrorKind::TemplateNotFound | ErrorKind::TemplateParameterMismatch => Next::FixTemplate,
            _ if !e.may_have_been_sent() => Next::Rejected(e),
            _ => Next::Reconcile,
        },
    }
}
```

`Error::may_have_been_sent()` is `false` when Meta provably did nothing (a
Graph error on a 4xx response, a throttling error on any status, a local
validation error, a connection that never opened) and `true` for a timeout,
any other 5xx or non-4xx status (with or without a Graph error) or an
unreadable response.

### Retries: "could succeed later" is not "safe to repeat"

- The client already retries **idempotent** requests (reads, deletes, POSTs
  that set a value) with jittered backoff: 3 retries, 250 ms base, 8 s cap
  (`RetryPolicy::default()`; `RetryPolicy::NONE` turns it off).
- A **send** is replayed only when Meta proves it did nothing (throttling:
  `RateLimited`, `PairRateLimited`, HTTP 429). A timeout or a 5xx on a send
  is returned, never replayed: a duplicate order confirmation or OTP is worse
  than an error. Do not wrap sends in your own blind retry loop either.
- `err.is_retryable()` answers "could the same request succeed later", not
  "is it safe to send again". It is deliberately `false` for 131049
  (per-user marketing limit) and 131048 (spam rate limit).
- To retry a send from a job queue, resend only when
  `!err.may_have_been_sent()` (and the cause is fixed or transient); for
  anything else first look for a status webhook carrying your
  `callback_data`.

The full `ErrorKind` table, with codes and advice, is in the skill's
[error-kinds reference](../../skills/wa-rs-errors/references/error-kinds.md).

## 5. One client, many merchants

A multi-tenant service builds a client without a token and derives one per
merchant:

```rust
let client = wa_rs::client_builder()?.build()?; // no default token
let merchant = client.with_token(merchant_token); // an AccessToken, e.g. from the TokenVault
merchant.messages(phone_number_id).send(&msg).await?;
```

`with_token` shares the transport, endpoint and retry policy; only the
token differs. Never build a client per request.

## 6. An endpoint wa-rs does not wrap

Use the client's request builders, so authentication, retries, error
decoding and the credential host allowlist still apply:

```rust
let info: serde_json::Value = client
    .get_at(&[waba_id.as_str()]) // one path segment per element, percent-encoded
    .query("fields", "id,name")
    .send()
    .await?;
```

Never `client.get(&format!("{id}/…"))`: the literal-path builders split on
`/`, so an id read from a database or a webhook could address another Graph
object with your token.

The token is only ever attached to the configured Graph endpoint (scheme,
host and port) and to `https://lookaside.fbsbx.com`, where Meta's media
download URLs point (default port). `client.request_url(method, url)` to
any other origin, `*.whatsapp.net` included, is refused with
`Error::Validation` on `url` before anything is sent.

## What wa-rs does not do

- No tenant model: it knows WABAs and phone number ids, not your merchants.
- No job queue, outbox or retry scheduler for sends.
- No opt-in or opt-out registry: recording consent is yours
  ([marketing-and-commerce.md](marketing-and-commerce.md)).
- Payments, Solution Partner APIs other than credit lines, conversation
  routing: [coverage.md](../coverage.md) rows 28–32.
