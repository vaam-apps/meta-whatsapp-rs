-- meta-whatsapp-server, migration 2: idempotency keys (docs/design/server.md,
-- section 5.4). One row per (tenant, key): the SHA-256 of the request that
-- claimed it (method, path, body), the claim id of that request, its
-- state (`in_progress` until its lease ends, then unknown; or
-- `completed`, with the answer byte for byte), and its expiry
-- (WA_SERVER_IDEMPOTENCY_TTL, 24 hours by default). A released key has no
-- row. Rows go with their tenant. Expand-only, like every migration of
-- the service; sqlx records this file's checksum: never edit it once
-- released, a comment included.

CREATE TABLE wa_server_idempotency (
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_tenants (id) ON DELETE CASCADE,
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

CREATE INDEX wa_server_idempotency_expires_idx ON wa_server_idempotency (expires_at);
