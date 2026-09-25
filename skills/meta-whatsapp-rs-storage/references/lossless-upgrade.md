# Upgrading a Postgres inbox to lossless content

> **Verified against wa-rs 0e63aba8378556b4e34cf4cd5b5392f18f2a5e00 (2026-09-25).** Source: the `wa_adapters::store::postgres` module docs ("Upgrading to lossless content", with the pre-flight query) and `crates/wa-adapters/migrations/0003_lossless_content.sql`.

Migration 3 of `postgres::migrate` converts `wa_messages.kind`, `text` and
`wa_conversations.last_text` to `BYTEA`, and `payload` and `error` to
`json`, renamed `kind_utf8`, `text_utf8`, `last_text_utf8`,
`payload_json` and `error_json`, in one transaction that locks both
tables. It is one-way: an older revision's `migrate` refuses the upgraded
database. In this order:

1. **Back up** both tables of every table prefix. The only way back is a
   restore, and it loses what was recorded since: webhooks the new
   instances acknowledged are not delivered again, and replies sent
   meanwhile leave the history.
2. **Stop every older instance that writes to them** (webhook receivers,
   anything calling `Inbox::send`). Every webhook consumer pauses (OTP
   delivery statuses and partner-removed revocations too); Meta's
   backoff decides how long the backlog takes afterwards. An older
   instance left running fails on every content statement: 500s that
   Meta redelivers, replies sent but not recorded.
3. **Drop your own objects on the content columns**; the pre-flight
   query in the module docs lists them. Migration 3 refuses to run under
   anything on `payload` or `error`, naming it and changing nothing: an
   expression such as `payload->>'type'` would fail every later insert
   of a payload holding a NUL. Postgres refuses views, rules, and
   trigram, pattern, full-text or lower-case expression indexes on the
   text columns. Triggers and functions that name `kind`, `text`, `payload`,
   `error` or `last_text` are not checked and would fail every insert,
   taking the inbox down: rewrite them. A plain b-tree index on text is
   rebuilt on the bytes and kept.
4. **Run `migrate` once from a one-off job**, per table prefix
   (`migrate_with_prefix`), on a connection whose Postgres settings put a
   lock timeout and no statement timeout (the module docs show how), with
   free disk for a copy of `wa_messages` and its indexes. The lock waits behind any transaction open on the tables,
   and every later query queues behind it. Measured: 200,006 messages (a
   153 MB table) in 1 to 2 seconds on a local Postgres 18.
5. **Update your own SQL**: the new names; decode `*_utf8` as UTF-8 and
   search the bytes (a recipe is in the module docs); on
   `json`, `=`, `DISTINCT`, `GROUP BY` and `UNION` fail on every row, and
   `->`, `->>` or a cast to `jsonb` fail on a row holding a NUL, taking
   the whole statement down. Change-data-capture consumers see the new
   names and types.
6. **Start the new revision**; its `migrate` at startup is then a no-op.

Existing rows keep their bytes: a U+FFFD an older revision stored for a
NUL stays one. A custom `ConversationStore` must now keep U+0000 in
content (`conversation_conformance::run`), or the inbox's webhooks fail
their batch.
