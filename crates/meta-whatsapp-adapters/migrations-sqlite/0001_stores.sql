-- The SQLite adapters' tables: `SqliteKvStore` and `SqliteConversationStore`
-- (meta-whatsapp-adapters, feature `sqlite`, roadmap L8).
--
-- This file is a template: `meta_whatsapp_adapters::store::sqlite::migrate`
-- replaces the brace-wrapped placeholder with the configured table prefix
-- (default `wa_`) before running it, so it is not meant for `sqlx migrate
-- run`. sqlx records its checksum: never edit it once released, add a
-- migration instead.
--
-- Times are INTEGER unix microseconds (Postgres' precision). Identifiers
-- (message ids, contacts, phone number ids) are TEXT, compared byte for byte
-- (SQLite's BINARY collation, the order of Rust's `str`); the adapter refuses
-- one holding U+0000. Content (a message's kind and text, the preview, a
-- synced contact's names) is BLOB, its UTF-8 bytes, so U+0000 round-trips;
-- payloads and errors are JSON text, in which serde_json escapes U+0000.

-- Key/value records. Namespace and key are BLOB (their UTF-8 bytes): a key
-- holding U+0000 is kept exactly. Deleted and expired records are removed
-- outright: versions come from `kv_version`, never from the row.
CREATE TABLE {prefix}kv (
    namespace  BLOB NOT NULL,
    key        BLOB NOT NULL,
    value      BLOB NOT NULL,
    version    INTEGER NOT NULL,
    expires_at INTEGER,
    PRIMARY KEY (namespace, key)
) WITHOUT ROWID;

CREATE INDEX {prefix}kv_expires_idx ON {prefix}kv (expires_at)
    WHERE expires_at IS NOT NULL;

-- One counter hands out versions for every key, under the write lock every
-- write transaction takes (`BEGIN IMMEDIATE`): never reused, even after a
-- delete.
CREATE TABLE {prefix}kv_version (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL
);

INSERT INTO {prefix}kv_version (id, version) VALUES (1, 0);

-- Messages, keyed by business number and id (OPEN_QUESTIONS.md #33: either
-- answer needs no change of key).
CREATE TABLE {prefix}messages (
    phone_number_id TEXT NOT NULL,
    id              TEXT NOT NULL,
    contact         TEXT NOT NULL,
    direction       TEXT NOT NULL,
    kind_utf8       BLOB NOT NULL,
    text_utf8       BLOB,
    payload_json    TEXT NOT NULL,
    status          TEXT NOT NULL,
    ts              INTEGER NOT NULL,
    status_at       INTEGER,
    error_json      TEXT,
    -- `StoredMessage::sender`, written with the message, read by erasures.
    sender          TEXT,
    PRIMARY KEY (phone_number_id, id)
) WITHOUT ROWID;

-- Today's rule (OPEN_QUESTIONS.md #33): a message id is stored once per
-- store, whatever the business number. Ids unique per number drop it.
CREATE UNIQUE INDEX {prefix}messages_id_idx ON {prefix}messages (id);

CREATE INDEX {prefix}messages_conversation_idx
    ON {prefix}messages (phone_number_id, contact, ts, id);

CREATE INDEX {prefix}messages_sender_idx ON {prefix}messages (phone_number_id, sender)
    WHERE sender IS NOT NULL;

CREATE INDEX {prefix}messages_ts_idx ON {prefix}messages (ts);

-- Inbox rows, maintained when a message is written.
CREATE TABLE {prefix}conversations (
    phone_number_id TEXT NOT NULL,
    contact         TEXT NOT NULL,
    last_message_at INTEGER NOT NULL,
    last_message_id TEXT NOT NULL,
    last_text_utf8  BLOB,
    last_inbound_at INTEGER,
    unread          INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (phone_number_id, contact)
) WITHOUT ROWID;

CREATE INDEX {prefix}conversations_list_idx
    ON {prefix}conversations (phone_number_id, last_message_at, contact);

CREATE INDEX {prefix}conversations_last_idx ON {prefix}conversations (last_message_at);

-- Calls and standby messages that reopen the window, by business number
-- and id.
CREATE TABLE {prefix}window_events (
    phone_number_id TEXT NOT NULL,
    id              TEXT NOT NULL,
    contact         TEXT NOT NULL,
    kind            TEXT NOT NULL,
    ts              INTEGER NOT NULL,
    PRIMARY KEY (phone_number_id, id)
) WITHOUT ROWID;

CREATE INDEX {prefix}window_events_conversation_idx
    ON {prefix}window_events (phone_number_id, contact, ts, id);

CREATE INDEX {prefix}window_events_ts_idx ON {prefix}window_events (ts);

-- Conversation Routing: the latest ownership record of each thread.
CREATE TABLE {prefix}thread_owners (
    phone_number_id TEXT NOT NULL,
    contact         TEXT NOT NULL,
    owner           TEXT NOT NULL,
    role            TEXT,
    app_id          TEXT,
    since           INTEGER NOT NULL,
    PRIMARY KEY (phone_number_id, contact)
) WITHOUT ROWID;

CREATE INDEX {prefix}thread_owners_since_idx ON {prefix}thread_owners (since);

-- The coexistence address book. A removal is a row with `removed` set and
-- nothing but its key and time.
CREATE TABLE {prefix}synced_contacts (
    phone_number_id  TEXT NOT NULL,
    contact          TEXT NOT NULL,
    full_name_utf8   BLOB,
    first_name_utf8  BLOB,
    phone_number     TEXT,
    user_id          TEXT,
    parent_user_id   TEXT,
    username_utf8    BLOB,
    synced_at        INTEGER NOT NULL,
    removed          INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (phone_number_id, contact)
) WITHOUT ROWID;

CREATE INDEX {prefix}synced_contacts_user_idx
    ON {prefix}synced_contacts (phone_number_id, user_id);

CREATE INDEX {prefix}synced_contacts_parent_idx
    ON {prefix}synced_contacts (phone_number_id, parent_user_id);

CREATE INDEX {prefix}synced_contacts_phone_idx
    ON {prefix}synced_contacts (phone_number_id, phone_number);

CREATE INDEX {prefix}synced_contacts_removed_idx ON {prefix}synced_contacts (synced_at)
    WHERE removed = 1;

-- Two identities of one person on one business number.
CREATE TABLE {prefix}identity_links (
    phone_number_id TEXT NOT NULL,
    previous        TEXT NOT NULL,
    current         TEXT NOT NULL,
    ts              INTEGER NOT NULL,
    PRIMARY KEY (phone_number_id, previous, current)
) WITHOUT ROWID;

CREATE INDEX {prefix}identity_links_current_idx
    ON {prefix}identity_links (phone_number_id, current);
