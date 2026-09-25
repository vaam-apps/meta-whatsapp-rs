---
name: meta-whatsapp-rs-storage
description: "Choosing and running meta-whatsapp-rs storage - the KvStore (token vault, OTP challenges, webhook dedup, signup sessions) and ConversationStore (inbox history) ports, MemoryKvStore and MemoryConversationStore for tests, PostgresKvStore and PostgresConversationStore (migrate at startup, table prefixes, purge_expired, U+0000 kept in message content and refused in ids and keys, the lossless-content upgrade), RedisKvStore (noeviction, persistence, prefixes, bring your own TLS connection), server clocks, and writing your own adapter that passes the conformance suites. Load when wiring databases for meta-whatsapp-rs, deploying Postgres or Redis for it, or implementing a custom KvStore or ConversationStore."
---

# meta-whatsapp-rs-storage

> **Verified against meta-whatsapp-rs d9f4c05393be9b6b7ce688efe1ad309b026fbd37 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/stores.rs](examples/stores.rs), compiled by
wa-rs's own gate; its tests run the conformance suites on the memory
stores.

## When to use

Before production: everything stateful in meta-whatsapp-rs sits on two ports, and
the typed stores (vault, OTP, dedup, sessions) are built on `KvStore`, so
one adapter serves them all.

| Data | Port | Namespace | Lose it and |
| --- | --- | --- | --- |
| merchants' tokens (`TokenVault`) | `KvStore` | `wa.token` | every merchant reconnects |
| signup attempts (`SignupSessions`) | `KvStore` | `wa.es.session` | attempts in flight fail |
| OTP challenges and limits (`OtpService`) | `KvStore` | `wa.otp`, `wa.otp.rate` | codes fail; limits reset |
| webhook dedup (`DedupGuard`) | `KvStore` | `wa.webhook.dedup` | Meta's retries delivered again |
| inbox history (`InboxSink`, `Inbox`) | `ConversationStore` | tables `wa_*` | the history |

| | Memory (`memory`) | Postgres (`postgres`) | Redis (`redis`) |
| --- | --- | --- | --- |
| `KvStore` | `MemoryKvStore` | `PostgresKvStore` | `RedisKvStore` |
| `ConversationStore` | `MemoryConversationStore` | `PostgresConversationStore` | — |
| shared, survives restarts | no | yes | yes, with persistence on |
| expiry clock | the process (`with_clock`) | the database server | the Redis server |

## Postgres: the simple choice

```rust
let pool = sqlx::PgPool::connect(database_url).await?; // sqlx as meta-whatsapp-rs re-exports it
postgres::migrate(&pool).await?; // idempotent, under a lock: any instance may run it
let kv = PostgresKvStore::new(pool.clone());
let purger = kv.clone();
tokio::spawn(async move {
    // One row per webhook event (dedup) and per OTP: purge what expired.
    let mut every = tokio::time::interval(Duration::from_secs(600));
    loop {
        every.tick().await;
        if let Err(e) = purger.purge_expired().await {
            tracing::warn!(error = %e, "purging expired meta-whatsapp-rs rows failed");
        }
    }
});
```

`sqlx` is `meta_whatsapp_rs::adapters::store::postgres::sqlx` (0.9): no pin of your
own. Migrations are embedded; their history table is `wa_sqlx_migrations`,
separate from yours. Another prefix (two deployments, one schema):
`TablePrefix::new("shop_wa_")?`, `postgres::migrate_with_prefix`,
`PostgresKvStore::with_prefix`.

Message content keeps U+0000: `wa_messages.kind_utf8`, `text_utf8` and
`wa_conversations.last_text_utf8` are `BYTEA` (UTF-8), `payload_json`
and `error_json` are `json`. Your own SQL decodes the `*_utf8` columns as
UTF-8 and searches the bytes (a recipe is in the `postgres` module docs);
it never indexes, extracts or compares payload fields (`->`, `->>`, a
cast to `jsonb` fail on a document holding a NUL, so an index on one
fails the insert; `json` has no `=`). Upgrading a database written before
that is one-way: back up, stop the older writers, drop your own objects
on those columns, run `migrate` once from a job, in that order
([references/lossless-upgrade.md](references/lossless-upgrade.md)).

