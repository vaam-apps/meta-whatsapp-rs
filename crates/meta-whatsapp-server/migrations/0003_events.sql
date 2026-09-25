-- meta-whatsapp-server, migration 3: the event outbox (Meta's webhook
-- events, routed to their tenant, or to nobody: operator-only rows have no
-- tenant). Expand-only, like every migration of the service
-- (docs/design/server.md, section 2.4). sqlx records this file's checksum:
-- never edit it once released, a comment included.
--
-- `data` is `json`, not `jsonb`: it keeps the event's JSON as written, and
-- `jsonb` refuses the `\u0000` escape a message text may hold (the
-- library's inbox tables made the same choice). Inserts take an advisory
-- transaction lock, so sequences commit in order and a poll that saw one
-- sequence saw every event before it.

CREATE TABLE wa_server_events (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    id TEXT COLLATE "C" NOT NULL UNIQUE,
    dedup_key TEXT COLLATE "C" UNIQUE,
    tenant_id TEXT COLLATE "C",
    phone_number_id TEXT COLLATE "C",
    waba_id TEXT COLLATE "C",
    event_type TEXT COLLATE "C" NOT NULL,
    data JSON NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX wa_server_events_tenant_idx ON wa_server_events (tenant_id, sequence);
CREATE INDEX wa_server_events_created_idx ON wa_server_events (created_at);

CREATE TABLE wa_server_event_purges (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    purged_through BIGINT NOT NULL,
    purged_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
