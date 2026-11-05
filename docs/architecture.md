# meta-whatsapp-rs architecture

This is the spec. Code that disagrees with it is a bug in one of the two;
fix whichever is wrong, in the same PR. For task-oriented integration
walkthroughs (Meta setup, onboarding, inbox, OTP, production), see the
[integrator guides](guides/README.md).

## Goals

A Rust toolkit for Meta's WhatsApp Business Platform that serves three
concrete products:

1. **E-commerce marketing** — templates (create, get approved, send), the
   Marketing Messages API, In-App Signup opt-ins, catalogs and product
   messages, delivery/read analytics, respecting opt-outs (`131050`) and
   per-user marketing limits (`131049`).
2. **CMS in-app chat** — each merchant onboards *their own* WhatsApp number
   through **Embedded Signup**; their customers' messages arrive by
   webhook, are stored per conversation, streamed live to the merchant's
   inbox UI, and answered with the merchant's business token, inside the
   24-hour customer service window (templates outside it).
3. **Authentication** — OTP over authentication templates (copy code,
   one-tap, zero-tap), issued, stored hashed, verified with attempt limits.

## Layout

```
crates/
  meta-whatsapp-core       error tree, ids, config, ports (traits), open_enum!. No I/O, no runtime.
  meta-whatsapp-client     Graph API client; one module per endpoint family.
  meta-whatsapp-webhooks   verify, parse, normalize, dedup, dispatch; axum router (feature).
  meta-whatsapp-adapters   port implementations: reqwest, memory/Postgres/SQLite/Redis stores, sinks.
  meta-whatsapp-typst      Typst → PDF/PNG for document and image messages.
  meta-whatsapp-bot        bot framework over webhooks: commands, guards, cooldowns, middleware,
                           compile-time plugins, Markdown → WhatsApp formatting, paced broadcasts.
  meta-whatsapp-rs         facade: re-exports, prelude, `client(token)`, the CMS inbox,
                           feature flags, runnable examples. What integrators depend on.
  meta-whatsapp-server-core  the service's framework-free core: its domain, authorization,
                           error model as data, and the ports its backends implement.
  meta-whatsapp-server-store-memory    the service's memory backend (development).
  meta-whatsapp-server-store-postgres  the service's Postgres backend and its migrations.
  meta-whatsapp-server-store-mongodb   a spike: the service's event outbox on a MongoDB
                           replica set, composed into no binary (roadmap S9).
  meta-whatsapp-server-http  the service's HTTP layer: listeners, authentication before
                           the body, the error renderer, Meta's webhook, media, operations.
  meta-whatsapp-server-api-axum  the service's /v1 routes on axum and their OpenAPI document.
  meta-whatsapp-server-api-cratestack  the service's API on CrateStack (a .cstack schema of
                           procedures): one procedure since roadmap S10, served by nothing yet.
  meta-whatsapp-server     the HTTP service (a binary composing the crates above by its Cargo
                           features, on the facade and the core; the MongoDB spike aside):
                           tenants, keys, the /v1 API for apps not written in Rust. See
                           "Service" below.
.xtask                     repo automation (`cargo xtask meta-docs`): a workspace of its own,
                           with its own Cargo.lock, excluded from the root one, so its ureq
                           (rustls with `ring`) never enters a library or test build.
tools/docs-mcp             the docs MCP server (`just docs-mcp`): the docs and skills over MCP's
                           stdio transport. A binary on serde_json alone, a member of the
                           root workspace but not a default one; nothing depends on it.
```