## Redis

```rust
let client = redis::Client::open(url)?; // `rediss://`: see the skill (your TLS feature and provider)
let conn = client.get_connection_manager().await?;
Ok(Arc::new(RedisKvStore::new(conn).with_prefix("shop:wa:")))
```

Only with persistence (AOF or RDB): a Redis that forgets on restart
forgets every merchant's token. Only with `maxmemory-policy noeviction`,
on an instance of its own: every key with a TTL enforces a limit (OTP
issue logs and cooldowns, dedup markers, sessions), and a `volatile-*`
policy evicts them silently. Size `maxmemory` for 7 days of dedup markers.
No `ConversationStore` on Redis. `redis` here is meta-whatsapp-rs's re-export
(`meta_whatsapp_rs::adapters::store::redis`, feature `redis`), so the connection types
always match `RedisKvStore::new` — no redis dependency of your own. For
`rediss://`, add your own `redis` with `tokio-rustls-comp` at the same
version, install a rustls crypto provider once at startup, and hand the
connection to `RedisKvStore::new`.

~~`noeviction` or `volatile-*`~~: wrong until 1a3cfc6 (2026-09-24);
`volatile-*` evicts OTP limits and dedup markers.

## Your own adapter

Implement `KvStore` (`get`, `put`, `put_if_absent`, `compare_and_swap`,
`delete`) or `ConversationStore`, then prove it with the executable
contracts in `meta_whatsapp_rs::adapters::store`:

```rust
let clock = std::sync::Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
let store = MemoryKvStore::with_clock(clock.clone());
conformance::run(&store, &|d| clock.advance(d)).await; // panics on a violation
```

A store that expires on a server's clock runs
`conformance::run_with_real_time(&store, tick)`; a conversation store
runs `conversation_conformance::run(&store)`. `update_status` takes the
business `phone_number_id` first: a status only changes a message of the
number it arrived on. `append_synced` (a batch of coexistence history:
one round trip if you can) must not move `last_inbound_at` nor the unread
count; `fill_media_placeholder` rewrites only a row whose `kind` is
`StoredMessage::MEDIA_PLACEHOLDER` and whose status is not `Deleted`;
`revoke` matches number and direction and stores
`StoredMessage::tombstone` when the id is unknown, as history only (the
summary never sees it). The suite checks all three, and that content
(kind, text, payload strings and keys, status error, preview) reads back
exactly, U+0000 included. A `KvStore` keeps values as any bytes; a key
holding U+0000 may be refused, never stored as another key.
~~Six methods to implement~~: until 6d50701 and a9593f3 (2026-09-24),
which added `append_synced`, `fill_media_placeholder` and `revoke`.
~~A tombstone is part of the summary; a revoked placeholder may be
filled~~: until af5b1f8 (2026-09-25). ~~Content may lose U+0000~~:
until the pull request that made U+0000 lossless (PR #7, 2026-09-25).

## Pitfalls

- **Memory stores give each instance its own dedup, OTP limits and
  vault**: tests and single-instance demos only.
- **U+0000 on Postgres**: message content keeps it (above); ids,
  contacts, phone number ids and `StoreKey`s are `text` and refuse it
  (`StorageError::Backend`). Meta never assigns an id with one; keys of
  your own must not carry one.
- Postgres and Redis decide expiry by their own clock: keep hosts on NTP.
  `Expiry::After` past year 9999 means never.
- Keep the vault key and the OTP pepper out of the database that holds
  these rows.

## What meta-whatsapp-rs does not do

- No built-in Redis TLS
  ([open question 19](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#storage)),
  no Redis `ConversationStore`, no backups or retention policy for inbox
  history.
- Message ids are unique per store, not per business number
  ([open question 33](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#cms-inbox)).

## Related skills

`wa-rs-token-vault`, `wa-rs-otp-login`, `wa-rs-webhook-endpoint` (dedup),
`wa-rs-cms-inbox`, `wa-rs-production`, `wa-rs-testing`.
