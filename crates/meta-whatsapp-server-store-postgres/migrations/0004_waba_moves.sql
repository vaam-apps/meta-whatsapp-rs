-- meta-whatsapp-server, migration 4: a WABA's moves between tenants, and
-- its binding's generation (roadmap S2b, design D36). Expand-only, like
-- every migration of the service (docs/design/server.md, section 2.4):
-- new columns with defaults that keep the existing rows' behaviour, and a
-- new table. sqlx records this file's checksum: never edit it once
-- released, a comment included.
--
-- `generation` counts the attaches of a binding: 1 when it begins, one
-- more on every attach of the WABA to the tenant holding it (a refresh,
-- which keeps `attached_at`). A capability's conditioned writes compare
-- it, so one made before a refresh acts on nothing after it. Existing
-- rows, and those a replica of the previous release inserts, are at 1.
--
-- `previous_tenant_unbound_at` is when the WABA was last unbound from a
-- tenant other than the binding's, read from its tombstone when the
-- binding began; NULL when no other tenant held it (a first onboarding).
-- The default, for existing rows and those a replica of the previous
-- release inserts, is the Unix epoch: "a previous tenant may have held
-- it, unbound before the records began", which keeps the rules of before
-- this migration for them (an event of the binding's own second stays
-- operator-only; an undated event reaches the holder). This release
-- writes it on every new binding.
--
-- `wa_server_waba_tombstones` keeps each WABA's last unbinding: the
-- tenant it was unbound from (NULL once that tenant is deleted, so a
-- tenant created again under its id is another tenant), when, and the
-- unbound binding's `previous_tenant_unbound_at`. Every unbind writes it
-- in the unbinding statement; a binding reads it.

ALTER TABLE wa_server_wabas ADD COLUMN generation BIGINT NOT NULL DEFAULT 1;

ALTER TABLE wa_server_wabas
    ADD COLUMN previous_tenant_unbound_at TIMESTAMPTZ DEFAULT '1970-01-01 00:00:00+00';

CREATE TABLE wa_server_waba_tombstones (
    waba_id TEXT COLLATE "C" PRIMARY KEY,
    tenant_id TEXT COLLATE "C" REFERENCES wa_server_tenants (id) ON DELETE SET NULL,
    unbound_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    previous_tenant_unbound_at TIMESTAMPTZ
);
