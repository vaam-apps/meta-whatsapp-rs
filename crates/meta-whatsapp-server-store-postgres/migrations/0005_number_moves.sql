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
-- binding's `previous_tenant_released_at`. A binding reads it.
--
-- The trigger `wa_server_numbers_released` (its function
-- `wa_server_number_released`, which resolves the table in the schema
-- this migration ran in) writes it, in the releasing
-- statement, for every row deleted from `wa_server_numbers`: an unbind of
-- its WABA (the foreign key's cascade fires it too), a binding that no
-- longer lists it, and the same statements run by a replica of the
-- previous release during a rolling deploy, which knows nothing of the
-- tombstones. Without it, a number such a replica released would read as
-- never held when this release binds it to another tenant, and show that
-- tenant the previous one's history.

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

CREATE FUNCTION wa_server_number_released() RETURNS trigger
LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
BEGIN
    INSERT INTO wa_server_number_tombstones
        (phone_number_id, tenant_id, released_at, previous_tenant_released_at)
    VALUES (OLD.phone_number_id, OLD.tenant_id, now(), OLD.previous_tenant_released_at)
    ON CONFLICT (phone_number_id) DO UPDATE SET
        tenant_id = EXCLUDED.tenant_id,
        released_at = EXCLUDED.released_at,
        previous_tenant_released_at = EXCLUDED.previous_tenant_released_at;
    RETURN OLD;
END;
$$;

CREATE TRIGGER wa_server_numbers_released
    AFTER DELETE ON wa_server_numbers
    FOR EACH ROW EXECUTE FUNCTION wa_server_number_released();
