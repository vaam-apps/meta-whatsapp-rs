-- What `PostgresConversationStore` keeps beside the message history
-- (roadmap L5): window events, thread owners, the coexistence address
-- book and the links between a person's identities, a message's sender,
-- and the indexes purge by age and erasure read.
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
-- business number and its own id, a link by business number and its two
-- identities), so an erasure deletes by that key.

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

-- A removed contact leaves a row with `removed` set, every other column
-- NULL and `synced_at` the removal's time, so a sync older than the removal
-- that arrives after it cannot bring the contact back.
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
    removed         BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY (phone_number_id, contact)
);

-- Purge by age of the removals.
CREATE INDEX {prefix}contact_removals_idx
    ON {prefix}synced_contacts (synced_at) WHERE removed;

-- Purge by age, across every business number.
CREATE INDEX {prefix}messages_ts_idx
    ON {prefix}messages (ts);

CREATE INDEX {prefix}conversations_last_idx
    ON {prefix}conversations (last_message_at);

-- Two identities of one person on one business number, as Meta reported
-- them (`user_id_update`, a number change): `previous` became `current`.
CREATE TABLE {prefix}identity_links (
    phone_number_id TEXT COLLATE "C" NOT NULL,
    previous        TEXT COLLATE "C" NOT NULL,
    current         TEXT COLLATE "C" NOT NULL,
    ts              TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (phone_number_id, previous, current)
);

-- Following a link from its `current` side.
CREATE INDEX {prefix}identity_links_idx
    ON {prefix}identity_links (phone_number_id, current);

-- Who sent an inbound message (`StoredMessage::sender`: the payload's
-- `from_user_id`, else its `from`), which the adapter writes with the
-- message, so that an erasure finds a person's messages in a group
-- conversation. NULL for an outbound message.
ALTER TABLE {prefix}messages ADD COLUMN sender TEXT COLLATE "C";

-- The messages recorded before this migration, by the same rule. A payload
-- holding U+0000 anywhere cannot be read field by field (the `json`
-- operators fail on it): such a row is left without a sender here, and
-- gets it from the first erasure on its number (below).
UPDATE {prefix}messages SET sender = COALESCE(
        CASE WHEN json_typeof(payload_json -> 'from_user_id') = 'string'
             THEN NULLIF(payload_json ->> 'from_user_id', '') END,
        CASE WHEN json_typeof(payload_json -> 'from') = 'string'
             THEN NULLIF(payload_json ->> 'from', '') END)
    WHERE direction = 'inbound' AND strpos(payload_json::text, E'\\u0000') = 0;

CREATE INDEX {prefix}messages_sender_idx
    ON {prefix}messages (phone_number_id, sender) WHERE sender IS NOT NULL;

-- The inbound messages without a sender, this crate's own kinds aside (a
-- revoke's tombstone, a redacted message: their payload is `{}`): those
-- the update above skipped, and those an instance of the previous
-- revision records after it (it writes no sender). `erase_all` reads
-- their payloads first, fills in their sender by the same rule, and then
-- matches them like any other.
CREATE INDEX {prefix}messages_unsent_idx
    ON {prefix}messages (phone_number_id) WHERE sender IS NULL AND direction = 'inbound'
     AND kind_utf8 <> 'revoked'::bytea AND kind_utf8 <> 'erased'::bytea;
