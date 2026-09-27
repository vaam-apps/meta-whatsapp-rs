---
name: meta-whatsapp-rs-storage
description: "Choosing and running meta-whatsapp-rs storage - the KvStore (token vault, OTP challenges, webhook dedup, signup sessions) and ConversationStore (inbox history) ports, MemoryKvStore and MemoryConversationStore for tests, PostgresKvStore and PostgresConversationStore (migrate at startup, table prefixes, purge_expired, U+0000 kept in message content and refused in ids and keys, the lossless-content upgrade, inbox retention and erasure with with_retention, apply_retention, identities and erase_all), RedisKvStore (noeviction, persistence, prefixes, bring your own TLS connection), server clocks, and writing your own adapter that passes the conformance suites. Load when wiring databases for meta-whatsapp-rs, deploying Postgres or Redis for it, or implementing a custom KvStore or ConversationStore."
---

# meta-whatsapp-rs-storage

> **Verified against meta-whatsapp-rs b7438e26d691338af798e102e24637744aabf31b (2026-09-27).** On another revision, trust the code over this page.

Reference code: [examples/stores.rs](examples/stores.rs), compiled by
meta-whatsapp-rs's own gate; its tests run the conformance suites on the memory
stores.

## When to use

Before production: everything stateful in meta-whatsapp-rs sits on two ports, and the typed
stores (vault, OTP, dedup, sessions, bot cooldowns) are built on `KvStore`, so one adapter serves them all.

| Data | Port | Namespace | Lose it and |
| --- | --- | --- | --- |
| merchants' tokens (`TokenVault`) | `KvStore` | `wa.token` | every merchant reconnects |
| signup attempts (`SignupSessions`) | `KvStore` | `wa.es.session` | attempts in flight fail |
| OTP challenges and limits (`OtpService`) | `KvStore` | `wa.otp`, `wa.otp.rate` | codes fail; limits reset |
| webhook dedup (`DedupGuard`) | `KvStore` | `wa.webhook.dedup` | Meta's retries delivered again |
| bot cooldowns (`KvCooldowns`) | `KvStore` | `wa.bot.cooldown` | running cooldowns reset |
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

`sqlx` is `meta_whatsapp_rs::adapters::store::postgres::sqlx` (0.9): no pin of your own. Migrations
are embedded; their history table is `wa_sqlx_migrations`, separate from yours. Another prefix (two
deployments, one schema): `TablePrefix::new("shop_wa_")?`, `postgres::migrate_with_prefix`,
`PostgresKvStore::with_prefix`.

Message content keeps U+0000: `wa_messages.kind_utf8`, `text_utf8` and
`wa_conversations.last_text_utf8` are `BYTEA` (UTF-8), `payload_json` and `error_json` are `json`.
Your own SQL decodes the `*_utf8` columns as UTF-8 and searches the bytes (a recipe is in the
`postgres` module docs); it never indexes, extracts or compares payload fields (`->`, `->>`, a cast
to `jsonb` fail on a document holding a NUL, so an index on one fails the insert; `json` has no
`=`). Upgrading a database written before that is one-way: back up, stop the older writers, drop
your own objects on those columns, run `migrate` once from a job, in that order
([references/lossless-upgrade.md](references/lossless-upgrade.md)).

Inbox history is kept unless you set a retention (`with_retention(Retention::days(90))` on the
conversation store) and schedule `apply_retention(now)` (from any replica: purges take turns).
Migration 4 adds four tables, a `wa_messages.sender` column it back-fills, and indexes (reads and
writes of `wa_messages` wait meanwhile: migrate a large inbox from a one-off job with a `lock_timeout`).
A row it cannot back-fill, or that an older instance writes, gets its sender from the next erasure.

