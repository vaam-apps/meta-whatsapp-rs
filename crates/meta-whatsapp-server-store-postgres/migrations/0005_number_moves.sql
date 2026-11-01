-- meta-whatsapp-server, migration 5: a number's moves between tenants
-- (roadmap M2h, design D10), the number's counterpart of migration 4's
-- WABA moves. Expand-only, like every migration of the service
-- (docs/design/server.md, section 2.4), and with no table rewrite: each
-- new column is added without a default or with a constant one, and the
-- existing rows are not updated. sqlx records this file's checksum: never
-- edit it once released, a comment included.
--
-- `attached_at` is when the number came under its tenant: when a binding
-- of the tenant first listed it (a refresh, and a move to another WABA of
-- the same tenant, keep it). The rows written before this migration hold
-- NULL, which the service reads as its WABA's `attached_at` (when the
-- binding that lists it began); those a replica of the previous release
-- inserts get the column's default, now(). This release writes it on
-- every new number binding, never before the number's last release.
--
-- `previous_tenant_released_at` is when the number was last released by a
-- tenant other than its current one, read from its tombstone when it came
-- under that tenant; NULL when no other tenant held it. The default, for
-- existing rows and those a replica of the previous release inserts, is
-- the Unix epoch: "a previous tenant may have held it, released before
-- the records began", read as a move (the inbox shows such a number
-- nothing dated before its `attached_at`: fail closed).
--
-- `wa_server_number_tombstones` keeps each number's last release: the
-- tenant that released it (NULL once that tenant is deleted, so a tenant
-- created again under its id is another tenant), when, and the released
-- binding's `previous_tenant_released_at`. Every release writes it in the
-- releasing statement (an unbind of its WABA, a binding that no longer
-- lists it); a binding reads it.

ALTER TABLE wa_server_numbers ADD COLUMN attached_at TIMESTAMPTZ;

ALTER TABLE wa_server_numbers ALTER COLUMN attached_at SET DEFAULT now();

ALTER TABLE wa_server_numbers
    ADD COLUMN previous_tenant_released_at TIMESTAMPTZ DEFAULT '1970-01-01 00:00:00+00';

CREATE TABLE wa_server_number_tombstones (
    phone_number_id TEXT COLLATE "C" PRIMARY KEY,
    tenant_id TEXT COLLATE "C" REFERENCES wa_server_tenants (id) ON DELETE SET NULL,
    released_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    previous_tenant_released_at TIMESTAMPTZ
);
