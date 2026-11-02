-- meta-whatsapp-server, migration 6: Embedded Signup's resumable attempts
-- and the token vault's records kept past a binding (roadmap M3a).
-- Expand-only, like every migration of the service (docs/design/server.md,
-- section 2.4): two new tables, which a replica of the previous release
-- never reads or writes. sqlx records this file's checksum: never edit it
-- once released, a comment included. Every name here is a stored name,
-- pending the owner's confirmation (docs/architecture.md, § Stable
-- identifiers).
--
-- `wa_server_signup_attempts` keeps one attempt per WABA: an onboarding
-- that stopped after its token was stored, which its tenant resumes. Never
-- the code or the PIN: the session info is the browser's ids, as the
-- library serializes them, and `finish_kind` the flow's completion event.
-- An attempt goes with its binding (the foreign key on `wa_server_wabas`,
-- ON DELETE CASCADE: every unbind), so a WABA bound again, to any tenant,
-- has none.
--
-- `wa_server_vault_records` names the records the library's token vault
-- keeps past a binding, which the vault cannot list: a WABA's credit
-- ledger (`kind` `waba`) and a business's revocation marker (`business`),
-- written in Solution Partner mode. A key rotation walks them besides the
-- bindings. Kept for good, as the vault's records are: no foreign key.

CREATE TABLE wa_server_signup_attempts (
    waba_id TEXT COLLATE "C" PRIMARY KEY
        REFERENCES wa_server_wabas (waba_id) ON DELETE CASCADE,
    tenant_id TEXT COLLATE "C" NOT NULL REFERENCES wa_server_tenants (id) ON DELETE CASCADE,
    failed_step TEXT NOT NULL,
    session JSONB NOT NULL DEFAULT '{}'::jsonb,
    finish_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX wa_server_signup_attempts_tenant_idx
    ON wa_server_signup_attempts (tenant_id, waba_id);

CREATE TABLE wa_server_vault_records (
    kind TEXT COLLATE "C" NOT NULL,
    id TEXT COLLATE "C" NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, id)
);