Erasing a customer is a procedure (`meta-whatsapp-rs-cms-inbox`): (1) collect every identity
(`identities` on each of the merchant's numbers, and yours); (2) `erase_all(number, &ids)` on each,
behind your ownership check (it deletes their records, contacts and links, the appends in flight
included, and redacts their group messages, which keep their ids, a `wamid` encoding the phone number,
or deletes them with `with_erasure_mode`); (3) delete your media copies, outbox rows and dead letters;
(4) delete Meta's contact-book entry, BSUIDs only (`UserId::is_bsuid`, `delete_contact_book_entry`);
(5) journal it (an HMAC of `phone_number_id|contact`, and the time) and replay the journal after any
restore; (6) erase again after Meta's 7-day redelivery window, keeping the dedup markers meanwhile.
Deleted rows live on until VACUUM, in the WAL, replicas and backups; set `log_parameter_max_length` to
0 for the application's role, or Postgres statement logs keep message text.

## Redis

```rust
let client = redis::Client::open(url)?; // `rediss://`: see the skill (your TLS feature and provider)
let conn = client.get_connection_manager().await?;
Ok(Arc::new(RedisKvStore::new(conn).with_prefix("shop:wa:")))
```

Only with persistence (AOF or RDB): a Redis that forgets on restart forgets every merchant's token.
Only with `maxmemory-policy noeviction`, on an instance of its own: every key with a TTL enforces a
limit (OTP issue logs and cooldowns, dedup markers, sessions), and a `volatile-*` policy evicts them
silently. Size `maxmemory` for 7 days of dedup markers. No `ConversationStore` on Redis. `redis`
here is meta-whatsapp-rs's re-export (`meta_whatsapp_rs::adapters::store::redis`, feature `redis`),
so the connection types always match `RedisKvStore::new` — no redis dependency of your own. For
`rediss://`, add your own `redis` with `tokio-rustls-comp` at the same version, install a rustls
crypto provider once at startup, and hand the connection to `RedisKvStore::new`.

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

A store that expires on a server's clock runs `conformance::run_with_real_time(&store, tick)`; a
conversation store runs `conversation_conformance::run(&store)`. `update_status` takes the business
`phone_number_id` first: a status only changes a message of the number it arrived on.
`append_synced` (a batch of coexistence history: one round trip if you can) must not move
`last_inbound_at` nor the unread count; `fill_media_placeholder` rewrites only a row whose `kind` is
`StoredMessage::MEDIA_PLACEHOLDER` and whose status is not `Deleted`; `revoke` matches number and
direction and stores `StoredMessage::tombstone` when the id is unknown, as history only (the summary
never sees it). Since roadmap L5: `message` (scoped to the number), window events, thread ownership
and synced contacts (the latest record wins; a removal is kept, key and time only, and refuses an
older sync), identity links, `identities`, `erase_all` (one step; group messages per `erasure_mode`;
`erase` is provided) and `purge_before`. The suite checks all of it, and that content (kind, text,
payload strings and keys, status error, preview) reads back exactly, U+0000 included. A `KvStore`
keeps values as any bytes; a key holding U+0000 may be refused, never stored as another key. ~~Six
methods to implement~~: until 6d50701 and a9593f3 (2026-09-24), which added `append_synced`,
`fill_media_placeholder` and `revoke`. ~~A tombstone is part of the summary; a revoked placeholder
may be filled~~: until af5b1f8 (2026-09-25). ~~Content may lose U+0000~~: until the pull request
that made U+0000 lossless (PR #7, 2026-09-25).

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

- No built-in Redis TLS yet
  ([OPEN_QUESTIONS.md #19](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#storage),
  decided on 2026-09-26: `rediss://` with an explicit provider, roadmap L21c), no Redis
  `ConversationStore`, no backups of inbox history.
- Message ids are unique per store, not per business number
  ([open question 33](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#cms-inbox)).

## Related skills

`meta-whatsapp-rs-token-vault`, `meta-whatsapp-rs-otp-login`, `meta-whatsapp-rs-webhook-endpoint` (dedup),
`meta-whatsapp-rs-cms-inbox`, `meta-whatsapp-rs-production`, `meta-whatsapp-rs-testing`.
