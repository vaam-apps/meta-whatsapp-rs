# Production

**Goal:** run wa-rs on several instances without losing webhooks or
tokens, without leaking secrets or customer data into logs, and without
being surprised by Meta's limits or API versions.

Agent skills: [`wa-rs-production`](../../skills/wa-rs-production/SKILL.md),
[`wa-rs-storage`](../../skills/wa-rs-storage/SKILL.md). Design background:
[architecture.md](../architecture.md) (adapters, error tree, security
rules).

## 1. Storage

Everything stateful sits on two ports. The typed stores are built on
`KvStore`, so one adapter serves them all:

| Data | Port | Namespace | Lose it and |
| --- | --- | --- | --- |
| merchants' business tokens (`TokenVault`) | `KvStore` | `wa.token` | every merchant reconnects |
| Embedded Signup attempts (`SignupSessions`) | `KvStore` | `wa.es.session` | attempts in flight fail |
| OTP challenges and issue limits (`OtpService`) | `KvStore` | `wa.otp`, `wa.otp.rate` | codes in flight fail; limits reset |
| webhook dedup (`DedupGuard`) | `KvStore` | `wa.webhook.dedup` | Meta's retries are delivered again |
| inbox history (`InboxSink`, `Inbox`) | `ConversationStore` | tables `wa_*` | the history |

| | Memory | Postgres (`postgres`) | Redis (`redis`) |
| --- | --- | --- | --- |
| `KvStore` | `MemoryKvStore` | `PostgresKvStore` | `RedisKvStore` |
| `ConversationStore` | `MemoryConversationStore` | `PostgresConversationStore` | — |
| survives restarts, shared by instances | no | yes | yes, with persistence on |
| expiry clock | the process | the database server | the Redis server |
| maintenance | — | `migrate` at startup; `purge_expired()` every few minutes to hourly | eviction policy `noeviction`, on an instance of its own |
| limits | one instance | refuses U+0000 in stored text (the inbox stores it as U+FFFD) | no built-in TLS |

Postgres alone covers everything and is the simplest choice. Put the vault
on Redis only with persistence (AOF or RDB): a Redis that forgets on
restart forgets every merchant's token. And only with `noeviction`: every
key with a TTL there enforces a limit (OTP issue logs and cooldowns, dedup
markers, signup sessions), and a `volatile-*` policy evicts them silently
under memory pressure, handing out more OTP codes and delivering Meta's
retries twice. A full Redis then fails writes (`StorageError::Backend`):
size `maxmemory` for 7 days of dedup markers.

```rust
use std::sync::Arc;
use std::time::Duration;
use wa_rs::adapters::store::postgres::{self, PostgresKvStore, sqlx};

let pool = sqlx::PgPool::connect(&database_url).await?;
postgres::migrate(&pool).await?; // idempotent, takes a lock: any instance may run it
let kv = PostgresKvStore::new(pool.clone());
let purger = kv.clone();
tokio::spawn(async move {
    let mut every = tokio::time::interval(Duration::from_secs(600));
    loop {
        every.tick().await;
        if let Err(e) = purger.purge_expired().await {
            tracing::warn!(error = %e, "purging expired wa-rs rows failed");
        }
    }
});
let kv: Arc<dyn wa_rs::core::store::KvStore> = Arc::new(kv);
```

