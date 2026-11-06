-- meta-whatsapp-server, migration 10: an operator's idempotency keys
-- (roadmap M5j2, docs/design/server.md D290). An admin key has no tenant, so
-- its records are stored under the namespace `admin/<key id>` in the
-- `tenant_id` column, a value no tenant id can spell (a tenant id never
-- holds a `/`), and no `wa_server_tenants` row exists for it: this drops
-- the foreign key that tied every record to a tenant. The records of a
-- deleted tenant no longer go with it by cascade: the backend deletes them
-- in the same transaction as the tenant (a replica of the release before
-- this one leaves them, and they expire with the janitor's purge). Nothing
-- else changed, and a replica of the previous release never notices the
-- constraint gone: expand-compatible, like every migration of the service
-- (docs/design/server.md, section 2.4). sqlx records this file's
-- checksum: never edit it once released, a comment included. The name of
-- the namespace is pending the owner's confirmation as a stable identifier
-- (docs/architecture.md).

ALTER TABLE wa_server_idempotency
    DROP CONSTRAINT IF EXISTS wa_server_idempotency_tenant_id_fkey;
