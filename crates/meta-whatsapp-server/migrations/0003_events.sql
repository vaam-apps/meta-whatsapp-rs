-- meta-whatsapp-server, migration 3: the event outbox (Meta's webhook
-- events, routed to their tenant, or to nobody: operator-only rows have no
-- tenant). Expand-only, like every migration of the service
-- (docs/design/server.md, section 2.4). sqlx records this file's checksum:
-- never edit it once released, a comment included.
--
-- Each tenant has its own stream of sequences, and the operator-only rows
-- one of their own (`stream` ''): `wa_server_event_streams` holds each
-- stream's last sequence, whose row an insert locks until it commits (so a
-- stream's sequences commit in order), and how far it was purged.
--
-- `data` is `json`, not `jsonb`: it keeps the event's JSON as written, and
-- `jsonb` refuses the `\u0000` escape a message text may hold (the
-- library's inbox tables made the same choice). `data_bytes` is its size,
-- so a page is cut to its budget before any data is read.
--
-- `tenant_id` references the tenant: deleting a tenant deletes its events
-- in the same transaction, so a tenant created later with the same id
-- never polls them.

CREATE TABLE wa_server_event_streams (
    stream TEXT COLLATE "C" PRIMARY KEY,
    last_sequence BIGINT NOT NULL,
    purged_through BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE wa_server_events (
    tenant_id TEXT COLLATE "C" REFERENCES wa_server_tenants (id) ON DELETE CASCADE,
    stream TEXT COLLATE "C" GENERATED ALWAYS AS (COALESCE(tenant_id, '')) STORED,
    sequence BIGINT NOT NULL,
    id TEXT COLLATE "C" NOT NULL UNIQUE,
    dedup_key TEXT COLLATE "C" UNIQUE,
    phone_number_id TEXT COLLATE "C",
    waba_id TEXT COLLATE "C",
    event_type TEXT COLLATE "C" NOT NULL,
    data JSON NOT NULL,
    data_bytes INTEGER NOT NULL CHECK (data_bytes >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (stream, sequence)
);

CREATE INDEX wa_server_events_created_idx ON wa_server_events (created_at);