Redis over TLS (`rediss://`): wa-rs enables no TLS feature of redis on
purpose (two rustls crypto providers in one binary make the first TLS
connection panic). Enable redis's `tokio-rustls-comp` in your own crate,
install a provider at startup, and hand the connection to
`RedisKvStore::new(conn)`; the type's rustdoc shows how
([open question](../../OPEN_QUESTIONS.md#storage) 19).

Every store adapter runs an executable conformance suite; `just test-live`
runs it against real Postgres and Redis.

## 2. Secrets and keys

| Secret | Used for | Keep it | Rotate |
| --- | --- | --- | --- |
| system user token | your own number (OTP, order messages) | secret manager | generate a new one in Business Settings, deploy, revoke the old |
| app secret | webhook signatures, the code exchange, the app token for `debug_token` | secret manager | list old and new in `SignatureVerifier::new` while rolling out, then drop the old |
| verify token | webhook `GET` verification | secret manager | change it in the dashboard and in your config together |
| vault key(s) | encrypting merchants' tokens | secret manager, **not** the vault's database | `VaultKeys::new(new).with_previous(old)`, `vault.rotate(&waba_id)` for each WABA, drop the old key |
| OTP pepper | keyed hashes of codes and numbers | secret manager, not the OTP database | invalidates outstanding codes and resets limits |
| merchants' business tokens | acting as a merchant | the vault only | merchant reconnects (no refresh) |
| two-step PINs | registering numbers | not stored by wa-rs; the examples ask the merchant per attempt | your policy ([open question](../../OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants) 4) |

`AccessToken`, `AppSecret`, `VerifyToken`, `SecretBytes`, `SignupCode`,
`TwoStepPin`, `OtpPepper` and `VaultKey` print `[REDACTED]` (or only an id)
in `Debug`; the value comes out only through `expose_secret()`. Call it at
the boundary that needs it and nowhere near a log line.

The client attaches a token to two origins only: the configured Graph
endpoint (scheme, host and port) and `https://lookaside.fbsbx.com`, Meta's
media download host. A request to any other URL with a token fails locally
(`Error::Validation` on `url`). Behind a Graph proxy
(`ClientBuilder::endpoint`), the token goes to the proxy and the media host,
not to `graph.facebook.com`.

## 3. Logs and observability

wa-rs logs through `tracing`; install a subscriber and filter with
`RUST_LOG`:

```rust
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()) // RUST_LOG=info,wa_client=debug
    .init();
```

| Level | What |
| --- | --- |
| `debug` (`wa_client`) | each Graph request (method, path, attempt); each retry (error kind, Graph code, delay); OTP challenges issued and verified (challenge id only) |
| `warn` (`wa_webhooks`) | rejected deliveries (signature, size) and verification requests; changes kept untyped (field name); sink failures and events in flight elsewhere (the non-`200` answers); dedup leases that expired before the event was marked done |
| `warn` (`wa_client`) | the token vault failing to re-encrypt a record under the active key (retried on the next read) |
| `error` | signed bodies that are not webhooks (size and SHA-256 only); dedup markers that could not be written or released; a reply sent but not recorded in the inbox |

**Never logged:** tokens, the app secret, Embedded Signup codes, PINs, OTP
codes, request query strings (they can carry `client_secret` or a code),
and webhook payload values. Errors from the code exchange drop any text
that could contain the URL, and an unreadable send response
(`Error::Decode` from `Messages::send`, `Marketing::send` or an OTP issue)
carries neither the body nor serde's message: the response names the
recipient. Keep it that way in your code: do not log request bodies, the
signature header, `WebhookEvent`'s `Debug`, or anything from
`expose_secret()`. `TracingSink::new()` logs event kinds only;
`with_payload(true)` logs customer data. Request paths at `debug` contain
WABA and phone number ids: identifiers, not secrets.

Worth a metric: `WebhookEvent::kind()` counts, the `DeliveryReport`
(`delivered`, `duplicates`, `unparsed`), webhook answers by status (a
stream of `503`s means sinks outlast the dedup lease), `err.kind()` of
failed sends, and the count of `Unknown` events.

## 4. Limits, throughput and retries

| Limit (Meta) | Value | In wa-rs |
| --- | --- | --- |
| messages per number | 80/s by default, up to 1,000 | `RateLimited` (130429), replayed within the retry budget |
| same user | about one message per 6 s, short bursts borrowed from later | `PairRateLimited` (131056), replayed within the budget; Meta suggests backing off 4^n seconds after that |
| templates to new users | portfolio messaging limit, 250 to unlimited per rolling 24 h | not tracked: count in your campaign queue |
| management endpoints | 200 requests/hour per app per WABA (5,000 for active WABAs) | don't list templates or phone numbers per request: cache them |
| number registration | 10 per 72 h | 133016 locks for 72 h, never retried |
| webhooks | answer with median ≤ 250 ms; size for ~3× outbound + 1× inbound | see [webhooks.md](webhooks.md#3-answer-fast-what-the-sink-does) |

The client's defaults: 30 s timeout (`DEFAULT_TIMEOUT`), 3 retries with
full-jitter backoff from 250 ms to 8 s. Change them once, at startup:

```rust
use std::time::Duration;
use wa_rs::RetryPolicy;

let client = wa_rs::client_builder()?
    .timeout(Duration::from_secs(15))
    .retry(RetryPolicy { max_retries: 2, base_delay: Duration::from_millis(500), max_delay: Duration::from_secs(4) })
    .build()?;
```

Sends are never replayed after a timeout or a 5xx. A campaign or
notification queue on top must be idempotent itself: tag each message with
`callback_data`, and before resending look for its status webhook (see
[getting-started.md](getting-started.md#4-handle-errors)).

## 5. Graph API version

- `ApiVersion::DEFAULT` is **v25.0**, the version Meta's docs used on
  2026-09-24, and the one wa-rs was tested against. Pin wa-rs by `rev` and
  the version moves only when you move the pin.
- To hold a version across a wa-rs upgrade:
  `.api_version(ApiVersion::new(25, 0))` on the builder
  (`wa_rs::core::config::ApiVersion`). Use the same value in the Embedded
  Signup page's `FB.init`.
- Before moving: read Meta's changelog for the new version, rerun your
  integration tests against a test WABA, and watch the `Unknown` count:
  webhook fields Meta adds arrive as `Unknown` instead of failing.
- Embedded Signup v2 and v3, including their public previews, are
  deprecated on 2026-10-15 (the matching `EsVersion` values are
  `#[deprecated]`); `LaunchOptions` targets v4 (the login configuration
  selects it).

## 6. Several instances

- **Shared stores** for everything in section 1; the memory stores give each
  instance its own dedup, limits and vault.
- **Clocks:** Postgres and Redis decide expiry by their own clock; keep all
  hosts on NTP.
- **One `Client` per process**, cloned; `with_token` per merchant. Each
  `wa_rs::client()` call creates a new connection pool.
- **Live inbox:** the broadcast channel behind SSE is per process. Relay
  events between instances yourself (Postgres `LISTEN/NOTIFY`, Redis
  pub/sub), or pin each merchant's browser and webhooks to one instance.
- **Queues:** a `ChannelSink` queue lives in memory and dies with the
  process. Persist before acknowledging anything you cannot lose.
- **Deploys and crashes:** a webhook delivery cut mid-way leaves a dedup
  lease that expires after 60 s; Meta's retry then delivers it. Nothing to
  do, as long as sinks are idempotent.
- **Load balancer or proxy in front of the webhook:** HTTPS with a valid
  certificate, body limit at least 3 MiB, no body rewriting or
  decompression (the signature covers the raw bytes), and a timeout longer
  than your slowest sink.

## 7. Before going live

- Business verified; payment method in WhatsApp Manager (yours and each
  merchant's); templates approved in every language you send.
- Tech Provider: App Review passed with Advanced access; Embedded Signup
  domains and configuration set ([embedded-signup.md](embedded-signup.md)).
- Solution Partner: the system user's token, id and your credit line id in
  the secret manager and configuration; a currency for every merchant (or
  a default); `PartnerRemoved` wired to `revoke_credit_line`
  ([embedded-signup.md](embedded-signup.md#solution-partner-mode)).
- Webhook fields subscribed; alerts wired ([webhooks.md](webhooks.md#8-operational-alerts)).
- Secrets from the secret manager, none in the repository or the database.
- [OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md) read: several defaults there
  (OTP issue limit, PIN policy, the provisional NUL replacement, a
  dead-letter path for webhook batches, token refresh, how synced
  coexistence history counts in the inbox) are product decisions still
  open. The OTP namespace is required since d67b3ac.
- Upgrading from a wa-rs revision before e40b86f: outstanding OTP codes
  become `NotFound` once (their store keys now include the sending number),
  and issue limits restart ([otp-login.md](otp-login.md#3-wire-the-service)).

## The dev container

The repository's `.devcontainer/` is for working **on** wa-rs, not for
deploying it: the pinned Rust toolchain, `just`, `cargo-deny`, the `typst`
CLI, Postgres and Redis sidecars, and a default-deny egress firewall (it
fails closed) that still lets `graph.facebook.com` through. Inside it,
`just ci` and `just test-live` use the sidecars. Details:
[dev-environment.md](../dev-environment.md). To run the examples against
Meta from inside it, pass the tokens as environment variables; nothing in
the container stores them.
