-- Message history and inbox summaries for `PostgresConversationStore`.
--
-- A template like 0001: the brace-wrapped placeholder becomes the table
-- prefix before the migration runs.
--
-- Ids and contacts use the "C" collation so `(ts, id)` and
-- `(last_message_at, contact)` order exactly like Rust's `str` ordering; a
-- locale collation would page in a different order than the in-memory store.

CREATE TABLE {prefix}messages (
    id              TEXT COLLATE "C" PRIMARY KEY,
    phone_number_id TEXT COLLATE "C" NOT NULL,
    contact         TEXT COLLATE "C" NOT NULL,
    direction       TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    kind            TEXT NOT NULL,
    text            TEXT,
    payload         JSONB NOT NULL,
    -- `DeliveryStatus` in its serde (snake_case) form.
    status          TEXT NOT NULL,
    ts              TIMESTAMPTZ NOT NULL,
    status_at       TIMESTAMPTZ,
    error           JSONB
);

CREATE INDEX {prefix}messages_conversation_idx
    ON {prefix}messages (phone_number_id, contact, ts DESC, id DESC);

CREATE TABLE {prefix}conversations (
    phone_number_id TEXT COLLATE "C" NOT NULL,
    contact         TEXT COLLATE "C" NOT NULL,
    -- `(last_message_at, last_message_id)` is the newest message by the same
    -- `(timestamp, id)` order the history uses; `last_text` belongs to it.
    last_message_at TIMESTAMPTZ NOT NULL,
    last_message_id TEXT COLLATE "C" NOT NULL,
    last_text       TEXT,
    last_inbound_at TIMESTAMPTZ,
    -- Inbound messages appended since the last mark_read.
    unread          BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (phone_number_id, contact)
);

CREATE INDEX {prefix}conversations_recent_idx
    ON {prefix}conversations (phone_number_id, last_message_at DESC, contact DESC);
