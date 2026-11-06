-- meta-whatsapp-server, migration 10: an operator's idempotency keys
-- (roadmap M5j2, docs/design/server.md D290). An admin key has no tenant,
-- so its records live in a table of their own: the columns of
-- `wa_server_idempotency` without the foreign key to the tenants, with the
-- records' scope `admin/<key id>` (a name no tenant id can spell) in
-- `tenant_id`. The backend picks the table by that scope. Nothing else
-- changed: expand-only, like every migration of the service
-- (docs/design/server.md, section 2.4); a replica of the previous release
-- never reads it. sqlx records this file's checksum: never edit it once
-- released, a comment included. The names below are pending the owner's
-- confirmation as stable identifiers (docs/architecture.md).

CREATE TABLE wa_server_admin_idempotency (
    tenant_id TEXT COLLATE "C" NOT NULL,
    idempotency_key TEXT COLLATE "C" NOT NULL,
    request_sha256 BYTEA NOT NULL CHECK (octet_length(request_sha256) = 32),
    claim TEXT NOT NULL,
    state TEXT NOT NULL,
    lease_until TIMESTAMPTZ NOT NULL,
    response_status INTEGER,
    response_body BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id, idempotency_key)
);

CREATE INDEX wa_server_admin_idempotency_expires_idx ON wa_server_admin_idempotency (expires_at);
