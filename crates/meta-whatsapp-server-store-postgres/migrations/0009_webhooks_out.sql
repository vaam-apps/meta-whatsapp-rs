-- meta-whatsapp-server, migration 9: webhooks out (roadmap M2c). Two new
-- tables, nothing else changed: expand-only, like every migration of the
-- service (docs/design/server.md, section 2.4); a replica of the previous
-- release never reads them. sqlx records this file's checksum: never edit
-- it once released, a comment included. Numbered 9 while migration 8
-- (roadmap M2f) is in flight; the names below are pending the owner's
-- confirmation as stable identifiers (docs/architecture.md).
--
-- An endpoint is its tenant's, and goes with it. `secret` and
-- `previous_secret` are the signing secrets sealed under
-- WA_SERVER_DATA_KEY (`v1.<key id>.<base64>`), never in the clear.
-- `cursor` is the last sequence of the tenant's event stream fanned out to
-- it: the dispatcher moves it from the value it read, in the same
-- transaction as the deliveries it records.

CREATE TABLE wa_server_webhook_endpoints (
    id TEXT COLLATE "C" PRIMARY KEY,
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_tenants (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    event_types TEXT[] NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    disabled_reason TEXT,
    secret TEXT NOT NULL,
    previous_secret TEXT,
    previous_secret_until TIMESTAMPTZ,
    cursor BIGINT NOT NULL,
    failing_since TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX wa_server_webhook_endpoints_tenant_idx ON wa_server_webhook_endpoints (tenant_id, id);

-- A delivery: one event (or a ping, `event_id` NULL) to one endpoint, and
-- its attempts so far; no body (it is built from the outbox at each
-- attempt). `tenant_id` is its endpoint's. `lease_until` and `claim` are
-- the claim of the replica attempting it (`FOR UPDATE SKIP LOCKED`), the
-- times all the dispatcher's clock's. An event has one delivery per
-- endpoint.

CREATE TABLE wa_server_webhook_deliveries (
    id TEXT COLLATE "C" PRIMARY KEY,
    endpoint_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_webhook_endpoints (id) ON DELETE CASCADE,
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_tenants (id) ON DELETE CASCADE,
    event_id TEXT COLLATE "C",
    sequence BIGINT,
    event_type TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ,
    give_up_at TIMESTAMPTZ NOT NULL,
    lease_until TIMESTAMPTZ,
    claim TEXT,
    last_attempt_at TIMESTAMPTZ,
    last_status INTEGER,
    last_error TEXT,
    last_duration_ms INTEGER,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (endpoint_id, event_id)
);

-- The claim: each tenant's oldest due delivery.
CREATE INDEX wa_server_webhook_deliveries_due_idx ON wa_server_webhook_deliveries (tenant_id, next_attempt_at)
    WHERE status = 'pending';

-- An endpoint's deliveries, newest first.
CREATE INDEX wa_server_webhook_deliveries_endpoint_idx ON wa_server_webhook_deliveries (endpoint_id, id);
