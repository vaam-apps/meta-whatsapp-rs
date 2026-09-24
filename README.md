# wa-rs

WhatsApp Business Platform for Rust — a typed client for Meta's Cloud API
and Business Management API, webhooks, pluggable storage/transport/sink
adapters, and Typst-rendered documents.

Built for three jobs:

- **Marketing** for an e-commerce store: templates, the Marketing Messages
  API, In-App Signup opt-ins, catalogs and product messages.
- **In-app chat** in a CMS: each merchant onboards their own number with
  **Embedded Signup**; customer messages arrive by webhook, are stored per
  conversation and streamed live to the merchant's inbox.
- **Authentication**: WhatsApp OTP through authentication templates.

What is implemented, and what is not, is in
[docs/coverage.md](docs/coverage.md); the design is
[docs/architecture.md](docs/architecture.md). The API reference is the
rustdoc: `cargo doc -p wa-rs --all-features --open`.

## Quick start

`wa-rs` is not on crates.io (see [Naming](#naming)); depend on it from git,
pinned to a commit:

```toml
[dependencies]
wa-rs = { git = "https://github.com/vaam-apps/wa-rs", rev = "<commit>", features = ["axum", "postgres"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

axum and sqlx types are part of the API (`Router`, `PgPool`), so use the
versions wa-rs was built with, re-exported: `wa_rs::webhooks::axum`
(feature `axum`) and `wa_rs::adapters::store::postgres::sqlx` (feature
`postgres`), as the examples do. If you need axum features wa-rs does not
turn on, add `axum = "0.8"` with them yourself: Cargo builds one axum 0.8
for both, so the types still match.

`use wa_rs::prelude::*;` brings in the client, ids, `Recipient`, the
message and template builders, the webhook pieces, the storage and sink
ports, and the inbox. The snippets below are excerpts of the runnable
programs in [`crates/wa-rs/examples/`](crates/wa-rs/examples) (a test
keeps them in sync); there, `env("X")` is `std::env::var("X")` with the
variable's name in the error. Each example's header lists the variables it
reads and the command that runs it.

### E-commerce: send messages

From [`send_message.rs`](crates/wa-rs/examples/send_message.rs). A
free-form message only reaches a customer who wrote to you in the last 24
hours; a template reaches anyone who opted in. Errors are branched on
`ErrorKind`, never on their text.

```rust
let client = wa_rs::client(env("WA_TOKEN")?)?;
let messages = client.messages(env("WA_PHONE_NUMBER_ID")?);
let to = Recipient::phone(env("WA_TO")?); // E.164, with `+`

let text = OutboundMessage::text(to.clone(), "Your order has shipped.");
match messages.send(&text).await {
    Ok(sent) => println!("text accepted as {:?}", sent.message_id()),
    Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => println!("window closed"),
    Err(e) => return Err(e.into()),
}

let hello = TemplateMessage::new(
    env_or("WA_TEMPLATE", "hello_world"),
    env_or("WA_TEMPLATE_LANGUAGE", "en_US"),
);
let sent = messages.send(&OutboundMessage::template(to, hello)).await?;
```

[`invoice_document.rs`](crates/wa-rs/examples/invoice_document.rs) renders
an invoice PDF with Typst, uploads it and sends it as a document.

### CMS: merchants connect their number (Embedded Signup)

From [`embedded_signup.rs`](crates/wa-rs/examples/embedded_signup.rs), a
server with the Facebook JavaScript SDK page. When a merchant starts, bind
the attempt to them and give the page the `FB.login` options:

```rust
let state = signup.sessions.start(&tenant, ATTEMPT_TTL).await?;
let launch_options = LaunchOptions::new(signup.config_id.as_str()).to_json()?;
```

When the page posts back the code, the `WA_EMBEDDED_SIGNUP` event and the
number's two-step verification PIN (the merchant's own, typed into the page),
redeem the state for the merchant your own authentication says is calling —
never a tenant named by the page or the URL — then onboard: exchange the
code, check the WABA and number with Meta, store the token encrypted,
subscribe the app, register the number.

```rust
// Exactly once, and only for the merchant who started the attempt.
if !signup.sessions.redeem(&state, &tenant).await? {
    return Err(ApiError::StaleAttempt);
}

let onboarded = signup.onboarding.onboard(&request, &signup.vault).await;
```

### CMS: the inbox

From [`cms_inbox.rs`](crates/wa-rs/examples/cms_inbox.rs). Webhook events
are verified, deduplicated, recorded per conversation and published for
the live view:

```rust
let (live, _) = broadcast::channel(256);
let sink = FanoutSink::new()
    .with(InboxSink::new(conversations.clone()))
    .with(BroadcastSink::from_sender(live.clone()));
let handler = WebhookHandler::builder(
    SignatureVerifier::new(vec![app_secret])?, // X-Hub-Signature-256
    verify_token,
    Arc::new(sink),
)
.dedup(DedupGuard::new(kv)) // Meta retries for 7 days: record each event once
.build();
let webhook = wa_rs::webhooks::router(Arc::new(handler)); // GET verify, POST deliver
```

Every `/inbox` route first asks who is calling (a bearer token in the
example, your session in your CMS) and whether that merchant owns the
number; only then is the merchant's token taken from the vault. Replies go
out as that merchant, and only inside the 24-hour window:

```rust
// Your own table says which numbers this tenant owns; ask it before
// touching the vault, whose tokens belong to every merchant.
if !state.tenants.owns(tenant, &number) {
    return Err(ApiError::Forbidden);
}
// The business token Embedded Signup stored for this number's WABA.
let Some(merchant) = state.vault.get_by_phone_number(&number).await? else {
    return Err(ApiError::NotConnected);
};

let key = inbox.key(body.contact);
// Refused locally, before any request, outside the 24-hour window.
let sent = inbox.reply(&key, Text::new(body.text).into()).await?;
```

Both servers listen on `127.0.0.1` unless `WA_BIND` says otherwise, and
refuse to start without `WA_TENANTS`, the stand-in for your authentication
(bearer token → tenant, and tenant → phone numbers for the inbox). Replace
it with your own sessions and tenant table; keep the checks where they are.

### OTP login

From [`otp_login.rs`](crates/wa-rs/examples/otp_login.rs). Codes are stored
as keyed hashes and bound to the sending number and `OtpConfig::namespace`
(set it to the tenant id when one number sends codes for several tenants:
with the default, they share codes), issuing is rate-limited per number,
and verify attempts are counted atomically:

```rust
let otp = OtpService::new(
    wa_rs::client(env("WA_TOKEN")?)?,
    env("WA_PHONE_NUMBER_ID")?,
    OtpTemplate::new(env("WA_OTP_TEMPLATE")?, env_or("WA_OTP_LANGUAGE", "en_US")),
    Arc::new(MemoryKvStore::new()), // Postgres or Redis with several instances
    Arc::new(SystemClock),
    OtpPepper::new(env("WA_OTP_PEPPER")?)?, // >= 32 bytes, not stored with the codes
    OtpConfig::default(),
)?;
let user = Recipient::phone(env("WA_TO")?); // strict E.164, with `+`

let issued = otp.issue(&user, PURPOSE).await; // Sent, CoolingDown or RateLimited

let verified = otp.verify(&user, PURPOSE, code.trim()).await?; // counts as an attempt
```

### Run the examples

| Example | What | Command |
| --- | --- | --- |
| `send_message` | a text, then a template | `cargo run -p wa-rs --example send_message` |
| `invoice_document` | Typst invoice → upload → document message | `cargo run -p wa-rs --example invoice_document --features typst` |
| `embedded_signup` | onboarding server and launch page | `WA_TENANTS=… WA_APP_ID=… WA_APP_SECRET=… WA_ES_CONFIG_ID=… cargo run -p wa-rs --example embedded_signup --features axum` |
| `cms_inbox` | webhook endpoint, inbox, SSE, replies | `WA_TENANTS=… WA_APP_SECRET=… WA_VERIFY_TOKEN=… cargo run -p wa-rs --example cms_inbox --features axum` |
| `otp_login` | issue and verify a code | `cargo run -p wa-rs --example otp_login` |

`embedded_signup` and `cms_inbox` refuse to start without `WA_TENANTS`
(their headers show a one-line setup) and listen on `127.0.0.1` unless
`WA_BIND` says otherwise. Add `postgres` to the features and set `DATABASE_URL` to
run them on Postgres; with the same `DATABASE_URL`, `WA_VAULT_KEY` and
`WA_TENANTS`, the merchant you connect in the first is the one you chat as
in the second (list the connected number under that tenant's
`phone_number_ids`).

## Feature flags

| Feature | Default | Adds |
| --- | --- | --- |
| `reqwest` | yes | `adapters::http::ReqwestTransport` (rustls, HTTP/2) and the `wa_rs::client()` / `wa_rs::client_builder()` shortcuts |
| `memory` | yes | `MemoryKvStore`, `MemoryConversationStore`: tests, development, one instance |
| `sinks` | yes | channel, broadcast, fan-out, filter, fn and tracing sinks |
| `postgres` | | `PostgresKvStore`, `PostgresConversationStore`, embedded migrations (sqlx) |
| `redis` | | `RedisKvStore` |
| `axum` | | `webhooks::router` (webhook endpoint) and `webhooks::sse` (live inbox stream) |
| `typst` | | `wa_rs::typst`: invoice, receipt and voucher templates → PDF/PNG |
| `flows-endpoint` | | WhatsApp Flows data-endpoint crypto (aws-lc-rs) |
| `full` | | all of the above |

## Crates

| Crate | Role |
| --- | --- |
| `wa-rs` | Facade: depend on this. Re-exports the others (`wa_rs::client`, `webhooks`, `adapters`, `core`, `typst`), a `prelude`, the CMS `inbox`, and the `client()` shortcut. Feature flags pick adapters. |
| `wa-core` | Error tree, ids, secrets, ports (`HttpTransport`, `KvStore`, `ConversationStore`, `EventSink`, `Clock`). |
| `wa-client` | Graph API client, one module per endpoint family; OTP service, Embedded Signup onboarding and token vault. |
| `wa-webhooks` | Signature/verify-token checks, typed payloads, normalized events, dedup, axum router + SSE. |
| `wa-adapters` | reqwest transport; memory, Postgres, Redis stores; channel/broadcast/fan-out sinks. |
| `wa-typst` | Typst → PDF/PNG (invoices, receipts, vouchers) for document and image messages. |

## Development

```bash
just            # list recipes
just ci         # the gate CI runs: lint, check, test, skills-check, doc, features, deny, test-live
just test       # unit and in-process tests (live adapter tests skip)
just test-live  # adapter tests against real Postgres and Redis
just meta-docs  # mirror Meta's docs locally (gitignored) for grep
```

Open the repo in the dev container (`.devcontainer/`) for a sandboxed
Claude Code environment: pinned toolchain, default-deny egress firewall,
Postgres and Redis sidecars.

Agents: see [AGENTS.md](AGENTS.md). Claude Code project skills and agents
live in `.claude/`.

Coding agents in the repositories that *use* wa-rs (the store, the CMS) get
consumer skills from [`skills/`](skills/README.md): 23 small, task-shaped
skills (`wa-rs` is the map; `wa-rs-send-messages`, `wa-rs-webhook-endpoint`,
`wa-rs-otp-login`, `wa-rs-cms-inbox`, …). Install all of them with
`npx skills add vaam-apps/wa-rs`, or a subset with
`npx skills add vaam-apps/wa-rs -s wa-rs -s wa-rs-cms-inbox`. Each is stamped
with the wa-rs commit it was verified against, and `just ci` keeps them
true: their Rust blocks are excerpts of example files it compiles and
tests, and every Rust name they use must exist. A public API change
updates them in the same PR.

## Naming

`wa-rs` is already taken on crates.io by an unrelated project, so the
workspace is `publish = false` until crate names are chosen.

## Documentation

| Read | For |
| --- | --- |
| [docs/guides/](docs/guides/README.md) | integrator guides: Meta setup, Embedded Signup, webhooks, CMS inbox, marketing, OTP login, documents, production |
| [docs/coverage.md](docs/coverage.md) | what is implemented, per Meta feature |
| [docs/architecture.md](docs/architecture.md) | the design spec: ports, error tree, security rules |
| [OPEN_QUESTIONS.md](OPEN_QUESTIONS.md) | product decisions still open (read before production) |
| [CONTRIBUTING.md](CONTRIBUTING.md) | setup, the `just ci` gate, review pipeline, docs/skills parity |
| [docs/dev-environment.md](docs/dev-environment.md) | the Claude Code dev container and its firewall |
| [CHANGELOG.md](CHANGELOG.md) | what changed |
| [skills/](skills/README.md) | agent skills for code that uses wa-rs |
| `cargo doc --open -p wa-rs --all-features` | the API reference |

## License

MIT