Dependency rule: every library crate depends on `meta-whatsapp-core`; no
library crate depends on `meta-whatsapp-rs`; binaries may, and the
[service](#service-meta-whatsapp-server)'s crates do: they depend on
`meta-whatsapp-rs` and on each other, never on another crate of the
library, reaching axum and sqlx through the facade's re-exports, as an
outside integrator would; no library crate depends on a service crate;
`meta-whatsapp-client` and `meta-whatsapp-webhooks` never depend on each
other or on `meta-whatsapp-adapters` (except as a dev-dependency for
tests); `meta-whatsapp-bot` sits above both and depends on core, client
and webhooks only, never on the adapters (dev-dependency aside) or the
facade, which re-exports it behind its `bot` feature. An adapter never
leaks its library's types through a port.

## Ports (`meta-whatsapp-core`)

| Port | Methods | Contract lives in |
| --- | --- | --- |
| `transport::HttpTransport` | `send`, `send_streaming` | rustdoc; non-2xx is *not* an error at this layer |
| `store::KvStore` | `get`, `put`, `put_if_absent`, `compare_and_swap`, `delete` | `meta_whatsapp_adapters::store::conformance` (executable) |
| `store::ConversationStore` | `append`, `append_synced` (a batch of coexistence history: no window, never unread), `fill_media_placeholder`, `revoke` (number and direction scoped, never the conversation; the content kept; a tombstone, history only, when the message is not stored yet), `update_status` (scoped: `phone_number_id, id, status, at, error`), `messages`, `conversations`, `mark_read`, `last_inbound_at`; `message` (the lookup, scoped: `phone_number_id, id`, never another number's); `record_window_event`, `window_events` (a call or a standby message that reopens the window: never history, never the summary); `set_thread_owner`, `thread_owner` (Conversation Routing: the latest record wins); `put_contact`, `remove_contact`, `contact`, `contacts` (the coexistence address book; a removal is kept, key and time only, so an older sync arriving after it is refused); `link_identity`, `identity_links` (two identities of one person on one number, as Meta reports a BSUID or number change); `identities` (a person's keys on one number: the closure over the synced contacts and the links, read only); `erase_all` (a person on one number, deleted: every record under their keys, the contacts and links naming them, and their messages in groups redacted or deleted per the store's `ErasureMode`); `purge_before` (by age, one number or all); `trim_conversations` (a cap on each conversation's history: its newest rows kept, every row counting, the summary deleted when its latest message goes; one number or all); `erase` (one key), `erasure_mode`, `retention`, `apply_retention` (provided: redact group messages and keep everything unless the adapter is configured) | `meta_whatsapp_adapters::store::conversation_conformance` (executable) |
| `sink::EventSink<E>` | `deliver` | rustdoc |
| `clock::Clock` | `now` | — |
| `media::MediaConverter` | `convert` (a `MediaInput` into a `TargetFormat`: `OggOpus`, a voice message's mono Ogg Opus, uploaded as `audio/ogg`; `StaticSticker`, a static WebP sticker, and `AnimatedSticker`, an animated one, both `image/webp`; `Jpeg` and `Png`, 8-bit images, `image/jpeg` and `image/png`; `TargetFormat::max_bytes` is core's own `media::AUDIO_MAX_BYTES`, `media::STATIC_STICKER_MAX_BYTES` (100 000), `media::ANIMATED_STICKER_MAX_BYTES` (500 000) and `media::IMAGE_MAX_BYTES` (5 000 000: Meta's figures read as decimal), kept within the client's upload table by `every_target_format_is_what_the_upload_check_takes`) | rustdoc; `meta-whatsapp-client` re-exports it from `client::media` (`Media::upload_converted`) |

Typed stores are built **on `KvStore`**, never as new ports: token vault,
OTP challenges, webhook dedup, Embedded Signup sessions, bot cooldowns,
the bot's durable jobs. An adapter author implements five methods once
and every feature works.

What the inbox keeps about a conversation beside its messages (window
events, thread ownership, the coexistence address book, the links
between a person's identities) is on `ConversationStore` itself, not on
`KvStore`: it is keyed like the history, and an erasure must reach it in
the same step. Its personal data is the records keyed by a contact; a
person also appears in group conversations (a message's `sender`: the
BSUID, else the phone number, of an inbound message, a column on
Postgres) and in other conversations' payloads (a quoted message, a
shared contact card), which no erasure reaches. A person has several
keys on one number (a history thread under their phone number, live
messages under their BSUID, an earlier BSUID), so erasure is by person
(design D30): `identities(key)` collects the keys the store connects,
and `erase_all(phone_number_id, keys)` deletes, in one step, every
record under them, the synced contacts and identity links naming them,
and their group messages, redacted in place (the default) or deleted
(`ErasureMode`, a setting per store, design D31); `erase(key)` is its
one-key case. There is no erasure across numbers: a `wa_id` is the same
on every number, so one would delete other businesses' customers. What
it does not reach, and what an integrator does about it, is in
[cms-inbox.md § 8](guides/cms-inbox.md#8-erasing-a-customer-and-retention)
and [production.md § 8](guides/production.md#8-retention-and-erasure-on-postgres):
quotes and contact cards in other people's messages, the ids of redacted
group messages, identities nothing connects, the webhook dedup markers
(kept on purpose: they stop Meta's redeliveries from recording the data
again), the service's outbox and idempotency answers, the webhook dead
letters (their own store, with their own index and `erase`: the facade's
`erasure::Eraser` erases a person from both, roadmap L21a), integrators'
copies, dead Postgres rows, WAL, replicas and backups, logs, and Meta's
side; and records created again after the erasure. Retention is set per
store (design D10): the adapters take a `Retention` (`with_retention`,
keep by default) that `apply_retention(now)` applies; nothing purges on
its own, and a caller with another policy calls `purge_before` with its
own cutoff. A cap on each conversation's history is `trim_conversations`;
the bot framework schedules both (`meta_whatsapp_bot::retention`,
`RetentionPolicy` and `RetentionSweeper`, roadmap B4).

## Stable identifiers

These never change, whatever the project is called: each is encrypted
into, hashed into, or names data already stored (or, for a lock, is what
replicas of two releases must agree on while a deploy rolls), so changing
one strands that data (or makes `migrate` refuse the database) on the
next upgrade. The library's predate the rename from wa-rs to
meta-whatsapp-rs and keep the old name; the service's
(`meta-whatsapp-server`) postdate it and carry the new one, and are just
as fixed. A test pins each one's exact bytes; a change that fails it is a
data migration, not a rename. The pins spell the `w` of their literals
`\x77`, or pin opaque bytes (captured records, digests, known answers
computed apart from the code), so that a search-and-replace cannot
rewrite them along with the code.

| Identifier | Defined in | What it is | Pinned by |
| --- | --- | --- | --- |
| `wa-rs/token-vault/v1` | `AAD_TAG`, `meta-whatsapp-client/src/embedded_signup/vault.rs` | associated data of every sealed token | `a_record_written_before_solution_partner_mode_still_opens` (bytes written by b805dac) |
| `wa-rs/token-vault/ledger/v1` | `LEDGER_AAD_TAG`, same file | associated data of every credit ledger record | `ledger_records_written_before_the_rename_still_open` (bytes written by 16b61db) |
| `wa.token` | `TOKEN_NAMESPACE`, same file | store namespace of tokens, the phone index and the credit ledger; also in their associated data | both tests above |
| `wa.otp.key`, `wa.otp.code` | `KEY_DOMAIN`, `CODE_DOMAIN`, `meta-whatsapp-client/src/authentication/otp.rs` | HMAC domains of the challenge's store key and of the code hash | `the_derivations_and_namespaces_are_pinned` (known answers), `the_key_derivation_is_pinned`, `the_code_hash_is_pinned_and_covers_the_key` |
| `wa.otp`, `wa.otp.rate` | `NAMESPACE`, `ISSUE_LOG_NAMESPACE`, same file | store namespaces of the challenges and of the issue limit | `the_derivations_and_namespaces_are_pinned` |
| `wa.es.session` | `SESSION_NAMESPACE`, `meta-whatsapp-client/src/embedded_signup/session.rs` | store namespace of signup sessions in flight | `the_session_namespace_is_pinned` |
| `wa.es.coexistence` (added by roadmap L11d; the namespace and the record layout **pending the owner's confirmation**, roadmap § Owner touchpoints) | `COEXISTENCE_NAMESPACE`, `meta-whatsapp-client/src/embedded_signup/coexistence.rs` | store namespace of the coexistence sync records (key: the phone number id; value: plain JSON, `"v":1`, the WABA id, the stored token record's `created_at` in Unix seconds, and per request, `contacts` and `history`, its `state`, `pending`, `requested` with Meta's `request_id` or `refused` with its code, and `at`). **Not ephemeral**: changing it forgets which requests an onboarding made, and Meta allows each once per number, so a `resume` would post again one that was accepted, or whose answer was lost | `the_sync_record_is_pinned` |
| `wa.webhook.dedup` | `DEDUP_NAMESPACE`, `meta-whatsapp-webhooks/src/dedup.rs` | store namespace of the dedup markers (key: SHA-256 hex of the event's dedup key) | `the_marker_key_is_pinned` |
| `wa_` | `TablePrefix::DEFAULT`, `meta-whatsapp-adapters/src/store/table_prefix.rs` (in `store/postgres/mod.rs` until roadmap L8; the SQLite adapters share it) | default table prefix: `wa_kv`, `wa_messages`, `wa_conversations`, `wa_window_events`, `wa_thread_owners`, `wa_synced_contacts`, `wa_identity_links`, `wa_sqlx_migrations` | `the_default_tables_and_migration_checksums_are_pinned`, `prefix_validation`, the `live_postgres_*` table-name tests |
| `customer_call`, `call_accepted`, `standby_message`; `this_app`, `another_app`, `idle`; `revoked`, `erased` | `WindowEventKind::as_str`, `ThreadOwner` (its serde names), `StoredMessage::REVOKED`, `StoredMessage::ERASED`, `meta-whatsapp-core/src/store/conversation.rs` | the names a conversation store keeps for a window event's kind, a thread's owner and this crate's own message kinds (`wa_window_events.kind`, `wa_thread_owners.owner`, `wa_messages.kind_utf8`) | `stored_names_are_pinned` |
| the number lock `('wa_messages'::regclass::oid::int4, hashtext(<phone_number_id>))`, the purge lock `('wa_conversations'::regclass::oid::int4, 0)` | `number_lock`, `purge_lock`, `meta-whatsapp-adapters/src/store/postgres/conversation.rs` | Postgres advisory locks (the two-key form, the table's object id as class) that order `erase`, `purge_before` and `trim_conversations` (under the purge lock, like a purge) against appends and each other: replicas of two releases must take the same ones | `the_locks_are_pinned`, `live_postgres_the_locks_are_the_documented_ones` |
| the migration files | `meta-whatsapp-adapters/migrations/*.sql` | sqlx records each file's checksum; an edit, a comment included, makes `migrate` refuse every database migrated before. So they keep naming `wa_adapters`, including in the hint migration 3 raises, and the columns and indexes they create keep their names (migration 4's `wa_messages.sender`, `wa_messages_sender_idx`, `wa_messages_unsent_idx`, `wa_identity_links_idx`, among others) | `the_default_tables_and_migration_checksums_are_pinned` |
| a message's sender: the payload's `from_user_id`, else its `from`, of an inbound message (an empty value, or one holding U+0000, is none) | `StoredMessage::sender`, `meta-whatsapp-core/src/store/conversation.rs`; migration 4's back-fill | what a store keeps beside each message (`wa_messages.sender`) and an erasure matches a person's group messages on: a changed rule leaves the rows written before out of every erasure | `the_sender_is_the_bsuid_else_the_phone_number_of_an_inbound_message`, `live_postgres_migration_4_backfills_senders_by_the_rust_rule` |
| `wa:` | `RedisKvStore::new`, `meta-whatsapp-adapters/src/store/redis_kv.rs` | default Redis key prefix, before `{<len>:<namespace>}:<key>` | `the_default_prefix_and_key_layout_are_pinned` |
| the SQLite migration files and tables (added by roadmap L8; **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `MIGRATIONS`, `meta-whatsapp-adapters/src/store/sqlite/mod.rs`; `meta-whatsapp-adapters/migrations-sqlite/*.sql` | the SQLite adapters' schema under the default prefix: `wa_kv`, `wa_kv_version` (the version counter), `wa_messages` (keyed by business number and id; `wa_messages_id_idx`, unique, keeps an id once per store, `OPEN_QUESTIONS.md` #33), `wa_conversations`, `wa_window_events`, `wa_thread_owners`, `wa_synced_contacts`, `wa_identity_links`, `wa_sqlx_migrations`, their columns (`kind_utf8`, `text_utf8`, `payload_json`, `ts` in unix microseconds, …) and the stored names the Postgres adapter keeps (statuses, directions, owners, kinds); sqlx records each file's checksum, so an edit, a comment included, makes `migrate` refuse a database migrated before | `the_default_tables_and_migration_checksums_are_pinned` (`store::sqlite`) |
| `{conversations:<prefix>}` and the Redis conversation layout (added by roadmap L8; **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `RedisConversationStore::base`, `key`, `ts16`, `PRELUDE`, `meta-whatsapp-adapters/src/store/redis_conversation.rs` | the hash tag after the `wa:` prefix of every key of a conversation store (naming the prefix: one slot per store), the keys under it (`:m:`, `:id:`, `:h:`, `:s:`, `:l:`, … with length-prefixed parts), the fields of each record (`c`, `d`, `k`, `t`, `p`, `s`, `ts`, …) and the time encoding (16 hex digits of the unix microseconds, sign bit flipped) | `the_conversation_layout_is_pinned`, `each_prefix_has_its_own_hash_tag`, `live_redis_conversation_layout_and_erasure_leaves_nothing` |
| `meta-whatsapp-server/outbox-key/v1` | `outbox_key`, `meta-whatsapp-server-core/src/events.rs` | domain of the SHA-256 stored as `wa_server_events.dedup_key` (with the tags `library` and `delivery`, the NUL separators and a keyless event's position as 8 big-endian bytes): derived otherwise, a redelivery across the upgrade is recorded twice | `the_outbox_key_is_pinned` (known answers) |
| `meta-whatsapp-server/event-id/v1`, `evt_` + 32 hex digits | `EventIdKey`, same file | HMAC label of the key event ids are derived with under the app secret, and the id's shape: stored as `wa_server_events.id` and what receivers deduplicate on | `event_ids_are_pinned` (known answers) |
| `meta-whatsapp-server/number-event/v1`; the types `number_connected`, `number_disconnected`, `number_reconnect_required` and the `reason` values `tenant_disconnected`, `operator_unbound`, `partner_app_uninstalled`, `account_deleted` (added by roadmap M2d; **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints, design D80, D83) | `NumberEvent::outbox_key`, `NumberEvent::kind`, `DisconnectReason::as_str`, `meta-whatsapp-server-core/src/number_events.rs` | domain of the SHA-256 stored as a number event's `wa_server_events.dedup_key` (then its type, number, WABA, tenant, binding start in Unix nanoseconds and generation, each NUL-separated), from which its id is derived; the types stored as `wa_server_events.event_type` and served as `KnownEventType`s; the reasons stored in `data`: derived otherwise, an event of the same binding is recorded twice | `the_number_event_key_is_pinned` (a known answer), `each_number_events_data_is_pinned` |
| `meta-whatsapp-server/migrate`, `meta-whatsapp-server/housekeeping` | `MIGRATION_LOCK`, `HOUSEKEEPING_LOCK`, `lock_key`, `meta-whatsapp-server-store-postgres/src/postgres.rs` (`meta-whatsapp-server/src/store/postgres.rs` until roadmap S7) | Postgres advisory lock keys (the first 8 bytes of their SHA-256; `lock_key` derives the leader lock's the same way, so its `housekeeping` turn is `HOUSEKEEPING_LOCK`): replicas of two releases take the same ones | `the_lock_key_is_derived_as_documented`, `the_leader_locks_keys_are_the_services_locks` |
| the service's migration files, `wa_server_sqlx_migrations` | `meta-whatsapp-server-store-postgres/migrations/*.sql` (moved byte for byte from `meta-whatsapp-server/migrations/` in roadmap S7: sqlx knows a migration by its version and checksum, never its path), `MIGRATIONS_TABLE` | the service's `wa_server_*` tables (the operator-only event stream `''` included) and its migration history; an edited file makes `migrate` refuse every database migrated before | `the_migrations_and_their_checksums_are_pinned`, `live_postgres_a_database_mains_binary_migrated_starts_this_one_with_no_migration` |
| `wa_server_wabas.generation`, `wa_server_wabas.previous_tenant_unbound_at` and its default `1970-01-01 00:00:00+00` (`UNRECORDED_MOVE`), `wa_server_waba_tombstones` (`waba_id`, `tenant_id` with `ON DELETE SET NULL`, `unbound_at`, `previous_tenant_unbound_at`), migration 4's description `waba moves` (added by roadmap S2b; the names, the default's meaning and the tombstone's rules **pending the owner's confirmation**, roadmap § Owner touchpoints) | `meta-whatsapp-server-store-postgres/migrations/0004_waba_moves.sql`, `WABA_COLUMNS` and `unbind_sql`, `meta-whatsapp-server-store-postgres/src/postgres.rs`; `WabaBinding`, `WabaTombstone`, `UNRECORDED_MOVE`, `meta-whatsapp-server-core/src/model.rs` | a binding's attach count (what a capability's conditioned writes compare beside `attached_at`) and when its WABA was last unbound from another tenant (`NULL`: never; the epoch: a binding made before migration 4, its history unknown), and each WABA's last unbinding, which a new binding reads: what decides whether an event is routed by the rules after a move (design D36). Renamed, the rows written before lose their history (every move forgotten, so the window after it fails open); the epoch default read otherwise would give `main`'s bindings the first-onboarding rules | `the_migrations_and_their_checksums_are_pinned` (checksum and description), `live_postgres_a_database_mains_binary_migrated_starts_this_one_with_no_migration` (the default's semantics on `main`'s rows), `records::a_move_is_recorded`, `records::a_refresh_moves_the_generation` (both backends) |
| `wa_server_numbers.attached_at` (no default for the rows before it, read as their WABA's `attached_at`; `now()` for the rows inserted after), `wa_server_numbers.previous_tenant_released_at` and its default `1970-01-01 00:00:00+00` (`UNRECORDED_MOVE`), `wa_server_number_tombstones` (`phone_number_id`, `tenant_id` with `ON DELETE SET NULL`, `released_at`, `previous_tenant_released_at`), the trigger `wa_server_numbers_released` on `wa_server_numbers` and its function `wa_server_number_released()` (every deleted number row leaves its tombstone, a previous release's replica's deletions included), migration 5's description `number moves` (added by roadmap M2h, folded into M2a; the names, the defaults' meaning and the tombstone's rules **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `meta-whatsapp-server-store-postgres/migrations/0005_number_moves.sql`, `NUMBER_COLUMNS` and `unbind_sql`, `meta-whatsapp-server-store-postgres/src/postgres.rs`; `NumberBinding`, `NumberTombstone`, `UNRECORDED_MOVE`, `meta-whatsapp-server-core/src/model.rs` | when a number came under its tenant and when it was last released by another tenant (`NULL`: never; the epoch: a number bound before migration 5, its history unknown, read as a move), and each number's last release, which a new binding of it reads: what the inbox's binding epoch cuts a number's history at (`authz::history_from`, design D10). Renamed, the rows written before lose their history (every number's move forgotten: a migrated number's previous holder's history shown to its new tenant); the epoch default read otherwise would show `main`'s numbers everything | `the_migrations_and_their_checksums_are_pinned` (checksum and description), `live_postgres_a_database_mains_binary_migrated_starts_this_one_with_no_migration` and `live_postgres_migration_5_rewrites_no_table` (the defaults' semantics on rows written before), `live_postgres_a_release_by_the_previous_release_is_recorded` (the trigger), `records::a_numbers_moves_are_recorded`, `races::a_number_taken_while_released_records_the_move` (both backends) |
| `wa_server_events.media_id` (no default: `NULL` for the rows before it and for those a previous release inserts), its partial index `wa_server_events_media_idx` (`(media_id, created_at) WHERE media_id IS NOT NULL`), migration 7's description `received media` (added by roadmap M2e; the names and the column's rule **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints, design D77) | `meta-whatsapp-server-store-postgres/migrations/0007_received_media.sql`, `PgEventStore::insert` and `received_media`, `meta-whatsapp-server-store-postgres/src/events.rs`; `NewEvent::received_media`, `received_media_id`, `meta-whatsapp-server-core/src/outbox.rs` | the media id an outbox row's `message_received` event records as received (the five media types' `{type}.id`, an edit's), which the media routes read: one recorded on the caller's own number and tenant is asked from Meta without `phone_number_id`, one recorded on any other row is `404` with nothing asked. Renamed, the rows written before lose it (their media keep Meta's check, for the outbox's retention: no file leaks, received files may answer `404`); a rule that read another field would exempt ids the customer did not send | `the_migrations_and_their_checksums_are_pinned` (checksum and description), `live_postgres_migration_7_rewrites_no_table_and_records_from_its_release_on`, `metas_media_messages_record_their_media_id_and_nothing_else_does`, `outbox::received_media` (both backends) |
| `wa_server_events.contacts` (`TEXT[]`, no default: `NULL` for the rows before it, for those a previous release inserts, and for an event naming nobody), its partial GIN index `wa_server_events_contacts_idx` (`USING gin (contacts) WHERE contacts IS NOT NULL`), migration 8's description `erasure`; the key scope `erasure`; the journal's `tracing` target `erasure_journal`, its message `erasure journal entry` and version 1 layout (`v`, `erasure_id`, `at`, `tenant`, `phone_number_id`, `identities`), its HMAC's input `<phone_number_id>\|<contact>` (hex HMAC-SHA256 under `WA_SERVER_ERASURE_KEY`), the erasure id's form `era_` and 32 hex digits, the audit action `customer_erased` (added by roadmap M2f; every name **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints, design D86–D91) | `meta-whatsapp-server-store-postgres/migrations/0008_erasure.sql`, `PgEventStore::insert`, `ERASE_SQL`, `meta-whatsapp-server-store-postgres/src/events.rs`; `NewEvent::contacts`, `event_contacts`, `CONTACT_KEYS`, `meta-whatsapp-server-core/src/outbox.rs`; `Scope::Erasure`, `meta-whatsapp-server-core/src/model.rs`; `JOURNAL_TARGET`, `JOURNAL_VERSION`, `ErasureKey::journal_hmac`, `ERASURE_ACTION`, `LogJournal`, `meta-whatsapp-server-core/src/erasure.rs` | the identifiers an outbox row's event names its customers by, which an erasure deletes the row by (renamed or read by another rule, the rows written before stop being found: they keep the person until the outbox's retention); the stored name of the scope the erasure route needs (`wa_server_api_keys.scopes`: renamed, every key minted with it fails to load); the journal an operator replays after a restore (renamed, a log pipeline routing it by target or message loses entries; another HMAC input or layout, a replay written for this one matches nothing: the erased come back); the id and the action an operator correlates entries and audit events by | `the_migrations_and_their_checksums_are_pinned` (checksum and description), `live_postgres_migration_8_rewrites_no_table_and_names_from_its_release_on`, `live_postgres_the_erasure_probes_the_contacts_index`, `every_identifier_an_event_holds_is_one_of_its_contacts`, `metas_examples_name_their_customers`, `the_journal_hmac_is_pinned` (a known answer), `the_journal_and_the_audit_event_hold_no_identity_in_clear` (the layout, the HMAC computed apart), `outbox::erase_contacts` (both backends) |
| a tenant's OTP settings: the key `otp` of `wa_server_tenants.settings` and its version 1 layout (`v`, `phone_number_id`, `template.name`, `template.language`, `code_length`, `ttl_seconds`, `max_attempts`, `resend_cooldown_seconds`, `issue_limit.max_issues`, `issue_limit.window_seconds`); the label `meta-whatsapp-server/otp-fingerprint/v1` (added by roadmap M3c); the shared issue log's store namespace `wa_server.otp.shared_rate`, its key's derivation and record (`{"issued_at": [<ms>, …]}`), and its label `meta-whatsapp-server/otp-shared-limit/v1` (added by M3c's review, design D59) (all **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `OtpSettings::to_stored`, `from_stored`, `meta-whatsapp-server-core/src/model.rs`, kept by `set_otp_settings` in both backends (`meta-whatsapp-server-store-postgres/src/postgres.rs`: `jsonb_set` on the existing column, no migration); `FINGERPRINT_LABEL`, `SHARED_LIMIT_LABEL`, `SHARED_ISSUE_LOG`, `OtpKeys::shared_key`, `meta-whatsapp-server-core/src/otp.rs` | which number and template send a tenant's codes, and its limits (renamed, every tenant's settings stop reading: a storage failure, not "no settings"); the HMAC label of the key an OTP issue's idempotency fingerprint is keyed with, under `WA_OTP_PEPPER` (changed, a repeat within the TTL of an issue made before is `422 idempotency_key_reused`, never a second code); the shared issue log, per sending number of the default OTP tenant and recipient (key: hex HMAC-SHA256 of the netstrings of the number's id and the recipient's digits; **ephemeral**: changed, every shared count starts again, one window's worth of codes more, nothing stranded). The codes themselves stay the library's `wa.otp` and `wa.otp.rate`, with the tenant's id as the namespace (`model.rs`'s `TenantId`: immutable for that reason) and `login` as the purpose of a request naming none (ephemeral: changing it strands only the codes in flight) | `the_stored_otp_settings_are_pinned`, `the_otp_fingerprint_is_keyed`, `the_shared_issue_logs_key_is_pinned_and_keyed` (known answers derived apart from the code), `live_postgres_m3_5_a_code_verifies_at_no_other_tenant_on_the_same_number` |
| `wa_server_signup_attempts` (`waba_id`, its primary key, referencing `wa_server_wabas` `ON DELETE CASCADE`; `tenant_id`; `failed_step`, one of the library's step names; `session`, the library's `SessionInfo` as JSON; `finish_kind`, a `FinishKind`'s event string; `created_at`, `updated_at`), `wa_server_vault_records` (`kind`, `waba` or `business`; `id`; `recorded_at`; primary key `(kind, id)`), migration 6 (`signup`) (added by roadmap M3a; every name **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `meta-whatsapp-server-store-postgres/migrations/0006_signup.sql`; `NewSignupAttempt`, `SignupAttempt`, `VaultEntry`, `meta-whatsapp-server-core/src/model.rs`; both backends' `save_signup_attempt`, `signup_attempt`, `signup_attempts`, `delete_signup_attempt`, `record_vault_entry`, `vault_entries` | the attempts an Embedded Signup tenant resumes (renamed, the kept attempts are lost: each merchant connects again from the start, nothing else is stranded); the records of the token vault kept past a binding (Solution Partner mode's credit ledgers, `credit/<WABA_ID>`, and revocation markers, `revoked/<BUSINESS_ID>`, of the library's `wa.token`) that a key rotation walks besides the bindings: renamed, a rotation no longer reaches them, and dropping the old key afterwards leaves them undecryptable (an offboarded merchant's line can no longer be revoked from the ledger). The library's `wa.es.session` (the attempts' states) and `wa.token` are unchanged | `the_migrations_and_their_checksums_are_pinned` (migration 6's checksum), `the_vault_entry_kinds_are_pinned`, `conformance::signup::the_records_keep_attempts_and_vault_entries` (memory, and live on Postgres) |
| `wa_server_events`, the channel; `<sequence> <tenant>`, its payload (added by roadmap M2b; both **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints, design D40) | `EVENTS_CHANNEL`, `payload`, `wakeup`, `meta-whatsapp-server-store-postgres/src/notify.rs` | the Postgres channel every replica of a deployment `NOTIFY`s when its webhook pipeline recorded a tenant's event, and `LISTEN`s on to wake its live streams; the payload is the event's sequence in its tenant's stream, a space, and the tenant's id. Not stored: what replicas of two releases must agree on while a deploy rolls (a replica on another channel, or reading another payload, wakes its streams by their re-poll only, every 15 s) | `the_payload_is_the_sequence_and_the_tenant`, `live_postgres_the_channel_is_the_deployments` |
| `payments`, a key scope (added by roadmap P2; the name **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `Scope::Payments`, `Scope::as_str`, `meta-whatsapp-server-core/src/model.rs` | the stored name of the scope a key needs for the payment routes, in `wa_server_api_keys.scopes` beside the older scopes' names (`send`, `numbers`, …), and the API's name for it (`Scope` in `openapi/v1.json`). Renamed, every key minted with it fails to load (`Scope::parse` refuses the stored name: the key is corrupt). The payment operations' ids (`create_payment_configuration`, `regenerate_payment_oauth_link`, `refund_payment`) are hashed into idempotency fingerprints, which expire with their records (`WA_SERVER_IDEMPOTENCY_TTL`): ephemeral | `each_operation_has_one_id`, `live_postgres_a_payments_key_reaches_its_wabas_configurations` |
| the MongoDB outbox's collections `wa_server_events`, `wa_server_event_streams` and `wa_server_wabas`, their fields (an event's `_id` its public id, `stream` its tenant's id or `""`, `sequence`, `tenant_id`, `dedup_key`, `dedup_until`, `phone_number_id`, `waba_id`, `event_type`, `data` (a string), `data_bytes`, `created_at`, `media_id` and `media_order` (an `ObjectId`, beside `media_id` only; both added on 2026-11-02 for roadmap M2e's `Outbox::received_media`), `contacts` (an array of strings, absent for a row naming nobody; added on 2026-11-02 for roadmap M2f's `Outbox::erase_contacts`); a stream's `_id`, `last_sequence`, `purged_through`; a WABA's `_id`, `tenant_id`, `attached_at`, `previous_tenant_unbound_at`, `numbers`, `insert_checks`) and its index names `wa_server_events_stream_sequence`, `wa_server_events_dedup_key` (partial, over string keys), `wa_server_events_created_idx`, `wa_server_events_media_idx` (partial, over string `media_id`s: `media_id`, `created_at` descending, `media_order` descending; added with `media_id`) and `wa_server_events_contacts_idx` (partial and multikey, over the rows holding `contacts`: `contacts`; added with `contacts`, the Postgres index's name) (added by roadmap S9, a spike composed into no binary; the names and the documents' shapes **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `meta-whatsapp-server-store-mongodb/src/schema.rs` | where a MongoDB backend keeps the event outbox and the bindings its inserts check; the names follow the Postgres tables and columns where one exists. Renamed once a database holds events, a stream's sequences restart and its events are lost to the service; `media_id` renamed, the rows written before lose it (their media keep Meta's `phone_number_id` check, as on Postgres); `contacts` renamed, the rows written before name nobody, and an erasure leaves them (they go by the outbox's retention, as on Postgres) | `the_stored_names_are_pinned`, `a_media_row_stores_its_media_id_and_no_other_row_does`, `a_row_naming_customers_stores_its_contacts_and_no_other_row_does`, `live_mongodb_migrate_makes_the_named_collections_and_indexes_once`, `live_mongodb_received_media_reads_its_index_in_order`, `live_mongodb_erase_contacts_reads_its_index_and_keeps_the_rest` |
| the MongoDB library adapters' collections `wa_kv`, `wa_messages`, `wa_conversations`, `wa_window_events`, `wa_thread_owners`, `wa_synced_contacts` and `wa_identity_links` (the Postgres tables' names), their fields (a key/value record's `_id` its `namespace` then its `key`, `value` (BSON binary; absent in a tombstone), `version`, `expires_at`, `purge_after`, `claim` (an `ObjectId`, only after a `put_if_absent` that wrote); a message's `_id` its id, `phone_number_id`, `contact`, `direction`, `kind`, `text`, `payload_json` and `error_json` (JSON text), `status`, `ts`, `status_at`, `sender`; a summary's `phone_number_id`, `contact`, `last_message_at`, `last_message_id`, `last_text`, `last_inbound_at`, `unread`, `appends`; a window event's `phone_number_id`, `id`, `contact`, `kind`, `ts`; an ownership record's `owner`, `role`, `app_id`, `since`; a synced contact's `full_name`, `first_name`, `phone_number`, `user_id`, `parent_user_id`, `username`, `synced_at`, `removed`; a link's `previous`, `current`, `ts`), its index names (`wa_kv_purge_idx`, a TTL index; `wa_messages_conversation_idx`, `wa_messages_ts_idx`, `wa_messages_sender_idx`, `wa_conversations_key_idx`, `wa_conversations_recent_idx`, `wa_conversations_last_idx`, `wa_window_events_key_idx`, `wa_window_events_conv_idx`, `wa_window_events_ts_idx`, `wa_thread_owners_key_idx`, `wa_synced_contacts_key_idx`, `wa_contact_removals_idx`, `wa_identity_links_key_idx`, `wa_identity_links_idx`) and a version's encoding (the larger of the stored version plus one and the server's milliseconds times 2^20, a dead document kept ten minutes) (added by roadmap S17; the names, the documents' shapes and the version's encoding **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `meta-whatsapp-adapters/src/store/mongodb/schema.rs`, `VERSION_SCALE` and `PURGE_GRACE` in `store/mongodb/kv.rs` | where `MongoKvStore` and `MongoConversationStore` keep their records; the KV namespaces stored under them are the library's own (`wa.token`, `wa.otp.*`, `wa.es.session`, `wa.webhook.dedup`, `wa.bot.cooldown`), unchanged. Renamed once a database holds records, they are stranded (tokens, OTP challenges, dedup markers, the inbox); the version's scale lowered, a key written again after its document's removal could be handed a version it had before | `the_stored_names_are_pinned`, `the_indexes_are_pinned`, `a_message_document_is_pinned_and_reads_back`, `the_other_records_are_pinned_and_read_back`, `a_record_is_keyed_by_its_namespace_then_its_key`, `a_write_is_one_pipeline_over_the_record_as_it_was`, `live_mongodb_migrate_is_idempotent_and_creates_the_pinned_indexes`, `live_mongodb_kv_layout_tombstones_and_the_ttl_index`, `live_mongodb_conversation_layout` |
| `wak_` | `PREFIX`, `KEY_ID_CHARS`, `SECRET_CHARS`, `meta-whatsapp-server-core/src/keys.rs` | API keys' prefix and layout (`wak_` + 17 + `_` + 43 base62 characters), held by every integrator | `the_key_layout_is_pinned` (literals); `a_minted_key_parses_and_matches_its_digest_only`, `malformed_keys_do_not_parse`, `base62_is_fixed_width_big_endian` |
| `wa.bot.cooldown`, `wa.bot.cooldown.notice` | `COOLDOWN_NAMESPACE`, `COOLDOWN_NOTICE_NAMESPACE`, `meta-whatsapp-bot/src/guard.rs` | store namespaces of the bot's command cooldowns and of their "please wait" markers (key: SHA-256 hex of the length-prefixed business number, command and user key; for a subcommand, the command part is its path, `order status`). **Ephemeral**: changing it forgets running cooldowns (they last one period at most) and may repeat one notice each; no data is stranded | `the_cooldown_key_is_pinned_and_hashed`, `a_subcommands_cooldown_key_is_over_its_path` |
| `wa.bot.pacer` (added by roadmap B2b; the namespace and the record layout **pending the owner's confirmation**, roadmap § Owner touchpoints) | `PACER_NAMESPACE`, `meta-whatsapp-bot/src/pacer/kv.rs` | store namespace of `KvRateLimiter`'s per-number schedules (key: SHA-256 hex of the length-prefixed phone number id, `<len>:<id>,`; value: the 49-byte version 1 record, its first byte the version). **Ephemeral**: changing it forgets each number's schedule, slow-down and raised rate (records expire 24 hours after their last slot by default), so while replicas of two releases run side by side they book from two budgets; no data is stranded | `the_pacer_key_is_pinned_and_hashed` (known answer), `the_pacer_record_is_pinned` |
| `wa.webhook.deadletter`, `wa.webhook.deadletter.contact` (added by roadmap L21a; the namespaces, the keys' derivations and the records' shapes **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `DEAD_LETTER_NAMESPACE`, `DEAD_LETTER_INDEX_NAMESPACE`, `slot_key`, `index_key`, `meta-whatsapp-webhooks/src/deadletter.rs` | store namespaces of the webhook dead letters: the ring (slots `0`…`capacity - 1`, decimal, each a letter at `id % capacity`, and its head under the key `head`, `{"v":1,"next":<id>}`, never expiring) and the per-contact index (key: SHA-256 hex of the length-prefixed business number, empty for an event that names none, and contact: `<len>:<phone_number_id>,<len>:<contact>,`; value `{"v":1,"letters":[[<id>,<slot>],…]}`); a letter is JSON with `"v":1`, in a pinned field order (`id`, `at`, `attempts`, `kind`, `error`, `phone_number_id`, `contacts`, `event_bytes`, `event`). **Not ephemeral**: changing a name, a derivation or a record's shape strands the letters written before, and an erasure no longer reaches them (they age out after 14 days by default) | `the_keys_are_pinned_and_hashed` (known answers), `the_record_bytes_are_pinned` (written and read back) |
| `wa.bot.jobs`, `wa.bot.jobs.due`, `wa.bot.jobs.sent`, `wa.bot.jobs.message`, `wa.bot.jobs.limited` (**pending the owner's confirmation before the first release**, roadmap § Owner touchpoints; added by B3) | `JOBS_NAMESPACE`, `JOBS_DUE_NAMESPACE`, `JOBS_SENT_NAMESPACE`, `JOBS_MESSAGE_NAMESPACE`, `JOBS_LIMITED_NAMESPACE`, `BUCKET_SECONDS`, `meta-whatsapp-bot/src/jobs/mod.rs`; the records, `meta-whatsapp-bot/src/jobs/store.rs` | store namespaces of the durable jobs: the job records (key: SHA-256 hex of the length-prefixed job id), the due index (pages `<minute start, unix seconds>:<page>` of job ids, and its `cursor`, with the page versions of the minute behind it once moved forward), the per-recipient markers (digest of the job id, the run and the recipient's key: `user:<BSUID>` first, else `phone:<digits>`, or `group:<id>`), the sent messages' ids (digest of the `wamid`) and the marketing-limited markers (digest of the job id and the recipient's key); every record JSON with `"v":1`, in a pinned field order. **Not ephemeral**: changing a name, a key's derivation or a record's shape strands the jobs scheduled, or makes a resumed job send again to recipients it sent to | `the_job_keys_are_pinned_and_hashed`, `a_recipient_is_keyed_by_bsuid_first`, `buckets_are_minutes_in_unix_seconds` (key bytes, known answers); `the_job_records_are_pinned`, `the_index_and_marker_records_are_pinned` (record bytes, written and read back) |
| `wa.server.registration` (added by roadmap M3d; the namespace, the key layout and the record **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints) | `ATTEMPTS_NAMESPACE`, `RegistrationAttempts::store_key`, `meta-whatsapp-server-core/src/registration.rs` | store namespace of a number's registration and verification attempt counters (key: `registration:<phone_number_id>` or `verification:<phone_number_id>`; value: plain JSON `{"v":1,"since":<Unix seconds>,"count":<n>}`, expiring with its window). **Ephemeral**: changing it forgets the counts (one window at most), so while replicas of two releases run side by side they count apart; no data is stranded | `the_counter_key_and_record_are_pinned` |
| `wa_server_webhook_endpoints` (`id`, `tenant_id` `ON DELETE CASCADE`, `url`, `event_types`, `enabled`, `disabled_reason` and its value `failing`, `secret`, `previous_secret`, `previous_secret_until`, `cursor`, `failing_since`, `created_at`, `updated_at`; index `wa_server_webhook_endpoints_tenant_idx`), `wa_server_webhook_deliveries` (`id`, `endpoint_id` and `tenant_id` cascading, `event_id`, `sequence`, `event_type` and its value `ping`, `status` and its values `pending`, `delivered`, `failed`, `attempts`, `next_attempt_at`, `give_up_at`, `lease_until`, `claim`, `last_attempt_at`, `last_status`, `last_error` and its values `timeout`, `connect`, `tls`, `status`, `protocol`, `destination_refused`, `resolve_failed`, `secret_unavailable`, `event_gone`, `last_duration_ms`, `created_at`; `UNIQUE (endpoint_id, event_id)`; indexes `wa_server_webhook_deliveries_due_idx`, `…_endpoint_idx`), migration 9's description `webhooks out` (numbered 9 while M2f holds 8); the sealed secret's format `v1.<key id>.<base64 of nonce and ciphertext>` and its associated data `wa-server/webhook-secret/v1`, NUL, the endpoint's id, NUL, the key id; the ids `whe_` + 32 hex digits and `whd_` + 12 hex digits of milliseconds + 20 random; the secrets' `whsec_` + base64 of 32 bytes (Standard Webhooks' prefix); the ping body (`id`, `type: ping`, `api_version`, `tenant_id`, `endpoint_id`, `sent_at`); `WA_SERVER_DATA_KEY_ID`'s default `k1` (added by roadmap M2c; every name **pending the owner's confirmation before the first release**, roadmap § Owner touchpoints, design D105, D110) | `meta-whatsapp-server-store-postgres/migrations/0009_webhooks_out.sql`; `AAD_TAG`, `VERSION`, `meta-whatsapp-server-core/src/webhooks_out/seal.rs`; `EndpointId`, `DeliveryId`, `DeliveryStatus`, `AttemptError`, `DisabledReason`, `PING`, `webhooks_out/mod.rs`; `SECRET_PREFIX`, `webhooks_out/signature.rs` | the service's webhooks-out records and the sealed form of their secrets: renamed, a database's endpoints and their secrets are stranded | `the_migrations_and_their_checksums_are_pinned`; `the_sealed_format_is_pinned` (a secret sealed by hand); `the_standard_webhooks_test_vectors`; `conformance::webhooks_out` |

Library store namespaces are `wa.<module>[.<purpose>]` (`wa.token`,
`wa.otp.rate`, `wa.webhook.dedup`, `wa.bot.cooldown`): a new typed store
takes one under its module, and an integrator who keeps their own
records in the same `KvStore` stays clear of `wa.`. A row marked pending
was added after the table was last confirmed: the owner confirms it
before the first release, and until then it may still change without a
data migration.

Not ours to rename either: Meta's names (`wa_id`, `wamid`, `waba_id`,
`wa.me`, `WA_EMBEDDED_SIGNUP`, …), and the `WA_` environment variables
of the examples and the dev container (`WA_TENANTS`, `WA_OTP_NAMESPACE`,
`WA_FIREWALL_*`, …), which are about WhatsApp, not about this project.

## Error tree

`meta_whatsapp_core::Error` is the root; every public fallible function returns
`meta_whatsapp_core::Result<T>`. See `crates/meta-whatsapp-core/src/error/mod.rs` for the tree.

- Every public fallible function returns `meta_whatsapp_core::Result<T>`, with one
  exception: pure, I/O-free functions (signature/token verification,
  Flows endpoint crypto, Typst rendering, and every public `validate()`)
  may return their precise error (`CryptoError`, `WebhookError`,
  `meta_whatsapp_typst::RenderError`, `Result<(), ValidationError>`); `?` lifts them
  into `meta_whatsapp_core::Error`.
- `thiserror` for every typed node. `anyhow::Error` only as the opaque leaf
  for failures raised by code we do not own (adapters, integrators):
  `TransportError::{Connect, Backend}`, `StorageError::Backend`,
  `SinkError::Delivery`, `Error::Other`.
- Branch on `Error::kind()` → `ErrorKind` (classified from Graph error
  `code`, per Meta's guidance) and `Error::is_retryable()`. Never on message
  text, HTTP status, or subcode. A `ValidationError` is `InvalidParameter`
  (fix the input), except the inbox's two local refusals of a state that
  must change first, each with a field constant, a constructor, a
  predicate and a kind of its own: the closed window
  (`ValidationError::customer_service_window_closed()`) is
  `CustomerServiceWindowClosed`, like Meta's `131047`, so one condition has
  one kind; a thread another app owns under Conversation Routing
  (`ValidationError::thread_owned_elsewhere()`) is `ThreadOwnedElsewhere`,
  like Meta's `131070` (a service message without owning the thread,
  `conversation-routing/thread-control` § Errors). `Error::kind` looks
  through `Step`, and so does
  `meta_whatsapp_rs::inbox::is_thread_owned_elsewhere`. The service answers
  both `409`.
- Two questions about a failed send, two methods.
  `Error::may_have_been_sent` says whether it may have reached Meta
  (`false`: fix and resend; `true`: reconcile with the status webhooks
  first). `Error::may_resend` is the one rule for resending it
  *automatically*, with no person or reconciliation in between, and it
  is conservative: only an error that proves Meta refused the request
  before doing anything (a throttling code on any status, the Business
  Use Case limits `80008`, `80009` and `80014` excepted, an HTTP 429,
  `131057` on a 4xx). The client's `RetryPolicy` (for a request that is
  not idempotent) and the bot's broadcast both follow it, and a policy of
  theirs can only be stricter. A `131000` on a 400 is not resent
  automatically, although `may_have_been_sent` is `false`.
- Multi-step flows (Embedded Signup onboarding) wrap failures with
  `Error::in_step("stable_step_name")` so callers know how far they got.
- **`Error::Credit(CreditError)`**, the first leaf added for a feature
  module, and why: a Solution Partner credit line is money (the partner
  pays for every message on a shared line, and an attached line cannot be
  taken back), and its steps stop in ways no existing leaf describes. A
  `ValidationError` is never retryable and never sent, but a busy credit
  step is worth retrying, a share that raced a revocation did reach Meta,
  and a revocation that stopped part-way has already sent `DELETE`s and
  carries a report the caller must act on. `CreditError` decides
  `is_retryable` and `may_have_been_sent` per variant: `Busy` (retryable;
  `posted` only for the two-call method's recorded share), `Revoked`
  (`posted` when a share raced a revocation and was revoked at once, or
  is recorded for the next revocation), `OwnerUnknown`, `StatusUnknown`
  and `ApprovalRequired` (nothing sent, not retryable), `Reconcile` (sent,
  not retryable: a share may be live, its answer lost or not found by a
  racing revocation; a person checks Meta Business Suite), `AttachFailed`
  (the two-call method's share went out, its attach did not; retryable
  and kind as the attach's error) and `RevocationIncomplete`, whose
  `RevocationIncomplete` struct holds the `CreditRevocation` report, the
  `failed`, `unconfirmed` and `unattributed` records, `share_pending` (a
  share with no recorded outcome the call revoked nothing for),
  `deletes_sent`, a ledger failure as `ledger` and the first failure on
  Meta's side as its `source`; it is retryable unless a record names no
  business or the source is not (a ledger failure never makes it
  unretryable: a repeat writes the ledger again). `CreditError::kind()`
  is `ServiceUnavailable` for `Busy` and for an incomplete revocation
  that only a later call can finish, `Unknown` for the states only a
  person can settle (`Reconcile`, records naming no business), the
  source's kind for an incomplete revocation with one, and
  `InvalidParameter` for the refusals. Helpers:
  `Error::credit()` (looks through `Step`, like `graph()`),
  `CreditError::revocation()`, and `EmbeddedSignup::is_credit_line_revoked`
  / `is_credit_step_busy`.
- **`SinkError::Permanent(anyhow::Error)`**, the one variant added to
  an existing leaf for dead-lettering (roadmap L21a, `OPEN_QUESTIONS.md`
  #30), and why: only the sink knows whether an event can ever be
  accepted (a value its database refuses forever, a shape its code cannot
  handle), and the webhook handler must tell that from "try again later"
  without knowing the sink's error types. `SinkError::permanent(e)`
  builds it, `SinkError::is_permanent` asks; every other variant is
  transient, so a sink that never classifies keeps today's behaviour
  (`500`, Meta redelivers the batch). Not an `ErrorKind` (those classify
  Graph codes) and not a method on `Error` (the handler sees the
  `SinkError` before it becomes one). `is_retryable` and
  `may_have_been_sent` are `false`, like every sink error. `FanoutSink`
  returns a transient error before a permanent one, so a fan-out is
  permanent only when every failing sink was.
- **`Error::Conversion(ConversionError)`**, the second leaf added for a
  feature, and why: a `media::MediaConverter` (roadmap L19) fails in ways
  that are not input errors alone. A converter that is not installed
  (`BinaryMissing`) or lacks an encoder (`EncoderMissing`) is the
  deployment's to fix (`FeatureNotAvailable`); a run past its time limit
  (`Timeout`, `ServiceUnavailable`) is the one worth retrying; an input
  it cannot read (`InputRejected`) is `InvalidParameter`; an output of
  the wrong format (`OutputInvalid`) or a process killed (`Killed`) is
  `Unknown`. A `ValidationError` would make the first two the caller's
  fault and the timeout final; a `TransportError::Timeout` would say a
  conversion may have reached Meta. None of them was sent:
  `may_have_been_sent` is `false` for every variant. An empty input, or
  one too large before or after conversion, stays a `ValidationError` on
  `input`. A missing binary is this node, not a `ConfigError`, so that a
  caller handles every converter failure in one place. It adds no
  `ErrorKind`, so the service's error codes are unchanged (its
  `classify` answers it `internal` until a route converts, M5c1). Helper:
  `Error::conversion()` (looks through `Step`).
- Examples and binaries use `anyhow::Result` at the edge.
- No module adds a variant to the root for its own convenience; a new leaf
  needs a reason in this document.

## Client (`meta-whatsapp-client`)

`Client` = `Arc<Shared{transport, endpoint, retry, timeout, user_agent}>` +
optional `AccessToken`. `client.with_token(t)` is the multi-tenant switch.

Endpoint modules follow one pattern (`src/messages/mod.rs`, abridged):

```rust
impl Client { pub fn messages(&self, id: impl Into<PhoneNumberId>) -> Messages }
impl Messages {
    pub async fn send(&self, message: &OutboundMessage) -> Result<SendResponse> {
        message.validate()?; // documented limits, before any request (rule 3)
        self.client.post_at(&[self.phone_number_id.as_str(), "messages"])
            .json(message)
            .context(SEND_CONTEXT)
            .send_private() // the response names the recipient: see rule 8
            .await
    }
}
```

Rules for every endpoint module:

1. **Build requests only through `GraphRequest`** (`client.get_at/post_at/
   delete_at(&[segments])` for any path with an id; `client.get/post/delete`
   for literal paths only). It owns auth, retries, error decoding, the
   credential host allowlist, and segment-safe paths (an id containing `/`,
   `?` or `#` stays inside its segment; empty, `.` and `..` are refused).
   Ids have one of two shapes (`meta_whatsapp_core::ids`, roadmap L20c).
   The ids Meta documents as numeric strings (`AppId`, `BusinessId`,
   `WabaId`, `PhoneNumberId`, `MediaId`, `TemplateId`, `ProductItemId`,
   `ProductSetId`) refuse anything
   but 1 to 64 ASCII digits when built (`new`, `FromStr`, `TryFrom`: a
   `ValidationError` on the type's `FIELD`), so a numeric id cannot carry
   a `/` into a path. Every other id is opaque (any string: BSUIDs, group
   ids, message ids, …) and relies on the whole-segment encoding alone:
   Meta documents no shape that would not refuse a legitimate one (a
   standard-base64 group id may hold `/`). Some opaque ids are digits in
   Meta's examples (Flow, catalog and template group ids, credit lines,
   allocation configurations, verification submissions, bot ids), but
   no page states their shape, so they stay opaque until one does (the
   follow-up in roadmap L20c). Deserializing never checks, so
   a response or webhook never fails on an id's shape; `new_unchecked`
   does the same for Meta's values that arrive as plain strings and for
   rows read back from a store, and is never for untrusted input (a form,
   a URL, a request body: use `new`). What stays unverified: whether
   Graph decodes `%2F` before routing. If it does, a caller who controls
   an opaque id (or one built unchecked) could reach another edge of an
   object the token can access; the endpoint modules' own checks (digits
   where a page asks for them) narrow that, and refusing `/` in opaque
   ids outright is the swap, at the cost of a legitimate id.
2. **Requests are typed structs with `Serialize`, responses typed with
   `Deserialize`.** Unknown response fields are ignored (never
   `deny_unknown_fields`); enums Meta may extend are **open enums**, all
   of one shape (roadmap L20a): generated by
   `meta_whatsapp_core::open_enum!`, the documented values as variants
   and any other value kept verbatim in `Other(String)`, so a new value
   never breaks parsing and serializes back unchanged. Never write one
   by hand (the exceptions, two request values Meta never sends, are
   listed with their reasons in the test's `NOT_OPEN`: `WabaCurrency`,
   whose parse refuses an unlisted code, and `GatewayProvider`, whose
   variants carry fields), and never a unit `#[serde(other)] Unknown`
   (it drops Meta's value, and writes its own name back where Meta's
   was); a value Meta documents as `UNKNOWN` is a variant like any
   other. The macro's
   options cover what Meta's pages need: `#![case_insensitive]`,
   `#![normalize = fn]` (one value spelled two ways), and
   `#![string_only]` for documents this crate writes and reads back whole
   (Flow JSON), where a non-string value must fail rather than be kept as
   its JSON text. `crates/meta-whatsapp-rs/tests/open_enums.rs` checks
   every one and scans the source for any other shape; a structured
   catch-all (`Unknown { raw }`, the templates' `Other(Value)`) is not a
   string enum and stays as it is. The `#[non_exhaustive]` policy was
   decided on 2026-09-26 (#28) and lands with roadmap L20b; the macro's
   enums already carry it.
3. **Validate locally what Meta documents as a hard limit** (lengths,
   counts, formats) and return `ValidationError` naming the field. Do not
   invent limits the docs do not state.
4. **Idempotency**: POSTs are non-idempotent by default. Mark a POST
   `.idempotent(true)` only when replaying it cannot duplicate an effect
   (e.g. setting a field to a value).
5. **Lists** return `Page<T>` and take their cursors in their query
   struct (`after`, `before`: the next page is the same query with
   `after = page.next_cursor()`); each offers a `…_stream()` via
   `GraphRequest::paginate`, which manages the cursors and refuses a query
   that already holds one (a `ValidationError`, the stream's single item).
   A list whose page documents no pagination (`waba.subscribed_apps`,
   `templates.library`) takes no cursor and says so. The exception: a
   list whose page documents no pagination and no `paging` object in its
   response may return a `Vec<T>` instead (`authentication::previews`,
   `Business::messaging_customer_bases`, `PhoneNumber::username_suggestions`,
   `PhoneNumber::business_compliance_info`); a new one says so in its
   rustdoc. If Meta documents paging for one later, it moves to `Page<T>`
   (a breaking change).
6. **Tests** use `meta_whatsapp_core::testing::ScriptedTransport`: assert method, path,
   query, auth header and exact JSON body; feed responses copied from the
   docs' examples. Every test that scripts N responses asserts
   `remaining() == 0`.
7. **Docs**: every public item has rustdoc; each module doc names the Meta
   doc paths it implements (relative to
   `https://developers.facebook.com/documentation/business-messaging/whatsapp/`).
8. **Responses that name people** (the send responses of `Messages::send`
   and `Marketing::send` echo the recipient's number) decode with
   `GraphRequest::send_private`: a decode error carries a placeholder
   instead of the body snippet, and the error category and position instead
   of serde's message (which quotes the offending value). **Paged answers**
   are withheld the same way without the call site asking: Meta's
   `paging.next` and `paging.previous` links can embed the `access_token`
   the request was sent with, so `GraphRequest::send` (and every other
   decode of a 2xx body, through `request::decode_json`) withholds the
   snippet and serde's message of any body that names a `paging` key or
   `access_token`. The rule reads the body, not the target type: it holds
   for `Page<T>`, for a module's own struct with a `paging` field, and for
   a page nested in an object, and for a stream's items. An `Error::Http`
   built from a body (a `"success": false` answer, a non-Graph error body)
   takes its snippet from `request::safe_snippet`, under the same rule.
   Other answers keep their snippet and serde's message. A module that
   decodes a `send_raw()` body itself calls `decode_json` rather than
   passing the body to `Error::decode`, and builds `body_snippet` with
   `safe_snippet`; `request::body_guard_tests` scans the crate's sources
   (outside comments, literals and test code) for any other way, each
   exception pinned to its function. A decoded `Paging` keeps no
   credentials: its `next` and `previous` links lose their `access_token`
   and `appsecret_proof` parameters, and `HttpResponse`'s `Debug` prints
   the body's length only. A response that carries another secret
   decodes with `send_withheld(placeholder)` (`Flow::preview`: its link's
   `token` opens the Flow).
9. **One type per concept.** A type two endpoint families share is
   defined once in `meta_whatsapp_client::common` (`MediaSource`, `FlowAction`,
   `QualityRating`) and re-exported by each module that uses it; a type
   of one module never shares its name with a type of another
   (`flows::endpoint::EndpointAction` is the endpoint request's `action`,
   `common::FlowAction` the `flow_action` a Flow starts with). New code
   types every Graph id with a `meta_whatsapp_core::ids` newtype. Known exceptions,
   still `String` (typing them is a breaking change each): the groups'
   `request_id` and `join_request_id`; `MessagingCustomerBase::id`,
   `CreatedMessagingCustomerBase::messaging_customer_base_id` and the
   signups' `default_messaging_customer_base_id`; `OnboardingRequested::request_id`;
   the calling settings' `app_id`; `SubscribedAppData::id` and
   `AssignedUser::id`; `CommerceSettings::id`; `LibraryTemplate::id`; the
   template's `ad_*_id`s; the phone number `request_id` (a new
   number's `preverified_id` is a `PreVerifiedPhoneNumberId` since
   roadmap L11c); the ids of an Embedded Signup session event
   (`ad_account_ids`, `page_ids`, `dataset_ids`, `catalog_ids`,
   `instagram_account_ids`, `session_id`), the launch's `solution_id` and
   the token's `user_id`; and `HealthEntity::id`, on purpose: a health
   status entity's id is a phone number, template, WABA, business or app
   id depending on its `entity_type`, so no one newtype fits it. Merchant-chosen ids (product retailer ids,
   button ids) and our own (an OTP challenge id) are strings on purpose.
10. **List queries are named `List*`** (`ListQrCodes`, `ListSignups`,
   `ListFlows`, `ListFlowAssets`, `ListAssignedUsers`, `ListClientWabas`,
   `ListGroups`, `ListBlockedUsers`, …) and carry `after`/`before`; the
   one-page method sends them, the `…_stream(&query)` refuses them. The
   older `*Query` names stay as they are (renaming them would break
   integrators for no gain): `PhoneNumbersQuery`, `WabaListQuery`,
   `TemplateListQuery`, `LibraryQuery`, `TemplateAnalyticsQuery`,
   `TemplateGroupAnalyticsQuery`, `GroupAnalyticsQuery`.

### Retries

`RetryPolicy` (`src/retry.rs`): idempotent requests retry on any retryable
error; non-idempotent requests only when the error proves Meta rejected the
request before processing (throttling). A timeout on a send is never
replayed — a duplicate OTP or order confirmation is worse than an error.

### Credentials never leave Meta

`GraphRequest` attaches a token to the configured Graph endpoint (same
scheme, host and port; `https://graph.facebook.com` unless
`ClientBuilder::endpoint` names a proxy, in which case production Graph is
just another host) and to what its credential rules allow. The rules are
data (`CREDENTIAL_RULES`, next to `GraphRequest`), one line per documented
need; every line is `https` on the default port and an exact host, and
adds a method and a path rule:

| Rule | Why |
| --- | --- |
| `https://lookaside.fbsbx.com`, any method, any path | media download URLs point there, and Meta refuses a download without the token |
| `GET https://api.facebook.com/{digits}/parent-bsuid-accounts`, nothing else on that host | the Parent BSUID Accounts API (`Business::parent_bsuid_account`) is served there, not on Graph, and takes no API version (`business-scoped-user-ids`, § Get parent BSUID account; the page's changelog entry of May 28, 2026 corrected its host from `graph.facebook.com`) |

The second line's path is matched as it goes on the wire, after URL
parsing (percent-encoding kept, so `%2F` is not a separator and `%31` not
a digit; `.` and `..` resolved): exactly two segments, the first ASCII
digits, the second the literal edge, and no query, fragment or user info.
A version segment, a trailing segment or slash, another method, `http`,
another port, or any other path on `api.facebook.com` is refused. Neither
rule follows the configured endpoint: behind a proxy the token goes to the
proxy and to these two, never to `graph.facebook.com` (an egress proxy
that must see every request belongs in the `HttpTransport`). A new line
widens where credentials go, so it goes through the security review.

Any other URL with a token (another Meta host such as `*.whatsapp.net` or
`*.fbcdn.net`, a subdomain, a look-alike — the host is compared exactly
after URL parsing, so suffixes, trailing dots and IDN homographs do not
match —, another port, plain HTTP) fails with `ValidationError` on `url`
before the transport sees it; the error names the host, the method and the
rules, never the path, the query or the token. Inside the library, the
only requests to an absolute URL are `Media::download_with_info` and
`Business::parent_bsuid_account`; integrators reach the same check through
`Client::request_url`, and cannot step around it with a header:
`GraphRequest::header` refuses `Authorization`, `Proxy-Authorization` and
`Cookie` (any case, with or without `no_auth()`; a validation error on the
name before any request), and a URL with a user name or password is
refused on send (reqwest would send them as `Authorization: Basic`, to
any host), so `bearer()` and `oauth()`, which the check covers, are the
only ways to attach an `Authorization` header. A secret an integrator puts
in a query parameter or in another header goes where that URL goes: the
check does not read them. Pagination re-issues the original request
with `after=` rather than following `paging.next`.

The check covers the URL the client hands to the transport, so a redirect
must not carry the token further: that is the `HttpTransport` port's rule
(its rustdoc). reqwest rebuilds every hop from the request's original
headers and drops the credentials only on a hop that changes scheme, host
or port from the URL that answered it, so it would send the token to
another path on `api.facebook.com`, past the second rule, and, after a hop
to another origin, to any URL within that origin. The stock
`ReqwestTransport` therefore sends a request that carries a credential
(a credential header, or user info in its URL, which reqwest turns into
one) through a client that follows no hop keeping scheme, host and port
(the 3xx is the response, an `Error::Http` in the client), and follows a
hop to another origin without it, up to 10. That policy is public,
`meta_whatsapp_adapters::http::credential_redirect_policy()`. A request
without credentials follows reqwest's default. Integrators who build
their own `reqwest::Client`s pass two to `ReqwestTransport::with_clients`,
the credentialed one built with that policy (or with no redirects). The
single client given to `ReqwestTransport::with_client` sends every
request, so its policy is the credentialed requests' too: the transport
cannot change a built client's policy, nor stop reqwest following a
redirect inside `execute`, so that client must be built with the policy
or with no redirects (its rustdoc says so, and points to `with_clients`).

**Decided 2026-09-27 (coordinator, owner's delegation)**, on the choices
roadmap L10a's draft proposed:

- The rules do not follow a configured Graph endpoint or proxy: behind
  one, the token goes to the proxy, the media host and the one Parent
  BSUID Accounts URL. A Graph proxy stands in for Graph, which serves
  neither. Swappable by setting the proxy on the transport
  (`HTTPS_PROXY`, or proxied clients in `ReqwestTransport::with_clients`,
  or another `HttpTransport`), which then sees every request. Roadmap
  L10a.
- The business id in the rule and in `Business::parent_bsuid_account`
  is ASCII digits: fail closed, since Meta documents no other shape and
  every id its pages show is digits. Swappable only by a reviewed change
  to the rule's line. Roadmap L10a.
- An exact rule allows no query, fragment or user info: the one URL it
  exists for has none. Swappable only by a reviewed change to
  `PathRule::Exact`. Roadmap L10a.
- `ParentBsuidAccount::parent_bsuid_account_id` is a `String`: core has
  no id type for it. Swappable by a typed id in core, a change of the
  field's type. Roadmap L10a.
- `parent_bsuid_account_id` is required and `enrolled_business_portfolios`
  defaults to empty: what Meta answers for a portfolio that is not
  enrolled is not documented, so an answer without the id is a decode
  error rather than a guess. Swappable by reading the same URL as JSON
  (`client.request_url(Method::GET, url).send::<serde_json::Value>()`,
  which the rule allows). Roadmap L10a.
- The media host's rule is unchanged (any method, any path; pinned by
  `the_media_rule_is_unchanged`): narrowing it to `GET` is a separate
  change. No code.

## Feature modules

### Messages (`meta_whatsapp_client::messages`)

`OutboundMessage { recipient: Recipient (flattened), context?, biz_opaque_callback_data?, category? (Direct Send), ttl_seconds? (Direct Send), direct_send_config? (Direct Send), content: MessageContent }`
where `MessageContent` is an internally tagged enum on `type`: text, image,
audio, video, document, sticker, location, contacts, interactive (button,
list, cta_url, location_request_message, flow, product, product_list,
catalog_message, carousel, voice_call, call_permission_request,
request_contact_info, address_message), reaction, pin/unpin, template, and a
`Raw` escape hatch that refuses envelope keys. Constructors cover the common
cases (`OutboundMessage::text(to, body)`, …). Every documented limit is
checked locally before sending; templates run `TemplateMessage::validate`.
`mark_read(message_id)` (idempotent) and
`mark_read_with_typing_indicator(message_id)` (not replayed: a late retry
could show "typing…" after the reply). `SendResponse { messaging_product,
contacts: [{input, wa_id?, user_id?, parent_user_id?}], messages: [{id,
group_id?, message_status?}] }` — shared with the MM API.

Recipient addressing follows the BSUID rules in `meta_whatsapp_core::recipient`.

### Media (`meta_whatsapp_client::media`)

Upload (multipart: `file`, `type`, `messaging_product`; MIME type and size
checked first), `url(media_id)` → `{url, mime_type, sha256, file_size, id}`,
`download(media_id)` → streaming body with a `verified()` adaptor that
hashes while streaming (a mismatch is `TransportError::Integrity`,
retryable), `download_bytes` with a size cap (pre-allocation never trusts
Meta's reported size beyond 16 MiB), `delete`. A download that answers
non-2xx is read to at most 64 KiB (`request::STREAMED_ERROR_BODY_CAP`),
then the stream is dropped and what was read decoded as any error body
(`GraphRequest::send_streaming`); the 2xx body is the caller's to bound.
Resumable Upload API
(`/{app_id}/uploads` → `upload:<id>` sessions, `file_offset`, `Authorization:
OAuth` on every step so the token never sits in a URL) returning a
`meta_whatsapp_core::ids::UploadHandle` for template `header_handle`s.

### Templates (`meta_whatsapp_client::templates`)

CRUD + library + migrate + compare. Two builder families that must not be
confused:

- **Definition** (creating/editing a template): `TemplateDefinition { name,
  language, category, parameter_format, components: [Header|Body|Footer|Buttons|Carousel|LimitedTimeOffer…] }` with examples.
- **Invocation** (sending): `TemplateMessage { name, language, components:
  [header params, body params, button params by index/sub_type] }`, embedded
  in `MessageContent::Template`.

Named and positional parameters are both supported (`parameter_format`).

### Authentication (`meta_whatsapp_client::authentication`)

Authentication template definitions (copy code, one-tap with
`supported_apps` package/signature hash, zero-tap with
`zero_tap_terms_accepted`), `add_security_recommendation`,
`code_expiration_minutes`, previews, bulk upsert.

`OtpService` on `KvStore` + `Clock`:

- Recipients are **strict E.164 with `+`**. Meta prepends the sending
  number's country code to a number without `+`, so a digits-only key would
  let `12015553931` (delivered to `+91 12015553931`) verify `+12015553931` —
  an account takeover. The code is sent to exactly `+<digits>` and keyed by
  those digits. BSUID-only recipients are refused locally (OTP buttons need
  a phone number; Meta's `131062`).
- Challenges are **scoped to the service**: the store key is
  HMAC(pepper, `"wa.otp.key" | scope | digits | purpose`), where the scope is
  the sending `phone_number_id` and the required `OtpConfig::namespace`
  (the tenant; `OtpConfig::new(namespace)`, no `Default`),
  netstring-encoded so neither can be shifted into the other. Services
  sharing a store and a pepper (several merchants of one integrator) never
  see each other's codes, cooldowns or issue limits, on their own numbers
  or on a shared one. A blank namespace (or one with edge whitespace,
  control characters or format characters, `Cf`) is a config error;
  changing the
  scope (or upgrading across the commit that introduced it) invalidates
  outstanding codes. Making the namespace required kept the encoding: a
  service that had set one derives the same keys.
- `issue(recipient, purpose) → IssueOutcome { Sent(Challenge{id, expires_at,
  message_id}), CoolingDown{retry_after}, RateLimited{retry_after} }`:
  CSPRNG numeric code (length 4–8), only an HMAC-SHA256 of it stored under a
  server pepper (`SecretBytes`): HMAC(pepper, `"wa.otp.code" | key |
  challenge id | code`), so a record copied to another key (by anyone who
  can write the store but lacks the pepper) never verifies there. Keys are
  HMACs too (no raw phone number in the store); the namespace and the
  purpose are server-side constants, never request input. The code goes out through `Messages::send`
  (`OutboundMessage::template`), so the send checks and the private
  response decoding apply. Resend cooldown (30 s) and a per-recipient
  issue limit (default 5 per sliding hour per number and purpose, per
  service scope; opting out is explicit) bound brute force to ~0.06 %/day
  for 6 digits. A challenge is removed only
  when the send was provably rejected (4xx, throttling); after a timeout or
  5xx it stays, because the code may have been delivered. The line is
  `Error::may_have_been_sent`, the one integrators use for their own
  sends.
- `verify(recipient, purpose, code) → VerifyOutcome { Verified, Invalid {
  attempts_left }, Expired, TooManyAttempts, NotFound }`: constant-time
  compare; attempts counted with `compare_and_swap` before comparing, so
  concurrent guesses cannot exceed the limit; a verified challenge is
  consumed (single use).
- The code never appears in logs, errors, or `Debug` output.

### Embedded Signup (`meta_whatsapp_client::embedded_signup`)

The most important flow. Frontend (Facebook JS SDK) runs `FB.login` with
the app's configuration id; the page receives a `WA_EMBEDDED_SIGNUP`
message event (`FINISH` with `waba_id`, `phone_number_id`, `business_id`;
`CANCEL` with `current_step`; `ERROR`) and a short-lived `code`. The backend:

1. `exchange_code(code)` → business integration system user token
   (`GET /oauth/access_token?client_id&client_secret&code`, no bearer).
2. Optionally `debug_token` (app token) to confirm granular scopes and the
   WABA ids the token can reach — used when the session info is missing.
3. `waba(waba_id).subscribe_app(Option<&CallbackOverride>)`
   (`POST /{waba}/subscribed_apps`) so webhooks flow; `Some` sends this
   WABA's webhooks to another callback.
4. `phone_number(id).register(&TwoStepPin, Option<&DataLocalizationRegion>)`
   for Cloud API numbers (two-step PIN; optional local storage).
5. Persist the token in `TokenVault` (on `KvStore`, **encrypted at rest**
   with AES-256-GCM under an integrator-supplied key, key id recorded for
   rotation) keyed by WABA id, with a phone-number → WABA index.

`EmbeddedSignup::onboard(&request, &vault) → Onboarded` (Tech Provider;
Solution Partner mode requires `onboard_with_approval`) runs, in order:
exchange code → `debug_token` → **verify** that the WABA id from the browser
event is among the token's grants and that the phone number belongs to that
WABA (browser-supplied ids are never trusted — otherwise one merchant could
overwrite another's vault entry) → [`onboard_with_approval` only: the
integrator's **approval** of the verified WABA, owner and numbers, whose
refusal stops the flow before anything is written] → **store** the token →
subscribe app →
[Solution Partner mode: add the partner's system user to the WABA → share
the credit line] → register number. The token is stored *before* the
fallible later steps because the code is single-use and short-lived:
storing last would lose the token whenever a later step fails (e.g. wrong
PIN, `133005`).
`verify_assets` also reads the WABA's owning business from Meta
(`owner_business_info`); a `business_id` claimed by the browser is ignored
(it is what credit-line sharing keys on). Every number Meta lists on the
WABA is stored in the phone → WABA index, so onboarding a second number
never unroutes the first. `resume(&waba_id, &request, &vault)` loads the
stored token and reruns the steps after `store_token`, refusing a session that names
another WABA or an unverified number; `request.code` is not used, so after
a restart a caller builds the request with no code,
`OnboardingRequest::for_resume(saved_session)` (`onboard` refuses a
code-less request before any request). `SignupSessions::redeem(state, tenant)` checks the tenant
inside the library, is single-use, and does not consume the state on a
tenant mismatch. Hosted Embedded Signup
(`onboard_hosted(&HostedSignup, &system_token, &request, &vault)`, from the
`PARTNER_ADDED` `account_update` webhook's WABA and owner business) runs
`"fetch_token"` (`POST /{business}/system_user_access_tokens` with the
system token's `appsecret_proof` in a form body) in place of
`exchange_code`, then the same steps, writing the same vault record; the
WABA's owner as Meta reports it must be the webhook's, and a WABA the vault
already holds is refused before any request (`PARTNER_ADDED` also comes for
the code flow's onboardings), and one stored after that read is not replaced
(`store_token` stores only where none is, `put_if_absent`). Several WABAs in one signup (roadmap L11b,
`OPEN_QUESTIONS.md` #5): with `OnboardingRequest::every_granted_waba(true)`
(off by default), `onboard_wabas(&request, &vault) → WabasOnboarded` (and
`onboard_wabas_with_approval`, its approval called once per WABA) exchanges
the code and inspects the token once, refuses a signup whose session info
names a WABA the token was not granted, then runs `verify_assets` to the end
of the tail for each granted WABA on its own (its own vault record, phone
index, approval, subscription and credit line), the claimed one first with
the request as it is, the others registering and syncing nothing, refused
unless Meta reports the claimed WABA's owner as theirs, left alone when the
vault holds them and stored only where it still holds none (`put_if_absent`);
a WABA that fails is reported in its
`WabaOutcome`, the next one runs, and each is resumed on its own. `onboard`
and `onboard_hosted` refuse the option. Nothing new is stored. Each failure is
`Error::in_step("exchange_code" | "debug_token" | "verify_assets" |
"approve" | "store_token" | "subscribe_app" | "assign_system_user" |
"share_credit_line" | "register_phone")`; `offboard` adds
`"revoke_credit_line" | "delete_token"`. **Retrying the whole
`onboard` call is not safe** (the code is spent); retry with `resume()`.
`LaunchOptions` builds the JSON for `FB.login` `extras` (version,
`featureType` — incl. coexistence `whatsapp_business_app_onboarding`,
`setup` pre-fill). `SessionInfo` parses the message event.
`SignupSessions` (on `KvStore`) binds an opaque state id to the merchant that
started the flow, so a callback can't be attributed to another tenant.

**Tech Provider or Solution Partner, per deployment** (owner's decision,
2026-09-24). Without `EmbeddedSignup::solution_partner(SolutionPartner)`,
onboarding is the Tech Provider flow above, request for request. With it
(`SolutionPartner { system_token (private), system_user_id, credit_line_id,
method, default_currency, system_user_tasks }`), following Meta's Solution
Partner order (subscribe → share the credit line → register). The partner
is liable to Meta for every message on a shared line and an attached line
cannot be taken back, so the design is fail-closed:

- The WABA currency (`WabaCurrency`: AUD EUR GBP IDR INR USD, `Other`
  only on purpose) comes from `OnboardingRequest::currency`, else the
  default; missing, it is a `ValidationError` before the code is
  exchanged. A Tech Provider request naming one (or a re-share) is refused
  the same way. The first currency is sealed before the first share is
  posted; another one is refused later.
- **Approval required before sharing** (the owner confirmed it on
  2026-09-25): plain `onboard` is refused
  (`CreditError::ApprovalRequired`, before the code is exchanged); tenant
  checks on the verified ids go in `onboard_with_approval`, which runs
  before `store_token` and records the approval in the credit ledger
  (`approved_at`), bound to the token record it stores
  (`approved_token_created_at`). `resume` shares only for a WABA whose
  stored token record was approved so; `resume_with_approval` approves a
  token stored without one (Tech Provider mode before a switch, or stored
  again since). The policy stays the integrator's (`OPEN_QUESTIONS.md`
  #6).
- `CreditSharing::ShareAndAttach` (default, Meta's current method):
  `assign_system_user` (`POST /{waba}/assigned_users`, system user token,
  the method's documented prerequisite), then
  `whatsapp_credit_sharing_and_attach` (system user token).
  `CreditSharing::ShareThenAttach` (Meta's alternate method):
  `whatsapp_credit_sharing` with the **verified** owner business (system
  user token), then `whatsapp_credit_attach` with the merchant's business
  token.
- `share_credit_line` holds a per-WABA lease (`put_if_absent` with a
  300 s expiry, renewed by compare-and-swap right before each POST; lost,
  the step posts nothing more; the request timeout must stay well below
  it, and a post that outlives it anyway, whose answer is lost or not
  recorded, sets the pending-share flag again whatever cleared it
  meanwhile) and checks before it posts, in `onboard` and
  `resume` alike: the line's records for the owner business
  (`owning_credit_allocation_configs`, only records naming that business)
  plus the recorded allocation, each with its `request_status` (the lookup
  does not say whether a record was revoked; no status is active, an
  undocumented one is `StatusUnknown`); an active one whose receiving
  credential is the WABA's `primary_funding_id` means nothing is posted. A
  POST whose answer is lost (a timeout, a 5xx) may have succeeded and Meta
  refuses a second attach, so it is `Reconcile` (not retryable), and a
  share is never posted again without that check: the ledger seals
  `pending_share` before each POST and clears it once the allocation is
  recorded (merged by compare-and-swap, never reported busy after a POST)
  or Meta provably did nothing, and a pending share nothing explains on a
  funded WABA is `Reconcile`. When nothing funds the WABA the share is
  posted again, which assumes Meta's lookup and `primary_funding_id` show
  an applied share at once (undocumented). A refused attach after the
  two-call method's share is `AttachFailed` and keeps the flag.
  No owner business, no share (`OwnerUnknown`). A business marked
  revoked, or with only `DELETED` records, is refused unless
  `OnboardingRequest::reshare_after_revocation` opts in.
- **A revocation racing a share**: the revocation writes its marker
  before it looks anything up; the share reads the marker after its POST,
  whatever the POST's outcome (a lost answer included). One of them sees
  the other: a share that finds a new or changed marker revokes by
  business what it may have made (the allocation it got back or
  recorded; with a lost answer, every active record naming the business,
  of which only one revoked now can be that share): `Revoked` with
  `posted` once that is revoked (the flag settled), else `Reconcile` with
  the share kept pending. The revocation, meanwhile, re-reads the ledger
  after marking: a pending share of which it revoked nothing makes it
  `RevocationIncomplete` with `share_pending` (retryable), never `Ok`, and
  one it revoked a record for is settled. A marker plus a pending share
  makes a later onboarding `Reconcile`, not `Revoked`. An opted-in
  re-share clears the marker only by compare-and-swap on the version it
  read; a marker another opted-in re-share deleted counts as none (its
  opt-in funds the business again anyway), so a revocation between that
  re-share's read and its clear, whose lookup missed a third share, goes
  unseen by that third share.
- **A pending share Meta never lists** (a post that never reached it)
  would keep every revocation incomplete for good, so an operator clears
  it (the owner's decision, 2026-09-25):
  `clear_pending_share(&waba_id, cleared_by, acknowledged_funding,
  &vault)`, after checking Meta Business Suite. It posts nothing and
  holds the WABA's credit lease (renewed before any ledger write; the
  clear is compare-and-swapped on the version it read). It checks Meta
  first, as a share does: the line's records for the owner business and
  the recorded allocation, with their `request_status`, and the WABA's
  `primary_funding_id` (merchant token). An active record, one of
  undocumented status, or one the lookup returned naming no business
  clears nothing (`PendingShareClearance::NotCleared`; a record funding
  the WABA is recorded as its allocation). Nor does a
  `primary_funding_id` that no record explains, unless
  `acknowledged_funding` is exactly that id: it may be the lost share
  itself, applied before Meta's lookup lists it (the state a share calls
  `Reconcile`), and only a person looking at Meta Business Suite can
  tell it from the merchant's own card. Otherwise the flag is cleared and
  a `ClearedShare` (who, when, the pending share's time, the acknowledged
  funding) is appended to the record's audit trail. Revocation and
  `offboard` then behave as if nothing had been posted. The library
  imposes no minimum age on the pending share (Meta documents no delay;
  the owner rejected an automatic settle time): the operator waits.
- **The credit ledger** (`TokenVault::credit`, `revoked_business`):
  `credit/<WABA>` holds the owner, the allocation, the currency, the
  approval, the pending-share flag and the operator clearances
  (`cleared_shares`), written by compare-and-swap, with fields a later
  revision adds (to the record or to an audit entry) kept as read so a
  rollback does not drop them (a revision older than the trail drops
  the trail itself; integrators keep their own append-only log);
  `revoked/<BUSINESS>` marks a revoked business; both sealed with the
  vault keys (AAD bound to the store key), re-sealed on read like tokens,
  and left by `TokenVault::delete`, so revocation works after the token
  is gone (`rotate` covers them, `rotate_business` a marker no credit
  record names). Sealing stops forgery and moving; it does not stop
  someone with write access to the store from deleting a record or
  putting back an older copy (rollback is out of scope). The token record
  keeps its pre-Solution-Partner format.
- `revoke_credit_line(&waba_id, owner_business_id, &vault)` resolves the
  business (recorded at onboarding, else named by Meta's record of the
  recorded allocation and written back, else a signed webhook's
  `owner_business_id` if the line has records naming it; a contradicting
  one revokes nothing; one whose check failed is marked once the
  revocation's own lookup finds records naming it), marks it revoked
  (replacing an unreadable marker; a failed marker write does not stop
  the `DELETE`s), then revokes every active record naming it plus the
  recorded allocation, confirming each; every record is attempted, and
  what is left undone is `RevocationIncomplete` with the report.
  `revoke_business_credit_line` does the same from a business id alone.
  Nothing calls either for the integrator: the owner's decision
  (2026-09-25) is that its `account_update` handler revokes at once on
  every `PARTNER_REMOVED` of its solution, coexistence ones with
  `disconnection_info` included; a merchant who reconnects onboards again
  with `reshare_after_revocation`.
  `offboard` revokes first and deletes the token second; with nothing to
  address and no share in the ledger it just deletes, a recorded share
  revocation cannot find is `Reconcile` with the token kept, and a
  pending share it revoked nothing for is `RevocationIncomplete` with the
  token kept. `offboard_if_unchanged` revokes the same way, then deletes
  only the token at the version the caller read (`delete_if_unchanged`):
  for a caller whose records may move the WABA to another holder during
  the revocation's calls (the service). A recorded business is marked revoked even when nothing
  was ever shared with it (the owner's decision, 2026-09-25): the marker
  precedes any lookup, which is what stops a racing share from
  surviving, and funding that business later takes
  `reshare_after_revocation`.

The calls themselves are `meta_whatsapp_client::credit_lines` (list, share-and-attach,
share, attach, receiving credential, primary funding, find records,
revoke, status, and `is_shared`): each authenticates with its client's
token, the partner's system user token except for `attach` and
`primary_funding` (the merchant's business token).

Partner-led business verification is `meta_whatsapp_client::business_verification`
(submit a merchant's business with its documents, list submissions,
business verification status), outside onboarding: submitting and listing
take the partner's system user token, the status the merchant's business
token. Documents are checked before the request (count, type, size,
content matching the type), and a submission is never replayed (Meta
allows three per merchant). The decision is the `account_update` webhook
`PARTNER_CLIENT_CERTIFICATION_STATUS_UPDATE`.

Multi-Partner Solutions and migration intents are `meta_whatsapp_client::partners`
(`solutions`: create, accept, reject, the three deactivation calls, a
solution and its partners, an app's solutions and connected client
businesses, and a client's business token through a solution;
`migration`: `set_solution_migration_intent` and reading an intent),
also outside onboarding and each on its client's token. The business
token a solution gives is an `AccessToken`, decoded so that a body that
fails to parse reaches no error; no call there is replayed after a
timeout but the reads.

Payments (India and Brazil, roadmap P1a to P1d) are
`meta_whatsapp_client::payments`: the `order_details` and `order_status`
messages are `Interactive` variants of `messages` (re-exported there),
checked against every rule the pages state for their currency (`INR`
India's, `BRL` Brazil's), with amounts as `{value, offset}`; the payment
templates' order details and payment request buttons and order status
component are `templates` parts, checked by the same rules, and a
payment object the client does not type is refused, never sent
unchecked. The payment webhooks (India's status of type `payment`,
Brazil's `payment_method` reply) are typed in
`meta_whatsapp_webhooks::fields::payments`, and never fail their change.
The payment lookup
and refunds hang off the phone number (`Client::payments`), the payment
configurations off the WABA (`Client::payment_configurations`). A refund,
a configuration's creation and an OAuth link are never replayed after a
timeout; the OAuth link is a `SecretUrl`. No call accepts the WhatsApp
Payments terms of service. India's checkout button templates (P1d) add
the checkout endpoint, `payments::checkout` behind `flows-endpoint`: it
reuses the Flows endpoint's key and crypto (`FlowEndpointKey`, one
generic decryption inside the crate), never a copy, and checks an
answer before it is sealed. A third party validation's encrypted bank
details are a secret newtype, out of `Debug`.

`EsVersion`'s v2, v3 and their public previews are `#[deprecated]`: Meta
deprecates them on 2026-10-15.

Coexistence: `smb_app_data` sync (contacts, history) within 24h.

### In-App Signup (`meta_whatsapp_client::signups`), WABA (`waba`), phone numbers (`phone_numbers`), business profile, QR codes, block users, commerce, analytics, marketing (MM API), flows, groups, calling, thread control (`thread_control`)

Typed endpoint wrappers per the rules above. Flows additionally provides
the data-endpoint crypto (feature `flows-endpoint`): decrypt
`{encrypted_flow_data, encrypted_aes_key, initial_vector}` with the business
RSA private key (RSA-OAEP-SHA256 → AES-128-GCM), and encrypt the response
with the same key and the bit-flipped IV. RSA goes through `aws-lc-rs`
(constant-time), never the `rsa` crate (RUSTSEC-2023-0071, Marvin timing
attack, unfixed) — the endpoint decrypts attacker-supplied ciphertext.

## Webhooks (`meta-whatsapp-webhooks`)

```
POST body ─► verify X-Hub-Signature-256 (HMAC-SHA256, constant-time, any of N app secrets)
         ─► parse WebhookPayload{object, entry[{id, time?, changes[{field, value}]}]}
         ─► normalize into Vec<WebhookEvent> (one per message/status/change)
         ─► optional DedupGuard: lease a claim (pending, 60 s) → EventSink<WebhookEvent>
             → on SinkError::Permanent, with DeadLetters: write the dead letter, then go on
             → confirm (done, TTL 7 days + 1 h — Meta retries for 7) or release on sink error
```

Dedup is a **lease, not a marker**: a marker written before delivery would
swallow Meta's retry if the request died between marking and delivering
(timeout, crash, deploy), losing the event for 7 days. A retry that finds a
live lease gets `WebhookError::ClaimInFlight` (answered `503`, Meta retries
later). Every claim transition is a version-checked `compare_and_swap`, so an
expired lease can't overwrite a newer claim. Dedup keys are hashed before
they reach the store (group status keys contain participants' phone numbers).
Logs carry sizes, digests and field names only — never payload values.

**Dead letters** (`deadletter::DeadLetters`, `WebhookHandlerBuilder::dead_letters`;
roadmap L21a). An event the sink refuses permanently
(`SinkError::Permanent`) is written to a typed store on `KvStore`, its
dedup claim marked done, and the rest of the batch delivered; the
delivery is acknowledged only after that write succeeded (a failed write
releases the claim and answers `500`). Transient and unclassified sink
errors answer `500` as before, and so does a permanent one without a
store. The store is bounded by count (a ring of `capacity` slots, 1000 by
default: letter `id` lives in slot `id % capacity`, so the oldest is
overwritten without a scan, with a warning), by age (each record's TTL,
14 days by default: the retention of the personal data an event holds)
and by size (an event over 256 KiB serialized is kept without its
content). Alert: a `tracing::error!` per letter (id, kind, business
number; never content or the sink's error text) and
`DeliveryReport::dead_lettered`. Replay (`replay(id, sink)`,
`replay_all(sink)`) hands the stored event to a sink past the dedup guard:
delivered, the letter goes; refused permanently again, it stays with one
more attempt. Erasure: `KvStore` has no scan, so the store keeps a
per-contact index (every identifier the event gives: BSUIDs, parent
BSUIDs, phone numbers without `+`; the business's own number left out)
keyed by business number and contact, and
`DeadLetters::erase(phone_number_id, contacts)`; the facade's
`erasure::Eraser` calls it beside `ConversationStore::erase_all`.

- `verify_subscription(query, &VerifyToken) -> Result<String /*challenge*/>`
  (constant-time token compare, `hub.mode == "subscribe"`).
- Typed values for every documented field (`messages` — all inbound types,
  statuses with pricing, errors; template status/quality/category/components
  updates; phone number name/quality; account update/review/alerts;
  business capability; security; user preferences (marketing opt-out);
  history, smb app state sync, smb message echoes (coexistence); partner
  solutions; payment configuration; calls; flows; groups;
  business_username_updates, user_id_update; Conversation Routing's
  messaging_handovers and standby, and the `conversation_context` summary
  on messages; the Marketing Messages API's `user_actions` click events).
  Every example payload on a page that prints a webhook body is a fixture,
  listed with its page in `tests/conformance/manifest.rs`, whose test
  checks the exact events, that every value survives the typed parse, that
  no value falls into a catch-all, and signed delivery through the handler
  (a fixture missing from the manifest fails it). Unknown fields and unknown
  message types parse into `Unknown { … raw: serde_json::Value }` — **a new
  Meta field must never fail a delivery**.
- `WebhookEvent` carries `waba_id`, `phone_number_id` (when the field has
  one), and the user identity (`wa_id?`, `user_id?` BSUID, `parent_user_id?`,
  `username?`). Standby copies (`StandbyObserved`) are a variant of their
  own, never a `MessageReceived`: a responder that only observes a thread
  must not answer it.
- A body that verifies but fails to parse is acknowledged (so Meta stops
  retrying for 7 days) and surfaced as `WebhookEvent::Unparsed{raw}` +
  a `tracing::error!` carrying only size and digest. A bad signature is
  rejected (401); blank app secrets and blank verify tokens fail closed
  (a blank app secret at `SignatureVerifier::new`; a blank verify token
  at each verification, and at startup when the caller runs
  `WebhookHandler::check`, since the builder stays infallible).
  Default body limit: 3 MiB (Meta: payloads up to 3 MB).
- `axum` feature: `router(handler)` with `GET` verify + `POST` receive
  (raw bytes, body limit), and an SSE helper that turns a broadcast
  subscription (filtered by phone number id) into `text/event-stream` for
  a live inbox. The `POST` route checks that `X-Hub-Signature-256` is
  present and well-formed (`sha256=` + 64 hex; no secret or body needed)
  before reading the body, so an unsigned request is a `401` without being
  buffered; `SIGNATURE_HEADER` is defined outside the `axum` feature for
  framework-free integrations. The SSE helper reads a
  `broadcast::Receiver<Arc<WebhookEvent>>`: every subscriber gets the
  same allocation of an event, never a copy of its own (roadmap L21b),
  and its filter runs on that shared event.

## Adapters (`meta-whatsapp-adapters`)

- `http::ReqwestTransport` (feature `reqwest`, rustls): streaming download,
  multipart, per-request timeout, error mapping (timeout → `Timeout`,
  connect → `Connect`, else `Backend`), a bound on every buffered body
  (`max_response_bytes`, 256 MiB by default, `None` for none; past it a
  `Backend` error naming only the bound, the body never drained: an
  HTTP/1.1 connection is closed, an HTTP/2 stream reset). A streamed body
  is its reader's to bound.
- `store::{MemoryKvStore, MemoryConversationStore}` (feature `memory`).
- `store::{PostgresKvStore, PostgresConversationStore}` (feature
  `postgres`, sqlx, embedded migrations, `wa_` table prefix configurable).
  Migration 4 adds `wa_window_events`, `wa_thread_owners`,
  `wa_synced_contacts` (keyed by business number and contact; a window
  event by business number and id) and `wa_identity_links`, a nullable
  `wa_messages.sender` (back-filled from the payloads already stored,
  indexed with the business number; a row the back-fill could not read,
  or that the previous revision wrote, gets it from the next erasure on
  its number, in Rust) and the indexes purge by age reads; it changes no
  existing column, so the previous revision keeps working beside it
  (writing no sender), but its `migrate` refuses the database.
  `erase_all` and `purge_before` delete in one statement each, under two
  transaction-level advisory locks (design D33): the **number lock**
  (`append` and `append_synced` take it shared, `erase_all` exclusive, so
  an append in flight is either erased with the rest or recorded after
  the erasure with a summary of its own, never left without one) and the
  **purge lock** (`purge_before` and `trim_conversations` exclusive,
  `erase_all` shared: purges and trims take turns, and an erasure and a
  purge never deadlock on rows their plans visit in opposite orders); a
  trim ranks each conversation's rows on the conversation index (no
  index of its own, no migration) and deletes, by id, only rows of its
  snapshot, so an append meanwhile is never lost, and a summary goes only
  while it still names a deleted message (an append that moved it keeps
  it); the group messages an erasure
  redacts or deletes are the number's, so the number lock covers them;
  `set_thread_owner` and `put_contact` one upsert
  whose update applies only when the stored record is not later.
- `store::{SqliteKvStore, SqliteConversationStore}` (feature `sqlite`,
  roadmap L8; sqlx's SQLite driver with the library compiled in, no TLS;
  embedded migrations of their own, `migrations-sqlite/`, under the same
  `TablePrefix`). For one process on one machine (a CLI, an edge or
  desktop deployment, a small single-instance service, tests); several
  processes may share the file, never over a network file system. Every
  write is one `BEGIN IMMEDIATE` transaction: SQLite's write lock is
  taken first, so each write reads, decides in Rust and writes as one
  step, across processes too (`store::sqlite::open` sets a 30-second busy
  timeout). Times are `INTEGER` microseconds; identifiers `TEXT`
  compared byte for byte, refusing U+0000 as on Postgres; content
  `BLOB` (UTF-8 bytes) and payloads JSON text, so U+0000 round-trips.
  The key/value store decides expiry with the application's clock (an
  injectable `Clock`) and draws versions from one counter row
  (`wa_kv_version`) under the write lock. `store::sqlite::memory` is a
  private in-memory database on one connection the pool never closes.
  `trim_conversations` (roadmap B4) is one write transaction too: it
  ranks each conversation's rows on the conversation index, deletes the
  summaries whose latest message ranks beyond the cap, then those rows;
  appends wait for the write lock, so none is ranked half-way or lost.
- `store::RedisConversationStore` (feature `redis`, roadmap L8): every
  key under `<prefix>{conversations:<prefix>}`, one hash tag per store
  (it names the prefix), so a store's inbox is one slot (one shard on
  Cluster; stores of other prefixes hash elsewhere, which spreads inboxes) and every write is one Lua script over it; status updates are
  a compare-and-swap decided in Rust, as on Postgres; history, the
  conversation list, window events and contacts page with
  `ZRANGEBYLEX` over sorted sets of 16-hex-digit times and ids (byte
  order is time order); the scripts compare strings byte by byte, never
  with Lua's locale-dependent `<`. A purge of every number runs one
  script per number. `trim_conversations` (roadmap B4) runs one script
  per conversation (from a set of each number's conversations with a
  history, `B:c:<pn>`), each deleting at most 1024 of its oldest rows
  beyond the cap (and the summary when its latest message is one of
  them), so a conversation further over takes several; each is atomic
  with the appends, so an append is never lost. Needs `noeviction` and
  persistence (AOF or snapshots): the history is data.
- `media::FfmpegConverter` (feature `ffmpeg`): core's
  `media::MediaConverter` port over an `ffmpeg` binary the integrator
  installs, run with `tokio::process` (no shell) and fixed literal
  arguments; nothing is linked. The input goes on stdin and the result
  comes from stdout (no file); `-protocol_whitelist pipe` and a
  `-format_whitelist` of recording formats keep a crafted input from
  making ffmpeg open a file or a URL (MP4 data references, the one
  reference those formats carry, are not followed: ffmpeg's defaults,
  pinned by a test); the process gets no environment but `PATH` (no
  `FFREPORT`); input, output (the upload limit), run time and any single
  allocation in ffmpeg (`-max_alloc`, 16 MiB: a header declaring an
  absurd sample rate) are bounded, the process killed past the output
  and time bounds; an MP4 whose index follows its media data is put
  index first in memory before ffmpeg reads it (a pipe cannot seek
  back); its stderr is drained and never kept (it names files); an input
  that decodes to no audio fails (`-abort_on empty_output`), and the
  output is checked to be mono Ogg Opus. Images (a static WebP sticker,
  a JPEG, a PNG; roadmap L19b) are made from a PNG or a JPEG only:
  checked by the input's first bytes before ffmpeg starts, and by
  ffmpeg's own detection (`-format_whitelist png_pipe,jpeg_pipe`); a
  PNG walked chunk by chunk first (one cut short refused, its text and
  EXIF chunks, any colour profile over 16 KiB or after the first, and
  anything after its end not passed on: ffmpeg inflates every `zTXt`
  and every profile and keeps them); a JPEG walked segment by segment
  (one cut short, or with a length running past the end, refused; only
  the segments ffmpeg reads whole passed on, its tables, frame header,
  scans and their data, the JFIF and Adobe segments at their fixed
  size, its colour profile when it is one chunk of at most 16 KiB (as a
  PNG's: each keeps one profile of at most 16 KiB; a profile in several
  chunks is dropped), and in place of its EXIF a minimal one of the
  Orientation alone:
  ffmpeg copies an EXIF value once for every entry pointing at it, and
  reads every `APP1`); the
  first frame, no metadata, at most 32 Mi pixels (`-max_pixels`) and
  256 MiB a single allocation; brought under the format's limit by a
  fixed ladder of sizes and qualities (each attempt killed once past
  it, the whole ladder within the timeout), the result's header
  checked. An animated sticker (roadmap L19c) is made from a GIF only
  (its first bytes, then `-format_whitelist gif`; ffmpeg 6.1 reads no
  animated PNG or WebP from a pipe), walked block by block first: one
  cut short or with a block GIF does not define is refused, and so is
  one out of bounds (a canvas over 2 Mi pixels, fewer than 2 frames or
  more than 500, more than 100 of 512 × 512 or larger, longer than
  10 s), and ffmpeg gets it without its comments, plain text and
  application extensions, with only the last graphic control extension
  before each frame, and without anything after its trailer (ffmpeg holds them in memory with the first frame);
  `libwebp_anim` on a fixed ladder of sizes, frame rates and qualities,
  256 MiB a single allocation (the whole file is one packet), and the
  result's chunks checked (`VP8X` with the animation flag, `ANIM`
  before the `ANMF` frames, none outside them); frames all alike, which
  libwebp makes a still image of, are refused. An empty or oversized
  input (or a result over the limit
  after every attempt) is a `ValidationError` on `input`; every other
  failure an `Error::Conversion` (see Error tree).
- `store::RedisKvStore` (feature `redis`; CAS via Lua). Requires the
  `noeviction` policy: every key with a TTL here enforces a limit (OTP
  issue logs, dedup markers), and Redis evicts silently.
- `sink::{ChannelSink, BroadcastSink, FanoutSink, FilterSink, FnSink,
  TracingSink}` — feature `sinks`, generic over the event type, `Debug`
  redacted. `BroadcastSink<E>` is still an `EventSink<E>` (the port is
  unchanged) but wraps each event in one `Arc` and publishes
  `Arc<E>` on its tokio channel, so its subscribers and the SSE helper
  share that allocation. (The inbox sink needs webhook event types, so it lives in the
  facade: `meta_whatsapp_rs::inbox::InboxSink`.)
- Postgres: one table-wide version sequence (versions never reused, even
  after purge); deleted keys leave marker rows purged after 10 minutes;
  migrations are templates with a validated table prefix and a per-prefix
  history table, applied under a database-wide lock. **Message content
  keeps U+0000** (decided by the owner, formerly `OPEN_QUESTIONS.md` #18):
  Postgres `text` cannot hold it and `jsonb` refuses a `\u0000` escape, so
  `kind`, `text` and the summary's preview are `BYTEA` (`kind_utf8`,
  `text_utf8`, `last_text_utf8`, the UTF-8 bytes) and `payload` and
  `error` are `json` (`payload_json`, `error_json`, the text as written);
  migration 3 converted the older `text`/`jsonb` columns in place, in one
  transaction, and existing rows kept their content (a U+FFFD an older
  revision stored for a NUL stays U+FFFD); it refuses to run under an
  object of the operator's own that depends on the payload or error
  column (an expression index there would fail every later insert of a
  payload holding a NUL), and a content column that is not UTF-8 reads as
  `StorageError::Corrupt`. The cost is SQL-side: search
  on bytes (`position(convert_to(…) IN text_utf8)`), no index or field
  extraction on a payload holding a NUL. Ordering never involves content.
  **Identifiers refuse U+0000**: ids, contacts and phone number ids stay
  `TEXT COLLATE "C"` (Meta never assigns one with a NUL), as do
  `KvStore` keys; memory and Redis accept it there, and the key/value
  conformance suite allows either answer but never another key.
  `KvStore` values are `BYTEA`. `messages.id` is the primary key on its
  own, so
  a message id is stored once per store whatever the business number (the
  memory store does the same; `OPEN_QUESTIONS.md` #33). The SQLite and
  Redis stores (roadmap L8) key a message by business number and id and
  keep that rule with a separate index (`wa_messages_id_idx`, unique;
  `{conversations:<prefix>}:id:<id>`), which ids unique per number would drop.
  `conversation_conformance::ids_are_stored_once_per_store` (outside
  `run`: the port allows either answer) pins today's rule; every adapter
  passes it.
- Redis: per-namespace version counter (a per-key counter would leak one key
  per dedup marker forever); every mutating op is one Lua script. **TLS**
  (`rediss://`, roadmap L21c) is `store::RedisTls`, not redis-rs's TLS:
  redis-rs builds its rustls configuration from the process-wide default
  provider, which panics when both aws-lc-rs and ring are linked and none
  was installed. `RedisTls` builds the configuration on an explicit
  aws-lc-rs provider, does the handshake (tokio-rustls) and hands the
  stream to redis-rs's `MultiplexedConnection::new_with_config`;
  `RedisTlsConnection` reconnects on its own. An integrator-built
  connection still works.
- MongoDB (feature `mongodb`, roadmap S17; `store::mongodb`, on a replica
  set): `MongoKvStore` makes every operation one document's atomic update
  (an aggregation pipeline computing version, value and expiry from the
  document as it was, against the server's `$$NOW`), compare-and-swap
  filtered on the expected version of a live record; a delete leaves a
  tombstone keeping the version, which a TTL index removes ten minutes
  later, and a version is the larger of the stored one plus one and the
  server's milliseconds times 2^20, so a key's versions keep increasing
  after its document is gone (as long as the primary's clock does not go
  back by more than those ten minutes). `MongoConversationStore` writes
  what touches several documents in one multi-document transaction
  (snapshot reads, majority writes), tried again after a write conflict
  for a budget (10 s, `with_busy_after`), then `StorageError::Busy`; every
  append writes its conversation's summary (its `appends` counter), since
  snapshot transactions conflict on writes only: that write orders an
  erasure against an append of its conversation, as Postgres' number lock
  does. Content is BSON strings, U+0000 kept, payloads and errors their
  JSON text; identifiers are kept exactly too; instants are milliseconds;
  ids once per store (the message's `_id`). The collections are named
  after the Postgres tables, with no prefix setting.
- reqwest: errors never carry the request URL; redirects send no `Referer`
  (it would carry the query to the redirect target), and a request that
  carries a credential header follows no redirect that would keep it (see
  "Credentials never leave Meta"); `HTTP(S)_PROXY` is honoured.
- Live tests are named `live_*`, read `META_WHATSAPP_RS_TEST_POSTGRES_URL` /
  `META_WHATSAPP_RS_TEST_REDIS_URL` / `META_WHATSAPP_RS_TEST_REDISS_URL` (a
  TLS Redis requiring client certificates; its CA and client PEM files in
  `META_WHATSAPP_RS_TEST_REDISS_CA`, `…_CLIENT_CERT`, `…_CLIENT_KEY`) /
  `META_WHATSAPP_RS_TEST_MONGODB_URL` (a single-node replica set, with
  `directConnection=true`; roadmap S8), skip when unset, and **fail** when unset under
  `META_WHATSAPP_RS_REQUIRE_LIVE=1` (`just test-live` sets it). Every `KvStore`
  adapter runs `store::conformance`, every `ConversationStore` adapter runs
  `store::conversation_conformance` (including concurrency, collation and
  real-time expiry under load). The SQLite adapters run both in
  process (`just test`); the Redis conversation store's suite is live
  (`live_redis_conversation_*`), and so are the MongoDB stores' suites
  (`live_mongodb_kv_*`, `live_mongodb_conversation_*`, roadmap S17).

## CMS inbox (`meta_whatsapp_rs::inbox`)

`InboxSink` (an `EventSink<WebhookEvent>`) records inbound messages,
status updates, and coexistence echoes and history into a
`ConversationStore`, and beside them (roadmap L7) the calls and standby
messages that reopen the window, thread ownership under Conversation
Routing, and the links between a customer's identities; `Inbox` (one per
merchant phone number, built with that merchant's token) lists
conversations and history, exposes the 24-hour `CustomerServiceWindow`
and the thread's owner, and sends replies.

- The library knows WABAs and phone numbers, not the integrator's tenants.
  Whoever builds an `Inbox` for a request first checks that the
  authenticated tenant owns that phone number, *before* reading the token
  vault (whose tokens belong to every merchant). The `cms_inbox` and
  `embedded_signup` examples show where: a bearer-token stand-in for the
  CMS's sessions, the tenant taken only from it, and a tenant → phone
  number table.
- Keys: business phone number id + contact, where contact is the BSUID
  when Meta sent one, else the `wa_id`, else (group messages) the group id.
- Replies to a `wa_id` go to `+<wa_id>` (Meta prepends the business
  number's country code to numbers without `+`). `Inbox::send` refuses a
  message not addressed to the conversation's contact.
- Free-form replies outside the window are refused locally
  (`ValidationError::customer_service_window_closed()`, kind
  `CustomerServiceWindowClosed`); templates and Direct Send `utility` and
  `authentication` are exempt (a Direct Send `service` message is not:
  Meta drops it outside the window). The window opens from the latest
  of the recorded inbound messages and the
  conversation's window events: the calls that reopen it on Meta's side
  (`calling/pricing`: the customer's call, answered or not, and the
  customer accepting the business's call) and the customer's messages
  seen in standby, which are never history nor unread
  (`OPEN_QUESTIONS.md` #32, #44). After a handover to this app newer than
  the customer's last recorded message, the window check lets Meta
  decide until that message or 24 hours after the handover
  (`ReplyChecks::trust_handover`): an app without standby copies never
  saw the customer's messages to the previous owner.
  `Inbox::check_reply` runs `reply`'s checks without sending.
- Conversation Routing: `InboxSink` records ownership from the handovers
  (`control_passed`: this app; `control_taken`: another app) and from
  standby copies (another app); a handover names the customer by phone
  number only and is recorded under the key that number leads to (the
  identity links, else a synced contact's BSUID, else the phone number).
  `Inbox::thread_owner` derives the rest at read time: a later message on
  `messages` means this app owns the thread (not a call permission
  reply, which Meta also sends to the Incoming Call primary and the
  standby partners), 24 hours without the customer mean it is idle;
  `Inbox::record_release` and `Inbox::record_thread_owner` record this
  app's own `release`, `pass` and `take`, which no webhook reports to it.
  `Inbox::send` refuses a
  service message locally while another app owns the thread
  (`ValidationError::thread_owned_elsewhere()`, kind
  `ThreadOwnedElsewhere`); templates and Direct Send `utility` and
  `authentication` need no ownership. Both local checks are advisory
  (Meta enforces them) and `ReplyChecks` turns each off per inbox: the
  escalation partner's ownership check, and the ownership check of an app
  sharing the number with a Meta Business Agent without routing.
- Every rule is swappable: `InboxSink::with_recording` turns off the
  recording of calls, standby copies, handovers or identity links (you
  record that kind yourself, through the same port methods, reusing the
  public rules `call_window`, `call_key`, `call_status_key`,
  `handover_key`); `Inbox::with_thread_idle_after` sets the idle
  timeout; `ReplyChecks` switches each local check (window, trusted
  handover, owner).
- Identity links: `InboxSink` links a phone number to the BSUID an
  inbound message carries with it, a previous BSUID and the update's
  `wa_id` to the current one (`user_id_update`), and a number change's
  old identity to the new one
  (a `system` message), so that `identities` and an erasure reach every
  key of a customer; never an empty value, one with U+0000, or a value
  to itself.
- Statuses and revokes change only a message of the business number they
  arrived on (`update_status` takes the `phone_number_id`); a revoke also
  only a message of its direction (`ConversationStore::revoke`: a customer
  revokes what they sent, the business what it sent).
- Content never fails a delivery: it is recorded exactly, U+0000
  included, and every adapter stores it (see Adapters). Storage errors
  still do (500, Meta redelivers the batch), as does, on Postgres, U+0000
  in a Meta-assigned id of a live message (`OPEN_QUESTIONS.md` #30: the
  inbox's sink classifies no error as permanent yet, so the dead letters
  of roadmap L21a do not take it; a history item with one is skipped,
  below).
- A storage (or serialization) failure *after* a successful send is
  logged without the message's content, never returned — an error would
  invite a retry that sends twice.
- Revokes mark the original `Deleted` if it was stored for the revoke's
  number and in its direction, whatever conversation key the revoke
  arrived with (a history thread keyed by phone number and a live revoke
  keyed by BSUID can be the same customer's), and keep its text and payload
  for the merchant's records: both the owner's decisions (2026-09-25), the
  first pinned by the conformance suite. A revoke that
  arrives before its message stores a tombstone (kind `revoked`, no
  content, `Deleted`) under the message's id, so the message, live or
  synced, is never stored with the content its sender deleted. The
  tombstone is history only, never in the conversation's summary (no
  latest message, preview, window or unread count, and no summary of its
  own). A media placeholder revoked before its content arrives is never
  filled.
- The coexistence address book (`smb_app_state_sync`, roadmap L8):
  `InboxSink` stores an `add` (also how an edit arrives) as a synced
  contact under `synced_contact_key` (the BSUID, else the phone number
  without `+`, the form conversations are keyed by, also the phone number
  stored) and applies a `remove` under each key it names, so that an
  entry keyed by the phone number before Meta sent the BSUID goes too;
  the store keeps the latest by the change's `metadata.timestamp`, a
  removal kept (key and time) so that an older `add` is refused. A
  change without a timestamp, without a usable id, or with U+0000 in
  one is skipped and logged without content. `Inbox::contacts` and
  `Inbox::contact` read them back; `RecordingSwitches::contacts` turns
  the recording off.
- Coexistence (a merchant who keeps the WhatsApp Business app): echoes
  (`MessageEchoed`, messages the merchant sent from the app) are recorded
  as `Outbound`, status `Sent` (the payload has none), in the customer's
  conversation (BSUID, else `wa_id`/`to` without `+`); an echoed revoke
  deletes the original. Synced history (`HistorySynced`) is recorded
  message by message in its documented direction (`from` = the business
  number → `Outbound` with its `history_context` status; else `Inbound`),
  each with its own device timestamp (bounded by the sink's clock plus 5
  minutes, so a phone with a wrong clock cannot pin a conversation to the
  top), so chunks may arrive in any order and a redelivered one is a no-op
  (ids are stored once); the exception is a revoke in a chunk that arrives
  before the chunk carrying its message, which leaves a tombstone in the
  message's place. Each chunk is one `append_synced` batch (the Postgres
  adapter: one statement), then its revokes: a history webhook can carry
  thousands of messages, and one round trip each could outlast the
  webhook's dedup lease. A declined sync
  (`2593109`) records nothing. One malformed history item never fails the
  delivery: an item without a direction or customer, with U+0000 in an
  id, or that does not parse on its own (when one bad item turned the
  whole value into `WebhookEvent::Unknown`) is skipped and logged by
  position, without content; storage errors still fail it.
  Synced history goes through `ConversationStore::append_synced`: an
  inbound synced message neither moves `last_inbound_at` (Meta opens no
  window for a message sent before onboarding,
  `embedded-signup/onboarding-business-app-users`) nor counts as unread
  (the merchant read it in the app). The media content Meta sends after a
  `media_placeholder` (`history`'s `messages` / `message_echoes`) replaces
  the placeholder's kind, text and payload through
  `ConversationStore::fill_media_placeholder`; the row keeps the thread's
  conversation, direction, status and timestamp. Both are part of the
  executable conformance suite, so every adapter proves them. The Postgres
  adapter needs no schema change: the summary is maintained when a row is
  written, so the difference lives in the write.

## Bot framework (`meta-whatsapp-bot`)

A `Bot` is an `EventSink<WebhookEvent>`, so it sits behind
`WebhookHandler` and its `DedupGuard` like any sink. Per event, in this
order:

1. **Ban.** A received message from a banned sender stops here: no match,
   no middleware (so no read receipt or typing indicator), no command, no
   listener. `Refusals` hears of it as `Refusal::Banned`.
2. **Match.** For a received message: typed text or an image or video
   caption (unless `BotBuilder::commands_from_captions(false)`) the
   `CommandParser` accepts, or a reply button, list row or
   template quick-reply button whose id is a registered payload. A match
   sets `Ctx::invocation`; a parsed name no command has sets
   `Ctx::unknown_command`.
3. **Middleware**, in registration order; each gets the context (the
   match included) and `Next`, and not calling it stops the event.
4. **The command's guards**, in order (scope, owner, undeclared flags
   on a command that declares any, cooldown last so a refusal starts
   none; for a subcommand, its parent's scope and owner first; a command
   that declares no flag reads none), then its handler; else the
   unknown-command handler, if one is registered; else the listeners,
   which get every other event too. A subcommand's cooldown is kept
   under the key of its path (`order status`), the same digest over
   `wa.bot.cooldown`, so a command's key does not change when it gains
   subcommands.

Standby copies, echoes and synchronized history are other events: they
never run a command. Zaileys, whose framework features this mirrors,
runs middleware for commands only and after their guards; here it runs
for every event, after the ban and the match.

- **Traits with defaults**: `Outbound` (`ClientOutbound`, the client's
  `messages(pn).send` and read receipts), `CommandParser` (`PrefixParser`;
  it also normalizes names, so case folding is its option), `AccessPolicy`
  (`AccessList`), `Cooldowns` (`KvCooldowns`), `Refusals`
  (`ReplyRefusals`), `ErrorHandler` (`LogErrors`), `MarkdownRenderer`
  (`markdown::Renderer`) and its `Escape` (`NoEscape`; `WordJoinerEscape`
  opt-in), `HelpFormatter` (`CategoryHelp`; the help command's name,
  description and default category are builder options). Each has an
  `Arc<T>` implementation, so one instance serves several bots. Not
  traits: the order above, and the reply helpers' target (the group, else
  the BSUID, else `+<wa_id>`, quoting), which `Ctx::send` bypasses.
- **Identity** is the BSUID first (`BotSender::key`), else `wa_id`. Bans
  and owners listed by phone number cannot match a sender without one.
- **Cooldowns** are a typed store on `KvStore` (namespace
  `wa.bot.cooldown`, key: SHA-256 of the length-prefixed business
  number, command and user key, so no phone number is stored), started
  with `put_if_absent`: atomic across instances sharing the store. A
  refusal writes a marker in `wa.bot.cooldown.notice` expiring with the
  cooldown, and only the refusal that creates it tells the user, so
  repeated attempts get one notice per period. Records expire in place:
  invisible at once, deleted by Redis itself or by `purge_expired` on the
  memory, Postgres and SQLite stores.
- **Errors**: a handler's error is logged (kinds only) and acknowledged
  by default, because an error answers `500` and Meta redelivers the
  whole batch, repeating replies already sent; a transient failure is
  then lost but for the log (the webhook dead letters of roadmap L21a take
  only an error a sink classifies as permanent, and the bot classifies
  none). `PropagateErrors` opts into
  redelivery. The `ErrorHandler` gets the context from before the
  middleware (the match set, a middleware's values not). After
  `Bot::unload`, delivery fails with `SinkError::Closed`.
- **Plugins** are compiled in (`Plugin::setup` registers commands,
  middleware and listeners; `on_unload` at `Bot::unload`). No dynamic
  loading or hot reload: Rust has no stable ABI.
- **Command menu**: `Bot::sync_command_menu` sends the visible commands
  through the client's conversational automation call, under its limits
  (30 commands, names up to 32 characters, descriptions 1 to 256).
- **Markdown**: `markdown::Renderer` (pulldown-cmark) converts CommonMark
  to WhatsApp formatting and packs blocks into messages of at most 4096
  (the client's `TEXT_BODY_MAX_CHARS`), measured in UTF-16 code units
  since Meta does not say which unit it counts (never fewer than the
  client's count of characters), never cutting a code block that fits.
  Code a fence cannot hold and emphasis inside a word go out as plain
  text. A table is padded columns while a padded row fits 60 characters,
  else one `header: value` line per cell, and either only while it is at
  most twice the table's unpadded rows (or one message), else those rows:
  a reply's length, so its number of billable sends, stays proportional
  to the Markdown's (both limits are `Renderer` options). Meta documents
  no escape syntax: the default leaves text as written, so copied
  addresses, codes and commands work;
  `WordJoinerEscape` (U+2060 around literal markup characters, copied
  along with the text) is the opt-in.
- **Listeners** name what they get: received messages, one message
  `type`, an event kind (checked against `WebhookEvent::KINDS` at build),
  or everything.
- **Paced broadcasts** (`broadcast`, `pacer`): a `Broadcast` sends one
  message, or one per recipient, from one business number through an
  `Outbound`, each send (retries included) after a slot of the number's
  `Pacer`, up to a configurable number in flight; `run` returns a report
  per recipient, and a `BroadcastHandle` gives progress and a cancel
  (no send starts after it, and the slots waited for go back; sends in
  flight finish). The pacer is a `RateLimiter` (default `TokenBucket`:
  per number, in this process, evenly spaced at `Rate::DEFAULT`, 80 a
  second, Meta's `throughput` default, with per-number rates settable
  while running, a burst and a slow-down on throttling whose factor,
  spacing and recovery are settings), a `SlowDownRule` (which errors slow
  a number down, default `ThrottlingErrors`) and a `Timer` (a `Clock`
  that can wait until a deadline: `SystemClock` on Tokio, `ManualClock`
  in tests). A limiter reserves (takes a `SlotRequest`: number, now,
  cost; returns a `Reservation` with the wait) rather than waits, and
  can take an unused slot back, so a shared one, across replicas, is a
  `RateLimiter` too: `KvRateLimiter` (opt-in, roadmap B2b), a typed store
  on `KvStore` (namespace `wa.bot.pacer`, no new port) keeping each
  number's schedule, slow-down and raised rate as a versioned record
  booked by `compare_and_swap`, a bounded number of attempts under
  contention (then `StorageError::Busy`), failing closed when the store
  does, and assuming the replicas' clocks synchronized (a skew lets up
  to `⌈skew × rate⌉` more into a one-second window); without it each
  replica has its own budget. `RateLimiter::raise_rate` (default: raises
  nothing) is how a throughput upgrade reaches a limiter, in this
  process (`TokenBucket`) or for every replica (`KvRateLimiter`), never
  lowering a rate. Failed sends go to a `BroadcastPolicy` (default
  `Backoff`, on `ErrorKind`: the pair limit defers one recipient on
  Meta's `4^X` schedule, throughput retries, a number in maintenance
  (`131057`) is retried every 20 s, the per-user marketing limit is
  reported, number-wide kinds stop the run and so do a template's own
  refusals when everyone gets the same message; every delay capped), but
  a send is repeated only when `Error::may_resend` holds, whatever the
  policy (enforced outside it; the client's rule too).
  `BroadcastBuilder::client` turns the client's own replays off
  (`Client::with_retry`), so every retry is paced. A person listed twice
  is sent once (`BroadcastBuilder::dedupe`); each line can go to a
  `ReportSink` as it settles instead of the report
  (`BroadcastBuilder::report_to`), so memory does not grow with the
  lines; the run keeps its own time, so a wall clock stepping back
  neither pauses the pacer nor puts off a retry.
  `BotBuilder::pacer` wraps a bot's outbound in a `PacedOutbound`, so its
  replies, read receipts and typing indicators share the budget and slow
  it down when throttled (a bot built on a client retries there, each
  retry paced, instead of in the client); `Ctx::pacer` hands the pacer to
  handlers; `PacedGroups` and `PacedGroup` wrap the client's group
  operations the same way, and `Pacer::acquire` is the hook for any other
  call. No new port and no persistence: a run lives in memory.
- **Durable jobs** (`jobs`, roadmap B3, B3b; design D29): a `JobStore`
  keeps `Job`s (a number, a due time, the recipients each person once, a
  JSON payload); by default `KvJobs`, a typed store on `KvStore`, no
  port of its own. `KvStore`
  has no range scan, so due times are bucketed by the minute: a job's id
  is appended to its minute's page by compare-and-swap (256 ids a page,
  then the next page; a due time already past, to the current minute's),
  and a `cursor` record names the oldest minute still to read. A `JobRunner` reads from the cursor up to now, claims
  each due job by compare-and-swap (pending, or claimed under an expired
  lease, to claimed with a lease naming it), and sends through a paced
  `Broadcast` whose outbound checks before each send that the job is
  still its own and not cancelled (renewing the lease) and creates the
  recipient's marker with `put_if_absent` before the request, then
  reads the job once more (a cancel or a re-run meanwhile: no request):
  a "sending" marker nobody completed is an outcome unknown, never
  resent (at most once, not exactly once). It moves the cursor past a
  minute whose jobs are all settled, keeping that minute's page
  versions in it, reads that minute again and moves it back if it
  changed meanwhile (every tick starts by the same second read, so a
  runner failing between the two loses nothing), and indexing into a
  minute behind the cursor moves the cursor back, so no job is skipped
  by a race. A pass that leaves recipients provably unsent
  (`Error::may_resend`, a stop, a store failure) is retried with backoff,
  then stopped. A message's id is kept for its status webhooks: a
  `131049` marks that recipient for 24 hours (`JobStore::observe`,
  `JobFeedback`), and a re-run (`JobStore::rerun`) skips them. The job
  store is a trait (`JobStore`, B3b), object-safe and held by the runner
  as an `Arc<dyn JobStore>`: scheduling, cancel, re-run, reads, the
  `131049` feedback, and the runner's own steps (claiming what is due,
  holding the claim, each recipient's marker, settling), with the
  contract under concurrency in its rustdoc (claims atomic under a
  lease, a claim checked on every write, markers created atomically
  before the send and only their runner's, cancels winning or losing
  whole) and `jobs::conformance` its executable form, which `KvJobs`
  and `MemoryJobs` (in memory behind one mutex, independent of
  `KvStore`) pass. The runner's outbound, pacer, failure policy and
  composer are traits too.

## Typst (`meta-whatsapp-typst`)

Render a Typst source with JSON inputs (`sys.inputs.data`, read with
`json(bytes(sys.inputs.data))`) to PDF or PNG, with bundled fonts so output
is byte-identical on every machine. `today()` comes only from
`Renderer::with_today` (never the system clock). The world is a sandbox: no
file reads beyond the main source, no packages, no network; PNG size is
bounded (`MAX_PNG_PIXELS`) and ppi validated before any allocation; the main
file is always `/main.typ` (typst interns file ids process-wide). Ships templates for
e-commerce: `invoice`, `receipt` (order confirmation), `voucher` (coupon /
gift card image for marketing headers). Output is bytes + MIME + filename,
ready for `media().upload()` and a document/image message or template
header. Never renders OTP codes (those go through authentication
templates only).

## Service (`meta-whatsapp-server`)

Apps not written in Rust use meta-whatsapp-rs through a service, deployed
as a container and called over HTTP. Its design, with the owner's
decisions and the delivery milestones, is
[docs/design/server.md](design/server.md); what is built so far is in
[docs/coverage.md](coverage.md). The rules that bind it to the library:

- **A family of crates on the facade.** The service's crates depend on
  `meta-whatsapp-rs` and on each other, never on another crate of the
  library, and no library crate depends on them. Each is `publish =
  false`, a workspace member (so `just ci` covers it) but not a default
  one. What the service cannot build from the facade, an integrator could
  not either: the gap is a library change of its own, not a private
  shortcut.
  - `crates/meta-whatsapp-server-core`: the framework-free core. The
    domain (tenants, keys, bindings, idempotency records), the
    authorization order (credential to `Caller`; ownership to
    `OwnedNumber`/`OwnedWaba`, the only way to a vault token; each
    capability works only with the `Authorizer` that made it, which
    refuses another's with `403`; an adapter that takes a capability
    from outside its own code calls `Authorizer::admit` or
    `admit_admin` before acting on it, as the service's extractors do:
    design §8.1), event
    routing and polling, the idempotency engine (a key's fingerprint
    the operation and its canonical input, since S6), the rate limiter
    (a request charged by its operation's class, since S6), the error
    model as data (`ServiceError`: an `ErrorCode`, one constant per row
    of `CODES`, generated from one table since S6, its status as a
    number, `retryable`, `may_have_been_sent`), and the operations
    as services an API adapter parses for, calls and renders: since
    roadmap S5a the admin ones (`admin::Admin`, opened with an
    `AdminCaller` its `Authorizer` admits), since S5b the messages
    (`messages::Messages` on an `OwnedNumber` its `Authorizer` made and
    admits, over the send request
    `messages::SendMessage`, which names no API toolkit's type: an
    adapter converts its own request into it) and running an operation
    under an idempotency key (`idempotency::run`, for a `Caller` its
    `Authorizer` admits: a status, a body, and whether it replayed),
    since S5c the numbers (`numbers::Numbers` on an admitted
    `OwnedNumber`: details, the business profile read and changed;
    `numbers::Waba` on an `OwnedWaba` its `Authorizer` admits,
    `admit_waba`: disconnecting) and the templates
    (`templates::Templates` on an admitted `OwnedWaba` of the admitted
    `Caller`'s own tenant, over the adapter's `templates::TemplateCache`:
    the list, the lookup of an id in its WABA, create, delete), and
    since S5d the media (`media::Media` on an admitted `OwnedNumber`,
    with the adapter's transfer slots: an `Upload` checked before any
    request, a download verified against Meta's SHA-256, whole or as
    `media::MediaStream`, a `futures::Stream` of the library's `Bytes`
    held one chunk behind, which ends with a `StreamStopped` instead of
    its last chunk on a mismatch, and a deletion of the number's own
    media only), and since S5e the webhook pipeline (`inbound::Pipeline`,
    built on a `Backend` alone: a delivery's signature checked against
    every app secret before any store is touched, then the library's
    `WebhookHandler`, each event under its dedup lease, routed by the
    records, never by the payload, into the inbox and the outbox; the
    answer, `Delivered` or a `Refused` with its status, only once every
    event is recorded), and since S6 the tenant operations by id
    (`operation::Operation`: the HTTP adapter's `operationId`s), which a
    request's rate class and its idempotency fingerprint follow whatever
    the adapter, with the `Authorizer`'s writes to the vault reached
    only through `admin::Admin`. It depends on the
    facade without its adapters' features: no axum, no sqlx, no utoipa
    (the `http` crate is in its graph only through the library, whose
    `HttpTransport` port and client speak it; no `http` type crosses the
    core's API).
  - The backends (roadmap S7), each on the core and the facade alone:
    `crates/meta-whatsapp-server-store-memory` (`MemoryBackend`,
    development and tests) and `crates/meta-whatsapp-server-store-postgres`
    (`PgBackend` through sqlx, the facade's
    `meta_whatsapp_rs::adapters::store::postgres::sqlx`, with the
    service's migrations and their migrator). Each runs the core's
    conformance suite in its own tests (live for Postgres). Beside them,
    composed into no binary, `crates/meta-whatsapp-server-store-mongodb`
    (roadmap S9), a spike: `MongoOutbox`, the `Outbox` port alone on a
    MongoDB replica set through the `mongodb` driver (no `ring`), every
    write a multi-document transaction; it writes the guarded binding's
    document with each insert (MongoDB's snapshot transactions see
    write-write conflicts only) and draws the sequence from its stream's
    document. Its live tests run the conformance suites with it as a
    bundle's outbox beside the memory backend's other ports, whose
    bindings the test fixture writes into the outbox's database
    (`MongoOutbox::record_binding`) until S18 brings the records.
  - `crates/meta-whatsapp-server-http`: what every API adapter shares
    over HTTP (axum through `meta_whatsapp_rs::webhooks::axum`): the
    state (`AppState`, composed from a `Backend`, and the `Grant` its
    composer is handed, the only way an API adapter's handlers reach the
    records, the authorization order and the idempotency engine), the
    listeners' accept loop, telemetry and metrics, authentication before
    the body (the guards and extractors), the §5 renderer of the core's
    errors, the webhook listener (the signature header's shape before
    the body, the places, the body's size and read time, the refusals'
    log, over the core's pipeline), media, and the operations routes.
  - `crates/meta-whatsapp-server-api-axum`: today's `/v1` resource routes
    and the OpenAPI document generated from them (utoipa), with the HTTP
    crate's media and operations groups; permanent, the swap target
    (design D26).
  - `crates/meta-whatsapp-server-api-cratestack` (roadmap S10): the API
    on CrateStack, its contract `schema/api.cstack` (`db = None`,
    procedures only, REST, JSON), compiled with CrateStack's
    `cratestack-api` (the only crate that depends on CrateStack); one
    procedure so far, `version`. S13 brings the rest of the routes,
    S16 makes it the default.
  - `crates/meta-whatsapp-server` (binary `meta-whatsapp-server`): the
    process (configuration, `serve`, the command line), composing one
    API adapter (feature `api-axum`) and the backends the command line
    can open (`store-memory`, `store-postgres`), all three by default;
    with the feature `api-cratestack` (not a default) it compiles the
    CrateStack adapter in, which nothing serves before S13. Every
    command first installs aws-lc-rs as rustls' process-default crypto
    provider (`meta_whatsapp_server::tls`, design D20 (b)): CrateStack
    links rustls' `ring` provider too, and with both rustls cannot pick
    a default by itself.
    It re-exports the moved modules under their old paths
    (`meta_whatsapp_server::api`, `::state`, `::store`, …), and keeps
    the committed `openapi/v1.json`, which its `openapi` command prints.

  ```text
  meta-whatsapp-server (binary) ─┬─► meta-whatsapp-server-api-axum ─► meta-whatsapp-server-http ─┐
                                 ├─► meta-whatsapp-server-api-cratestack ─► (the same, and CrateStack)
                                 ├─► meta-whatsapp-server-http ──────────────────────────────────┤
                                 ├─► meta-whatsapp-server-store-memory ──────────────────────────┤
                                 └─► meta-whatsapp-server-store-postgres ────────────────────────┴─► meta-whatsapp-server-core ─► meta-whatsapp-rs (facade)
  ```

  No store crate depends on the HTTP crates, and the HTTP crate depends
  on no API adapter or backend (dev-dependencies aside: its unit tests
  run on the memory backend). Every log line of a moved module keeps its
  target, `meta_whatsapp_server::<module>` as it was before the split
  (explicit since S7): an operator's filters name them.
- **Ports, and a backend as the unit of swapping.** The core stores
  through ports: `RecordStore` (tenants, keys, bindings),
  `IdempotencyRecords`, `Outbox`, `EventNotifier` (since roadmap M2b:
  what wakes every replica's live event streams on a new event, a
  wake-up only, the outbox staying the source of truth; Postgres
  `LISTEN`/`NOTIFY`, a broadcast in memory), `LeaderLock` (a lease,
  `try_exclusive(name, lease)`), `Janitor` (expired rows nothing else
  deletes) and `SchemaMigrator`. A `Backend` gives all of them, and the
  library's `KvStore` and `ConversationStore`, over one database, because
  they share its guarantees (deleting a tenant reaches its idempotency
  records and its event stream; every outbox insert checks the routing
  again against the bindings, atomically with the insert). The service
  has two, memory and Postgres, and is composed from a backend alone
  (roadmap S4): its state (`AppState::from_backend`) builds the token
  vault and the webhook pipeline on the bundle's stores, `serve_with`
  serves, migrates and closes it, housekeeping runs over it (the core's
  `housekeeping`), and the command line opens it through a
  `BackendFactory`, so no constructor takes a port on its own and a
  backend an integrator brings needs no change to the server. No port
  names a database driver's, an HTTP framework's or an API toolkit's
  type.
- **What every backend guarantees**, as the ports' contracts require it
  (roadmap S2, so that a MongoDB or a CrateStack-models backend cannot
  meet the traits without it; the `meta_whatsapp_server_core::store`
  module documents each): the outbox keeps a row's tenant only while the
  binding it was routed by (a typed `RouteGuard`) still holds, checked
  with the insert; binding a WABA to a missing tenant is
  `BindOutcome::NoSuchTenant`, and a binding and a deletion of one
  tenant serialize; one clock all replicas agree on (the database's,
  else the service's `Clock`, which the core's `Authorizer` takes
  injected); contention is the library's `StorageError::Busy`; purges
  take no lock of their own, under one `LeaderLock` turn per
  housekeeping round; listings are in byte order; each `Backend`
  accessor hands out the same data on every call; and a capability
  (`OwnedNumber`, `OwnedWaba`) writes only to the binding (a
  `BindingEpoch`) and the vault record it was made from, and is made
  only from a token read while that binding held. That last guarantee
  rests on a rule between the bindings and the library's `TokenVault`,
  which no transaction spans: the vault is written for a WABA only by
  its current holder, an attach binds, stores, then confirms (it reads
  its binding again and, moved meanwhile, takes back exactly its own
  write), and every unbind deletes the token before the binding.
  These contracts are executable (roadmap S3):
  `meta_whatsapp_server_core::conformance` (feature `conformance`) runs
  them over a `Backend`, and both in-tree backends pass it in the
  service's tests (Postgres live), as a new backend must.
- **One multi-tenant deployment per Meta app** (the owner's decision D1):
  every merchant onboarded through the app delivers to its one callback
  URL. Tenants are the integrator's ids; a tenant owns WABAs, a WABA owns
  phone numbers, and the business token stays in the library's
  `TokenVault`, keyed by WABA.
- **Two listeners.** The public one serves Meta's webhook and a liveness
  probe, nothing else; the internal one (loopback by default) serves the
  API, readiness, metrics and the OpenAPI document.
- **Credentials** (D2): tenant keys, and platform keys that name a tenant
  per request (`WA-Tenant`), within their allowed set; an admin key for
  `/v1/admin` only. Keys are stored as SHA-256 digests and compared in
  constant time.
- **Ownership before the vault.** Every route that names a number or a
  WABA checks, in this order: the key, the tenant, the scope, that the
  tenant owns the number or WABA (else `404`, the answer for one that does
  not exist), and only then reads the vault and calls
  `Client::with_token`. The library leaves this check to the integrator
  (see [CMS inbox](#cms-inbox-meta_whatsapp_rsinbox)); the service is one.
  A token may reach several tenants' WABAs, so an object a path names by
  id (a media id, a template id) must also be the number's or WABA's own,
  established through the number or the WABA (`phone_number_id={pn}` on
  every media call; a template id found in the WABA's own template list).
  The bare id is only used to read its name (a template's, which that
  list is searched by), never to act on it or to answer anything of it;
  one that is not the number's or WABA's is `404` like a missing one. A
  route for a new kind of id (flows, groups, QR codes) owes the same
  check.
- **Errors keep the library's classification.** A Graph failure's error
  code is `ErrorKind::as_str()`; its HTTP status comes from a table the
  service owns, tested over `ErrorKind::ALL` so a new kind cannot fall
  through unmapped; `may_have_been_sent` and `retryable` are
  `Error::may_have_been_sent` and `Error::is_retryable`. Meta's error
  message, title and user texts never reach a response; `graph.details`
  is Meta's text, not the service's, kept only on the routes that opt in
  (never OTP or signup), bounded and without control or format
  characters or line separators.
- **Sends are never repeated behind the caller's back.** The service
  adds no send retries to the library's; a caller's `Idempotency-Key`
  (sends, inbox replies, marking a conversation read, uploads, template
  creation) keeps the answer of a request that
  may have taken effect and replays it, and releases the key only when
  the library's `Error::may_have_been_sent` is `false`. A key's
  fingerprint is the operation and its canonical input (the number or
  WABA as a decoded id, the body canonical), never the request's method
  or path, so a key means the same through every API adapter (design
  D27). Fingerprints are stored with their key but are not stable
  identifiers: records last `WA_SERVER_IDEMPOTENCY_TTL`, and a
  fingerprint derived otherwise after an upgrade answers a repeat across
  it `422 idempotency_key_reused` for that long, never a second send,
  unless the release also matches the fingerprint of before, as S6's
  does for one TTL (`Fingerprint::or_legacy`, until the first
  release).
- **Webhooks in** go through the library's `WebhookHandler` (signature
  before parsing, 3 MiB, the leased `DedupGuard` in the shared
  `KvStore`) into `InboxSink`, then the service's event outbox, one
  stream of sequences per tenant. Routing to a tenant is an allow-list:
  the tenant bound to the event's number or WABA since before Meta dated
  it, for the event types the service reviewed; everything else is an
  operator-only row.
- **Storage** is Postgres (memory only in development). The service's
  tables are `wa_server_*` with their own migration history, run with the
  library's migrations under an advisory lock of the service's own;
  migrations are expand-then-contract so replicas of two versions can
  share a database during a rolling deploy.

## Verification

`just ci` is the gate (lint, check, test, doc, per-feature builds, deny,
live adapter tests). CI runs exactly it. See `AGENTS.md`.
