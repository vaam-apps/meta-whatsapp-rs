# Getting started

meta-whatsapp-rs is a Cargo workspace. Applications depend on one crate, the
`meta-whatsapp-rs` facade, and choose adapters with feature flags; the other
crates are re-exported through it. This page covers adding the dependency,
the feature flags, the crates and the runnable examples. To set up the Meta
app and send a first message, continue with
[Meta setup and first message](guides/getting-started.md).

## Add the dependency

`meta-whatsapp-rs` is not on crates.io: the workspace is `publish = false`
until a release is decided. Depend on it from git, pinned to a commit:

```toml
[dependencies]
meta-whatsapp-rs = { git = "https://github.com/vaam-apps/meta-whatsapp-rs", rev = "<commit>", features = ["axum", "postgres"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

axum and sqlx types are part of the API (`Router`, `PgPool`), so use the
versions meta-whatsapp-rs was built with, re-exported:
`meta_whatsapp_rs::webhooks::axum` (feature `axum`) and
`meta_whatsapp_rs::adapters::store::postgres::sqlx` (feature `postgres`). If
you need axum features meta-whatsapp-rs does not turn on, add `axum = "0.8"`
with them yourself: Cargo builds one axum 0.8 for both, so the types still
match.

`use meta_whatsapp_rs::prelude::*;` brings in the client, ids, `Recipient`,
the message and template builders, the webhook pieces, the storage and sink
ports, and the inbox.

## Feature flags

| Feature          | Default | Adds                                                                                                                                      |
| ---------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `reqwest`        | yes     | `adapters::http::ReqwestTransport` (rustls, HTTP/2) and the `meta_whatsapp_rs::client()` / `meta_whatsapp_rs::client_builder()` shortcuts |
| `memory`         | yes     | `MemoryKvStore`, `MemoryConversationStore`: tests, development, one instance                                                              |
| `sinks`          | yes     | channel, broadcast, fan-out, filter, fn and tracing sinks                                                                                 |
| `postgres`       |         | `PostgresKvStore`, `PostgresConversationStore`, embedded migrations (sqlx)                                                                |
| `redis`          |         | `RedisKvStore`                                                                                                                            |
| `axum`           |         | `webhooks::router` (webhook endpoint) and `webhooks::sse` (live inbox stream)                                                             |
| `typst`          |         | `meta_whatsapp_rs::typst`: invoice, receipt and voucher templates to PDF/PNG                                                              |
| `flows-endpoint` |         | WhatsApp Flows data-endpoint crypto (aws-lc-rs)                                                                                           |
| `bot`            |         | `meta_whatsapp_rs::bot`: a bot framework (commands, guards, cooldowns, middleware, plugins, Markdown replies, paced broadcasts)           |
| `testing`        |         | `core::testing::ScriptedTransport` for your own tests (enable in `[dev-dependencies]`)                                                    |
| `full`           |         | all of the above                                                                                                                          |

## The crates

The library is hexagonal: `meta-whatsapp-core` owns the error tree, ids,
secrets and the ports, and every other crate depends on it. The
[architecture](architecture.md) is the full design.

| Crate                       | Role                                                                                                                                                                                                                |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `meta-whatsapp-rs`          | Facade: depend on this. Re-exports the others (`meta_whatsapp_rs::client`, `webhooks`, `adapters`, `core`, `typst`, `bot`), a `prelude`, the CMS `inbox`, and the `client()` shortcut. Feature flags pick adapters. |
| `meta-whatsapp-core`        | Error tree, ids, secrets, ports (`HttpTransport`, `KvStore`, `ConversationStore`, `EventSink`, `Clock`).                                                                                                            |
| `meta-whatsapp-client`      | Graph API client, one module per endpoint family; OTP service, Embedded Signup onboarding and token vault.                                                                                                          |
| `meta-whatsapp-webhooks`    | Signature and verify-token checks, typed payloads, normalized events, dedup, axum router and SSE.                                                                                                                   |
| `meta-whatsapp-adapters`    | reqwest transport; memory, Postgres, Redis stores; channel, broadcast and fan-out sinks.                                                                                                                            |
| `meta-whatsapp-typst`       | Typst to PDF/PNG (invoices, receipts, vouchers) for document and image messages.                                                                                                                                    |
| `meta-whatsapp-bot`         | Bot framework over webhooks: commands, guards, cooldowns, middleware, compile-time plugins, Markdown to WhatsApp formatting, paced broadcasts.                                                                      |
| `meta-whatsapp-server`      | The HTTP service (a binary, not a dependency): tenants, keys, the `/v1` API over the facade.                                                                                                                        |
| `meta-whatsapp-server-core` | The service's framework-free core (not a dependency either): its domain, authorization order, error model as data, and the storage ports its memory and Postgres backends implement.                                |

### API reference

The API reference is the rustdoc. As the crates are not published, it is not
on docs.rs; build it from a checkout:

```bash
cargo doc -p meta-whatsapp-rs --all-features --open
```

## Run the examples

The runnable programs live in
[`crates/meta-whatsapp-rs/examples/`](https://github.com/vaam-apps/meta-whatsapp-rs/tree/main/crates/meta-whatsapp-rs/examples).
Each example's header lists the environment variables it reads and the command
that runs it.

| Example            | What                                         | Command                                                                                                                              |
| ------------------ | -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `send_message`     | a text, then a template                      | `cargo run -p meta-whatsapp-rs --example send_message`                                                                               |
| `invoice_document` | Typst invoice, upload, then document message | `cargo run -p meta-whatsapp-rs --example invoice_document --features typst`                                                          |
| `embedded_signup`  | onboarding server and launch page            | `WA_TENANTS=… WA_APP_ID=… WA_APP_SECRET=… WA_ES_CONFIG_ID=… cargo run -p meta-whatsapp-rs --example embedded_signup --features axum` |
| `cms_inbox`        | webhook endpoint, inbox, SSE, replies        | `WA_TENANTS=… WA_APP_SECRET=… WA_VERIFY_TOKEN=… cargo run -p meta-whatsapp-rs --example cms_inbox --features axum`                   |
| `otp_login`        | issue and verify a code                      | `cargo run -p meta-whatsapp-rs --example otp_login`                                                                                  |

`embedded_signup` and `cms_inbox` refuse to start without `WA_TENANTS` (their
headers show a one-line setup) and listen on `127.0.0.1` unless `WA_BIND` says
otherwise. Add `postgres` to the features and set `DATABASE_URL` to run them
on Postgres; with the same `DATABASE_URL`, `WA_VAULT_KEY` and `WA_TENANTS`,
the merchant you connect in the first is the one you chat as in the second
(list the connected number under that tenant's `phone_number_ids`).

## Not writing Rust? Run the service

Apps in other stacks use meta-whatsapp-rs through `meta-whatsapp-server`, an
HTTP service built on the library and deployed next to them: one deployment
per Meta app, many tenants, and the `/v1` REST API described by a committed
OpenAPI document,
[`crates/meta-whatsapp-server/openapi/v1.json`](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/crates/meta-whatsapp-server/openapi/v1.json).

```bash
cargo build --release -p meta-whatsapp-server
export DATABASE_URL=postgres://… WA_APP_SECRET=… WA_VERIFY_TOKEN=…
export WA_VAULT_KEY="$(openssl rand -base64 32)" WA_OTP_PEPPER="$(openssl rand -hex 32)"  # keep both
./target/release/meta-whatsapp-server admin create-admin-key   # printed once
./target/release/meta-whatsapp-server serve                    # 127.0.0.1:8080 (Meta), :8081 (API)
```

It refuses to start on an unsafe setting (a blank secret, no vault key, memory
storage in production). Configuring it, creating tenants and keys and making
a first call are in [Run the service](guides/server.md).

## Agent skills

Coding agents in the repositories that use meta-whatsapp-rs get consumer
skills from
[`skills/`](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/skills/README.md):
small, task-shaped skills, each stamped with the commit it was verified
against. Install them with:

```bash
npx skills add vaam-apps/meta-whatsapp-rs
```

## Next steps

- [Meta setup and first message](guides/getting-started.md): the Meta app,
  tokens, and a first message with errors handled.
- [Integrator guides](guides/README.md): Embedded Signup, webhooks, the CMS
  inbox, marketing, OTP login, documents, bots and production.
- [Feature coverage](coverage.md): what is implemented, per Meta feature.
