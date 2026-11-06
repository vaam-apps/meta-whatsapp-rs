# Run the service: meta-whatsapp-server

Apps not written in Rust (a Medusa store in TypeScript, a CMS backend in
any stack) use meta-whatsapp-rs through **meta-whatsapp-server**, an HTTP
service built on the library. This guide runs it, configures it, creates
tenants and keys, and makes a first authenticated call. The design, with
its reasons and the owner's decisions, is
[docs/design/server.md](../design/server.md); the rules it keeps are in
[architecture.md](../architecture.md#service-meta-whatsapp-server).

> **What exists today (milestones M1a, M1b and M1c, M2a, M2b, M2g, M3a and M3c).** Tenants, API
> keys, the admin API, attaching the platform's own WhatsApp Business
> Accounts (WABAs), the numbers and business profile routes, vault key
> rotation, sending messages (with idempotency keys), read receipts,
> media upload, verified download and delete, template listing, creation
> and deletion, and since M5b their edits, unpausing, comparison,
> archiving, the Template Library, migrations and template groups,
> commerce settings and the catalog's reads, QR codes, analytics and the
> block list (M5g), per-tenant rate limits, Meta's webhooks into the inbox
> and the event outbox, polling events (`GET /v1/events`), one event by
> id and live events as Server-Sent Events (M2b), the inbox's
> conversations, history and replies (M2a), its window, read route and
> messages by id (M2g), OTP codes with per-tenant settings and
> authentication templates (M3c), Embedded Signup in both partner
> modes, its attempts and their resumption, and offboarding (M3a),
> hosted Embedded Signup, several WABAs in one signup and pre-verified
> numbers (M3f), health, metrics and the OpenAPI document. **Not yet**: webhooks to
> your backend (M2), zero-tap templates and
> template previews (M3g), the Docker image and the TypeScript client
> (M4).
> [coverage.md](../coverage.md) tracks it.

## The shape of it

One deployment serves one Meta app, for many tenants (your merchants, or
your store). It has two listeners:

| Listener | Default bind | Serves | Who reaches it |
| --- | --- | --- | --- |
| public | `127.0.0.1:8080` | `GET /webhooks/meta` (Meta's subscription check), `POST /webhooks/meta` (Meta's deliveries), `POST /webhooks/flows/{waba_id}` (Meta's Flows data endpoint, with `WA_SERVER_FLOWS_PRIVATE_KEY`; [Flows](#flows)), `GET /livez` | Meta, through your HTTPS ingress |
| internal | `127.0.0.1:8081` | the `/v1` API, `/v1/admin`, `/livez`, `/readyz`, `/metrics`, `/v1/openapi.json`, `/v1/version` | your backends and operators, on the private network only |

Both default to loopback; in a container, set them to `0.0.0.0:…`
explicitly and keep the internal one off the internet (a network policy
admitting your backends). The service answers no CORS request: browsers
never call it, your backend does.

## Build and run

There is no image yet (M4). Build the binary from this repository:

```bash
cargo build --release -p meta-whatsapp-server
./target/release/meta-whatsapp-server --help
```

The binary composes the service's crates by its Cargo features, all on
by default but `api-cratestack`:

| Feature | Compiles in | Without it |
| --- | --- | --- |
| `api-axum` | the `/v1` API and its OpenAPI document (`meta-whatsapp-server-api-axum`) | the build fails: it is the API adapter the internal listener serves |
| `store-postgres` | the Postgres backend and its migrations (`meta-whatsapp-server-store-postgres`), and sqlx | a set `DATABASE_URL` is refused at start, naming the feature |
| `store-memory` | memory storage, `WA_SERVER_ENV=development` only (`meta-whatsapp-server-store-memory`) | development without `DATABASE_URL` is refused at start, naming the feature |
| `api-cratestack` (not a default; its name is pending the owner's confirmation before the first release) | the CrateStack API adapter (`meta-whatsapp-server-api-cratestack`, [roadmap](../roadmap.md) S10): one procedure, `version`, which nothing serves yet; CrateStack and its dependencies, `ring` among them | nothing changes: no route uses it before S13 |

Every command first installs aws-lc-rs as rustls' process-default
crypto provider, unless one is installed already (`meta_whatsapp_server::tls`,
design D20 (b)): a build with `api-cratestack` compiles both of rustls'
providers in, `ring` through CrateStack's HTTP client, and rustls cannot
choose between them by itself. An integrator who runs the service from
their own binary without `cli::run_with` calls
`tls::install_default_provider()` first, or installs the provider they
prefer before it; the rustdoc of `serve::serve` and its siblings says so
too. Postgres TLS needs it as well in that build: sqlx names its
provider itself (aws-lc-rs), but with `sslmode=verify-ca` it builds the
certificate verifier from the process default, and panics without one.

A production binary without memory storage, or a development one
without sqlx in its build:

```bash
cargo build --release -p meta-whatsapp-server --no-default-features --features api-axum,store-postgres
cargo build --release -p meta-whatsapp-server --no-default-features --features api-axum,store-memory
```

The HTTP layer every API adapter shares (listeners, authentication,
the error body, Meta's webhook, media, health and metrics) is
`meta-whatsapp-server-http`, always compiled in. A database migrated by
an earlier binary is the same database to this one: the migrations,
their history table and their lock did not change when the crates
split (roadmap S7).

The commands:

| Command | Does |
| --- | --- |
| `meta-whatsapp-server serve` | validates the configuration, migrates (unless `WA_SERVER_MIGRATE=skip`), serves both listeners until `SIGTERM` |
| `meta-whatsapp-server migrate` | creates or upgrades the tables, then exits (for a one-off job) |
| `meta-whatsapp-server openapi` | prints the OpenAPI 3.1 document |
| `meta-whatsapp-server healthcheck` | exits 0 when the internal listener's `/livez` answers 200 |
| `meta-whatsapp-server doctor` | checks the configuration, the database, the app's webhook subscription and the tokens, read-only; exits non-zero on a failure ([below](#check-a-deployment-doctor)) |
| `meta-whatsapp-server admin create-admin-key [--expires-at …]` | mints an admin key and prints it once |
| `meta-whatsapp-server admin create-platform-key --tenants '*' --scopes numbers [--expires-at …]` | mints a platform key and prints it once |
| `meta-whatsapp-server admin list-keys [--tenant <id>]` | lists the admin and platform keys (or a tenant's), never a secret |
| `meta-whatsapp-server admin revoke-key <key_id>` | revokes a key of any kind |
| `meta-whatsapp-server vault rotate` | re-encrypts every WABA's token under the active vault key (as `POST /v1/admin/vault/rotate`, without its request deadline; Postgres only: memory storage is per process) |

## Configure

Everything comes from the environment. Every secret may instead come from
a file: set `<NAME>_FILE` to its path (not both; one trailing newline is
dropped). `WA_SERVER_CONFIG`, the design's TOML file of non-secrets, is not
read yet: set, it stops the start.

| Variable | Default | What |
| --- | --- | --- |
| `DATABASE_URL` | required outside development | Postgres (a secret: it holds a password) |
| `WA_SERVER_ENV` | `production` | `development` allows memory storage, a throwaway vault key and a plain-`http` `WA_GRAPH_ENDPOINT` and webhook destination |
| `WA_SERVER_PUBLIC_BIND`, `WA_SERVER_INTERNAL_BIND` | `127.0.0.1:8080`, `127.0.0.1:8081` | must differ |
| `WA_APP_SECRET` | required | the Meta app's secret (`WA_APP_SECRET_PREVIOUS` too while rotating) |
| `WA_VERIFY_TOKEN` | required | the token you enter in the App Dashboard for the webhook |
| `WA_APP_ID`, `WA_ES_CONFIG_ID` | — | the Meta app's id and the Facebook Login for Business configuration: Embedded Signup needs both ([below](#embedded-signup)); without them its routes answer `409 signup_not_configured` |
| `WA_VAULT_KEY` | required with Postgres | base64 of 32 random bytes (`openssl rand -base64 32`): encrypts the business tokens |
| `WA_VAULT_KEY_ID` | `k1` | the key's id, recorded with each token |
| `WA_VAULT_PREVIOUS_KEYS` | — | older keys still read, `<id>:<base64>` comma-separated; every id once, the active one's included |
| `WA_OTP_PEPPER` | required with Postgres | at least 32 bytes, kept out of the database: it keys every OTP code's and store key's hash, and an OTP issue's idempotency fingerprint; rotating it kills the codes in flight (memory storage in development: a throwaway one) |
| `WA_SERVER_ERASURE_KEY` | unset: erasure off | at least 32 bytes from your secret manager, kept out of the database and for as long as your backups: the key the erasure journal's HMACs are made with (roadmap M2f). Unset, `POST /v1/numbers/{pn}/erasures` answers `409 erasure_not_configured` (memory storage in development: a throwaway one) |
| `WA_SERVER_FLOWS_PRIVATE_KEY` | unset: the Flows data endpoint off | the deployment's Flow private key (roadmap M5d): an unencrypted 2048-bit RSA key in PEM, from your secret manager (`_FILE` works too). Unset, `POST /webhooks/flows/{waba_id}` answers `404`. A password-protected PEM (what Meta's `openssl genrsa -des3` writes) or a key of another size refuses the start; [Flows](#flows) says how to remove the password |
| `WA_SERVER_OTP_DEFAULT_TENANT` | unset | the tenant whose OTP settings a tenant without its own borrows: the platform's number and template (decision D8's default). Unset, a tenant without settings answers `409 otp_not_configured` |
| `WA_ONBOARDING_MODE` | `tech_provider` | `solution_partner` also needs `WA_PARTNER_SYSTEM_TOKEN`, `WA_PARTNER_SYSTEM_USER_ID`, `WA_CREDIT_LINE_ID`, `WA_WABA_CURRENCY` (AUD, EUR, GBP, IDR, INR or USD): your credit line funds every merchant onboarded ([Solution Partner mode](#solution-partner-mode)) |
| `WA_SIGNUP_EVERY_GRANTED_WABA` | `off` | `on`: a signup that grants several WABAs binds every one to its tenant ([Several WABAs](#several-wabas-in-one-signup)); pending the owner's confirmation before the first release |
| `WA_HOSTED_SIGNUP` | `off` | `on`: `PARTNER_ADDED` webhooks are remembered and the operator claims one for a tenant ([Hosted signup](#hosted-signup)); needs `WA_SYSTEM_TOKEN` or Solution Partner mode; pending the owner's confirmation before the first release |
| `WA_SYSTEM_TOKEN` | the partner's `WA_PARTNER_SYSTEM_TOKEN` | your system user token, with `business_management`: it fetches a hosted signup's business token and manages the pool of pre-verified numbers; pending the owner's confirmation before the first release |
| `WA_PARTNER_BUSINESS_ID` | unset: the pool's routes off | your own business portfolio, whose pre-verified pool `/v1/admin/pre-verified-numbers` manages ([Pre-verified numbers](#pre-verified-numbers)); pending the owner's confirmation before the first release |
| `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION` | `graph` | whether an `account_update` webhook that offboards a merchant or revokes their credit line is confirmed with Graph first (`graph`: `debug_token` on the WABA's stored token, which must no longer be valid or granted the WABA) or acted on by its signature alone (`signature`) ([Offboarding](#offboarding)) |
| `WA_GRAPH_API_VERSION`, `WA_GRAPH_ENDPOINT` | `v25.0`, Graph | a proxy or a test stub: every token travels to it, so `https` outside development, and `serve` warns with its host |
| `WA_SERVER_MIGRATE` | `auto` | `skip` when a job runs `meta-whatsapp-server migrate` |
| `WA_SERVER_SHUTDOWN_GRACE` | `25s` | how long open requests get after `SIGTERM` |
| `WA_SERVER_OUTBOX_RETENTION` | `7d` | how long the event outbox keeps an event (a positive duration, `30d`, `72h`); the outbox's own retention (design D10, decided on 2026-09-26 under the owner's delegation: retention is set per store) |
| `WA_SERVER_TOKEN_EXPIRY_LEAD` | `7d`, or the retention when shorter | how long before a stored token's expiry its tenant is told (`token_expiring`, once per token and expiry): a positive duration no longer than `WA_SERVER_OUTBOX_RETENTION` (the start is refused otherwise), or `off`. One replica at a time runs the check, every 10 minutes (roadmap M3e; design D180) |
| `WA_SERVER_INBOX_RETENTION` | `keep` | how long the inbox keeps a customer's messages, window events and thread ownership records: `keep` (everything, design D10's default) or a positive duration (`90d`); housekeeping purges what is older, on one replica at a time. The inbox store's own setting, built in with the backend: a backend of your own built without it is refused at start. Which one you set follows your privacy obligations |
| `WA_SERVER_INBOX_REPLY_CHECKS` | every check on every number | the local checks an inbox reply makes, per number: `<phone_number_id>:<checks>`, comma-separated, `<checks>` being `all`, `none`, or the checks left on joined by `+` (`window`, `trust_handover`, `thread_owner`; `trust_handover` only with `window`). `106540352242922:window+trust_handover` turns the ownership check off on the number of the account's escalation partner; `…:window+thread_owner` turns the handover trust off on a number that receives standby copies. A check off only lets Meta decide |
| `WA_SERVER_LOG_FORMAT`, `RUST_LOG` | `json`, `info` | `text` for humans |
| `WA_SERVER_IDEMPOTENCY_TTL` | `24h` | how long an `Idempotency-Key`'s answer is kept (more than the key's one-minute lease) |
| `WA_SERVER_MEDIA_MAX_BYTES` | `104857600` (100 MiB) | the largest upload, and the largest streamed download |
| `WA_SERVER_MEDIA_CONCURRENCY` | `4` | uploads and whole downloads held in memory at once on a replica, one tenant holding half of them at most (the next is `429`) |
| `WA_SERVER_LEGAL_ACTS` | `off` | `on` lets tenants take the legal act their keys allow ([Number settings](#number-settings)): the India compliance information declaration, and the operator's signing of the max price agreement ([Marketing](#marketing-messages-api-max-price-and-welcome-message-sequences)). Off, each answers `403 feature_not_available` and asks Meta nothing; the Official Business Account application and the contact book erasure are not gated by it |
| `WA_SERVER_MEDIA_CONVERTER`, `WA_SERVER_MEDIA_CONVERTER_PROGRAM` | `none`; `ffmpeg` from `PATH` | `ffmpeg` turns on conversion on upload (see [Media](#media)): a build with the `ffmpeg` feature (the image has none) and an `ffmpeg` you install; the program is only read with it |
| `WA_SERVER_MEDIA_STREAMS` | `16` | streamed downloads (`?stream=true`) at once on a replica, one tenant holding half of them at most (the next is `429`) |
| `WA_SERVER_RATE_SEND`, `WA_SERVER_RATE_SEND_BURST` | `20`, `40` | requests a second per tenant and replica for writes (sends, read receipts, media, profile) |
| `WA_SERVER_RATE_READ`, `WA_SERVER_RATE_READ_BURST` | `50`, `50` | the same for reads |
| `WA_SERVER_RATE_TEMPLATES`, `WA_SERVER_RATE_TEMPLATES_BURST` | `2`, `2` | the same for template management |
| `WA_SERVER_RATE_OTP_ISSUE`, `WA_SERVER_RATE_OTP_ISSUE_BURST` | `5`, `5` | the same for sending OTP codes (per recipient, the codes' own cooldown and issue limit apply too) |
| `WA_SERVER_RATE_OTP_VERIFY`, `WA_SERVER_RATE_OTP_VERIFY_BURST` | `20`, `20` | the same for checking OTP codes |
| `WA_SERVER_REGISTRATION_ATTEMPTS`, `WA_SERVER_REGISTRATION_WINDOW` | `5`, `72h` | registrations and deregistrations of one number per window, counted for every replica together (Meta allows 10 of each in 72 hours and locks the number on the 11th) |
| `WA_SERVER_VERIFICATION_ATTEMPTS`, `WA_SERVER_VERIFICATION_WINDOW` | `10`, `1h` | verification codes requested and checked for one number per window, the same way |
| `WA_SERVER_UNDATED_AFTER_MOVE` | `operator_only` | after a WABA moved to its tenant from another one, who gets its undated events (errors, history chunks, group updates without a date), which may be the previous tenant's that Meta redelivers: nobody within the window (`operator_only`, the operator still sees them), or the new holder (`current_holder`, the behaviour before this setting). A WABA no other tenant held is never concerned (design D36) |
| `WA_SERVER_UNDATED_AFTER_MOVE_WINDOW` | `169h` (7 days and an hour) | that window, from when the previous tenant's binding was removed; a positive duration, with `operator_only` only (set with `current_holder`, the start is refused) |
| `WA_SERVER_MOVE_SKEW_MARGIN` | `0s` | after a move only: how far past the second the new binding began an event Meta dated must be to reach the new holder (widen it if Meta's clock may run ahead of the database's) |
| `WA_SERVER_STREAMS_PER_TENANT` | `20` | live event streams one tenant holds open at once on a replica, its number streams included (the next is `429 too_many_streams`) |
| `WA_SERVER_DATA_KEY` | unset | base64 of 32 random bytes: seals the webhook endpoints' signing secrets ([Webhooks out](#webhooks-out)); unset, a throwaway key on memory storage, and on Postgres webhooks out are off (`409 webhooks_not_configured`) |
| `WA_SERVER_DATA_KEY_ID`, `WA_SERVER_DATA_PREVIOUS_KEYS` | `k1`, — | the data key's id (1 to 32 of `A-Z a-z 0-9 - _`) and older keys still read, `<id>:<base64>` comma-separated, every id once |
| `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS` | none: every destination refused | where webhook endpoints may point: host names (`hooks.example.com`), `*.`-prefixed ones (`*.example.com`) and CIDRs (`10.0.0.0/8`, never `/0`), comma-separated |
| `WA_SERVER_WEBHOOK_RETRY_WINDOW` | `72h` | how long a delivery is retried before it is `failed` |
| `WA_SERVER_WEBHOOK_IN_FLIGHT`, `WA_SERVER_WEBHOOK_IN_FLIGHT_PER_TENANT`, `WA_SERVER_WEBHOOK_IN_FLIGHT_PER_ENDPOINT` | `32`, `8`, `4` | webhook deliveries attempted at once on a replica: in all, of one tenant, to one endpoint |
| `WA_SERVER_WEBHOOK_LEASE` | `60s` | how long a replica holds a delivery it claimed before another may attempt it; at least `30s`; keep the replicas' clocks within it of each other |
| `WA_SERVER_STREAMS_PER_REPLICA` | `1000` | live event streams open at once on a replica, every tenant's (keep it under the internal listener's 4,096 connections) |

**The service refuses to start** rather than run unsafely: on a missing
or blank app secret or verify token, a missing vault key or a pepper
under 32 bytes with Postgres, memory storage outside
`WA_SERVER_ENV=development`, identical binds, Solution Partner mode
without its four settings, a plain-`http` Graph endpoint outside
development, a vault key id used twice, a value it cannot parse, or a
variable set both directly and as a file. The message names the
variable, never its value.

A minimal production start:

```bash
export DATABASE_URL=postgres://wa:…@db.internal:5432/wa
export WA_APP_SECRET_FILE=/run/secrets/wa_app_secret
export WA_VERIFY_TOKEN_FILE=/run/secrets/wa_verify_token
export WA_VAULT_KEY_FILE=/run/secrets/wa_vault_key
export WA_OTP_PEPPER_FILE=/run/secrets/wa_otp_pepper
export WA_SERVER_PUBLIC_BIND=0.0.0.0:8080 WA_SERVER_INTERNAL_BIND=0.0.0.0:8081
meta-whatsapp-server serve
```

For a local try without Postgres, `WA_SERVER_ENV=development` runs on
memory with a throwaway vault key; everything is lost on restart. The
admin CLI talks to the database and cannot reach it, so `serve` writes a
one-time admin key for that process to standard error at start (never
to the logs). For a local Postgres, `docker compose -f compose.test.yaml
up -d --wait` starts one on port 55432 (`postgres://wa:wa@127.0.0.1:55432/wa`).

### Storage and migrations

Postgres holds everything: the library's tables (`wa_kv`, the inbox's
`wa_messages`, `wa_conversations`, `wa_window_events`, `wa_thread_owners`,
`wa_synced_contacts` and `wa_identity_links`) and the service's (`wa_server_tenants`,
`wa_server_api_keys`, `wa_server_wabas`, `wa_server_numbers`,
`wa_server_waba_tombstones` (each WABA's last unbinding, which tells a
move from a first onboarding), `wa_server_idempotency`, the event outbox
`wa_server_events` and its per-tenant `wa_server_event_streams`), each
with its own migration history. `serve` and `migrate` run both under an
advisory lock, so replicas starting together never interleave them.
Service migrations only ever add (expand, then contract in a later
release), so replicas of two versions can share the database during a
rolling deploy. Back up the database and, separately, the vault key: a
backup without the key cannot decrypt a single token.

Service migration 4 (roadmap S2b) adds a binding's generation and the
record of a WABA's moves: a database an earlier binary migrated is
migrated forward on the next `serve` or `migrate`, adding columns and a
table and rewriting nothing. The WABAs bound before it keep the routing
they had (an event of their binding's own second stays operator-only,
their undated events reach their tenant): the service does not know
whether another tenant held them. A WABA unbound by an earlier release
leaves no record, before the upgrade as during its rolling deploy: bound
afterwards to another tenant, it counts as a first onboarding, and its
previous holder's undated events that Meta still redelivers reach the
new holder (as they did before this release); a WABA moved wholly before
the upgrade keeps the earlier rules, with the same result. Do not move
WABAs between tenants in the 7 days and an hour before the deploy, nor
during it.

Service migration 7 (roadmap M2e) adds the media id an event records as
received (`wa_server_events.media_id`, no default: nothing is rewritten)
and builds its index, which reads the event outbox once and holds its
inserts until it is done: webhook deliveries wait meanwhile (or Meta
retries them). The outbox holds only `WA_SERVER_OUTBOX_RETENTION` of
events; a million cached rows took about half a second on Postgres 16.
Rows from before it record no media id: their media keep Meta's
`phone_number_id` check ("Media" below).

Service migration 8 (roadmap M2f) adds the identifiers each event names
its customers by (`wa_server_events.contacts`, no default: nothing is
rewritten) and builds its GIN index, which reads the outbox once and
holds its inserts meanwhile, as migration 7's does. Rows from before it
name nobody to an erasure: they go with the outbox's retention.

The inbox tables are kept forever unless `WA_SERVER_INBOX_RETENTION` is
set (housekeeping then purges what is older than it, roadmap M2a). The
event outbox keeps whole events (message texts, BSUIDs, phone numbers),
and since migration 8 the identifiers they name, for
`WA_SERVER_OUTBOX_RETENTION` (7 days by default). A customer is erased
on a number when you ask ("Erasing a customer" below); Postgres's own
copies (dead rows, WAL, replicas, backups, statement logs) are yours to
handle ([production.md § 8](production.md#8-retention-and-erasure-on-postgres)).

## Tenants and keys

A tenant is one of your merchants (or your own store). Its id is yours
(the CMS merchant id, say): 1 to 64 characters of `[A-Za-z0-9._:-]`,
immutable. Three kinds of API key, all `wak_<key id>_<secret>`, sent as
`Authorization: Bearer <key>`:

| Key | Acts as | For |
| --- | --- | --- |
| tenant key | its tenant | a backend that is one tenant (the store) |
| platform key | the tenant named in the `WA-Tenant` header, if among its allowed tenants (`*` or a list) | the CMS backend, acting for each merchant |
| admin key | nobody: `/v1/admin` only | operators |

Tenant and platform keys carry scopes (`numbers`, `send`, `media`,
`templates`, `flows`, `inbox`, `events`, `otp`, `signup`, `payments`,
`erasure`, `groups`, `calls`, `commerce`, `qr_codes`, `analytics`, `contacts` and
`routing` today; `webhooks` for the routes to come). `payments` (roadmap P2),
`erasure` (roadmap M2f), `flows` (roadmap M5d), `groups` (roadmap M5f), `calls`
(roadmap M5e), the four of roadmap M5g and `routing` (roadmap M5l) are new: a key minted before them does not carry them, so
mint one that does for those routes. `templates` does not grant `flows`: a
key that edits templates does not manage Flows. `inbox` does not grant
`erasure`: give the key that erases to the part of your backend that
handles privacy requests only. The service keeps only each key's id and the SHA-256 of its
secret, and shows the key once. Rotate by minting a new one, deploying
it, then revoking the old one: revocation and suspension take effect on
the next request, on every replica.

**1. The first admin key** comes from the CLI, never from the
environment:

```bash
ADMIN_KEY=$(DATABASE_URL=… meta-whatsapp-server admin create-admin-key --name ops)
```

**2. A tenant and its key**, through the admin API:

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"id": "merchant-42", "name": "Lucky Shrub"}'
curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants/merchant-42/keys \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"scopes": ["numbers"], "name": "medusa"}'
```

The second answer is `{"key": "wak_…", "api_key": {…}}`: store `key` in
your backend's secret manager now; it is not shown again. A platform key
for the CMS is `POST /v1/admin/platform-keys` with
`{"tenants": "*", "scopes": ["numbers"]}` (or a list of tenant ids).

**3. A WABA for the tenant.** Merchants will connect their own numbers
with Embedded Signup (M3). The platform's own WABA is attached by an
operator with a system user token that can reach it; the service asks
Meta for the WABA's numbers with that token, binds exactly those to the
tenant, stores the token encrypted and subscribes the app to the WABA's
webhooks. Keep the token out of your shell history and the process list:
pass the body from a file.

```bash
# attach.json: {"waba_id": "102290129340398", "token": "EAAG…"} (then delete it)
curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants/merchant-42/wabas \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d @attach.json
```

A token Meta rejects while the numbers are listed is `422
invalid_request` on `token`, and nothing is bound or stored. Subscribing
the app comes last, once the WABA is bound and the token stored: if Meta
refuses it, the WABA stays attached and the call answers Meta's error
with `"step": "subscribe_app"` and `"resumable": true`; repeat it once
fixed. A token Meta rejects at that point is `409 reconnect_required`
(its numbers are marked so): repeat the attach with a valid token. An
attach during which another operator unbinds the WABA (and attaches it
again, to another tenant or the same one) answers `503
storage_unavailable` (retryable) and takes back the token it stored;
repeat it, and it answers for the WABA as it is then (refused, as
below, when another tenant holds it). A
WABA bound to one tenant is refused to another (`409
waba_owned_by_another_tenant`, decision D4, also for one of its
numbers); `GET /v1/admin/wabas/{waba_id}` says who holds it, and
`DELETE /v1/admin/wabas/{waba_id}/binding` frees it: the service
unsubscribes the app with the stored token if it still works (Meta
refusing does not stop it), then deletes the token and the bindings. That
is also the way to free a WABA whose token no longer works: its tenant
cannot disconnect it. Suspend a tenant with `PATCH
/v1/admin/tenants/{id}` and `{"status": "suspended"}`; delete it with
`DELETE /v1/admin/tenants/{id}`, which first disconnects each of its
WABAs from Meta and stops at the first that fails (`409` for one without
a usable token: unbind it first); its events go with it, and platform
keys listing it stop allowing it, for good (see
[Receiving Meta's webhooks](#receiving-metas-webhooks)). A tenant id
already taken is `409 tenant_exists`.

**One token, several tenants.** Nothing stops you attaching the same
system user token to WABAs of different tenants, and the service does
not ask Meta what else a token reaches when you attach it. Such a token
reaches every one of those WABAs, so what keeps one of those tenants out
of another's media and templates is only the service's check, on each
route taking an id, that the id is the path's number's or WABA's own
(see [Media](#media) and [Templates](#templates)). Where you can,
attach a token that reaches that tenant's WABAs only.

**4. Rotating the vault key.** Put the new key in `WA_VAULT_KEY` (with a
new `WA_VAULT_KEY_ID`) and the old one in `WA_VAULT_PREVIOUS_KEYS`,
deploy, then call `POST /v1/admin/vault/rotate` (or run
`meta-whatsapp-server vault rotate`): it re-encrypts every bound WABA's
token under the new key, then every record the vault keeps past a
binding, and answers `{wabas, rotated, failed, records,
failed_records}`. Drop the old key once both `failed` and
`failed_records` are empty. The route is a request like any other, cut
at 55 s: a walk too large to finish in time answers `504 timeout` with
part of the tokens rotated. Repeating it is safe (tokens already under
the new key are only read), and the command has no deadline. Rotate at
least yearly, and at once on a suspected exposure
([production guide](production.md#vault-key-cadence)).

Solution Partner onboarding keeps records past a WABA's binding: its
credit ledger outlives the token, and a business's revocation marker
outlives both. Since roadmap M3a the service remembers each such record
when it may be written (`wa_server_vault_records`: the WABA at its
onboarding's approval and its offboarding, the owner business with it),
and the rotation walks them besides the bindings (`records`; one that
fails is `failed_records`, as `waba/<id>` or `business/<id>`), so empty
lists again mean that nothing still needs the old key (the service
onboarded no Solution Partner merchant before M3a, so no earlier record
escapes the walk).

## A first call

With the tenant key (or the platform key and `WA-Tenant: merchant-42`):

```bash
curl -sS http://127.0.0.1:8081/v1/numbers -H "Authorization: Bearer $KEY"
curl -sS http://127.0.0.1:8081/v1/numbers/106540352242922 -H "Authorization: Bearer $KEY"
```

The first lists the tenant's numbers and their connection status from the
service's own records; the second asks Meta for the number's display
number, verified name, quality rating, name status and throughput, with
the WABA's token. Every call checks, in this order: the key (`401`), the
tenant (`403`), the scope (`403`), that the tenant owns the number or WABA
in the path (`404` otherwise, exactly as for a number that does not
exist), and only then reads the token. `GET`/`PATCH
/v1/numbers/{pn}/profile` reads and changes the business profile (its
picture too: [below](#profile-picture-and-display-name)), and
`DELETE /v1/wabas/{waba_id}` disconnects a WABA: the service unsubscribes
the app with the WABA's token, and deletes the token and bindings only if
Meta agreed.

## Send messages

`POST /v1/numbers/{pn}/messages` (scope `send`) takes Meta's message
object under the service's envelope: `to`, `type`, the object `type`
names, and optionally `reply_to` (a received message's id) and
`callback_data` (echoed in the message's status events as
`data.status.biz_opaque_callback_data`, Meta's name for it):

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/numbers/106540352242922/messages \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -H "Idempotency-Key: order:1234:shipped" \
  -d '{"to": {"phone": "+16505551234"}, "type": "text",
       "text": {"body": "Your order 1234 has shipped."}, "callback_data": "order:1234:shipped"}'
```

It answers `202 {"message_id": "wamid.…", "contacts": [...]}`: Meta
accepted the message; delivery arrives later as `status_updated` events
(`GET /v1/events`), whose `data.status.id` is that `message_id` and
`data.status.status` the new state (`sent`, `delivered`, `read`,
`played`, `failed`). After a `504`, which gives you no `message_id`, look for your
`callback_data` in `data.status.biz_opaque_callback_data` before
concluding the message never went out.

- **Recipients**: `{"phone": "+16505551234"}` in E.164 **with** its `+`
  (a number without it is refused, `422` on `to.phone`, before any
  request: Meta would read it as local to the sending number's country),
  `{"user_id": "US.13491208655302741918"}` (the business-scoped user id a
  webhook carries), both (Meta uses the phone number), or
  `{"group_id": "…"}`.
- **Types**: `text`; `image`, `video`, `audio`, `document`, `sticker` by
  uploaded `id` or `https://` `link` (Meta fetches it, the service never
  does); `location`, `contacts`, `reaction`; `template` (Meta's object:
  `name`, `language`, `components`); `interactive` of type `button`,
  `list` or `cta_url`, or the payment messages `order_details` and
  `order_status` ([Payments](#payments)); since roadmap M5a also `pin`
  (to a group: `pin` with `expiration_days`, 1 to 30, or `unpin`), and
  the `interactive` types `location_request_message`,
  `request_contact_info`, `call_permission_request` (its body only),
  `voice_call`, `flow`, `address_message` (India), `carousel` (2 to 10
  media cards, an image or video `link` each, or product cards),
  `product`, `product_list` and `catalog_message`. Each is written as
  Meta's page for it writes it, `card_index` and every `action.name`
  included. Anything else is `422 unsupported_message_type`. Every limit
  Meta documents is checked before the request (`422 invalid_request`
  with `field`), and so is every key: one the service would not pass on
  to Meta (a footer on a carousel, a misspelt Flow parameter, a card's
  media by `id`) is `422` on its path, never dropped.
- **Direct Send** (`direct-send`, for the accounts Meta made eligible):
  `category` (`utility`, `authentication` or `service`; any other is
  `422` on `body`), `ttl_seconds` and `direct_send_config:
  {template_name}` ride on any type, checked by the library's rules
  before the request: a TTL only with `utility` (30 to 43,200 seconds)
  or `authentication` (30 to 900), a business-named template with
  `utility` only (lowercase letters, digits and `_`), no
  `authentication` message to a `user_id` alone (`422` on `to`), and
  the tighter limits (1,024 characters of text, 60 of a header). Which
  formats Direct Send takes is Meta's to answer (its supported features
  page), as is an account's eligibility (`100`).
- **Objects named by id**: a Flow's `flow_id`, a product message's `catalog_id` and
  (since M5b) every `catalog_id` a template's product parameters carry
  are checked first to be the number's WABA's, with the
  number's token (`GET /{flow_id}`, `GET /{waba_id}/product_catalogs`);
  another WABA's, or an id Meta refuses, is `404 not_found` and nothing
  is sent (a failed check: `may_have_been_sent: false`). A Flow's
  `flow_name` is Meta's to find in the number's WABA. The `flow_token`
  is never logged.
- Meta enforces the 24-hour window: a free-form message outside it is
  `409 customer_service_window_closed` (send a template). Every type
  above is free-form but `template` and a Direct Send `utility` or
  `authentication` message, which Meta sends as a template; a call
  permission request, a call button, a Flow or a catalog outside the
  window goes as a template with that button instead.
- `POST /v1/numbers/{pn}/messages/{message_id}/read` shows the blue ticks
  on a received message; `{"typing_indicator": true}` shows "typing…" too,
  only when you are about to reply.

### Idempotency keys

Sends, uploads, template creation and a display name's request take an `Idempotency-Key` header (1
to 255 visible ASCII characters), scoped to the tenant. Derive it from
your own records (`order:1234:shipped`) and set `callback_data` to the
same. With one:

- the same key and request never reach Meta twice: a repeat gets the
  first answer back, byte for byte, with `Idempotent-Replayed: true`,
  including a `504 timeout` (the message may have gone out: reconcile
  through its status events, never with a new key);
- an answer that proves nothing was sent (`may_have_been_sent: false`: a
  validation refusal, a 4xx from Meta such as `409
  customer_service_window_closed`, throttling) **releases** the key, so
  you may repeat the request with it once the cause is fixed;
- the same key with another request is `422 idempotency_key_reused`; a
  repeat while the first still runs `409 idempotency_in_progress`; a
  repeat after the first died mid-way (its lease, twice the Graph
  timeout, ran out) `409 outcome_unknown`, `may_have_been_sent: true`,
  never a new send.

Answers are kept `WA_SERVER_IDEMPOTENCY_TTL` (24 hours), then the key is
free again.

What "the same request" means: the operation and what it acts on, never
how its path is spelled. A key covers the operation (a send, an upload,
a template creation), the number or WABA its path names, read as an id
(`/v1/numbers/106540352242922/messages` and the same path with the
digits percent-encoded are one request), and the body: a JSON body as
JSON (key order and whitespace do not count; `1` and `1.0` are two
bodies), an upload as its type, its file name as the service sends it to
Meta (cleaned: `résumé.pdf` goes as `r_sum_.pdf`) and its file. The same key and body on another of your numbers,
or for another operation, is `422 idempotency_key_reused`.

> **Changed before the first release** (roadmap S6, design D27): before
> it, a key covered the request's method, its path as sent and its
> body. **Keys claimed before the upgrade keep replaying:** a repeat of
> the same request, spelled as it was then (the same path, the same
> body), still gets the kept answer, for as long as the record lives
> (`WA_SERVER_IDEMPOTENCY_TTL` after the first request, 24 hours by
> default), because the service also matches the fingerprint of then;
> with another body it is `422 idempotency_key_reused`, as before. Two
> limits of that window: a repeat of an old key under another spelling of
> its path (percent-encoded digits) is `422`, as it was before the
> upgrade; and during a rolling deploy, a key claimed by an upgraded
> replica and repeated on one not yet upgraded is `422` there (the old
> release knows only the old fingerprint), never a second send. Treat a
> `422 idempotency_key_reused` on a key you reused on purpose as "this
> key was used": reconcile the first request through its status events,
> and do not resend it under a new key unless you know it never went out.

## Media

`POST /v1/numbers/{pn}/media` (scope `media`) uploads a
`multipart/form-data` form with `file` and `type` (the MIME type, one of
Meta's supported types) and answers `201 {"media_id": "…"}`, the id to
send in a message:

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/numbers/106540352242922/media \
  -H "Authorization: Bearer $KEY" -F type=image/png -F file=@voucher.png
```

Type and size are checked first: an unsupported type is `422` on `type`,
a file over its type's limit (5 MiB for images, 16 MiB for audio and
video, 500 KiB for stickers, 100 MiB for documents) or over
`WA_SERVER_MEDIA_MAX_BYTES` `413 media_too_large`. Send `type` before
`file`: the upload is then read no further than its type allows. A
form's framing (what precedes each part's data) is read up to 16 KiB;
past it, `422` on `body`. An upload is one request, and every request
is answered within 55 s: your file must reach the service, and the
service's copy reach Meta, within that time, or the answer is `504
timeout`. A 100 MiB document needs about 15 Mbit/s for your part
alone.

### Resumable uploads

Meta's Resumable Upload API gives the **handle** a template's
`header_handle` takes, and a profile picture's `profile_picture_handle`
(through the profile's route, which names the **upload** and keeps the
handle in the service: [Profile picture and display
name](#profile-picture-and-display-name)). It needs `WA_APP_ID`
(without it, `409 resumable_upload_not_configured`).

```bash
# 1. open it: the file's name, length and type (pdf, jpeg, jpg, png or mp4)
curl -sS -X POST http://127.0.0.1:8081/v1/numbers/106540352242922/media-uploads \
  -H "Authorization: Bearer $KEY" -H "Idempotency-Key: offer-header-1" \
  -H "Content-Type: application/json" \
  -d '{"file_name": "offer.png", "file_length": 12, "file_type": "image/png"}'
# → 201 {"upload_id": "up_…", "file_offset": 0, "complete": false, …}

# 2. send the bytes from file_offset (the whole file in one chunk is usual)
curl -sS -X POST "http://127.0.0.1:8081/v1/numbers/106540352242922/media-uploads/$UPLOAD_ID?file_offset=0" \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/octet-stream" \
  --data-binary @offer.png
# → 200 {"file_offset": 12, "complete": true, "handle": "2:…"}

# 3. after a timeout or a lost connection, ask where Meta is, and resume
curl -sS http://127.0.0.1:8081/v1/numbers/106540352242922/media-uploads/$UPLOAD_ID \
  -H "Authorization: Bearer $KEY"
```

The `upload_id` is the service's, for that number only: another
tenant's, another number's or an expired one is `404` and Meta is not
asked. Keep the **handle** from the answer that completes the file: the
service keeps it, for the status to answer again, 24 hours from the
start. Bounds, all checked before Meta is asked: the length from 1 byte
to its type's limit in Meta's media table (5 MiB for an image, 16 MiB
for a video, 100 MiB for a document) and `WA_SERVER_MEDIA_MAX_BYTES`
(`413 media_too_large`); a chunk of at least a byte, from an offset
before the end and no further than the declared file's end (`422` on
`file_offset` or `body`, `413`); each chunk holds one of the tenant's
media slots (`429` past its share). A chunk is not replayed: after a
`504 timeout` it may have arrived, so read the status and resume from
the offset it gives. Meta's pages state no chunk size, no total size and
no lifetime for a session or a handle: the limits are the service's.

### Converting on upload

With `WA_SERVER_MEDIA_CONVERTER=ffmpeg` (a build with the `ffmpeg`
feature and an `ffmpeg` installed), an upload may send a `convert` field
instead of `type`: `ogg_opus` (a voice message), `static_sticker`,
`animated_sticker`, `jpeg` or `png`. The file is converted and **the
converted file** is uploaded, as the format's type and named after your
file with the format's extension:

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/numbers/106540352242922/media \
  -H "Authorization: Bearer $KEY" -F convert=ogg_opus -F file=@recording.wav
```

The file's own bytes say what it is; media the converter does not read
is `422` on `file`. A converter that is missing, past its 20 s, or
killed is `502 media_conversion_failed` (retryable only after a
timeout), and nothing was sent to Meta. Without a converter configured
nothing changes: `convert` is an unknown field (`422` on `body`), and a
type Meta does not take is `422` on `type`.

Uploads and whole-file downloads share `WA_SERVER_MEDIA_CONCURRENCY`
slots on a replica, streamed downloads `WA_SERVER_MEDIA_STREAMS`, and
one tenant holds half of either at most: past its share, the next
transfer is `429 too_many_requests` (`Retry-After: 1`) before its body
is read or Meta is asked. Retry it once one of yours has finished.

`GET /v1/numbers/{pn}/media/{media_id}` downloads a file (an upload's or
a received message's), verified against the SHA-256 Meta reports, and
answers it with Meta's MIME type, `X-WA-SHA256` (hex), `Content-Disposition:
attachment` and a sandboxing `Content-Security-Policy` (a customer's
HTML or SVG never runs as a page). By default the whole file is read,
verified, and only then answered: at most `?max_bytes=` (default and
cap 16 MiB; more is `413 media_too_large`), and a mismatch is `502
integrity` with not one byte of the file. With `?stream=true` (up to
`WA_SERVER_MEDIA_MAX_BYTES`) the bytes are forwarded as they arrive, one
chunk behind, and a mismatch **aborts the connection** before the last
chunk: the body never ends cleanly (curl: "transfer closed with
outstanding read data remaining", or "Empty reply from server" for a
small file). Write the bytes somewhere temporary and use them only once
the body ended cleanly. `DELETE /v1/numbers/{pn}/media/{media_id}`
deletes an upload.

**A media id is the number's.** It is digits (`422` on `media_id`
otherwise). Both routes ask Meta with `phone_number_id={pn}`, so Meta
acts only on that number's media, and a deletion first checks the id is
that media (an id names any Graph object a token reaches: a flow, a
group). Another number's media id, another tenant's included, answers
`404 not_found`, exactly like one that does not exist.

**A file your customer sent.** Meta documents that check for media
uploaded on the number, not for a media id that arrived by webhook
([OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md) #43). So the service
remembers the media id of each image, video, audio, document or sticker
a customer sent (an edit into one included) with the event it recorded
for your tenant (`message_received`, the `id` of the message's media
object), and:

- a media id received **on this number, for your tenant**, downloads
  with `GET /{media_id}` and no `phone_number_id`, with your number's
  token, through the same checks (Meta's host only, `max_bytes`, the
  SHA-256). Download it from the number that received it. A deletion of
  it keeps the check;
- a media id received **anywhere else** (another number of yours,
  another tenant's number, or this number before it came under your
  tenant) answers `404 not_found` for a download and a deletion, and
  the service asks Meta nothing (so it answers sooner than Meta's check
  would: its timing tells a caller that an id it already holds was
  received elsewhere in the deployment, and nothing more; design D78);
- any other id (an upload's, or one received before your deployment ran
  this release, or longer ago than `WA_SERVER_OUTBOX_RETENTION`, 7 days
  by default: about as long as Meta keeps it) keeps the check.

A received file that answers `404` from the number that received it,
within those days, is worth reporting: Meta's behaviour for received
media under the check is still to be confirmed against its live API
(#43).

## Templates

With scope `templates`, on a WABA of the tenant:

| Route | Does |
| --- | --- |
| `GET /v1/wabas/{waba_id}/templates` | a page of `{id, name, language, status, category, components}`, `?status=`, `?name=`, `?limit=`, `?cursor=`; cached 60 s per WABA (Meta allows 200 management calls an hour per WABA) |
| `GET /v1/wabas/{waba_id}/templates/{id}` | one template of this WABA (2 to 6 management calls: its name, then the WABA's templates of that name, below) |
| `POST /v1/wabas/{waba_id}/templates` | create from Meta's JSON (`name`, `language`, `category`, `components`), checked locally first; `201 {id, status, category}`; takes an `Idempotency-Key` |
| `DELETE /v1/wabas/{waba_id}/templates?name=…[&id=…]` | every language of a name, or one (with an id, the WABA's templates of that name are listed first) |
| `POST /v1/wabas/{waba_id}/templates/authentication` | an authentication template in one or more languages (Meta's bulk management: each language created, or updated when it exists): `name`, `languages`, `otp_type` (`copy_code`, or `one_tap` with `supported_apps`), and optionally `add_security_recommendation`, `code_expiration_minutes` (keep it equal to the OTP settings' `ttl_seconds`) and `message_send_ttl_seconds`; `201 {data: [{id, status, language}]}`; takes an `Idempotency-Key`. `zero_tap` is refused (`422` on `otp_type`): creating one accepts Meta's zero-tap terms, which the service does not do for you (roadmap M3g) |

A definition breaking a documented limit is `422 invalid_request` with
`field`, before any request, and so is a key the service would not pass
on to Meta (a misspelling, or a field it does not know): never silently
dropped. The template definitions and sends of Meta's examples pass,
`body_text` as a flat list (`["Pablo", "860198"]`, the positional
parameters syntax) included, the payment templates among them (roadmap
P1c: the order details and payment request buttons, the order status
component, each checked by the library's payment rules; a payment button
of a currency or payment method the library has no rules for is `422
invalid_request` on its `parameters`, and an `order_details`,
`payment_request` or `order_status` object in a component or parameter
the library does not type, a `type` spelled `Button` for one, on that
component or parameter), but for shapes the library cannot carry yet,
refused on the key named:

- in a definition, `optimization_spec` (Marketing Messages API
  bidding), and `package_name` and `signature_hash` on a one-tap or zero-tap
  button itself, which Meta accepts on Graph API v20.0 and older only:
  write them in the button's `supported_apps`;
- a template as `GET` answers it, which is not a definition: its `id`,
  `status` or `correct_category`.

**Zero-tap is refused on both routes.** A zero-tap button's
`zero_tap_terms_accepted: true` accepts Meta's zero-tap terms, a legal
act the service does not take for a tenant: a definition holding it is
`422 invalid_request` on its path
(`components[2].buttons[0].zero_tap_terms_accepted`), and nothing
reaches Meta (it was passed through from M1b until M3c's review; `false`
is refused too, Meta would not create it). Who may accept them is the
owner's (roadmap M3g).

Meta refusing a definition is `422 template_rejected`, a WABA
at its limit `409 template_limit_reached`. Review results arrive as
`template_status_updated` events (`GET /v1/events`). A creation or deletion drops
the WABA's cached pages on the replica that made it; others may answer
the old list for up to 60 seconds.

**A template id is the WABA's.** Meta's template object does not say
which WABA it belongs to, so the service looks the id up among the
WABA's own templates: another WABA's template id, another tenant's
included, answers `404 not_found` like one that does not exist, and a
deletion by that id deletes nothing. A deletion is an `audit` event
with the public id of the key that asked it.

The lookup has limits to know about:

- It lists the WABA's templates with the list's `name=` filter, which
  Meta's reference for that list does not document (the service's tests
  cannot prove Meta honours it). If Meta ignored it, a template would
  be looked for among all the WABA's templates, as far as the pages
  below reach.
- It reads at most 5 pages of 100 templates (a name has one template
  per language, so one page is the rule): a template of the WABA beyond
  them answers `404 not_found`.
- Each lookup costs 2 to 6 of the WABA's 200 management calls an hour:
  `GET …/templates/{id}` asks for the id's name, then 1 to 5 pages; a
  deletion by id reads 1 to 5 pages, then deletes. Lists answered from
  the cache cost nothing, lookups are never cached.

### After the creation (roadmap M5b)

The same scope, the same WABA's token, each route one of the library's
calls:

| Route | Does |
| --- | --- |
| `PATCH /v1/wabas/{waba_id}/templates/{id}` | edit (`category`, `components`, which replaces all of them, `message_send_ttl_seconds`, …: Meta's `POST /{TEMPLATE_ID}` body); `204`. Only `APPROVED`, `REJECTED` or `PAUSED` templates; an approved one 10 times in 30 days, so the edit is never retried for you |
| `POST /v1/wabas/{waba_id}/templates/{id}/unpause` | unpause a template paused by its quality or by pacing; `204` |
| `GET /v1/wabas/{waba_id}/templates/{id}/compare?template_ids=…&start=…&end=…` | compare with another of the WABA's templates (one id, Meta's `template_ids`; each sent 1,000 times in the window): `{data: [{metric, type, …}]}` as Meta answers |
| `POST /v1/wabas/{waba_id}/templates/archive`, `…/unarchive` | `{"hsm_ids": [...]}`, 1 to 100; Meta's answer, its partial failures included (`failed_templates`: id → Meta's reason). An archived template is deleted 28 days later unless unarchived |
| `GET /v1/wabas/{waba_id}/templates/library` | the Template Library (`?search=`, `topic=`, `usecase=`, `industry=`, `language=`, `name=`): Meta's first page, `{data: [...]}` |
| `POST /v1/wabas/{waba_id}/templates/library` | create from a library entry (`name`, `language`, `category`, `library_template_name`, `library_template_button_inputs` as an **array of objects**, not the page's quoted string); `201 {id, status, category}`; takes an `Idempotency-Key` |
| `POST /v1/wabas/{waba_id}/templates/migrations` | `{source_waba_id, page_number?, count?, template_ids?}`: recreate another of **your tenant's** WABAs' templates here (Meta's rule: the same business; `APPROVED` and `GREEN` or `UNKNOWN` quality only); `{migrated_templates, failed_templates}`; takes an `Idempotency-Key` |
| `POST /v1/wabas/{waba_id}/template-groups` | create a group (`name`, `description`, `whatsapp_business_templates`: ids as strings or Meta's integers); `201 {id}`; takes an `Idempotency-Key` |
| `GET`, `PATCH`, `DELETE /v1/wabas/{waba_id}/template-groups/{group_id}` | read it (with the first page of its templates), change it (`name`, `description`, `add_templates`, `remove_templates`), delete it (its templates stay) |

What the service checks before Meta is asked, each refusal sending
nothing:

- **Every template id is the WABA's.** One id (an edit, an unpause,
  both of a comparison) is looked up by its name, as a get is. A list
  (an archive, an unarchive, a group's templates, a migration's) is
  found in one walk of the WABA's templates, 100 a page and **at most
  10 pages**: a WABA with more than 1,000 templates cannot act on those
  past them here (design D116). An unarchive walks the archived ones.
  An id not found is `404 not_found`, nothing done.
- **A migration's source is one of your tenant's WABAs**, else `404`
  before its token is read; its `template_ids` are found with its own
  token, the migration posted with the destination's (design D117).
- **A group is the WABA's**: read first, acted on only when Meta names
  the path's WABA as its owner (design D118); a list of a WABA's groups,
  and the pages after the first of a group's templates, are not offered
  (no page of Meta's documents them).
- Bodies are Meta's JSON, a key the service would not pass on refused on
  its path, and a `zero_tap_terms_accepted: true` anywhere (an edit's
  buttons, a library button input) refused as on a creation.

Each change that succeeded is an `audit` event (`template_edited`,
`template_unpaused`, `templates_archived`, `templates_unarchived`,
`templates_migrated`, `template_group_updated`,
`template_group_deleted`). Archiving, unarchiving, migrating and editing
drop the WABA's cached pages on the replica that made them.

## Flows

WhatsApp Flows' management and their data endpoint (roadmap M5d; Meta's
`flows/guides/flowsapi`, `flows/guides/lifecycle` and
`flows/guides/implementingyourflowendpoint`). Sending a Flow is a message
(`interactive` of type `flow`, or a template's Flow button: [Send
messages](#send-messages)).

**Managing a WABA's Flows** takes a key with the scope `flows` (not
`templates`):

| Route | What |
| --- | --- |
| `POST /v1/wabas/{waba_id}/flows` | create a Flow: `{name, categories, clone_flow_id?, endpoint_uri?, flow_json?, publish?}` → `201 {id, validation_errors}`. `flow_json` is the Flow JSON, the object itself or Meta's text. Takes an `Idempotency-Key` |
| `GET /v1/wabas/{waba_id}/flows?cursor=` | the WABA's Flows (Meta takes no page size: `limit` is `422`) |
| `GET`, `PATCH`, `DELETE /v1/wabas/{waba_id}/flows/{flow_id}` | read; change `name`, `categories`, `endpoint_uri`; delete (drafts only) |
| `PUT /v1/wabas/{waba_id}/flows/{flow_id}/json` | replace the Flow JSON: the body is the Flow JSON itself (an object, at most 2 MiB), sent to Meta as it arrived, so the lines of its validation errors are your text's → `{success, validation_errors}` |
| `POST /v1/wabas/{waba_id}/flows/{flow_id}/preview` | `{invalidate?}` → `{preview_url, expires_at}`: the link opens the Flow without a login, for 30 days; treat it as a secret |
| `POST …/flows/{flow_id}/publish`, `…/deprecate` | publish (a published Flow can no longer be deleted, only deprecated), deprecate (terminal) |

- **A Flow id is the WABA's, or it does not exist.** A Flow is read first
  (`GET /{flow_id}`) and acted on only when Meta names the path's WABA as
  its owner (also for a `clone_flow_id`): another WABA's Flow, an id
  Meta refuses, or an id that is not a Flow is `404 not_found`, and that
  read is the only request made (design D135).
- **Meta validates the Flow JSON.** A creation or an upload with errors
  is stored all the same and answers `validation_errors` as Meta wrote
  them; publishing is refused (`422 invalid_parameter`, with Meta's
  `details`) while any is left or a publishing check fails
  (`flows/guides/healthmonitoring`: a number with a business public key,
  the endpoint set and healthy, a connected Meta app).
- **Not offered**, the library lacking them: connecting a Meta app to a
  Flow with an endpoint (`application_id` on an update; do it in the
  Flow Builder), a Flow's `health_status`, and its assets.
- Each change that succeeded is an `audit` event (`flow_updated`,
  `flow_json_uploaded`, `flow_published`, `flow_deprecated`,
  `flow_deleted`; never a Flow's JSON); the rate class is `templates`.
- **A Flow cannot point at another WABA's data endpoint.** A creation
  or a change whose `endpoint_uri` is this service's address for Flows
  (`…/webhooks/flows/<id>`) with another WABA's id is `422
  invalid_request` on `endpoint_uri`; any other `https` URL is yours.

**The data endpoint.** A Flow with an endpoint is called by Meta at its
`endpoint_uri` on every screen that exchanges data. Either host your own
endpoint and set its URL, or let the service decrypt and answer for
you: set `WA_SERVER_FLOWS_PRIVATE_KEY`, give the Flow
`https://<your public host>/webhooks/flows/{waba_id}` as its
`endpoint_uri`, and set the key's **public half** as the business public
key of the number the Flow is sent from (Meta signs it on upload; the
service has no route for that yet, use `POST /{phone_number_id}/
whatsapp_business_encryption` with the WABA's token):

```sh
openssl genrsa -out private.pem 2048          # no -des3: no password
openssl rsa -in private.pem -pubout -out public.pem
```

(A key made with Meta's `-des3` command: `openssl pkcs8 -topk8 -nocrypt
-in private.pem -out private_unencrypted_pkcs8.pem`.) The route is on the
**public listener**, with no API key: Meta's `X-Hub-Signature-256`,
checked with the app secret (`WA_APP_SECRET`, and the previous one while
rotating), is its authentication, before anything else is read. It
answers the health check (`ping`) and an error notification itself; a
data exchange is decided by the integrating app, a `FlowHandler` the
embedder plugs in (design D134). **The binary plugs in none, so every
data exchange answers `503` until an embedder supplies a handler**: embed
the service with a `FlowHandler` (a callback URL the service calls is
planned, roadmap M5d2), so use the
service's endpoint with the binary only for a Flow whose health check
matters, or host your own endpoint. Statuses: `432` bad signature, `421`
cannot decrypt (Meta's client then downloads the public key again), `404`
no key, an unbound WABA or a suspended tenant, `503` the app unavailable
or slower than 5 s, `500` it failed, `413` over 256 KiB. Nothing of a
request's data, token or sealed text is logged;
`wa_server_flow_endpoint_requests_total{outcome}` counts the answers.
The request carries no Flow id, so a handler tells Flows apart by the
`flow_token` it put in the message (check it: it is the session). A
replayed signed request is answered again (Meta states no validity window
for a request; design D136).

**Check the `flow_token` against the tenant.** The `422` above stops
a Flow made *through the service* from naming another WABA's data
endpoint, but a merchant who manages a Flow on Meta directly (Flow
Builder, or the Graph API with its own token) can still point it at
`…/webhooks/flows/<another WABA>`. The signature names the app, not the
WABA, and with one deployment key (D133) the request decrypts either
way. So a handler must treat the WABA in the path as a routing hint
only: accept a data exchange only when its `flow_token` is one the app
itself issued for that WABA's tenant, and answer an unknown token with
an error screen. A key per tenant (roadmap M5d2) closes this in the
service.


## Payments

India's WhatsApp Payments and Brazil's payment messages (roadmap P2;
Meta's `payments/payments-in/*` and `payments/payments-br/*` pages).

- **The messages** are sends, scope `send`: `interactive` of type
  `order_details` (an order to review and pay, `action.name`
  `review_and_pay`) or `order_status` (its new status, `review_order`),
  with `action.parameters` exactly as Meta's pages write them (a UPI
  intent link, a payment link, a payment gateway's configuration in
  India; a Pix code, a payment link, a Boleto or a one-click card in
  Brazil). The library reads them, nothing dropped (a key it does not
  know is `422` on its path), and checks every rule of the order's
  currency, `INR` India's and `BRL` Brazil's (the total equation, the
  offsets, the `reference_id`'s alphabet and length, …), before the
  request. An `order_status` names no currency on the wire, so it is
  checked by Brazil's rules, the wider: an India order's `pending`
  status or `payment` update is Meta's to refuse. The header of an
  `order_details` is an image (`id` or `https://` `link`); the image's
  `provider` some examples print is in no table and refused. The order
  templates and Brazil's payment request button go through `type`
  `template`, as before.
- **The routes**, scope `payments`, India's alone (Brazil documents no
  configuration and no lookup: its payments are your bank's or PSP's):

| Route | Does |
| --- | --- |
| `GET /v1/wabas/{waba_id}/payment-configurations` | the WABA's payment configurations |
| `POST /v1/wabas/{waba_id}/payment-configurations` | create one (Meta's body: `configuration_name`, `provider_name` among `upi_vpa`, `razorpay`, `payu`, `zaakpay`, `merchant_category_code`, `purpose_code`, `merchant_vpa`, `redirect_url`); `201` with the gateway's OAuth link |
| `GET`, `DELETE /v1/wabas/{waba_id}/payment-configurations/{configuration_name}` | one; remove it (send no order naming it first) |
| `POST /v1/wabas/{waba_id}/payment-configurations/{configuration_name}/oauth-link` | a new OAuth link (`{"redirect_url": …}`, optional) |
| `GET /v1/numbers/{pn}/payments/{configuration_name}/{reference_id}` | an order's payment, its attempts and refunds |
| `POST /v1/numbers/{pn}/payments/refunds` | refund an order (Meta's body: `reference_id`, `speed`, `payment_config_id`, `amount` with `currency` `INR`, `value` and `offset`, as strings or numbers) |

- **Times** are RFC 3339 strings under Meta's names
  (`created_timestamp`, `expiration`), as every time of the service.
- **The OAuth link is a credential**: it links whatever gateway account
  opens it. Give it to the merchant only; its answers carry
  `Cache-Control: no-store`. A creation and a new link take
  an `Idempotency-Key`, and the answer the service keeps for a repeat has
  no `oauth_url` (`null`): the records never store the link; ask for a
  new one.
- **A refund is never made twice**: it requires an `Idempotency-Key`
  (without one, `428 idempotency_key_required`, nothing asked of Meta:
  design D56), the library never replays it after a timeout, and a `504` says
  `may_have_been_sent: true`. Look the payment up (its `refunds`) or
  repeat with the same key; never refund again under a new key. The
  amount is not compared with what was paid. A refund, a creation, a
  removal and a new link are audit events naming the key and the
  configuration; a refund's also names the number, the order's
  `reference_id`, the amount and Meta's refund id, and one cut by a
  timeout is audited with the `outcome` `unknown` (design D58).
- **The configuration a lookup or a refund names is looked for in the
  number's own WABA first** (one request more): a name it has none of is
  `404 not_found`, and the payment is neither read nor refunded (as is
  an answer naming another configuration; when Meta fails on that
  check, the answer says `may_have_been_sent: false`, and a refund's key
  is free to repeat). A configuration's name is
  one path segment: one holding `/` or `\` is `422 invalid_request`
  before any request (design D51). Meta asks you to read the payment
  here rather than trust the webhook alone.
- **Webhooks**: `payment_configuration_updated` events (a configuration's
  status), India's payment status in `status_updated` events
  (`data.status.type` `payment`, with `data.status.payment`), Brazil's
  `payment_method` reply in `message_received` events.
- **Meta's terms**: until the WABA accepted the WhatsApp Payments terms
  of service, Meta refuses (`403 permission`, `131005`; a send
  `403 payment`, `134011`), and `error.graph.details` carries Meta's
  link, which the merchant opens. No route of the service accepts them.
- India's checkout button templates (a template whose `order_details`
  button carries `shipping_info`) and the alpha third-party validation
  (a `razorpay` or `payu` payment setting with its
  `encrypted_payment_gateway_data`) go through the same send (roadmap
  P1d). Not yet (roadmap P2b): linking a configuration's data endpoint.

## Groups

A number's WhatsApp groups (roadmap M5f; Meta's `groups/reference` and
`groups/webhooks` pages, and `analytics` for the group analytics). Scope
`groups`, on a number of the key's tenant, with the WABA's token. Sending
a message *to* a group is a send (`POST /v1/numbers/{pn}/messages` with a
`group_id` recipient, scope `send`); these routes manage the group.

| Route | Does |
| --- | --- |
| `POST /v1/numbers/{pn}/groups` | ask Meta to create a group (`subject`, `description?`, `join_approval_mode?`: `approval_required` or `auto_approve`); **`202` with Meta's `request_id`**; takes an `Idempotency-Key` |
| `GET /v1/numbers/{pn}/groups` | the number's active groups (`?limit=` 1 to 1,024, `after`, `before`): `{data: [{id, subject, created_at}], next_after, previous_before}` |
| `GET`, `PATCH`, `DELETE /v1/numbers/{pn}/groups/{group_id}` | read (`?fields=`: the six scalar fields by default), change the `subject` and/or `description` (`204`), delete the group (`204`; the participants and the business are removed: there is no call to leave a group) |
| `GET`, `POST /v1/numbers/{pn}/groups/{group_id}/invite-link` | the invite link; replace it (every previous link stops working) |
| `GET /v1/numbers/{pn}/groups/{group_id}/participants` | the participants (`wa_id`, `user_id`, `parent_user_id`, `username`) and their count |
| `POST /v1/numbers/{pn}/groups/{group_id}/participants/remove` | remove 1 to 8 participants, each by exactly one of `phone` (E.164 with its `+`), `wa_id` (digits, as the list shows it) or `user_id` (a BSUID); `{partial}` is `true` when Meta says some were not removed |
| `GET /v1/numbers/{pn}/groups/{group_id}/join-requests` | the open join requests (`after`, `before`) |
| `POST /v1/numbers/{pn}/groups/{group_id}/join-requests/approve`, `…/reject` | `{join_request_ids}`, 1 to 100: `{done, failed: [{join_request_id, reasons}], reasons}`; a partial success is a `200` |
| `GET /v1/numbers/{pn}/groups/{group_id}/analytics?start=&end=&metrics=` | the group's analytics, a day a point: `start` (RFC 3339) at most 90 days back, `metrics` among `SENT`, `DELIVERED`, `READ`, `PARTICIPANTS_JOINED`, `PARTICIPANTS_LEFT` |

**A group is the number's, or it does not exist** (design D150). Meta
creates a group asynchronously: the answer to the creation is a
`request_id`, and the group's id and invite link arrive in a
`group_lifecycle_update` event (`group_updated` in `/v1/events`, type
`group_create`, with that `request_id`). The service keeps its own record
of the groups each number holds, and that event is what writes it (a
`group_delete` event removes it, and a creation or a deletion that failed
changes nothing); so does every page of `GET …/groups`, which is how to
adopt a group created before the service, or whose event was missed.
Every route on a group id looks it up in those records **before the
tenant's token is opened**: another tenant's group, another number's, one
the service has not seen and an id no group has are all `404 not_found`
(with nothing asked of Meta), so a lookup of a group made by hand for
another tenant tells nothing. Until its event arrives (or the number is
listed), a group you just asked for is `404` too; wait for the event, or
list. A number that moves to another tenant keeps its groups' records
under the old tenant: list its groups to make them the new one's.

What the service does not do, each a route away: pin and unpin a message,
the group's picture (a multipart upload), add participants (Meta: they
join through the invite link), delete the invite link, leave (Meta
documents no call; `docs/parity.md` row 78).

**What reaches a log.** A participant's phone number, BSUID or username and
a join request's id are in the answer that returns them and nowhere else:
an error carries Meta's code and the service's kind, **never Meta's text**
(it may quote them), and the changes' `audit` events hold counts and
outcomes only (`group_creation_requested`, `group_settings_updated`,
`group_deleted`, `group_invite_link_reset`, `group_participants_removed`,
`group_join_requests_approved`, `group_join_requests_rejected`, with the
tenant, the WABA, the number's id, the key's id, `users`, `done`,
`failed` and `outcome`, `unknown` when the call may have gone through
unanswered). The invite link is a credential: it is in the answer to its
two routes and in no log. A group's id embeds the business number and is
in none of the service's own lines; the library's debug line for a Graph
request names the path it calls, so at `debug` it is in that one.

## Calling

A number's calling (roadmap M5e; Meta's `calling` pages and
`reference/whatsapp-business-phone-number/calling-api`). Scope `calls` (the
name is **pending the owner's confirmation before the first release**, and
`send` does not grant it), on a number of the key's tenant, with the WABA's
token. The service does **signalling only**: the SDP offers and answers are
yours, produced and consumed by your own WebRTC stack, and are passed to
Meta unparsed. A user's call reaches you as a `call_updated` event (the
`connect` event carries the user's SDP offer) in `/v1/events` and your
webhook endpoints, as before.

| Route | Does |
| --- | --- |
| `GET`, `PATCH /v1/numbers/{pn}/call-settings` | the `calling` object: `status`, `call_icon_visibility`, `call_icons`, `call_hours`, `callback_permission_status`, `sip`, `audio`, `voicemail`; the read adds Meta's `restrictions`. A change sets the fields it names (at least one), **`call_hours` as a whole** (a `holiday_schedule` left out clears it), `204`; users' apps may take up to 7 days to show it |
| `GET /v1/numbers/{pn}/call-permissions?phone=&user_id=` | whether the number may call a user and what it may do next: `{status, expiration_time, actions: [{action_name, can_perform_action, limits}]}`. The user is a phone number (E.164 with its `+`, encoded `%2B`), a BSUID or a parent BSUID; with both Meta uses the phone. **Name the user by `user_id` (the BSUID) first**: a phone number may be absent from a webhook (BSUIDs are mandatory), and no record of the service is keyed by either |
| `POST /v1/numbers/{pn}/calls` | place a call: `{to: {phone?, user_id?}, sdp_offer, biz_opaque_callback_data?, recording?, transcription?}` → **`201 {call_id}`**; takes an `Idempotency-Key`; needs the user's permission first (Meta's `138006` otherwise) |
| `POST /v1/numbers/{pn}/calls/{call_id}/pre-accept`, `…/accept` | answer a user's call with `{sdp_answer}` (`accept` also takes `biz_opaque_callback_data`, `recording`, `transcription`); `204`. Accept within 30 to 60 seconds of the `connect` event, and start sending media only after it |
| `POST /v1/numbers/{pn}/calls/{call_id}/reject`, `…/terminate` | reject a user's call; end a call in either direction (`204`; do it also after the media ended, for an accurate price) |
| `GET /v1/wabas/{waba_id}/analytics/calls` | call analytics, scope `analytics`: `start`, `end`, `granularity` (`HALF_HOUR`, `DAILY`, `MONTHLY`), `phone_numbers`, `country_codes`, `directions` (`USER_INITIATED`, `BUSINESS_INITIATED`), `dimensions` (`phone`, `direction`, `country`), `metric_types` (`COUNT`, `COST`, `AVERAGE_DURATION`) |

**A call is the number's, or it does not exist** (design D210). Meta
documents no way to read a call by its id, so the service keeps a record
of the calls each number holds, and a call action looks the call up there
**before the tenant's token is opened**: another tenant's call, another
number's, one the service has not seen and an id no call has are all `404
not_found`, with nothing asked of Meta. Two things write the record: a
`calls` event for the number (a user's call ringing, a status, a hang-up),
and the call you place yourself. It lives 24 hours since the call's last
event (so a call action a day after the call is `404`). A call signalled
over SIP is not these routes' to answer, and Meta delivers its webhooks only
with `sip.webhook_delivery` enabled: without an event the service holds no
record of it.

**Recording and transcription are your choice for each call.** The
service sends `recording` and `transcription` (`{status: ENABLED, purpose,
announcement_language}`; `DISABLED` opts out) only when the request carries
them, and never by default. When enabled, Meta plays an announcement of the
purpose to both participants first, and a participant who does not consent
ends the call: the consent is Meta's announcement and your policy, and the
service decides nothing about it. The result arrives as a
`call_recording_available` or `call_transcription_available` event with a
media id (downloaded within 7 days through the media route; whether Meta
serves a recording to `?phone_number_id={pn}` has not been run against
Meta).

**Secrets.** An SDP offer or answer holds addresses and ICE credentials; it
is in the request that carries it and in no log, error or audit event. A
SIP server's password is **never read from Meta and never answered**
(`include_sip_credentials` is never sent); read it in WhatsApp Manager. A
failed call answers Meta's code and the service's kind (`409` and
`call_permission_missing` for `138006`, `429` and `call_limit_reached`,
`502` and `call_connection_failed`, ... with `error.graph.code`; roadmap
L28), never Meta's text. The changes' `audit` events (`call_settings_updated`,
`call_connect_requested`, `call_pre_accepted`, `call_accepted`,
`call_rejected`, `call_terminated`) hold the tenant, the WABA, the number's
id, the key's id, the outcome (`unknown` when the call may have gone
through unanswered), the settings' **section names** and whether recording
or transcription was enabled; never who was called, which call, an SDP or
a SIP host.

Not offered: the SIP password, uploading a voicemail announcement (Meta
wants `use_case=call_voicemail_announcement`, which the media routes do
not send: upload it elsewhere, then name its media id, which the service
looks up as the number's own before it changes the settings), sending the
call permission request (a message: a send), call buttons and deep links.
Actions are never repeated after a timeout: read the call's events.

## Commerce, QR codes, analytics and the block list

Four scopes, one per family (roadmap M5g, design D140; the names are
**pending the owner's confirmation before the first release**):
`commerce`, `qr_codes`, `analytics` and `contacts`. A key minted before
M5g carries none of them, and `send` grants none: blocking acts on a
person and a QR code is public, so mint a key for these routes on
purpose. Each route is one of the library's calls, made with the WABA's
token.

| Scope | Route | Does |
| --- | --- | --- |
| `commerce` | `GET`, `PATCH /v1/numbers/{pn}/commerce-settings` | the cart and the catalog's visibility; the change takes `is_cart_enabled` and/or `is_catalog_visible` (at least one), `204` |
| `commerce` | `GET /v1/wabas/{waba_id}/catalogs`, `…/catalogs/{catalog_id}`, `…/catalogs/{catalog_id}/products` | the catalog the WABA connects, and its products, a page of 1 to 100 (`limit`, `cursor`, `return_only_approved_products`) |
| `qr_codes` | `POST`, `GET /v1/numbers/{pn}/qr-codes` | create (`prefilled_message`, 1 to 140 characters; `generate_qr_image`, `PNG` or `SVG`; takes an `Idempotency-Key`), `201`; list, a page of 1 to 25 |
| `qr_codes` | `GET`, `PATCH`, `DELETE /v1/numbers/{pn}/qr-codes/{code}` | read, change the message, delete (`204`, audited as `qr_code_deleted`) |
| `analytics` | `GET /v1/wabas/{waba_id}/analytics/messaging`, `…/conversations`, `…/pricing`, `…/templates` | the four analytics: `start` and `end` (RFC 3339; template analytics also take `YYYY-MM-DD` days), `granularity`, the filters as comma-separated lists |
| `contacts` | `GET`, `POST /v1/numbers/{pn}/blocked-users`, `POST …/blocked-users/unblock` | the block list (a page of 1 to 100); block or unblock 1 to 1,000 people, `{"users": [{"phone": "+1650…", "user_id": "US.…"}]}` (a phone with its `+`, a BSUID, or both); both take an `Idempotency-Key` |

What the service checks before Meta is asked, each refusal sending nothing
(or, for the two lookups, only the lookup):

- **A catalog is the WABA's** (D141): it must be among the WABA's own
  `product_catalogs` (one read), else `404 not_found`, and its products
  are never asked for. Only the first page of that list is read.
- **A QR code is the number's** (D144): the code is looked for in the
  number's own list first (`GET /{pn}/message_qrdls?code=…`) and read,
  changed or deleted only when that list holds it; another number's code,
  or none, is `404 not_found`. **This costs one more request to Meta**
  per read, change or deletion, and nothing is changed by a refusal.
  Another tenant's number is refused with no request at all.
- **Template analytics name the WABA's own templates** (D145, with the
  walk of D116): 1 to 10 `template_ids`, each among the WABA's templates,
  else `404 not_found` and the analytics are not asked for.
- **Ranges and filters are bounded** (D145): `start` at most a year before
  now (90 days for templates), `end` after `start`, a range of at most a
  year (90 days for templates), a `HALF_HOUR` range of at most 31 days (the
  service's bound: the answer holds a data point per half hour and
  dimension); a list filter holds at most 50 values, each one of Meta's
  documented values, a `phone_numbers` entry only digits (they are written
  into a field expansion) and a `country_codes` entry two letters.
- **Pages are bounded** (D142): `limit` 1 to 100 on products and the block
  list, 1 to 25 on QR codes. A `cursor` is the opaque `next_cursor` of the
  previous page; it only moves within the list the path names.
- A block names people by phone number **with its `+`** or by BSUID (a
  parent BSUID and a group are refused, `422`). Meta only blocks a person
  who wrote to the number in the last 24 hours.

A block or an unblock answers `200` even when Meta refused some people:
`{done, refused, error_code}`, each refusal carrying Meta's error code
and the service's kind for it (`reasons: [{code, kind}]`), **never Meta's
text**, which may quote the person. Neither a failed block nor a failed QR
creation or change carries or logs Meta's texts, for the same reason. A
block, an unblock and a deletion of a QR code are audit events
(`users_blocked`, `users_unblocked`, `qr_code_deleted`): the blocks by how
many people they named, blocked and Meta refused, **never by who**; a
timeout, or Meta's `139100` with an error status (some were not changed,
not which), is an `unknown` outcome.

Not routed (roadmap M5g2): a catalog's product sets, one product by its
id, the products edge's `filter`, template group analytics, and the two
analytics switches (template insights, button click tracking). Call
analytics wait for the calling routes (M5e); a group's analytics are the
groups' (above).

## Partner APIs

What a Tech Provider or Solution Partner does beyond onboarding (roadmap
M5j, design D250 to D257; Meta's `solution-providers/partner-led-business-verification`,
`solution-providers/multi-partner-solutions`,
`solution-providers/support/adding-waba-to-mps`,
`reference/user/assigned-whatsapp-business-accounts-api` and
`pricing/change-billing-currency`). Two kinds of caller:

- **The operator** (an admin key) acts with the deployment's own partner
  business and system token, the pair the pool of pre-verified numbers
  uses (`WA_PARTNER_BUSINESS_ID` and `WA_SYSTEM_TOKEN`, or a Solution
  Partner's); without them every route answers `409 signup_not_configured`.
- **A tenant** (the scope `partners`, **pending the owner's confirmation
  before the first release**, which no other scope grants) acts on a WABA
  of its own, with that WABA's token.

**Legal and irreversible acts.** Seven calls state something about a
business, cannot be taken back or move a business's assets. Each is taken
only when the deployment sets `WA_SERVER_LEGAL_ACTS=on` (off by default):
off, it is `403 feature_not_available` and nothing is asked of Meta; on, it
is audited, and nothing in the service takes one by itself or as part of
another route. The gate is answered before the body is read, so with the
setting off a malformed body is still `403`, never `422`. The evidence, page
by page, is D250 (D250 to D257: the coordinator's decisions of 2026-11-06
under the owner's delegation, each swappable).

| Act | Why | Who |
| --- | --- | --- |
| a business verification submission | a statement of a customer business's legal identity, with its documents; three submissions per customer | operator |
| accepting a solution | Meta: "solutions cannot be declined once they have been accepted" | operator |
| rejecting a solution | the reject page: "permanent and cannot be undone" | operator |
| accepting a deactivation request | the solution becomes `DEACTIVATED` and onboards no client | operator |
| a solution migration intent | asks a client to move its WABA to another partner; Meta accepts for the client after 90 days | tenant |
| a billing migration's start | clones the WABA under a new currency or payment method | tenant |
| a billing migration's resumption | switches the numbers' billing; the old WABA can no longer send | tenant |

Not legal acts, audited all the same: creating a solution (it binds nobody
until the partner accepts), asking for a deactivation, rejecting a
deactivation request.

### The operator's routes

| Route | Does |
| --- | --- |
| `POST /v1/admin/partners/business-verifications` | submit a customer business (`client_business_id`) with one to three `documents` (`file_name`, `content_base64`): PDF, JPEG or PNG, 5 MiB each, the content of the type the name says, all checked before any request (bodies up to 21 MiB). `201` with `verification_attempts` and `attempts_left`; never repeated after a timeout; the outcome arrives as an `account_update` event |
| `GET /v1/admin/partners/business-verifications?client_business_id=&cursor=` | the submissions made, with Meta's `rejection_reasons` |
| `POST`, `GET /v1/admin/partners/solutions` | create (`name`, `partner_app_id`, `owner_permissions`, `partner_permissions`) `201`; list (`role`, `limit`, `cursor`) |
| `GET /v1/admin/partners/solutions/{id}`, `…/partners` | a solution with its owner, permissions and partners (read it before accepting); its partners |
| `GET /v1/admin/partners/client-businesses` | the businesses onboarded through the deployment's app |
| `POST …/solutions/{id}/accept`, `…/reject` | legal acts, `204` |
| `POST …/solutions/{id}/deactivation-request`, `…/deactivation-request/accept`, `…/deactivation-request/reject` | ask, accept (a legal act), reject; `204` |
| `GET /v1/admin/partners/users/{user_id}/wabas` | the WABAs assigned to a system user (`id`, `name`) |

**The solution token is never served**: it is a bearer credential for a
client's assets, so no route, answer, log or audit event carries it
(hosted Embedded Signup, above, is where the service uses a client's
business token through a solution, and it stores it in the vault). The
operator's changes are admin routes: no `Idempotency-Key`, and a retry
after a `504` should read first (the library never replays them: a
submission counts towards the customer's three). An `Idempotency-Key` on
these is roadmap M5j2.

### A tenant's WABA

| Route | Does |
| --- | --- |
| `GET /v1/wabas/{waba_id}/business-verification` | the WABA's `business_verification_status` as Meta says it, and `verified` |
| `POST /v1/wabas/{waba_id}/solution-migration-intents` | `{"solution_id"}`: `201` with the intent's `id`; an `Idempotency-Key`; a legal act |
| `GET …/solution-migration-intents/{intent_id}` | the intent, read with the WABA's token and answered only if it names this WABA (as its `destination_waba` or its `waba`); another's, or one that names none, is `404` |
| `POST /v1/wabas/{waba_id}/billing-migrations` | `{"currency", "extended_credit_id"?}`: `201`; an `Idempotency-Key`; a legal act. The new WABA needs a payment method before it is resumed (a credit card cannot be attached by the API) |
| `GET …/billing-migrations/{migration_id}` | the migration's `status` (`READY_TO_COMPLETE` is the cue to resume) and the new WABA |
| `POST …/billing-migrations/{migration_id}/resume` | complete it; a legal act; not repeated after a timeout |

**A migration is its WABA's, or it does not exist** (D256): the service
remembers, under the namespace `wa.server.billing_migration`, which tenant
and WABA started each migration, and reads that record before it asks Meta
anything, so another WABA's, another tenant's and an unknown id are `404`
with Meta not asked. A migration started outside the service (in Meta
Business Suite) is not readable through it.

Not offered: moving a phone number between partners (Meta's page: not
supported on the new account model), a WABA's own solutions, and the
solution token.

## Conversation routing

One scope, `routing` (roadmap M5l, design D220; the name is **pending the
owner's confirmation before the first release**). A key minted before M5l
does not carry it, and neither `send` nor `inbox` grants it: a pass, a take
and a release hand a customer's conversation to another responder, so mint
a key for these routes on purpose. Each route is one of the library's
thread control calls, made with the WABA's token (design D220–D225, decided by the
coordinator on 2026-11-06 under the owner's delegation, each swappable).

| Route | Does |
| --- | --- |
| `GET /v1/numbers/{pn}/routing-roles` | the roles the number's business holds, `{"roles": ["customer_service", "escalation"], "is_standby": false}`; Meta's order, which means nothing; empty is a success |
| `POST /v1/numbers/{pn}/thread-control/pass` | pass the thread with a user: `{"user": {…}, "target_role": "ai_agent", "metadata": "…"}` (`target_role` and `metadata` optional) |
| `POST /v1/numbers/{pn}/thread-control/take` | take it, the escalation partner only: `{"user": {…}, "metadata": "…"}` |
| `POST /v1/numbers/{pn}/thread-control/release` | release it to idle: `{"user": {…}}` |

The three actions answer `200 {"request_id": "…"}` (Meta's, for its
support) and take an `Idempotency-Key`. Nothing is sent back to the caller
about the new owner: the new owner hears of a pass on its `control_passed`
event and the previous owner of a take on `control_taken` (both are tenant
events, `thread_control_changed`), and a release has no event. **No call
reports who owns a thread**, here or at Meta.

What the service checks before Meta is asked, each refusal sending
nothing:

- **The user is named one way** (D222): `user` holds exactly one of
  `phone` (E.164 with its `+`), `wa_id` (digits) and `user_id` (a
  business-scoped user id, Meta's preferred identifier; a parent BSUID is
  refused). Prefer `user_id`, first, and never key a person by phone number alone;
  nothing is stored keyed by the user. A group is refused.
- **A pass names a role, never an app.** `target_role` is one of `ai_agent`,
  `ctwa`, `customer_service`, `escalation`, `marketing`, `utility` (spelled
  so; anything else is `422 target_role`); without it the pass goes to the
  escalation partner, which therefore cannot make one. The business that
  owns the number configures who holds a role, so a pass cannot reach
  another tenant's app. `ai_agent` needs Meta Business Agent on the
  account, and Meta judges that.
- `metadata` is at most 2,000 characters. It is free text for the new (or
  previous) owner; the service logs and audits none of it.
- Another tenant's number is `404`, before the token is read.

Meta decides the rest, and answers with its code: a pass needs the caller
to own an active thread (not an idle one, not another responder's); a take
by a caller that is not the escalation partner is Meta's `2494191`
(`502 unknown`, the code in `error.graph.code`); a release of an idle
thread succeeds; `100` says the account is not enrolled for thread control
or routing, or the user is not one the number has talked to. **Meta's
texts are not in the answer or the logs of an action** (they may quote
the user); the roles read keeps them. The business must have accepted
Meta's Business Agents and Platform Terms of Service in Meta Business
Suite for thread control to work; **no call of the service does it**, and
nothing here is a legal act.

None of the three is retried: a timeout is `504 timeout` with
`may_have_been_sent`, and the thread may have changed hands. With an
`Idempotency-Key`, repeating the request answers the first answer and
sends nothing (D223). Each action is an audit event (`thread_passed`,
`thread_taken`, `thread_released`: the key, the tenant, the WABA, the
number and, for a pass, the role), with an outcome `answered` or
`unknown`, **never the user or the `metadata`**.

Not done (D224, roadmap M5l2): the service does not record your own
pass, take or release in the inbox's thread owner state, so after a pass
the inbox may still believe the thread is this app's until the next
event or message; a free-form reply it lets through is then refused by
Meta (`409 thread_owned_elsewhere`). Standby copies and handovers reach
you as events either way.

## The inbox

What the service received for a tenant's number, read and answered over
HTTP (scope `inbox`; roadmaps M2a and M2g). The inbox is the one the webhook
pipeline records into: a customer's message is there once Meta's
delivery is answered.

| Route | Does |
| --- | --- |
| `GET /v1/numbers/{pn}/conversations` | the number's conversations, most recent activity first: `contact` (the customer's BSUID, else their `wa_id`, else a group's id), `last_message_at`, `last_inbound_at`, `last_text`, `unread`, `window` (as the window route answers it) |
| `GET /v1/numbers/{pn}/conversations/{contact}/messages` | one conversation's history, newest first: `id`, `direction`, `kind`, `text`, `payload` (the message as Meta sent it, or as it was sent), `status`, `timestamp`, `status_at`, `error` |
| `GET /v1/numbers/{pn}/conversations/{contact}/window` | the conversation's 24-hour window: `open` (whether a free-form reply would go to Meta now) and `closes_at` (when the window the customer opened closes or closed; `null`: templates only) |
| `POST /v1/numbers/{pn}/conversations/{contact}/messages` | a reply to the conversation's contact: a message's content as `POST /v1/numbers/{pn}/messages` takes it, without `to`; `202` with `message_id`, recorded `accepted` until its status events move it on |
| `POST /v1/numbers/{pn}/conversations/{contact}/read` | the merchant read the conversation: `unread` back to 0; with `{"notify_customer": true}`, the latest message the customer sent is marked read on Meta first (the blue ticks); `204` |
| `GET /v1/numbers/{pn}/messages/{message_id}` | one message of the number's inbox, by id: `{contact, message}`, `message` as the history answers it; `404` for an id the number has none of |

```bash
curl -sS "$WA_SERVER/v1/numbers/106540352242922/conversations?limit=20" -H "Authorization: Bearer $KEY"
curl -sS -X POST "$WA_SERVER/v1/numbers/106540352242922/conversations/US.13491208655302741918/messages" \
  -H "Authorization: Bearer $KEY" -H "Idempotency-Key: ticket-881:reply-1" \
  -H "Content-Type: application/json" -d '{"type": "text", "text": {"body": "It ships tomorrow."}}'
curl -sS -X POST "$WA_SERVER/v1/numbers/106540352242922/conversations/US.13491208655302741918/read" \
  -H "Authorization: Bearer $KEY" -H "Idempotency-Key: ticket-881:read-1" \
  -H "Content-Type: application/json" -d '{"notify_customer": true}'
```

- Lists take `limit` (1 to 100, 50 by default) and `cursor`, and answer
  `next_cursor` (`null` on the last page), like every list but
  `GET /v1/events`. Percent-encode the contact in a path.
- **A reply is checked first**, through the library's `Inbox::reply`,
  and refused with nothing sent (`409`, `may_have_been_sent: false`):
  free-form content outside the 24-hour window (which counts the
  customer's calls and their messages seen in standby) is
  `customer_service_window_closed`; a service message while another app
  owns the thread under Conversation Routing (a `control_taken` the inbox
  recorded, or a standby copy of the customer's message) is
  `thread_owned_elsewhere`. You poll the events these come from too
  (`thread_control_changed`, `standby_observed`: roadmap M2d), so a UI
  can tell its user why. A template is exempt from both,
  and so is a Direct Send `utility` or `authentication` reply (Meta
  sends it as a template); a `service` one is free-form. Every type a
  send takes, a reply takes ([Send messages](#send-messages)), a `pin`
  in a group's conversation only (another is `422` on `contact`); a
  Flow or a catalog named by id is checked after the window, so a reply
  the window refuses asks Meta nothing.
  `WA_SERVER_INBOX_REPLY_CHECKS` turns a check off per number; Meta then
  decides (`131047`, `131070`). A reply takes an `Idempotency-Key`, as a
  send does, and its rate class is the writes'.
- **The window route runs those checks with nothing sent**: `open` is
  `true` exactly when a free-form reply would go to Meta now, so a UI
  choosing between the composer and a template picker never disagrees
  with the reply's `409`. `closes_at` is the window as recorded; it can be
  past while `open` is true (a handover to your app, or the window check
  turned off for the number) and ahead while it is false (another app
  owns the thread). The list carries the same `window` for each
  conversation, at a few reads of the inbox each.
- **The read route** resets `unread` and, with `notify_customer`, sends
  Meta one read receipt with the number's token, for the latest message
  the customer sent (Meta marks the earlier ones of a one-to-one
  conversation with it; in a group it marks only that one). No receipt
  goes for a message older than Meta's 30 days; the count is reset all the
  same. A receipt Meta refuses answers its error and resets nothing, and
  so does one Meta answers after the number moved (`503`). It
  takes an `Idempotency-Key`: a repeat with the same key and body gets
  the kept answer and sends no second receipt, unless the first was
  answered `503 storage_unavailable` (the receipt went, the reset
  failed), whose key is released, so its repeat sends the receipt again.
  A `504 timeout` is kept: the count is unchanged, and a new key resets
  it (and sends the receipt again). Its rate class is the writes'; the
  window and a message by id are reads.
- **A number that moved to the tenant from another one shows nothing
  from before its binding** (design D10: hidden, never purged): no
  conversation whose latest message is older, no message dated before
  the binding began, and a conversation's `last_inbound_at` and `unread`
  only when the customer wrote since. A number no other tenant held
  shows all of it, a coexistence history synced from before the binding
  included. The cut follows both the WABA's move record (roadmap S2b)
  and the number's own (roadmap M2h): a number Meta migrated from
  another tenant's WABA to one of yours, neither WABA moving, also shows
  nothing from before it came under you. A number bound before this
  release (Postgres migration 5) has no move record and is treated as
  moved, its cut at its WABA's binding. `unread` counts only the
  customer's messages since the cut. A free-form reply on such a number
  needs a window the customer opened since the cut (a message, a call):
  one the previous holder's customer opened is
  `409 customer_service_window_closed`, nothing sent; send a template to
  start the conversation. On such a number the window route shows no
  window the previous holder's customer opened (`open: false`,
  `closes_at: null`), a message by id dated before the cut is `404` like a
  missing one, and the read route sends no receipt for one.
- Reads answer `409 reconnect_required` or `number_not_connected` for a
  number whose token does not work, as every number route does.

## Erasing a customer

One route erases a customer on one of the tenant's numbers, from the
service's inbox and its event outbox (scope `erasure`; roadmap M2f). It
is irreversible, and it runs only when you call it: which erasure
requests to honour, and when, is your decision (design D10, "Legal
acts"); the service never erases on its own.

```bash
curl -sS -X POST "$WA_SERVER/v1/numbers/106540352242922/erasures" \
  -H "Authorization: Bearer $ERASURE_KEY" -H "Content-Type: application/json" \
  -d '{"confirm": "106540352242922", "contacts": ["US.13491208655302741918", "16505551234"]}'
# {"erasure_id": "era_…", "phone_number_id": "106540352242922", "erased_at": "2026-11-02T09:30:00Z"}
```

- **`contacts`** are the person's identities on the number, as the inbox
  names a conversation's contact: their BSUIDs and `wa_id`s (digits, no
  `+`), 1 to 16, the ones you hold yourself included. The service adds
  every identity its inbox connects to them on the number (the identity
  links the webhooks recorded: a phone number to its BSUID, a BSUID
  change), then erases them all. `confirm` is the path's number again.
  The identities travel in the body only, never in a path (an ingress
  logs paths).
- **What goes**: their conversations, messages, window events, thread
  ownership, synced contacts and identity links on the number; their
  messages in groups redacted (the library's `erase_all`, design D31);
  and every event of the tenant on the number whose data names one of
  them (`wa_server_events`, by its `contacts` column: whole events, so a
  history chunk or a group update that also names others goes too; `GET
  /v1/events` skips the gap, nothing is renumbered). Never an
  operator-only event of the number: those are the operator's (design
  D89).
- **The answer says nothing of what was found**: the same `200` whether
  the person was known or not, so a repeat is harmless (it erases
  nothing more). No `Idempotency-Key`: there is nothing to replay.
- **The journal**: before anything is deleted, the service writes one
  entry (target `erasure_journal` in its logs, `info`): `erasure_id`,
  `at`, `tenant`, `phone_number_id` and `identities`, the hex
  HMAC-SHA256 of `<phone_number_id>|<contact>` under
  `WA_SERVER_ERASURE_KEY` for each identity erased, comma-separated. No
  identity in clear. Ship that target to a store outside the database
  you back up: after a restore, compute the same HMAC over every
  identity the restored tables hold and erase again the ones an entry
  lists ([production.md § 8](production.md#8-retention-and-erasure-on-postgres)).
  A journal that cannot be written stops the erasure (`503`, nothing
  deleted).
- **The audit event** (`customer_erased`, target `audit`) names the key,
  the tenant, the WABA, the number, the erasure's id and what each store
  deleted, by count; never the person.
- **Refusals**: another tenant's number is `404` (it does not exist for
  you), a key without the scope `403 forbidden`, a wrong `confirm` or bad
  `contacts` `422`, no `WA_SERVER_ERASURE_KEY` `409
  erasure_not_configured`, a number that came to you from another tenant
  `409 erasure_crosses_binding` (below), a number whose token lapsed
  `409 reconnect_required` (the whole authorization order runs, as on
  the inbox's routes), a number that moved while it was erased `503`
  (repeat it). Nothing is erased on any refusal. Its rate class is the
  writes'.
- **What it does not reach**: the person on your other numbers (erase on
  each), the number's operator-only events and another tenant's (a
  previous holder of the number keeps its own: both go with the outbox's
  retention, `WA_SERVER_OUTBOX_RETENTION`), events recorded before
  migration 8, a body the library could not parse, the person quoted in
  someone else's message, the 24-hour idempotency answers of sends to
  them (`WA_SERVER_IDEMPOTENCY_TTL`), the webhook dedup markers (keyed
  hashes: keep them), what live streams already delivered, Meta's
  contact book (the library's `delete_contact_book_entry`). What arrives
  after the erasure is recorded again: erase again once Meta's 7-day
  redelivery window has passed.
- **A number that came to you from another tenant** (its WABA moved, or
  Meta migrated the number from another tenant's WABA) is refused whole:
  `409 erasure_crosses_binding`, nothing erased, no journal entry. The
  inbox is kept by number, and your reads stop at your binding's start
  (design D10), but the library's erasure takes no time bound, so it
  would also delete the previous holder's hidden records of the person
  (design D90). Ask the operator: they can erase on the number through
  the library. Whether the service may reach those records is the
  owner's open choice (roadmap § Owner touchpoints).

## OTP

One-time passcodes over WhatsApp, for a login or a password reset
(scope `otp`; roadmap M3c). The service runs the library's OTP service:
codes are random, kept only as keyed hashes (under `WA_OTP_PEPPER`),
compared in constant time, expire, and allow a few attempts; a recipient
gets a new code at most every 30 seconds and 5 an hour for one purpose.
You never see the code: Meta delivers it, your user types it, you check
it.

| Route | Does |
| --- | --- |
| `POST /v1/otp/issue` | `{phone, purpose?}`: send a code to `phone` (E.164 with its `+`) with your authentication template; `200` with `{"status": "sent", "challenge_id", "expires_at"}`, or `{"status": "cooling_down" \| "rate_limited", "retry_after_seconds"}` and nothing sent; takes an `Idempotency-Key` |
| `POST /v1/otp/verify` | `{phone, purpose?, code}`: check the code your user typed; `200` with `{"result": "verified"}`, `{"result": "invalid", "attempts_left"}`, `expired`, `too_many_attempts` or `not_found`. Every check of an outstanding code counts as an attempt |

```bash
curl -sS -X POST "$WA_SERVER/v1/otp/issue" -H "Authorization: Bearer $KEY" \
  -H "Idempotency-Key: login:user-881:1" -H "Content-Type: application/json" \
  -d '{"phone": "+16505551234", "purpose": "login"}'
curl -sS -X POST "$WA_SERVER/v1/otp/verify" -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"phone": "+16505551234", "purpose": "login", "code": "123456"}'
```

- **Outcomes are `200`**, never errors: branch on `status` and `result`.
  Errors are for what you must fix: `422` on `phone` (no `+`, separators,
  a BSUID: Meta cannot send an authentication template to a BSUID), on
  `purpose` (1 to 64 of `[A-Za-z0-9._:-]`, `login` when absent) or on
  `code` (1 to 15 characters); `409 otp_not_configured` (below); `429
  too_many_requests` past your tenant's budget (5 issues and 20 checks a
  second by default); Meta's errors on a send, with
  `may_have_been_sent`: when it is `true` (a timeout, a Meta failure)
  the code may be on its way and still verifies, so do not issue
  another at once, let the user type what arrives.
- **Codes are your tenant's.** A code verifies only for the tenant that
  issued it, the phone number and the purpose it was sent for: another
  tenant checking it, on the same sending number too, gets `not_found`
  and costs it no attempt. The purpose is a constant of your code
  (`login`, `reset_password`), never a value your end user chooses.
- **Which number sends it** (decision D8): the operator sets each
  tenant's OTP settings, the sending number (one of the tenant's own),
  the approved authentication template and its language, and the
  limits. A tenant without settings sends from the default OTP tenant's
  number (`WA_SERVER_OTP_DEFAULT_TENANT`, the platform's), with that
  tenant's template; the user then sees the platform's name. No
  settings and no default, a default tenant that is suspended, deleted
  or without settings, or settings naming a number the tenant no longer
  holds: `409 otp_not_configured`, nothing sent (`doctor` warns about
  the default tenant, below). Changing a tenant's sending number kills
  its codes in flight.
- **The platform's number has one limit per recipient.** On the default
  OTP tenant's number, every tenant and purpose sending from it share
  that tenant's issue limit per recipient (5 codes an hour by default),
  on top of each tenant's own: past it, `rate_limited`. So tenants
  sharing the platform's number cannot send one person five codes an
  hour each, on the platform's bill; the cost is that one tenant's codes
  to a recipient count against the others'. A tenant with its own
  number and settings has only its own limits. An answer that sent
  nothing (`cooling_down`, your own `rate_limited`, a refusal before the
  send) does not count; a timeout does.
- **The code is in no answer, log or error.** A failed send keeps Meta's
  code and trace id, never its text or `details` (a template error could
  quote its parameter, the code). The idempotency record of an issue
  keeps the answer (no code) and a fingerprint keyed with the pepper, so
  a database dump does not name the recipient. Throttling by IP stays
  yours: the service sees only your backend.

The operator's routes (admin key):

```bash
curl -sS -X PUT "$WA_SERVER/v1/admin/tenants/merchant-42/otp" \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"phone_number_id": "106540352242922",
       "template": {"name": "login_code", "language": "en_US"},
       "ttl_seconds": 600, "issue_limit": {"max_issues": 5, "window_seconds": 3600}}'
```

`PUT /v1/admin/tenants/{id}/otp` replaces the settings whole and answers
them with every limit filled in (6 digits, 600 seconds, 5 attempts, a
30-second cooldown, 5 codes an hour per recipient and purpose by
default; the issue limit cannot be turned off); `422` on
`phone_number_id` when the number is not the tenant's. `GET` reads them
(`404` when the tenant has none of its own), `DELETE` clears them. Keep
`ttl_seconds` equal to the template's `code_expiration_minutes`, so the
message never promises a lifetime the code does not have. Each change is
an `audit` event.

## Embedded Signup

A merchant connects their own WhatsApp number from your page, and the
service ends up holding their business token, encrypted in its vault
(roadmap M3a; scope `signup`). The library's onboarding does every step
with Meta (the code exchange, the token's check, the WABA, its owner and
its numbers verified, the token stored, the app subscribed, a Solution
Partner's credit line, the registration); the service binds the
verified WABA and all its numbers to the calling tenant, and keeps an
attempt that stopped after its token was stored, for your tenant to
resume. Set `WA_APP_ID` and `WA_ES_CONFIG_ID` (Facebook Login for
Business → Configurations), and serve the page over HTTPS from a domain
your app allows.

```text
merchant's browser       your CMS backend                 meta-whatsapp-server               Meta
"Connect" ────────────► POST /connect ─────────────────► POST /v1/signup/sessions (WA-Tenant: m42)
          ◄─ launch data ◄────────────────────────────── {state, app_id, config_id, launch_options}
FB.login(…) ─────────────────────────────────────────────────────────────────────────────► popup
          ◄─ code (30 s, single use) + WA_EMBEDDED_SIGNUP event ◄────────────────────────────┘
POST /connect/callback ► POST /v1/signup/complete ─────► redeemed for m42, onboarded ───────► exchange, verify, store,
  {state, code, event, pin}                              WABA + numbers bound to m42           subscribe, [credit line], register
```

| Route | Does |
| --- | --- |
| `POST /v1/signup/sessions` | `{coexistence?, pre_verified_phone_ids?}`: an attempt bound to your tenant for 15 minutes, and what the page needs: `{state, expires_at, app_id, config_id, graph_api_version, launch_options}` (`launch_options` is `FB.login`'s options, as they are). Asks Meta nothing |
| `POST /v1/signup/complete` | `{state, code, event, pin?}`, as the page collected them: the attempt redeemed for your tenant, then the onboarding. `200` with `{"status": "connected", "waba_id", "phone_number_ids", "steps_completed", "needs_coexistence_sync", "credit_allocation_id"?}`, or `{"status": "cancelled", "current_step"}` for a `CANCEL` event (nothing redeemed: the attempt may still complete) |
| `GET /v1/signup/attempts` | your attempts that stopped after their token was stored: `{data: [{waba_id, failed_step, updated_at, created_at}], next_cursor}` |
| `POST /v1/signup/resume` | `{waba_id, pin?}`: an attempt's steps after the stored token, again (a corrected PIN): answers as a completion, `steps_completed` starting with `load_token` |

```bash
curl -sS -X POST "$WA_SERVER/v1/signup/sessions" -H "Authorization: Bearer $KEY" \
  -H "WA-Tenant: merchant-42" -H "Content-Type: application/json" -d '{}'
curl -sS -X POST "$WA_SERVER/v1/signup/complete" -H "Authorization: Bearer $KEY" \
  -H "WA-Tenant: merchant-42" -H "Content-Type: application/json" \
  -d '{"state": "…", "code": "…", "pin": "581063",
       "event": {"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH",
                 "data": {"phone_number_id": "106540352242922", "waba_id": "524126980791429"}}}'
```

- **The tenant is the credential's**, never the body's. An attempt
  another tenant started is `403 stale_attempt`, as an expired or used
  one is, and is not burnt: its own tenant still completes it. The
  state travels in bodies only (URLs reach access logs).
- **Everything is checked first**: the state, the code, the PIN (6
  digits) and the event, each `422 invalid_request` on its field, before
  the attempt is redeemed, so a malformed post keeps the attempt. A body
  over 16 KiB is `413`. A body of the wrong shape is `422` on `body`,
  never quoting what it carried.
- **The PIN is the merchant's own** (decision D6): it becomes a new
  number's two-step verification PIN, or must match the one it has.
  Typed per attempt, never stored or logged; without it the number is
  left unregistered. A coexistence number is registered already: its PIN
  is ignored.
- **One WABA, one tenant** (decision D4). A WABA another tenant holds is
  `409 waba_owned_by_another_tenant`: the one the page claims, before
  the attempt is redeemed, and the one Meta verifies, before anything is
  stored, subscribed or shared (the library's approval); the holder's
  binding and token stay as they were. Moving a WABA is the operator's
  unbind.
- **A step that stops** is `502 onboarding_failed` with its `step`,
  `resumable`, and Meta's code and trace id in `graph` (never Meta's
  text or `details`). `resumable: true`: the token was stored and the
  attempt is kept (`GET /v1/signup/attempts`); fix the cause and `POST
  /v1/signup/resume` (a new PIN after `register_phone`'s `133005`), from
  any replica or after a restart. `resumable: false`: start again (the
  code is spent). Another tenant's resume is `404 nothing_to_resume`, as
  a finished one is.
- **Coexistence** (the WhatsApp Business app's number): start with
  `{"coexistence": true}`; the contacts and history syncs run as the
  onboarding's last steps, once each (decision D7).
- **No answer, log, error, audit event or kept attempt holds the
  merchant's token**, the PIN, the code or the state. A completion and a
  resumption are `audit` events (`signup_completed`, `signup_resumed`,
  with the tenant, the WABA and the key's id).

### Several WABAs in one signup

A merchant may grant the app several WABAs in one flow (the event's
`waba_ids`). By default the service binds **one**: the **primary** WABA,
the event's `waba_id`, else the first of `waba_ids`; the answer's `waba_id`
is that WABA, and `not_bound_waba_ids` lists the others the event named,
which are not bound (give each its own signup, or turn the option on).

With `WA_SIGNUP_EVERY_GRANTED_WABA=on` the library's several-WABA
onboarding runs and every WABA the token was granted is bound to the
tenant, each with its own token record, subscription and, in Solution
Partner mode, credit line. The primary WABA gets the PIN's registration
and a coexistence sync; the others register nothing. The answer gains
`wabas`, one entry per WABA, the primary first:
`{waba_id, primary, status, phone_number_ids, failed_step?, resumable,
code?}`, `status` being:

| `status` | Meaning |
| --- | --- |
| `connected` | onboarded and bound to the tenant |
| `held` | not the primary WABA, and the vault already held a token for it: left as it was |
| `refused` | bound to another tenant (`code`: `waba_owned_by_another_tenant`): nothing stored for it, the other tenant's binding and token untouched |
| `failed` | stopped at `failed_step`; `resumable: true` when its token was stored: `POST /v1/signup/resume` with its `waba_id` finishes it |

- **Every WABA keeps the ownership rule** (decision D4). A WABA the event
  names that another tenant holds refuses the whole signup, before the
  attempt is redeemed or Meta asked: `409 waba_owned_by_another_tenant`,
  nothing bound, and the attempt completes later without that WABA. A
  grant the event did not name cannot be checked until the token says so:
  it is `held` or `refused` in `wabas`, never taken, and the others are
  still bound.
- The call fails (as a single WABA's failure does) only when no WABA
  connected; otherwise read `wabas`. Every other grant must be the
  primary WABA's business's (the library checks Meta's owner for each).
- Set per deployment, not per signup (decision D200).

### Hosted signup

Meta's hosted Embedded Signup is a link to Meta's page: no code on yours.
It returns nothing to you and names no tenant; Meta sends an
`account_update` `PARTNER_ADDED` with the merchant's `waba_id` and
`owner_business_id`. With `WA_HOSTED_SIGNUP=on` (and `WA_SYSTEM_TOKEN`,
or Solution Partner mode, whose system token is used) the service
remembers each signed `PARTNER_ADDED` for 7 days (asking Meta nothing; a
WABA a tenant or the vault already holds is skipped, as the code flow's
onboardings send one too; an unsigned or forged delivery is `401` and
remembered nowhere). **The operator claims it**, with an admin key, naming
the tenant the merchant belongs to:

```bash
curl -sS -X POST "$WA_SERVER/v1/admin/signup/hosted" -H "Authorization: Bearer $ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"tenant_id": "merchant-42", "waba_id": "524126980791429", "owner_business_id": "2729063490586005", "pin": "581063"}'
```

It answers as a completion does (`steps_completed` starting with
`fetch_token`): the business token fetched from
`system_user_access_tokens` with your system token and its
`appsecret_proof`, then verified, bound to the named tenant, stored,
subscribed, funded in Solution Partner mode, and registered with `pin`
(the hosted page asks for none; choose one per number, never stored).

- **No arrival, or another business: `404 not_found`**, the answer for a
  WABA that does not exist; so is a `tenant_id` that names no tenant (a
  suspended one is `403 tenant_suspended`). Another tenant's WABA is `409
  waba_owned_by_another_tenant`, before anything is claimed. A tenant's
  key, a platform key included, is `403 forbidden`.
- **Who may claim** (decision D202, changed in review): the claim is the
  operator's because Meta's page carries no state and the WABA and
  business ids are no secrets. A route open to tenants would let one
  tenant take an arrival meant for another's merchant and have that
  merchant's business token stored under its own key. Your backend tells
  the operator (or your operator tooling calls this route) which tenant a
  merchant belongs to; an arrival is claimed once.
- A claim that stops before the token is stored is `502
  onboarding_failed` with `resumable: false` and the arrival is put back;
  one that stops after is kept as an attempt for the tenant
  (`POST /v1/signup/resume`). Off, the route is `409 signup_not_configured`
  and nothing is remembered.
- Audited: `hosted_signup_arrived` (the webhook, `origin: webhook`) and
  `hosted_signup_completed` (the operator's change: the admin key's id, the
  tenant, the WABA). Neither holds a token.

### Pre-verified numbers

Numbers you verify ahead of time, in a pool on your own business
portfolio, that a merchant picks in Embedded Signup without typing a
one-time code. The pool is yours, so its routes are the operator's (admin
key), each Meta's call with your system token
(`WA_PARTNER_BUSINESS_ID`, `WA_SYSTEM_TOKEN`; unset, `409
signup_not_configured`):

| Route | Does |
| --- | --- |
| `POST /v1/admin/pre-verified-numbers` | `{phone_number}`: add a number to the pool; `201 {id}` |
| `GET /v1/admin/pre-verified-numbers` | one page of the pool (`?phone_number=`, `?status=VERIFIED\|NOT_VERIFIED\|EXPIRED`, `?limit=`, `?cursor=`): `{data, next_cursor}` |
| `GET /v1/admin/pre-verified-numbers/{id}` | one number, its state and when its verification expires |
| `POST /v1/admin/pre-verified-numbers/{id}/code` | `{method: SMS\|VOICE, language}`: have Meta send the one-time code |
| `POST /v1/admin/pre-verified-numbers/{id}/verify` | `{code}`: check it; the number is then yours to offer for 90 days |
| `DELETE /v1/admin/pre-verified-numbers/{id}` | remove it from the pool (audited `pre_verified_number_deleted`) |
| `PUT`, `DELETE /v1/admin/pre-verified-numbers/{id}/reservation` | `{tenant_id}`: reserve the number for a tenant's launch, or release it; asks Meta nothing |

A tenant's launch offers a number only when it is reserved for it:
`POST /v1/signup/sessions` with `{"pre_verified_phone_ids": ["…"]}` (at
most 20) puts them in `launch_options` as Meta's `preVerifiedPhone`; an
unreserved id, another tenant's and a malformed one are the same `422`.
Tracking who claimed which number stays yours, as Meta says; the limits
(30 days to verify, 90 to be claimed or registered) are Meta's. Sharing a
number with a partner and listing who has it are not routes yet (they
concern Multi-Partner Solutions). The code is in no answer, log or audit
event. No step accepts terms or signs anything.

### Solution Partner mode

With `WA_ONBOARDING_MODE=solution_partner`, your credit line funds every
merchant: the onboarding adds your system user to the merchant's WABA,
checks the line does not fund it already and was not revoked, and shares
it (`whatsapp_credit_sharing_and_attach`), with **your system token,
never the merchant's**, between `subscribe_app` and `register_phone`,
in `WA_WABA_CURRENCY`. The allocation is in the answer
(`credit_allocation_id`) and on the WABA's binding. A line cannot be
taken back once attached, which is why the WABA is bound to your tenant
before anything is shared.

- **A business whose line was revoked stays revoked.** A resume or a new
  signup stops at `share_credit_line` (`502 onboarding_failed`), posting
  no share. Funding it again is your operator's explicit decision:
  `POST /v1/admin/wabas/{waba_id}/credit-line/reshare` (admin key)
  resumes the WABA's steps from its stored token, sharing the line again,
  and is an `audit` event (`credit_line_reshared`).
- A share whose answer was lost is reconciled by the library (it checks
  Meta's records before it posts again); one Meta never lists stays
  pending until an operator clears it with the library's
  `EmbeddedSignup::clear_pending_share`, which the service has no route
  for yet.

### Offboarding

- **Your tenant disconnects** (`DELETE /v1/wabas/{waba_id}`, scope
  `numbers`): the app is unsubscribed with the merchant's token, then, in
  Solution Partner mode, the line is revoked, then the token and the
  bindings are deleted. A revocation that fails deletes nothing (its
  error carries `step: revoke_credit_line`): repeat the call.
- **Meta's webhooks** (`account_update`): `PARTNER_APP_UNINSTALLED` of
  your app (`partner_app_id` is `WA_APP_ID`) and `ACCOUNT_DELETED`
  offboard the merchant the same way (the line revoked first, from the
  owner the vault recorded, else the webhook's `owner_business_id`);
  `PARTNER_REMOVED` revokes the line at once (decision D14) and leaves
  the token, whose next call answers `reconnect_required`. Either order of
  the two ends with the line revoked and the token and bindings gone.
  Each is confirmed with Graph first (`debug_token` on the stored token:
  one Meta still reports valid and granted the WABA contradicts the
  webhook, which is recorded and not acted on), unless
  `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION=signature`. The event is
  recorded first, so it reaches its tenant before the offboarding
  unbinds the WABA. A failure Meta or the database may recover from
  answers Meta `500`, and its redelivery (the event found recorded) runs
  it again; any other is logged at `warn` (target
  `meta_whatsapp_server::events`) for you to act on. A removal from a
  Multi-Partner Solution (`solution_partner_business_ids` listed) is not
  acted on: nothing tells whether your business is the partner removed;
  the event still reaches its tenant.
- **Deleting a tenant** (`DELETE /v1/admin/tenants/{id}`) in Solution
  Partner mode revokes each of its WABAs' lines the same way (decision
  D75): unsubscribed, revoked, then the token and the bindings. A
  revocation that fails deletes nothing and is the answer (`step:
  revoke_credit_line`): repeat it. Each revocation is an `audit` event
  (`credit_line_revoked`, with the key's id).
- **The operator's unbind revokes nothing**: it moves or frees a WABA,
  not end a merchant's funding. Revoke by hand (the library's
  `EmbeddedSignup::revoke_credit_line`) when that is what you mean.
- **A WABA that moves while it is offboarded** (unbound and attached to
  another tenant during the revocation's calls to Meta) keeps its new
  holder's token and binding: only the token and the binding the
  offboarding read are deleted. The disconnection answers `503`; the
  webhook is acknowledged and logged.
- **`WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION=signature`** makes your app
  secret the only gate: whoever holds it can sign a delivery that deletes
  any merchant's token. Even with `graph`, what Graph cannot confirm (no
  stored token, a removal naming no WABA) is a revocation on the
  signature alone, which the reshare route undoes.

## Registration and the two-step PIN

A number added in WhatsApp Manager, or by Embedded Signup, is registered
for the Cloud API before it sends (scope `numbers`; roadmap M3d). The
PIN is the merchant's: have them type it for each attempt and pass it
through; the service stores it nowhere (design D6).

| Route | Body | Does |
| --- | --- | --- |
| `POST /v1/numbers/{pn}/request-code` | `{"code_method": "SMS", "language": "en_US"}` (or `VOICE`) | Meta sends the number a code |
| `POST /v1/numbers/{pn}/verify-code` | `{"code": "123-830"}` (hyphens dropped) | submits it |
| `POST /v1/numbers/{pn}/register` | `{"pin": "212834"}`, optionally `"data_localization_region": "CH"` | registers the number with its PIN: the current one, or the one it gets when it has none |
| `POST /v1/numbers/{pn}/two-step-pin` | `{"pin": "212834"}` | sets a new PIN (there is no call to turn it off) |
| `POST /v1/numbers/{pn}/deregister` | `{"confirm": "106540352242922"}`, the path's number again | the number stops sending and receiving through the Cloud API until registered again |

Each answers `200` with `phone_number_id` and a `status`
(`code_requested`, `code_verified`, `registered`, `pin_set`,
`deregistered`).

```bash
curl -sS -X POST "$WA_SERVER/v1/numbers/106540352242922/register" \
  -H "Authorization: Bearer $KEY" -H "Idempotency-Key: number-106540352242922:register-1" \
  -H "Content-Type: application/json" -d '{"pin": "212834"}'
```

- **The PIN and the code go to Meta and nowhere else**: not into an
  answer, a log line at any level, an error, an audit event, an
  idempotency record, an event or a metric. A body that does not parse
  is `422` on `body`, without quoting it; a PIN that is not six digits is
  `422` on `pin`, and Meta is never asked.
- **Meta's error texts are dropped** on these routes, `details`
  included (they could quote the request): the answer keeps Meta's
  code and trace id. A wrong PIN is `409 two_step_verification`, Meta's
  72-hour lock or an unverified number `409 registration`.
- **Attempts are counted per number** for every replica together, after
  the number is found to be yours: 5 registrations and deregistrations
  in 72 hours (Meta allows 10 of each and then locks the number for 72 hours;
  two of the service's windows back to back stay within it), 10 codes
  requested and checked an hour. Past it, `429 too_many_requests` with
  `Retry-After` the rest of the window, and nothing is asked of Meta. A
  malformed body spends no attempt.
- **`Idempotency-Key`** on `request-code`, `register` and `deregister`.
  A registration's key covers the number and the region, never the PIN:
  a repeat under a kept key with another PIN gets the first answer back.
  A wrong PIN frees the key (nothing was registered), so retry the
  corrected PIN under it.
- **Deregistering is destructive**: the body names the number again, and
  each deregistration, like each PIN change, is an audit event with the
  key's public id. Meta keeps the number's history and its local storage
  setting, and refuses to deregister a number also on the WhatsApp
  Business app (with a code its pages do not list: `502 unknown`,
  `may_have_been_sent: false`, the key freed). The number stays bound to your tenant; its sends fail
  with `409 registration` until it is registered again.

## Profile picture and display name

Scope `numbers` (roadmap M5c2); the picture's upload is scope `media`.

`GET /v1/numbers/{pn}/profile` answers the business profile: `about`,
`address`, `description`, `email`, `profile_picture_url`, `websites` and
`vertical`. `PATCH` changes the fields it carries (the others stay) and
answers the whole profile after the change; an address is at most 256
characters and a vertical cannot be `UNDEFINED` or `NOT_A_BIZ`; a
website is an `http` or `https` URL without credentials, an email has
the shape `name@host`, and no text carries a control, bidi override or
zero-width character (`422` on the field, before Meta is asked).

**The picture** is set from a resumable upload of the same number
([Resumable uploads](#resumable-uploads): open it with a JPEG or PNG
`file_type`, send the bytes, and the chunk that completes the file
answers `complete: true`). Pass the **upload's id**, not Meta's handle:

```bash
curl -sS -X PATCH "$WA_SERVER/v1/numbers/106540352242922/profile" \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"profile_picture_upload_id": "up_0123456789abcdef0123456789abcdef"}'
```

The service reads the handle from the number's own record of that
upload and sends it to Meta as `profile_picture_handle`, in the same
`POST` as the other fields. The records are keyed by the upload's id and
cannot be searched by a handle, so a handle could not be checked against
the tenant and number that obtained it: `profile_picture_handle` is not a
field of the change (`422` on `body`). An upload of another tenant, of
another number, an expired one (24 hours from its start) and one that
was never made are all `404 not_found`, and Meta is not asked. An upload
that is not a JPEG or PNG, or not complete, or an id that is none, is
`422` on `profile_picture_upload_id`. Meta's pages state no picture
format or size: the types are the service's own bound, and the size
bound is the upload's (5 MiB for an image). One upload may set the
picture again; a handle Meta refuses as invalid or expired is its
`invalid_parameter`.

**The display name** is the name WhatsApp shows for the number, and a
change is reviewed by Meta first:

| Route | Body | Does |
| --- | --- | --- |
| `POST /v1/numbers/{pn}/display-name` | `{"new_display_name": "Lucky Shrub"}` | asks Meta to review the name (`POST /{pn}?new_display_name=`); `202 {"new_display_name": …}`; takes an `Idempotency-Key` |
| `GET /v1/numbers/{pn}/display-name` | | `verified_name`, `name_status`, `new_display_name`, `new_name_status`, each `null` when Meta answers none |

```bash
curl -sS -X POST "$WA_SERVER/v1/numbers/106540352242922/display-name" \
  -H "Authorization: Bearer $KEY" -H "Idempotency-Key: rename-106540352242922-1" \
  -H "Content-Type: application/json" -d '{"new_display_name": "Lucky Shrub"}'
```

- **What a request does not do.** The name changes only once Meta
  approves it (`new_name_status` `APPROVED`; the event
  `phone_number_name_updated` says so) **and** the number is registered
  again, with its two-step PIN, within 14 days of the approval: call
  `POST /v1/numbers/{pn}/register` ([Registration](#registration-and-the-two-step-pin)).
  Registering before the approval has no effect. The service does not
  register the number for you: it never holds the PIN. `verified_name`
  does not say a name is approved; `name_status` does.
- **Meta's limits are Meta's**: 10 changes per 30 days (Meta refuses the
  eleventh; the service keeps no count), and a number with Official
  Business Account status changes its name through Meta's support. The
  name is trimmed, 1 to 256 characters (the service's own bound: Meta's
  pages state none), with no control character and no bidi override or
  zero-width character (the ones that spoof a name; the joiners and
  direction marks that Persian, Indic and mixed-direction names need are
  kept): `422` on `new_display_name`, before Meta is asked. The guidelines Meta reviews
  against are its Help Center's.
- **Audited.** Each request is an audit event in the service's logs
  (action `display_name_change_requested`) with the tenant, the WABA, the
  number, your key's public id, the name's length and the outcome
  (`answered`, or `unknown` when the call may have gone through
  unanswered: read the status). The name itself is not in it.
- **`Idempotency-Key`**: a repeat under a kept key and body gets the
  first answer back, never a second review; another body under the key
  is `422 idempotency_key_reused`. A request cut by a timeout (`504`,
  `may_have_been_sent: true`) is not replayed, by the library or under
  its key: read `GET …/display-name` to see whether Meta took it.
- The picture's handle, Meta's upload session, the picture's URL and the
  upload ids are in no log line of the service.

## Number settings

Scope `numbers` (roadmap M5c3), on a number of your tenant (another's is
`404`, nothing asked of Meta). `GET /v1/numbers/{pn}` now reports the
number's `messaging_limit_tier` (`TIER_250`, `TIER_1K`, `TIER_10K`,
`TIER_100K`, `TIER_UNLIMITED`, …) beside its quality rating and throughput.

| Route | Does |
| --- | --- |
| `GET /v1/numbers/{pn}/settings` | local storage, and payload encryption's `status` and key fingerprint |
| `PATCH /v1/numbers/{pn}/settings` | **one** setting per call: `identity_key_check`, `local_storage` (`{enabled, data_localization_region}`; only while the number is not registered, Meta's rule), `search_visibility` (`VISIBLE`, `HIDDEN`), `security_notifications`, `notify_user_change_number`; answers `{setting}` |
| `PUT`, `DELETE /v1/numbers/{pn}/settings/payload-encryption` | payload encryption on (`{client_encryption_key}`, your **public** key in base64) or off; `204` |
| `GET`, `PUT`, `DELETE /v1/numbers/{pn}/username`, `GET …/username/suggestions` | the business username (3 to 35 characters, Meta's format; `force_transfer` moves it from another number of the portfolio); the usernames WhatsApp reserved |
| `GET`, `POST /v1/numbers/{pn}/official-business-account` | the status; the application (audited, takes an `Idempotency-Key`) |
| `GET`, `PUT /v1/numbers/{pn}/compliance-info` | India's business compliance information; the declaration (the legal act, below) |
| `GET /v1/numbers/{pn}/health-status` | whether messages can be sent through the number, and each node's status |
| `GET`, `PUT /v1/numbers/{pn}/conversational-components` | the welcome message flag, ice breakers (4 of 80 characters) and commands (30; names of 32 and hints of 256) |
| `DELETE /v1/numbers/{pn}/contact-book/{bsuid}` | erase one user's contact book entry at Meta (irreversible; scope `erasure`, audited) |

- **Payload encryption keys are secrets.** The key is in the request body
  only: not in a log, an error, a `Debug` or an answer (a private key or a
  value that is not base64 is `422 invalid_request` on
  `client_encryption_key`, never quoted). Meta's own `cloud_encryption_key`
  is not answered: the settings read gives the status and your key's
  fingerprint. Meta's error texts on these calls are withheld, and a
  call cut by a timeout is not replayed (`504`: read the settings before
  asking again).
- **One legal act is off until you turn it on.** The India compliance
  information declaration (`PUT …/compliance-info`) answers `403
  feature_not_available`, with nothing asked of Meta, unless the
  deployment sets `WA_SERVER_LEGAL_ACTS=on`; turned on, it is taken only
  when a tenant's key asks, and is **audited** (`compliance_info_set`,
  with the tenant, WABA, number, the key's public id and the outcome;
  never what it says). The Official Business Account application is not
  gated (it is an application for Meta's review): it is audited
  (`official_business_account_requested`), takes an `Idempotency-Key` and
  is never replayed after a timeout. The contact book erasure is not gated
  either: it is a data-subject erasure your tenant performs explicitly,
  with the separate scope `erasure`, audited (`contact_book_entry_deleted`,
  never whose entry), irreversible, and takes only a BSUID (a phone number
  is `422`).
- **Bounds.** Meta's documented limits are checked before any request
  (`422` on the field); where Meta states none the service's own apply:
  links are `http` or `https` without credentials, texts have no control,
  bidi override or zero-width character, short texts are at most 128
  characters and free texts 1024.
- **Not offered**: changing the number's `webhook_url` or
  `connection_status` (the service owns the number's events), calling
  settings (roadmap M5e), bot details and the coexistence data sync. No
  call of these routes was run against Meta live.

## Marketing Messages API, max price and welcome message sequences

Scope `marketing` (roadmap M5h; a key made before it has none, and `send`
and `templates` do not grant it). Everything is on your tenant's own
number or WABA: another's is `404`, nothing asked of Meta. A template or a
sequence an id names must be the WABA's own, else `404` with only the
lookup sent.

| Route | Does |
| --- | --- |
| `POST /v1/numbers/{pn}/marketing-messages` | sends a marketing template through Meta's Marketing Messages API: `{to, template, product_policy?, message_activity_sharing?, bid_spec?}` (`product_policy` is `CLOUD_API_FALLBACK` or `STRICT`; `bid_spec.per_message_bid_multiplier` scales the template's max price for this message); `202` as a message send; takes an `Idempotency-Key`, is never replayed after a timeout |
| `GET /v1/wabas/{waba_id}/marketing/onboarding` | the WABA's onboarding status and its owner business's terms state (`terms_status`: `TERM_OF_SERVICE_SIGNED`, `REQUEST_SENT`, `NOT_STARTED`) |
| `GET`, `PUT /v1/wabas/{waba_id}/marketing/cloud-api-marketing` | whether marketing templates are blocked on Cloud API `/messages` (`{disabled}`); audited |
| `POST /v1/wabas/{waba_id}/marketing/template-duplicates` | an approved marketing template of the WABA duplicated at another max price: `{source_template_id, name, optimization_spec: {bid_amount, bid_strategy?, bid_country_multiplier_overrides?}}`; `201`; takes an `Idempotency-Key`; audited |
| `GET /v1/wabas/{waba_id}/marketing/reach-estimates` | `?countries=IN,US&date_interval=L7D` (`L1D`, `L7D`, `L14D`, `L28D`): estimated deliveries and costs at several max prices |
| `GET`, `PUT`, `DELETE /v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price` | a template's max price read, set (the same `optimization_spec`; it applies within an hour) or returned to rate-card pricing; audited |
| `POST`, `GET /v1/wabas/{waba_id}/welcome-sequences`, `GET`, `PATCH`, `DELETE …/{sequence_id}` | Click-to-WhatsApp welcome message sequences: `{name, welcome_message_sequence: {text?, autofill_message?: {content}, ice_breakers?: [{title}]}}`; the creation takes an `Idempotency-Key`; changes are audited; Meta refuses to change or delete one an active ad uses |

- `bid_amount` is per 1,000 deliveries, in the smallest unit of the WABA's
  currency (a price of 0.87 per delivery is `87000`), from 1 to
  2147483647; multipliers are keyed by country (two capital letters),
  above 0 and at most 10, at most 240 of them.
- **The max price agreement is a legal act, and an operator's.** Max price
  needs Meta's beta agreement, signed once per business portfolio. No
  tenant route signs it, and neither does a duplicate, a price change or a
  send: when the agreement is missing you get Meta's refusal as it is.
  The operator signs it with an admin key:
  `POST /v1/admin/businesses/{business_id}/max-price-agreement` with
  `{signer_name, signer_email, token}` (the token: a system token or a
  business token of an admin of the business's Business Suite; used for
  this call, stored nowhere). Unless the deployment sets
  `WA_SERVER_LEGAL_ACTS=on` it answers `403 feature_not_available` with
  nothing asked of Meta; on, it makes one request, never replayed, and one
  audit event (`max_price_agreement_signed`, with the admin key's public id
  and the business, never the signer). Meta emails the signer an
  acceptance link, and a second call within five minutes is Meta's `429`.
  The signer's name and email are in no log, error or answer. Accepting
  the Marketing Messages API's Terms of Service is not a call: a person
  with full control of the portfolio does it in WhatsApp Manager.
- **Bounds.** Meta's documented limits are checked before any request
  (`422` on the field); where the pages state none the service's own apply:
  240 countries a reach estimate, a sequence name of 128 characters, welcome
  texts of 1,024, 20 quick replies, 100 sequences a listing.
- **A GIF header** is created through the templates route (`format: GIF`
  with its `example` handle); Meta's pages show no send-time parameter for
  it, so none is checked: the send takes the template as Meta's object.
- **Not offered**: the agreement's state read, the partner Intent API and
  the client WABAs listing (roadmap M5h2). Automatic events arrive as
  tenant events (`automatic_event_detected`), with no route. No call of
  these routes was run against Meta live.

## Rate limits

Each tenant has a budget per class of route, per replica (a deployment
of N replicas allows N times as much): writes (sends, read receipts,
media, profile changes) 20 a second with bursts of 40, reads 50 a second,
template management 2 a second, OTP issues 5 a second and OTP checks 20; Embedded Signup's routes count as writes (its attempts list as a read). A platform key acting for a tenant draws
from that tenant's budget. Past it: `429 too_many_requests`, `retryable:
true`, with `Retry-After` in seconds, before anything is read or sent.
Change them with `WA_SERVER_RATE_*` (the same for every tenant: per-tenant
limits are not settable yet). A number's registration attempts have
budgets of their own, per number ([above](#registration-and-the-two-step-pin)). Meta's own limits (80 messages a second per
number, pair and portfolio limits) still apply and answer their own codes
(`rate_limited`, `pair_rate_limited`, …); pacing campaigns is yours.

## The contract

The whole contract is the OpenAPI document: `GET /v1/openapi.json`, or
[crates/meta-whatsapp-server/openapi/v1.json](../../crates/meta-whatsapp-server/openapi/v1.json)
in the repository. Generate TypeScript types from it with
`npx openapi-typescript`. This contract stays: the service's axum API,
which serves it, is permanent, and remains a deployment's choice after
the default moves to CrateStack's procedures and generated client
([roadmap](../roadmap.md) S16, design D26). Swapping the API swaps the
contract and the client with it.

## Receiving Meta's webhooks

One callback URL per Meta app receives every merchant's events: point
the app at the public listener, and the service routes each event to the
tenant that owns its number.

**1. Point the Meta app at the service.** In the App Dashboard
(WhatsApp, Configuration), set the callback URL to
`https://<your public host>/webhooks/meta` and the verify token to
`WA_VERIFY_TOKEN`'s value; Meta's check (`GET /webhooks/meta`) answers
the challenge. Subscribe the fields you need (`messages` and `account_update` at least;
`meta-whatsapp-server doctor` names the ones missing).
Attaching a WABA subscribes the app to it (Embedded Signup will too, M3):
without that, Meta sends nothing for its numbers.

Your ingress must pass bodies of 3 MiB untouched (no decompression, no
rewriting: the signature covers the raw bytes), buffer each request
before it reaches the service (slow clients stay at the ingress), wait
longer than the slowest delivery, and **admit to `/webhooks/meta` (and
`/webhooks/flows/…`) only Meta's webhook IP ranges, or mutual TLS with Meta's client
certificate**. A signature is only checked once a body is read, and
anyone can send a well-formed one: without that filter, anyone on the
internet makes the service read bodies. The app secret signs every
tenant's deliveries: whoever holds it can forge any tenant's events.

**2. What the service does with a delivery** (`POST /webhooks/meta`):

- `401` without a well-formed `X-Hub-Signature-256` (before reading the
  body), or when no app secret produced it (`WA_APP_SECRET`, or
  `WA_APP_SECRET_PREVIOUS` while rotating); `413` past 3 MiB; `408` when
  the body takes over 15 s to arrive.
- A replica reads at most 64 deliveries at once and records at most 4
  (its pool of 10 connections keeps room for API calls): past that,
  `503` and Meta retries. So does a delivery whose tenant's events stay
  locked over 2 s by others being recorded.
- Each event is claimed for 60 s in the shared store: a delivery another
  replica is handling right now answers `503` and Meta retries; one
  already recorded is acknowledged without being recorded again.
- **Routing is an allow-list.** An event naming a business phone number
  goes to the tenant that number is bound to (and only when Meta names the
  WABA the service bound it under); one naming only a WABA (template
  reviews, account and quality updates) to the WABA's tenant; and only
  if Meta dated it no earlier than the second the WABA was attached in,
  so a WABA moved to another tenant does not bring the first one's late
  dated events along. After such a move, an event of the attaching's own
  second (and of `WA_SERVER_MOVE_SKEW_MARGIN` after it) goes to no
  tenant, as it may be the first one's; a WABA attached for the first
  time keeps the events of its first second. Errors, history chunks and
  group updates without a date carry no date to compare: after a WABA
  moved to another tenant, they go to no tenant for 7 days and an hour
  after the move (`WA_SERVER_UNDATED_AFTER_MOVE`), since Meta may be
  redelivering the first one's; otherwise they go to whoever holds the
  number or WABA when they arrive (a WABA attached for the first time
  gets its history sync). A tenant deleted and created again under the
  same id counts as another tenant. It is recorded in the inbox first,
  then in the event outbox.
- **Operator-only events** are recorded without a tenant, never shown to
  one, logged with their size and digest and counted
  (`wa_server_webhook_events_total{audience="operator"}`): events of a
  number or WABA no tenant holds, a field the library does not type
  (`unknown`), a signed body that is not a webhook (`unparsed`), partner
  solution updates, any event type the service has not reviewed yet
  (none today: Conversation Routing's `standby_observed` and
  `thread_control_changed`, and `user_action_reported`, a marketing
  message's click, reach their number's tenant since roadmap M2d),
  events Meta dated before the WABA's attaching (or, after a move from
  another tenant, in its second), undated events within the window
  after such a move, and replays (dated more than 7 days and an hour ago, what the dedup
  markers remember). A rising
  count usually means a WABA is subscribed but not attached.
- `200` once every event is recorded; `500` when recording failed: Meta
  retries the batch at once, then with decreasing frequency for up to 7
  days (`webhooks/create-webhook-endpoint`), the events already recorded
  are acknowledged as duplicates, and neither the inbox nor the outbox
  records one twice. An event that fails every time (a permanent sink
  error) holds its whole batch back for those 7 days, after which Meta
  drops it: the events after it in the same body are lost with it
  ([OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md) #30: the library's dead
  letters, roadmap L21a, take only an error a sink classifies as
  permanent, and the service's classifies none; watch
  `wa_server_webhook_sink_failures_total`).
- Errors and bodies that are not webhooks carry no id: they are told
  apart by the body they came in and their place in it, for an hour
  (design D23, a coordinator's decision that stands under the owner's
  delegation). This
  assumes Meta redelivers the same bytes, which Meta does not document.
  The same body again after that hour is recorded again, as a new event
  with its own id (the same error can legitimately recur): an outage
  longer than an hour, the database answering `500` while Meta retries,
  records the batch's errors twice, under new ids.

**3. Poll the events** with a key holding the `events` scope:

```bash
curl -sS "http://127.0.0.1:8081/v1/events?limit=100" -H "Authorization: Bearer $KEY"
curl -sS "http://127.0.0.1:8081/v1/events?after=18342&types=message_received,status_updated" \
  -H "Authorization: Bearer $KEY"
```

The answer is `{"data": [...], "next_after": 18350}`. Store `next_after`
and pass it as `after` next time: it is the last event's `sequence` when
more follow (poll again at once), else the tenant's newest sequence (wait
a little). Each tenant has its own sequence, increasing (with gaps)
for its events alone; a tenant created again under a deleted tenant's id
goes on after the deleted one's last sequence. Omitting `after` starts
at the oldest event kept. Each event is an envelope:

```json
{"id": "evt_3f9c…", "sequence": 18342, "type": "message_received", "api_version": "v1",
 "tenant_id": "merchant-42", "phone_number_id": "106540352242922", "waba_id": "102290129340398",
 "received_at": "2026-09-25T10:00:01Z", "truncated": false,
 "data": {"event": "message_received", "message": {"id": "wamid.…", "type": "text", "…": "…"}}}
```

`data` is meta-whatsapp-rs's `WebhookEvent` JSON (Meta's fields,
normalized; key customers by `contact.user_id`, the BSUID, since
`wa_id` may be absent). Deduplicate on `id` (an event recorded again,
Meta's late retry or a replay, keeps its id, unless `WA_APP_SECRET` was
rotated in between: ids are derived with it) and order on `sequence`;
what your backend does per message (an order confirmation), make
idempotent on the message id too (`data.message.id`), as a last guard.
Filter with `types` (comma-separated, or repeated) and `phone_number_id`;
a page stops before 8 MiB of `data` (history syncs are large). A
filtered poll's `next_after` moves past the events the filter left out
(to the tenant's newest sequence when no more match), so keep one cursor
per filter set (`types`, `phone_number_id`): a cursor saved by one
filter skips, for another, what the first left out. A cursor
older than what was purged, by retention or with a deleted tenant of the
same id, is `410 cursor_expired`: resynchronise (from the inbox, when it
lands in M2) and start again without `after`. A cursor past the tenant's
newest sequence (a restored database) is `422` on `after`.

The types (`KnownEventType` in the OpenAPI document) only grow within
v1: switch on the ones you handle and let any other fall to a default.
Among them since roadmap M2d:

- `standby_observed`: a copy of a thread your number observes without
  owning it (Conversation Routing): the customer's message, the owner's
  send, or its status. Never answer one: the inbox records the
  customer's message as a window event (no unread message) and the
  thread as another app's, so a free-form reply is `409
  thread_owned_elsewhere` until the thread comes back (or the number's
  `WA_SERVER_INBOX_REPLY_CHECKS` turn that check off).
- `thread_control_changed`: the thread was passed to you
  (`data.update.type: control_passed`) or taken from you
  (`control_taken`); the inbox's reply check follows it.
- `user_action_reported`: a customer clicked a marketing message's body
  or call-to-action (Meta names neither the customer nor the message:
  `data.action` holds the click's time, the component, and Meta's
  `click_id`, `tracking_token` and `product_id`).
- The service's own events about your numbers, whose `data` names the
  number and its WABA only: `number_connected` (attached or onboarded
  through Embedded Signup, its token stored: also after a
  reconnection), `number_disconnected` (with `data.reason`:
  `tenant_disconnected` for your `DELETE /v1/wabas/{waba_id}`,
  `operator_unbound` for the operator's unbind, `partner_app_uninstalled`
  and `account_deleted` when Meta's `account_update` offboards the WABA;
  more reasons may come: default on one you do not know),
  `number_reconnect_required` (Meta refused the number's token: calls on
  it answer `409 reconnect_required` until it is attached again). Each
  comes once per binding of the number: a token refused again before a
  reconnection records no second one. They tell you when to look, not
  the state itself: an event is recorded just before its change, so a
  change that then fails (a `503` your retry finishes) leaves it; a
  retried attach can repeat `number_connected`; and
  `number_reconnect_required` is best effort. `GET /v1/numbers` is the
  state: read it when one comes (design D81).
- `token_expiring`, the service's own about a stored token (roadmap M3e):
  the token of one of your WABAs expires within the lead time
  (`WA_SERVER_TOKEN_EXPIRY_LEAD`, 7 days by default; `off` turns it off), so
  run Embedded Signup again before then, or your calls answer `409
  reconnect_required`. `data` is `{event, waba_id, expires_at}` (RFC 3339,
  UTC): ids and the time only. You get one per token and expiry (a token
  stored anew has a new expiry, so it is told again), only for your own
  WABAs, and none for a token that expired before the service saw it. The
  counter `wa_server_token_expiry_events_total{outcome}` is the
  operator's. The lead may not exceed `WA_SERVER_OUTBOX_RETENTION`.

**4. One event by id.** `GET /v1/events/{id}` answers one of the
tenant's events, whole, as polling does: what a webhook delivery that
left a large `data` out (`truncated: true`, M2c) refers to. Another
tenant's id, an operator-only event's, one past retention and an id
that is no event's (`evt_` and 32 hex digits) are all `404 not_found`,
alike.

**5. Or follow them live** (Server-Sent Events, roadmap M2b), with the
same key and scope:

```bash
curl -sSN "http://127.0.0.1:8081/v1/events/stream?types=message_received" \
  -H "Authorization: Bearer $KEY" -H "Last-Event-ID: 18342"
curl -sSN "http://127.0.0.1:8081/v1/numbers/106540352242922/events/stream" \
  -H "Authorization: Bearer $KEY"
```

```text
event: whatsapp
id: 18343
data: {"id":"evt_…","sequence":18343,"type":"message_received",…}

: keepalive
```

- Each `whatsapp` frame's `id` is the event's `sequence` and its `data`
  the envelope above. A `: keepalive` comment comes every 15 s while
  nothing happens.
- **Resume** with `Last-Event-ID: <the last id you got>` (an
  `EventSource` sends it on its own when it reconnects) or `?after=`;
  the header wins. The stream sends exactly the events after it, on any
  replica, then the new ones. Without either it starts with the next
  event: catch up on history with `GET /v1/events` first, then stream
  from its `next_after`. The cursor is checked before the stream starts:
  below what was purged, `410 cursor_expired` (resynchronise, start
  again without one); past the tenant's newest sequence, `422` on
  `Last-Event-ID` or `after`.
- The filters are polling's: `types`, `phone_number_id` (or the
  number's own route, `/v1/numbers/{pn}/events/stream`, `404` for a
  number the tenant does not hold). Operator-only events are never
  streamed.
- **The stream can end** with one last frame, both with
  `data: {"after": <the last sequence it sent>}`: `event: lagged` when
  your client took nothing for 30 s while 1 MiB (or 64 frames) waited
  for it (what waited is dropped: it comes again when you resume), or
  when events after its cursor were purged before it got them; `event:
  shutdown` when the replica stops. Reconnect with `Last-Event-ID` set
  to that `after` (after `lagged` on a purge, the reconnection answers
  `410`: reload). It ends with no last frame once its key is revoked or
  expires, or the tenant is suspended or deleted (checked again before
  each read, within a second, or within 15 s on an idle stream): the
  reconnection answers `401` or `403`.
- At most 20 streams per tenant on a replica (number streams included)
  and 1,000 in all (`WA_SERVER_STREAMS_PER_TENANT`,
  `WA_SERVER_STREAMS_PER_REPLICA`): the next is `429 too_many_streams`,
  with `Retry-After: 5`. A stream gives its place back when it ends,
  even when its client never reads the last frame. Opening one counts
  once against the tenant's `read` budget.
- Browsers cannot send the key (`EventSource` sends no `Authorization`):
  relay the stream through your backend (design D3).
- Several replicas: each holds one `LISTEN` connection of its own on
  the database (outside its pool), and an event recorded by any replica
  wakes every replica's streams through Postgres `NOTIFY` on the channel
  `wa_server_events`. A wake-up is only that: every stream also reads
  the outbox every 15 s, so a notification lost (a connection dropped, a
  replica restarting) delays an event by 15 s at most and loses none.
  Two deployments sharing one database hear each other's wake-ups,
  which only cost a read.

**Restoring the database.** A point-in-time restore rolls the tenants'
sequences back: the events recorded after it are numbered again from
the restore point, so a sequence an integrator already saw can name
another event. The `422` above only catches a cursor still ahead when
it polls; one that polls after new events have passed it skips them
without an error. After a restore, tell every integrator to resynchronise
and reset their cursors (poll without `after`), as after `410`.

Events are kept `WA_SERVER_OUTBOX_RETENTION` (7 days by default, the
outbox's own setting: design D10 sets retention per store); every replica runs housekeeping
every 10 minutes, one at a time, which also deletes the expired webhook
dedup markers. Deleting a tenant deletes its events (design D22, a
coordinator's decision that stands under the owner's delegation), and
takes it out of every platform key's allowed tenants (D24,
likewise): a tenant created again with the same id starts with neither.
No route or command edits a platform key's allowed tenants, so mint a new
platform key for a tenant created again (a key allowing every tenant,
`*`, allows it at once).

## Webhooks out

For a backend that would rather receive than poll or hold a stream (a
Medusa store), the service pushes each tenant's events to URLs of its
own, signed the [Standard Webhooks](https://www.standardwebhooks.com/)
way (design §4.4; roadmap M2c). The operator sets two things:

- `WA_SERVER_DATA_KEY` (`openssl rand -base64 32`, kept out of the
  database like the vault key): the endpoints' signing secrets are sealed
  under it. Without it on Postgres, every webhook endpoint route answers
  `409 webhooks_not_configured` and nothing is delivered.
- `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`: where endpoints may point.
  A URL is `https` (plain `http` only with `WA_SERVER_ENV=development`),
  carries no user name or password, and its host is listed by name, or
  every address it resolves to is inside a listed CIDR. Private,
  loopback and link-local addresses pass only inside a listed CIDR (list
  `10.0.0.0/8`, say, for a Medusa in the same cluster); unspecified,
  multicast and the clouds' metadata addresses never do. The host is
  looked up when the endpoint is made and again at every attempt, and the
  attempt connects to the address it checked: a host that later resolves
  to a private address is refused then (`destination_refused`), nothing
  sent. Allow egress to those destinations. A listed CIDR lets every
  tenant reach every host and port inside it (each seeing the status its
  attempt got): list the receivers' own addresses, not a whole cluster's.

A tenant (scope `webhooks`) makes an endpoint:

```bash
curl -s -X POST http://127.0.0.1:8081/v1/webhook-endpoints \
  -H "Authorization: Bearer $TENANT_KEY" -H 'Content-Type: application/json' \
  -d '{"url": "https://shop.example.com/hooks/whatsapp", "types": ["message_received", "status_updated"]}'
```

The answer (`201`) carries its `id` and its `secret` (`whsec_…`), shown
this once: store it. The endpoint receives the tenant's events of those
types recorded from then on (10 endpoints a tenant,
`409 webhook_endpoint_limit_reached` past it).

- **Each delivery** is a `POST` of the envelope `GET /v1/events/{id}`
  answers, with `webhook-id` (the event's `id`: deduplicate on it),
  `webhook-timestamp` and `webhook-signature` (`v1,<base64 HMAC-SHA256>`
  over `id.timestamp.body`, the body's exact bytes). Verify it with any
  Standard Webhooks library, or as the events skill's example does, and
  refuse a timestamp over 5 minutes off. An event whose `data` is over
  256 KiB arrives with `data: null` and `truncated: true`: fetch it whole
  with `GET /v1/events/{id}`.
- **Answer 2xx within 10 s.** Anything else (a redirect included: it is
  never followed) is a failure, retried after 10 s, doubling to an hour a
  step, for 72 h (`WA_SERVER_WEBHOOK_RETRY_WINDOW`); then the delivery is
  `failed`, and an endpoint that failed the whole window is turned off
  (`enabled: false`, `disabled_reason: failing`): `PATCH` it
  `{"enabled": true}` once fixed, and it resumes from where it stopped,
  within the outbox's retention. At most 1 KiB of your answer is read.
  Deliveries come at least once and in no order: order on `sequence`.
- **Rotate** with `POST /v1/webhook-endpoints/{id}/rotate-secret`: the new
  secret is shown once, and for 24 hours every delivery carries two
  signatures, the new one first; accept either until
  `previous_secret_expires_at`.
- **Check** with `POST …/{id}/test`: a `ping` (`{"type": "ping", …}`),
  attempted once; `GET …/{id}/deliveries` lists the attempts, newest
  first (status, attempts, the last one's status, error and duration, the
  next one's time; no bodies), and `POST …/deliveries/{delivery_id}/retry`
  sends one again.

Every replica dispatches: a delivery is claimed by one of them under a
lease, so a replica that stops mid-attempt leaves it to another after a
minute (`WA_SERVER_WEBHOOK_LEASE`; the replicas' clocks must agree
within it). One slow receiver holds at most 4 attempts a replica; a
tenant, 8; a replica, 32 (`WA_SERVER_WEBHOOK_IN_FLIGHT…`). A platform key's endpoint is the tenant's it acts as
(`WA-Tenant`): make one per tenant.

## Errors

Every error answers one body:

```json
{"error": {"code": "reconnect_required",
  "message": "The number's token is no longer valid: connect it again.",
  "retryable": false, "may_have_been_sent": false, "field": null,
  "step": null, "resumable": null,
  "graph": {"code": 190, "subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn", "details": null},
  "request_id": "req_4f1c2a9b0d3e5f67"}}
```

- Branch on `code`, never on `message` (the service's sentence) or on the
  status alone. Codes only grow within `v1`; treat one you do not know by
  its status class. A failure Meta reported carries Meta's error kind as
  its code (`template_not_found`, `marketing_opted_out`, …; the list is
  the `ErrorCode` schema of the OpenAPI document) and Meta's code under
  `graph`; Meta's own error message never reaches you. `graph.details` is
  Meta's text, not the service's (at most 512 characters, without control
  or format characters or line separators; never on the OTP routes, where
  it could quote the code): show it to an operator, never branch on it.
- A path called with a method it does not take is `405
  method_not_allowed`, with `Allow`.
- **Resend only when `may_have_been_sent` is `false`.** A `504 timeout`
  or a `502` may have taken effect at Meta.
- `invalid_request` names the offending `field`; `401` is always
  `unauthenticated`; a number, WABA, media id or template id of another
  tenant is `404 not_found`, like one that does not exist.
- A token Meta rejects (`190`) marks the WABA's numbers
  `reconnect_required`: later calls on those numbers answer `409
  reconnect_required` without asking Meta, until the WABA is attached
  (or onboarded) again (a WABA route, templates say, asks Meta and gets
  the same answer). It is `409` on a route naming a media or template id
  too: a dead token is never taken for a missing object, and neither is
  Meta failing (a `5xx` stays `502`).

## Check a deployment: doctor

`meta-whatsapp-server doctor` reads the environment as `serve` does and
reports what would stop the service, or stop Meta's events and the
tokens from reaching it: one line per check, `OK`, `WARN`, `FAIL` or
`SKIP` (not checked, and why), then the counts. It exits non-zero when a
check failed, so a deploy job can run it before `serve`. It only reads:
no migration runs, the vault re-encrypts nothing, every Graph call is a
`GET`, and no secret is printed (variables are named, URLs shown
without their credentials, Meta's failures by code).

| Check | Fails when |
| --- | --- |
| `config.<VARIABLE>` | `serve` would refuse the variable. Every refusal is listed, not only the first: doctor replaces the refused variable with an accepted value and reads again (a replacement can hide a problem only the value you meant would show) |
| `config`, `config.WA_APP_SECRET`, `config.binds`, … | never: what was read, without a secret's value; warnings for a throwaway vault key, a `WA_GRAPH_ENDPOINT` that is not Meta's, no `WA_APP_ID` |
| `database` | the build lacks the backend's feature, or the database does not answer |
| `database.migrations` | (Postgres) a service migration is pending with `WA_SERVER_MIGRATE=skip` (a warning with `auto`, as `serve` applies it), a recorded one differs from this binary's, or one was left half-applied. Read with `SELECT`s; the library's history is counted, its versions not compared |
| `webhooks.subscription` | `GET /{app-id}/subscriptions` (with the app token) shows no active `whatsapp_business_account` subscription |
| `webhooks.fields` | `messages` or `account_update` is not subscribed; the other fields behind the service's event types (`account_alerts`, `account_review_update`, `business_capability_update`, the template and number updates, `security`, `user_preferences`) warn when missing |
| `token.partner` | (Solution Partner mode) `debug_token` says `WA_PARTNER_SYSTEM_TOKEN` is invalid, expired or another app's |
| `token.waba.<id>` | a bound WABA has no token in the vault, one the vault keys cannot open, or one `debug_token` says is invalid, expired (a warning within 7 days), another app's, without `whatsapp_business_management` or `whatsapp_business_messaging`, or granted for other WABAs |
| `webhooks.waba.<id>` | the app is not among the WABA's `subscribed_apps`: Meta sends nothing for its numbers (attach it again) |
| `otp.default_tenant` | never (with `WA_SERVER_OTP_DEFAULT_TENANT` set): it warns when the tenant named does not exist, is suspended, has no OTP settings, or no longer holds their sending number, since the tenants without settings of their own then get `409 otp_not_configured`; skipped on memory storage |

The Graph checks need `WA_APP_ID` (they skip without it) and an accepted
configuration. The WABA checks walk the first 100 bound WABAs, two calls
each, and skip on memory storage (the running service's records are in
its own process). A refused configuration still checks the database when
`DATABASE_URL` reads.

```text
$ meta-whatsapp-server doctor
FAIL config.WA_APP_SECRET: WA_APP_SECRET is required
FAIL config.WA_OTP_PEPPER: WA_OTP_PEPPER must be at least 32 bytes
OK   database: Postgres reachable
WARN database.migrations: service migrations 3 are pending: `serve` applies them at start (WA_SERVER_MIGRATE=auto)
SKIP webhooks.subscription: the configuration is refused
SKIP token.wabas: the configuration is refused
2 failed, 1 warnings, 1 ok, 2 skipped
```

## Operations

- `/livez` answers while the process runs; `/readyz` fails when Postgres
  does not answer or after `SIGTERM` (the listeners stop accepting at the
  same moment: keep a preStop delay in Kubernetes if the load balancer
  must see the replica unready first).
- Limits, per listener: a request head must arrive within 10 s (slow
  clients are cut off), 256 connections on the public listener and
  4,096 on the internal one (more wait), every request is answered
  within 55 s (past it, `504 timeout` with `may_have_been_sent: true`),
  and a connection that takes no byte of its answer for 60 s (a client
  that stopped reading a live stream or a download) is closed.
- `/metrics` (Prometheus) counts requests by listener, method, route
  template, status and error code, their duration, failed Graph calls by
  code, idempotent repeats by outcome (`replayed`, `reused`,
  `in_progress`, `outcome_unknown`), rate-limited requests by class,
  Meta's deliveries by outcome (`delivered`, `unauthenticated`,
  `payload_too_large`, `slow_body`, `busy`: the replica at capacity,
  `in_flight`: a run of them means recording outlasts the 60 s lease,
  `failed`), recorded events by type and audience (`tenant`,
  `operator`), duplicates and recording failures by stage, OTP issues
  and checks by outcome (`wa_server_otp_outcomes_total`), token-expiry
  events by outcome (`wa_server_token_expiry_events_total`: `recorded`,
  `failed`), and the live
  event streams open on the replica (`wa_server_event_streams_open`);
  never an id, a number, a code or a key.
- Expired idempotency records are purged every 10 minutes by one replica
  at a time, with the outbox's housekeeping.
- Logs are JSON, one line per request with its id (`X-Request-Id`, echoed
  or generated), route template (never the raw path), tenant, the public
  id of the key that made it, status and duration; every change an
  operator makes is also an `audit` event (action, admin key id, the
  tenant, key or WABA touched), and so is a tenant's template deletion
  (with the key's id). No secret, token, message text or phone number is
  logged, and no webhook body or event content (an operator-only event
  is logged by type, size and digest); refused deliveries at most once a
  minute per reason; Meta's error texts only at `debug`.
- Idempotency records keep each kept answer for
  `WA_SERVER_IDEMPOTENCY_TTL`: a send's holds the recipient's phone
  number, WhatsApp id or BSUID, in plain text. Database encryption at
  rest is yours.
- Known limit: key digests are unpeppered SHA-256 of random 256-bit
  secrets. Nobody can reverse one, but whoever can write the database's
  keys table can plant a key: guard write access to it.
- `SIGTERM` fails `/readyz`, stops both listeners from accepting and gives
  open requests `WA_SERVER_SHUTDOWN_GRACE`; every live event stream ends
  at once with `event: shutdown`, so its client resumes on another
  replica.

## Not yet

Re-sealing the webhook endpoints under a new data key (a previous key
keeps opening them), and an endpoint for several tenants (M2c's gaps);
disconnection by Meta's webhooks (and its
`number_disconnected`); clearing a lost credit share and the coexistence routes (M3b); zero-tap
authentication templates and template previews (M3g); the
Docker image, a Compose file, the documents route and the TypeScript
client (M4). The tenants' OTP settings have their own route
(`/v1/admin/tenants/{id}/otp`); per-tenant rate limits are not
settable: they are the deployment's, for every tenant. The design's TOML file of
non-secrets (`WA_SERVER_CONFIG`) is not read (set, it stops the start),
and OpenTelemetry export is not wired. The `/v1/version` revision reads
`unknown` unless the build sets `META_WHATSAPP_RS_REVISION`.
