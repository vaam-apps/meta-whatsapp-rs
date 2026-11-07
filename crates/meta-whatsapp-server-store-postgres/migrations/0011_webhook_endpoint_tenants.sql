-- meta-whatsapp-server, migration 11: the tenants of a platform key's
-- webhook endpoint (roadmap M2m). One new column and one new table,
-- nothing else changed: expand-only, like every migration of the service
-- (docs/design/server.md, section 2.4); a replica of the previous release
-- never reads them. sqlx records this file's checksum: never edit it once
-- released, a comment included. Numbered 11, after migration 10 (roadmap
-- M5j2); the names below are pending the owner's confirmation as stable
-- identifiers (docs/architecture.md).
--
-- `named_tenants` says an endpoint receives the tenants named in
-- `wa_server_webhook_endpoint_tenants` and not its own tenant's events:
-- the rows already there are all `false` (an endpoint of its tenant's own
-- events, whose `cursor` column is its cursor). It is a flag, not "has
-- rows", so that an endpoint whose named tenants were all deleted keeps
-- receiving nothing instead of turning back into its own tenant's.
--
-- A named tenant is a row with its own cursor in that tenant's stream, as
-- `wa_server_webhook_endpoints.cursor` is the endpoint's own tenant's.
-- Deleting the tenant deletes the row (its events stop; the endpoint is
-- never widened, and a tenant created later under the same id does not
-- join it), and deleting the endpoint deletes its rows. The deliveries of
-- such an endpoint carry the tenant of their event in their own
-- `tenant_id`.

ALTER TABLE wa_server_webhook_endpoints ADD COLUMN named_tenants BOOLEAN NOT NULL DEFAULT false;

CREATE TABLE wa_server_webhook_endpoint_tenants (
    endpoint_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_webhook_endpoints (id) ON DELETE CASCADE,
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_tenants (id) ON DELETE CASCADE,
    cursor BIGINT NOT NULL,
    PRIMARY KEY (endpoint_id, tenant_id)
);

-- A tenant's deletion, and the claim's tenants.
CREATE INDEX wa_server_webhook_endpoint_tenants_tenant_idx ON wa_server_webhook_endpoint_tenants (tenant_id);
