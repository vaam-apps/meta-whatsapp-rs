# Production

**Goal:** run meta-whatsapp-rs on several instances without losing webhooks or
tokens, without leaking secrets or customer data into logs, and without
being surprised by Meta's limits or API versions.

Agent skills: [`meta-whatsapp-rs-production`](../../skills/meta-whatsapp-rs-production/SKILL.md),
[`meta-whatsapp-rs-storage`](../../skills/meta-whatsapp-rs-storage/SKILL.md). Design background:
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
| maintenance | `apply_retention` on a schedule, when a retention is set | `migrate` at startup; `purge_expired()` every few minutes to hourly; `apply_retention` on a schedule, when a retention is set | eviction policy `noeviction`, on an instance of its own |
| limits | one instance | refuses U+0000 in ids and keys (message content keeps it, as bytes and `json`: search on bytes, no payload indexes) | no built-in TLS |

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
use meta_whatsapp_rs::adapters::store::postgres::{self, PostgresKvStore, sqlx};

let pool = sqlx::PgPool::connect(&database_url).await?;
postgres::migrate(&pool).await?; // idempotent, takes a lock: any instance may run it
let kv = PostgresKvStore::new(pool.clone());
let purger = kv.clone();
tokio::spawn(async move {
    let mut every = tokio::time::interval(Duration::from_secs(600));
    loop {
        every.tick().await;
        if let Err(e) = purger.purge_expired().await {
            tracing::warn!(error = %e, "purging expired meta-whatsapp-rs rows failed");
        }
    }
});
let kv: Arc<dyn meta_whatsapp_rs::core::store::KvStore> = Arc::new(kv);
```

An upgrade across a migration that rewrites tables (migration 3, lossless
message content) is the exception: run `migrate` once from a one-off job
first ([section 7](#7-before-going-live)).

Redis over TLS (`rediss://`): meta-whatsapp-rs enables no TLS feature of redis on
purpose (two rustls crypto providers in one binary make the first TLS
connection panic). Enable redis's `tokio-rustls-comp` in your own crate,
install a provider at startup, and hand the connection to
`RedisKvStore::new(conn)`; the type's rustdoc shows how
([decided](../../OPEN_QUESTIONS.md#storage) 19: `rediss://` with an explicit
provider, roadmap L21c, not built yet).

Every store adapter runs an executable conformance suite; `just test-live`
runs it against real Postgres and Redis.

## 2. Secrets and keys

| Secret | Used for | Keep it | Rotate |
| --- | --- | --- | --- |
| system user token | your own number (OTP, order messages) | secret manager | generate a new one in Business Settings, deploy, revoke the old |
| app secret | webhook signatures, the code exchange, the app token for `debug_token` | secret manager | list old and new in `SignatureVerifier::new` while rolling out, then drop the old |
| verify token | webhook `GET` verification | secret manager | change it in the dashboard and in your config together |
| vault key(s) | encrypting merchants' tokens, and a Solution Partner's credit ledger | secret manager, **not** the vault's database | `VaultKeys::new(new).with_previous(old)`, `vault.rotate(&waba_id)` for every WABA ever onboarded (offboarded ones too: the credit ledger outlives the token), `vault.rotate_business(&business_id)` for each business revoked by business id alone, then drop the old key |
| OTP pepper | keyed hashes of codes and numbers | secret manager, not the OTP database | invalidates outstanding codes and resets limits |
| merchants' business tokens | acting as a merchant | the vault only | merchant reconnects (no refresh) |
| two-step PINs | registering numbers | not stored by meta-whatsapp-rs; the examples ask the merchant per attempt | asked per attempt, never stored ([decided](../../OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants) 4) |

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

meta-whatsapp-rs logs through `tracing`; install a subscriber and filter with
`RUST_LOG`:

```rust
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()) // RUST_LOG=info,meta_whatsapp_client=debug
    .init();
```

| Level | What |
| --- | --- |
| `debug` (`meta_whatsapp_client`) | each Graph request (method, path, attempt); each retry (error kind, Graph code, delay); OTP challenges issued and verified (challenge id only) |
| `warn` (`meta_whatsapp_webhooks`) | rejected deliveries (signature, size) and verification requests; changes kept untyped (field name); sink failures and events in flight elsewhere (the non-`200` answers); dedup leases that expired before the event was marked done |
| `warn` (`meta_whatsapp_client`) | the token vault failing to re-encrypt a record under the active key (retried on the next read) |
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

| Limit (Meta) | Value | In meta-whatsapp-rs |
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
use meta_whatsapp_rs::RetryPolicy;

let client = meta_whatsapp_rs::client_builder()?
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
  2026-09-24, and the one meta-whatsapp-rs was tested against. Pin meta-whatsapp-rs by `rev` and
  the version moves only when you move the pin.
- To hold a version across a meta-whatsapp-rs upgrade:
  `.api_version(ApiVersion::new(25, 0))` on the builder
  (`meta_whatsapp_rs::core::config::ApiVersion`). Use the same value in the Embedded
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
  `meta_whatsapp_rs::client()` call creates a new connection pool.
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
- Solution Partner ([embedded-signup.md](embedded-signup.md#solution-partner-mode)):
  - the system user's token, id and your credit line id in the secret
    manager and configuration; a currency for every merchant (or a
    default);
  - the approval gate: onboarding through `onboard_with_approval` (plain
    `onboard` is refused), reserving the WABA for the merchant in one
    atomic write, and `resume_with_approval` for tokens stored before the
    deployment became a Solution Partner;
  - `PartnerRemoved` wired to `revoke_credit_line` (to
    `revoke_business_credit_line` when it names no WABA) at once, a
    coexistence disconnection included (the owner's decision,
    2026-09-25; a merchant who reconnects onboards again with
    `reshare_after_revocation`), ignoring one whose
    `waba_info.solution_partner_business_ids` does not list your business
    (a Multi-Partner Solution you are not in);
  - `PartnerAppUninstalled` wired to `offboard`, **only when its
    `waba_info.partner_app_id` is your app id**;
  - key rotation walking every WABA ever onboarded, offboarded ones
    included, and every business revoked by id (`rotate_business`),
    collecting failures instead of stopping at the first: the credit
    ledger is sealed with the vault keys;
  - alerts on `CreditError::Reconcile` and on a
    `CreditError::RevocationIncomplete` that is not retryable: both need a
    person and Meta Business Suite (both are `ErrorKind::Unknown`); a
    retryable one (Meta has not confirmed a `DELETE`, a pending share not
    found yet, a ledger write) is called again later; a share whose
    answer was lost (`Reconcile`) is never retried at once;
  - an admin action for a pending share Meta never lists (a revocation
    that keeps answering `share_pending`), on a staff-only route: after
    checking the WABA's funding in Meta Business Suite,
    `clear_pending_share` with the operator id of the authenticated staff
    session (not a name or an email: it is sealed in the ledger for as
    long as the WABA's credit record), acknowledging a funding it reports
    only once Business Suite shows it is not your line;
- Webhook fields subscribed; alerts wired ([webhooks.md](webhooks.md#8-operational-alerts)).
- Secrets from the secret manager, none in the repository or the database.
- Inbox history's retention chosen (kept by default; `with_retention` on
  the conversation store and a scheduled `apply_retention`), and, if
  your privacy obligations require erasure, the whole procedure of
  [section 8](#8-retention-and-erasure-on-postgres) wired, not
  `ConversationStore::erase` alone: every identity (`Inbox::identities`),
  `Inbox::erase_all` on each of the merchant's numbers, your own copies,
  outbox rows and dead letters, Meta's contact book (roadmap L9), an
  erasure journal, and a second erasure after 7 days
  ([cms-inbox.md](cms-inbox.md#8-erasing-a-customer-and-retention)).
- [OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md) read: its defaults (OTP
  issue limit, PIN policy, no token refresh) were decided on 2026-09-26,
  and some decisions are not built yet (a dead-letter path for webhook
  batches, Redis TLS). The OTP namespace is
  required since d67b3ac; a revoked message keeps its content in the inbox
  (decided on 2026-09-25).
- Upgrading from an older meta-whatsapp-rs revision, per commit crossed:
  - e40b86f: outstanding OTP codes become `NotFound` once (their store
    keys now include the sending number), and issue limits restart
    ([otp-login.md](otp-login.md#3-wire-the-service)).
  - 4b47bf7: a custom `ConversationStore`'s `update_status` takes the
    business `phone_number_id` first.
  - 6d50701 and a9593f3: a custom `ConversationStore` must implement
    three new methods, `append_synced`, `fill_media_placeholder` and
    `revoke`, and pass `conversation_conformance::run`
    ([cms-inbox.md](cms-inbox.md#1-storage-postgres-and-migrations)).
  - af5b1f8: that suite also fails a custom store that fills a revoked
    placeholder or lets a revoke's tombstone into the conversation
    summary.
  - 8238853: OTP codes in flight answer `Invalid` once (the code hash
    now covers its store key).
  - 8238853 and 7e4801f: `OtpService::new` refuses a namespace with edge
    whitespace, control or format characters (`Error::Config`); fixing
    it changes the store keys, so codes in flight answer `NotFound` once
    and limits restart ([otp-login.md](otp-login.md#3-wire-the-service)).
  - PR #7, lossless message content (the owner's decision of
    2026-09-25): Postgres migration 3 rewrites the inbox tables, and an
    older revision cannot run against them afterwards. In this order
    (details and a pre-flight query: the `meta_whatsapp_adapters::store::postgres`
    docs, "Upgrading to lossless content"):
    1. **Back up** `wa_messages` and `wa_conversations` of every table
       prefix. The only way back is a restore, which loses what was
       recorded after the upgrade: webhooks the new instances acknowledged
       are not delivered again, and replies sent meanwhile leave the
       history.
    2. **Stop every instance of the older revision** that writes to them
       (webhook receivers, anything calling `Inbox::send`). That pauses
       every webhook consumer, OTP delivery statuses and `PARTNER_REMOVED`
       revocations too; Meta's backoff decides how long the backlog takes
       afterwards. An older instance left running fails on every content
       statement (500s Meta redelivers, a reply sent but not recorded).
    3. **Drop the objects of your own** on `kind`, `text`, `payload`,
       `error` and `last_text` that the pre-flight query lists. Migration
       3 refuses to run under anything on `payload` or `error` (an
       expression such as `payload->>'type'` would fail every later
       insert of a payload holding a NUL); Postgres refuses views, rules,
       trigram and full-text indexes on the others; triggers and functions
       that name those columns are not checked and would fail every
       insert, so rewrite them. Plain b-tree indexes on text are kept.
    4. **Run `migrate` once, from a one-off job**, per table prefix, with
       a `lock_timeout` and no `statement_timeout` on its connection, and
       free disk for a copy of `wa_messages` and its indexes. It locks
       both tables for the rewrite: 200,006 messages (a 153 MB table)
       took 1 to 2 seconds on a local Postgres 18.
    5. **Update SQL of your own**
       ([cms-inbox.md](cms-inbox.md#1-storage-postgres-and-migrations)).
    6. **Start the new revision.**

    Existing rows keep their content; a NUL an older revision stored as
    U+FFFD stays U+FFFD. A custom `ConversationStore` now receives U+0000
    from the inbox, which no longer replaces it: one on `text` or `jsonb`
    fails its webhook batches and logs replies as "message sent but not
    recorded" until it keeps content exactly (the conformance suites say
    so).
  - The revision that adds `EmbeddedSignup::clear_pending_share` adds an
    audit trail to each WABA's credit record (`cleared_shares`). An older
    revision knows nothing of it: rolling back, or running an older
    revision beside a newer one, drops the trail whenever the older one
    writes that record. From that revision on, fields a later revision
    adds to a credit record or an audit entry are kept. Keep your own
    append-only log of each returned `ClearedShare` too.
  - The pull request of roadmap L5 (the `ConversationStore` port
    change): a custom `ConversationStore` must implement fourteen new
    methods (`message`, window events, thread ownership, synced contacts,
    identity links, `identities`, `erase_all`, `purge_before`; `erase`
    is provided) and pass `conversation_conformance::run`, under each
    `ErasureMode` it offers. Postgres migration 4 adds four tables, a
    `sender` column on `wa_messages` and four indexes on the existing
    tables, and changes no existing column: the previous revision keeps
    working beside it, but its `migrate` then refuses the database, so
    upgrade every instance that migrates at startup. It back-fills the
    sender of every inbound message already stored (one `UPDATE` of
    those rows): on a large inbox, run it from a one-off job with a
    `lock_timeout` (reads and writes of `wa_messages` wait meanwhile).
    An instance of the previous revision takes none of the advisory
    locks that keep an erasure and the appends in flight consistent, and
    writes no sender: the next erasure on a number fills in the senders
    missing there (from the payloads, in Rust) before it matches, but a
    group message such an instance records while an erasure runs is
    missed. Finish the upgrade before you erase.
  - Nothing else is back-filled: rows and conversation summaries
    recorded before an upgrade stay as they were written (synced history
    recorded before 6d50701 keeps the unread count and window it moved,
    for instance).

## 8. Retention and erasure on Postgres

The Postgres store deletes for real: `purge_before`, `apply_retention`
and `erase_all` run `DELETE`s (and, for a group message under
`ErasureMode::Redact`, an `UPDATE` that overwrites its content). What a
deletion leaves behind is Postgres's, and yours to handle
([cms-inbox.md § 8](cms-inbox.md#8-erasing-a-customer-and-retention)
says what the store itself does not reach):

- **Dead rows.** A deleted or overwritten row stays on disk, with its
  index entries (BSUIDs, phone numbers, message ids), until (auto)vacuum
  reclaims it, and freed space keeps its bytes until it is reused. Run
  `VACUUM` on `wa_messages`, `wa_conversations`, `wa_window_events`,
  `wa_thread_owners`, `wa_synced_contacts` and `wa_identity_links` after
  erasures; when you need a guarantee that the bytes are gone, rewrite
  them with `VACUUM FULL` (an `ACCESS EXCLUSIVE` lock: the inbox waits)
  or `pg_repack` (online).
- **Copies.** Backups, WAL archives (point-in-time recovery), streaming
  and logical replicas, and change-data-capture or ETL consumers of
  these tables keep what was deleted until their own retention drops it:
  that retention bounds how long an erased customer survives. A CDC
  consumer must apply the deletes, and a redaction's updates, downstream.
- **Restores bring erased customers back.** Keep an erasure journal
  outside the database you restore: per erasure, the time and an HMAC,
  under a key of your own from your secret manager, of
  `phone_number_id|contact` for each identity erased: it names nobody
  in clear. After any restore, and before the inbox serves again, replay
  it: compute the same HMAC over every identity the restored tables hold
  (the business number with each conversation's and synced contact's
  `contact`, each link's two identities, each message's `sender`) and
  `erase_all` the ones the journal lists.
- **Server logs.** With `log_min_duration_statement`, `log_statement` or
  `auto_explain` on, Postgres logs a statement's bound parameters in
  full (message text, names, BSUIDs; a large history chunk is the
  likeliest slow statement). Set `log_parameter_max_length = 0` for the
  application's role (a superuser runs
  `ALTER ROLE <role> SET log_parameter_max_length = 0`), and keep
  `log_parameter_max_length_on_error` at its default, 0.
- **Keep the dedup markers.** `DedupGuard` keeps a hashed marker per
  webhook event (`wa.webhook.dedup`) for 7 days and an hour; while it
  exists, Meta's redelivery of an event already delivered is dropped
  instead of recording the erased customer's message again. Never purge
  them as part of an erasure. What arrives after the erasure is recorded
  as any new event is (a late revoke's tombstone, an echo, a history
  chunk, an address book sync), so **erase again once Meta's 7-day
  redelivery window has passed.**

The procedure, per erasure request:

1. Collect every identity: `Inbox::identities(&key)` on each of the
   merchant's numbers, and the ones you hold yourself (the phone number
   the customer gave you, a BSUID in your CRM).
2. `Inbox::erase_all(&identities)` on each of the merchant's numbers,
   behind your ownership check of the number (an `Inbox` is bound to
   one number and refuses another's keys). Their group messages are
   redacted and keep their ids (a `wamid` encodes the phone number)
   unless the store deletes them (`ErasureMode::Delete`).
3. Delete your own copies (downloaded media, exports, SSE clients'
   caches), the service's outbox rows (roadmap M2f) and your dead
   letters (L21a) for that customer.
4. Delete the customer from Meta's contact book (roadmap L9).
5. Journal the erasure (above).
6. Erase again after 7 days (steps 1 and 2).

## The dev container

The repository's `.devcontainer/` is for working **on** meta-whatsapp-rs, not for
deploying it: the pinned Rust toolchain, `just`, `cargo-deny`, the `typst`
CLI, Postgres and Redis sidecars, and a default-deny egress firewall (it
fails closed) that still lets `graph.facebook.com` through. Inside it,
`just ci` and `just test-live` use the sidecars. Details:
[dev-environment.md](../dev-environment.md). To run the examples against
Meta from inside it, pass the tokens as environment variables; nothing in
the container stores them.
