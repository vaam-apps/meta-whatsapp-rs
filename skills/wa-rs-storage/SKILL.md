---
name: wa-rs-storage
description: "Choosing and running wa-rs storage - the KvStore (token vault, OTP challenges, webhook dedup, signup sessions) and ConversationStore (inbox history) ports, MemoryKvStore and MemoryConversationStore for tests, PostgresKvStore and PostgresConversationStore (migrate at startup, table prefixes, purge_expired, no U+0000), RedisKvStore (noeviction, persistence, prefixes, bring your own TLS connection), server clocks, and writing your own adapter that passes the conformance suites. Load when wiring databases for wa-rs, deploying Postgres or Redis for it, or implementing a custom KvStore or ConversationStore."
---

# wa-rs-storage

> **Verified against wa-rs 1e63b2ba9c94fb9a4f2895f0dc9efc27ee749274 (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/stores.rs](examples/stores.rs), compiled by
wa-rs's own gate; its tests run the conformance suites on the memory
stores.

## When to use

Before production: everything stateful in wa-rs sits on two ports, and
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
let pool = sqlx::PgPool::connect(database_url).await?; // sqlx as wa-rs re-exports it
postgres::migrate(&pool).await?; // idempotent, under a lock: any instance may run it
let kv = PostgresKvStore::new(pool.clone());
let purger = kv.clone();
tokio::spawn(async move {
    // One row per webhook event (dedup) and per OTP: purge what expired.
    let mut every = tokio::time::interval(Duration::from_secs(600));
    loop {
        every.tick().await;
        if let Err(e) = purger.purge_expired().await {
            tracing::warn!(error = %e, "purging expired wa-rs rows failed");
        }
    }
});
```

`sqlx` is `wa_rs::adapters::store::postgres::sqlx` (0.9): no pin of your
own. Migrations are embedded; their history table is `wa_sqlx_migrations`,
separate from yours. Another prefix (two deployments, one schema):
`TablePrefix::new("shop_wa_")?`, `postgres::migrate_with_prefix`,
`PostgresKvStore::with_prefix`.

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
No `ConversationStore` on Redis. `redis` here is wa-rs's re-export
(`wa_rs::adapters::store::redis`, feature `redis`), so the connection types
always match `RedisKvStore::new` — no redis dependency of your own. For
`rediss://`, add your own `redis` with `tokio-rustls-comp` at the same
version, install a rustls crypto provider once at startup, and hand the
connection to `RedisKvStore::new`.

~~`noeviction` or `volatile-*`~~: wrong until 1a3cfc6 (2026-09-24);
`volatile-*` evicts OTP limits and dedup markers.

## Your own adapter

Implement `KvStore` (`get`, `put`, `put_if_absent`, `compare_and_swap`,
`delete`) or `ConversationStore`, then prove it with the executable
contracts in `wa_rs::adapters::store`:

```rust
let clock = std::sync::Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
let store = MemoryKvStore::with_clock(clock.clone());
conformance::run(&store, &|d| clock.advance(d)).await; // panics on a violation
```

A store that expires on a server's clock runs
`conformance::run_with_real_time(&store, tick)`; a conversation store
runs `conversation_conformance::run(&store)`. `update_status` takes the
business `phone_number_id` first: a status only changes a message of the
number it arrived on. `append_synced` (coexistence history) must not move
`last_inbound_at` nor the unread count, and `fill_media_placeholder`
rewrites only a row whose `kind` is `StoredMessage::MEDIA_PLACEHOLDER`:
the suite checks both.
~~Six methods to implement~~: until the coexistence port change
(2026-09-24), which added `append_synced` and `fill_media_placeholder`.

## Pitfalls

- **Memory stores give each instance its own dedup, OTP limits and
  vault**: tests and single-instance demos only.
- **Postgres cannot store U+0000** in `text`/`jsonb`: the stores refuse it
  (`StorageError::Backend`). The inbox stores it as U+FFFD in message
  content (provisional,
  [open question 18](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#storage));
  anything else you hand the store must be clean.
- Postgres and Redis decide expiry by their own clock: keep hosts on NTP.
  `Expiry::After` past year 9999 means never.
- Keep the vault key and the OTP pepper out of the database that holds
  these rows.

## What wa-rs does not do

- No built-in Redis TLS
  ([open question 19](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#storage)),
  no Redis `ConversationStore`, no backups or retention policy for inbox
  history.
- Message ids are unique per store, not per business number
  ([open question 33](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#cms-inbox)).

## Related skills

`wa-rs-token-vault`, `wa-rs-otp-login`, `wa-rs-webhook-endpoint` (dedup),
`wa-rs-cms-inbox`, `wa-rs-production`, `wa-rs-testing`.
