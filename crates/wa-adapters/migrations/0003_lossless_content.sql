-- Message content keeps U+0000 (`OPEN_QUESTIONS.md` #18, decided by the
-- owner on 2026-09-25: store it losslessly).
--
-- A template like 0001 and 0002: the brace-wrapped placeholder becomes the
-- table prefix before the migration runs.
--
-- Postgres `text` cannot hold U+0000, and `jsonb` refuses a `\u0000`
-- escape. The content columns move to types that keep it:
--
--   kind, text           -> kind_utf8, text_utf8     BYTEA, the UTF-8 bytes
--   payload, error       -> payload_json, error_json JSON, the text as written
--   last_text (summary)  -> last_text_utf8           BYTEA, the UTF-8 bytes
--
-- `json` (not `jsonb`) stores the document's text and accepts `\u0000`.
-- Ids, contacts and phone number ids stay `TEXT COLLATE "C"`: Meta assigns
-- them and never with U+0000, and every order and cursor is on them and on
-- the timestamps, never on content.
--
-- Expand and contract in one step: each content column is converted in
-- place (one table rewrite, so no row keeps a dead copy of its old
-- content) and renamed, under the lock `ALTER TABLE` takes, in this
-- migration's single transaction. There is no half-migrated state. The new
-- names make every statement of a binary built before this migration that
-- touches content fail ("column does not exist") instead of reading or
-- writing the old representation. Existing rows keep their content byte for
-- byte; a U+FFFD that replaced a NUL before this migration stays a U+FFFD
-- (the NUL is gone, nothing can tell the two apart).

LOCK TABLE {prefix}messages, {prefix}conversations IN ACCESS EXCLUSIVE MODE;

ALTER TABLE {prefix}messages
    ALTER COLUMN kind    TYPE BYTEA USING convert_to(kind, 'UTF8'),
    ALTER COLUMN text    TYPE BYTEA USING convert_to(text, 'UTF8'),
    ALTER COLUMN payload TYPE JSON  USING payload::json,
    ALTER COLUMN error   TYPE JSON  USING error::json;

ALTER TABLE {prefix}messages RENAME COLUMN kind TO kind_utf8;
ALTER TABLE {prefix}messages RENAME COLUMN text TO text_utf8;
ALTER TABLE {prefix}messages RENAME COLUMN payload TO payload_json;
ALTER TABLE {prefix}messages RENAME COLUMN error TO error_json;

ALTER TABLE {prefix}conversations
    ALTER COLUMN last_text TYPE BYTEA USING convert_to(last_text, 'UTF8');

ALTER TABLE {prefix}conversations RENAME COLUMN last_text TO last_text_utf8;
