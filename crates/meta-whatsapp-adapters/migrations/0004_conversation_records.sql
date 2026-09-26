-- What `PostgresConversationStore` keeps beside the message history
-- (roadmap L5): window events, thread owners and the coexistence address
-- book, and the indexes purge by age reads.
--
-- A template like 0001 to 0003: the brace-wrapped placeholder becomes the
-- table prefix before the migration runs.
--
-- Identifiers (business numbers, contacts, event ids, BSUIDs, roles, app
-- ids) are `TEXT COLLATE "C"`, as in the messages table: byte order, and
-- they refuse U+0000. What a person typed (an address book name, a
-- username) is content, kept as its UTF-8 bytes like message text, so a
-- NUL in it is stored exactly. Nothing orders or matches on content.
--
-- Every table is keyed by business number and contact (a window event by
-- business number and its own id), so an erasure deletes by that key.

CREATE TABLE {prefix}window_events (
    phone_number_id TEXT COLLATE "C" NOT NULL,
    -- Meta's id of the call or of the standby message.
    id              TEXT COLLATE "C" NOT NULL,
    contact         TEXT COLLATE "C" NOT NULL,
    -- `WindowEventKind::as_str`.
    kind            TEXT COLLATE "C" NOT NULL,
    ts              TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (phone_number_id, id)
);

CREATE INDEX {prefix}window_events_conv_idx
    ON {prefix}window_events (phone_number_id, contact, ts DESC, id DESC);

CREATE INDEX {prefix}window_events_ts_idx
    ON {prefix}window_events (ts);

CREATE TABLE {prefix}thread_owners (
    phone_number_id TEXT COLLATE "C" NOT NULL,
    contact         TEXT COLLATE "C" NOT NULL,
    -- `ThreadOwner` in its serde (snake_case) form.
    owner           TEXT COLLATE "C" NOT NULL,
    role            TEXT COLLATE "C",
    app_id          TEXT COLLATE "C",
    since           TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (phone_number_id, contact)
);

CREATE TABLE {prefix}synced_contacts (
    phone_number_id TEXT COLLATE "C" NOT NULL,
    contact         TEXT COLLATE "C" NOT NULL,
    full_name_utf8  BYTEA,
    first_name_utf8 BYTEA,
    phone_number    TEXT COLLATE "C",
    user_id         TEXT COLLATE "C",
    parent_user_id  TEXT COLLATE "C",
    username_utf8   BYTEA,
    synced_at       TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (phone_number_id, contact)
);

-- Purge by age, across every business number.
CREATE INDEX {prefix}messages_ts_idx
    ON {prefix}messages (ts);

CREATE INDEX {prefix}conversations_last_idx
    ON {prefix}conversations (last_message_at);
