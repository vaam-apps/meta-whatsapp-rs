-- Message content keeps U+0000 (the owner's decision of 2026-09-25: store
-- it losslessly).
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

-- Refuse to convert under an object of the operator's own that depends on
-- `payload` or `error` (an expression or partial index, a check constraint,
-- a view, a policy, a generated column...). Some would fail the conversion
-- below anyway, but an expression such as `payload->>'type'` survives it
-- (no existing row holds a NUL) and then fails the insert of every payload
-- holding one, anywhere: the webhook batch fails with it, which is what
-- this migration exists to end. The exception rolls the whole migration
-- back. NOT NULL constraints are the table's own.
DO $guard$
DECLARE
    found text;
BEGIN
    SELECT string_agg(DISTINCT pg_describe_object(d.classid, d.objid, d.objsubid), '; ')
    INTO found
    FROM pg_depend d, pg_attribute a
    WHERE d.refclassid = 'pg_class'::regclass
      AND d.refobjid = '{prefix}messages'::regclass
      AND a.attrelid = d.refobjid
      AND a.attnum = d.refobjsubid
      AND a.attname IN ('payload', 'error')
      AND NOT (d.classid = 'pg_constraint'::regclass
               AND (SELECT c.contype FROM pg_constraint c WHERE c.oid = d.objid) = 'n');
    IF found IS NOT NULL THEN
        RAISE EXCEPTION 'lossless content (migration 3): drop what depends on the payload or error column of {prefix}messages first, nothing was changed: %', found
            USING HINT = 'A payload holding U+0000 would fail every such index, constraint or view. See the upgrade steps of wa_adapters::store::postgres.';
    END IF;
END
$guard$;

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
