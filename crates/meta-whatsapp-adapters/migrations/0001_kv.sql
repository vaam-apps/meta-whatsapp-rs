-- Key/value records for `PostgresKvStore`.
--
-- This file is a template: `wa_adapters::store::postgres::migrate` replaces
-- the brace-wrapped placeholder with the configured table prefix (default
-- `wa_`) before running it, so it is not meant for `sqlx migrate run`.

-- One sequence hands out versions for every key. Versions are therefore
-- unique across the whole table, and a key that is deleted (or purged) and
-- recreated can never repeat a version an old reader still holds.
-- CACHE 1 (the default, spelled out) keeps nextval() monotonic across
-- sessions; a per-session cache would let a later write draw a lower value.
CREATE SEQUENCE {prefix}kv_version_seq AS BIGINT MINVALUE 1 CACHE 1;

CREATE TABLE {prefix}kv (
    -- "C" collation: byte order, identical to Rust's `str` ordering.
    namespace  TEXT COLLATE "C" NOT NULL,
    key        TEXT COLLATE "C" NOT NULL,
    -- NULL marks a tombstone (deleted key). Tombstones and expired rows stay
    -- until `purge_expired()` removes them after a grace period, so a writer
    -- that drew its version before a delete meets a conflicting row and
    -- re-draws, instead of inserting a version lower than the deleted one.
    value      BYTEA,
    version    BIGINT NOT NULL,
    expires_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (namespace, key)
);

-- Purge candidates: tombstones and rows that can expire.
CREATE INDEX {prefix}kv_purge_idx ON {prefix}kv (updated_at)
    WHERE value IS NULL OR expires_at IS NOT NULL;
