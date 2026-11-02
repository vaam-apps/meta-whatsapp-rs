-- meta-whatsapp-server, migration 8: the identifiers each outbox row names
-- its customers by (roadmap M2f: erasing a customer on a number).
-- Expand-only, like every migration of the service (docs/design/server.md,
-- section 2.4), and with no table rewrite: the column is added without a
-- default, and the existing rows are not updated. sqlx records this file's
-- checksum: never edit it once released, a comment included.
--
-- `contacts` is what an event's data names a customer by (its phone
-- numbers, BSUIDs, parent BSUIDs, group ids: the core's
-- `NewEvent::contacts`), set at insert by this release; NULL for an event
-- that names none, for the rows written before this migration, and for
-- those a replica of the previous release inserts during a rolling deploy
-- (an erasure does not find those: they go with the outbox's retention).
-- It holds identifiers by design, as `data` does, and goes with its row:
-- by the outbox's retention (`WA_SERVER_OUTBOX_RETENTION`, 7 days by
-- default), with its tenant, or by an erasure, which deletes the rows
-- naming one of a person's identities on the number.
--
-- The index is GIN, for the erasure's `contacts && $3` (an overlap), and
-- partial: only the rows that name someone. Building it reads the table
-- once, holding its inserts meanwhile (every existing row is NULL, so it
-- starts empty). The erasure's statement leaves the planner no other
-- index than this one to use (it reads the tenant by `tenant_id`, which
-- no index leads with), so before the column has statistics it is this
-- index or a scan of the table, and the default estimate of an overlap
-- picks the index on any outbox worth one.

ALTER TABLE wa_server_events ADD COLUMN contacts TEXT[];

CREATE INDEX wa_server_events_contacts_idx ON wa_server_events USING gin (contacts)
    WHERE contacts IS NOT NULL;
