-- meta-whatsapp-server, migration 1: tenants, API keys, WABA and number
-- bindings. Expand-only, like every migration of the service: a later
-- revision adds tables and nullable columns, and removes one only in a
-- release after every replica stopped using it (docs/design/server.md,
-- section 2.4). sqlx records this file's checksum: never edit it once
-- released, a comment included.

CREATE TABLE wa_server_tenants (
    id TEXT COLLATE "C" PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'active',
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE wa_server_api_keys (
    key_id TEXT COLLATE "C" PRIMARY KEY,
    secret_sha256 BYTEA NOT NULL CHECK (octet_length(secret_sha256) = 32),
    kind TEXT NOT NULL,
    tenant_id TEXT COLLATE "C" REFERENCES wa_server_tenants (id) ON DELETE CASCADE,
    all_tenants BOOLEAN NOT NULL DEFAULT false,
    allowed_tenants TEXT[] NOT NULL DEFAULT '{}',
    scopes TEXT[] NOT NULL DEFAULT '{}',
    name TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ
);

CREATE INDEX wa_server_api_keys_tenant_idx ON wa_server_api_keys (tenant_id, key_id);

CREATE TABLE wa_server_wabas (
    waba_id TEXT COLLATE "C" PRIMARY KEY,
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_tenants (id),
    credit_allocation_id TEXT,
    attached_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX wa_server_wabas_tenant_idx ON wa_server_wabas (tenant_id, waba_id);

CREATE TABLE wa_server_numbers (
    phone_number_id TEXT COLLATE "C" PRIMARY KEY,
    waba_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_wabas (waba_id) ON DELETE CASCADE,
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_tenants (id),
    status TEXT NOT NULL DEFAULT 'connected',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX wa_server_numbers_tenant_idx ON wa_server_numbers (tenant_id, phone_number_id);
CREATE INDEX wa_server_numbers_waba_idx ON wa_server_numbers (waba_id);
