# Design: a deployable meta-whatsapp-rs service (`meta-whatsapp-server`)

> **Milestones M1a, M1b and M1c are implemented** in
> `crates/meta-whatsapp-server` (M1a: the crate, configuration, listeners,
> storage, tenants and keys, the admin API with attach, unbind and vault
> rotation, the numbers routes, errors, operations, the committed OpenAPI
> document; M1b: messages, read receipts, media, templates, idempotency
> keys and rate limits; M1c: `POST /webhooks/meta` into the inbox and the
> event outbox, and `GET /v1/events`; [§9](#9-delivery-plan) says which
> acceptance tests they meet, [coverage.md](../coverage.md) row 33 what
> is missing); the rest is design. Written against `main` =
> bbf24a3 (2026-09-24), Graph API v25.0; the library changes it assumed have
> since landed on `main` (#4: `OtpConfig::namespace` required, the Intent
> API's result renamed `marketing::OnboardingRequested`; #5: Solution Partner
> onboarding with `onboard_with_approval`, `offboard` and the credit ledger).
> Owner choices are marked **Decision for owner (Dn)** and listed in
> [§10](#10-decisions-for-the-owner). From 2026-09-26 the owner delegates
> them to the coordinator, on condition that each stays swappable
> (§10 says which remain the owner's); the same day, parity with Zaileys
> on Meta's Cloud API ([parity.md](../parity.md)) lifted most of
> [§1](#1-goals-and-non-goals)'s non-goals, and [§8](#8-modular-architecture-and-client-sdks)
> became the modular architecture ([roadmap.md](../roadmap.md) has the
> steps).

The product decision: apps not written in Rust (Medusa, in TypeScript; the
CMS, any stack) use meta-whatsapp-rs through a **service deployed as a Docker image
and called over HTTP**. In short: a binary crate, `crates/meta-whatsapp-server`
(axum), built only on the `meta-whatsapp-rs` facade, productizes the runnable examples
and keeps their security rules; one multi-tenant deployment per Meta app;
a public listener serving only Meta's webhook and an internal one for the
API; Postgres for all state, with an event outbox feeding SSE, polling and
signed webhooks; `/v1` REST/JSON, a committed OpenAPI 3.1 spec, a generated
TypeScript client; five milestones (M5, added on 2026-09-26, brings the
modules parity requires). M1 is built as that one crate; §8 splits it
into a framework-free core, API adapters and swappable backends, with
CrateStack for the API and the models as the default.

## 1. Goals and non-goals

| Caller | Reaches | Credential ([§3.2](#32-credentials)) | Typical calls |
| --- | --- | --- | --- |
| Medusa backend | internal API | tenant key (the store is a tenant) | order templates, invoices, OTP login, events by webhook |
| CMS backend | internal API | platform key + `WA-Tenant` header | Embedded Signup, inbox, replies, merchants' templates, events |
| Merchant's browser | the CMS only (D3) | the CMS session | — |
| Meta | public listener, `/webhooks/meta` | verify token, `X-Hub-Signature-256` | subscription check, deliveries |
| Operator | `/v1/admin`, the CLI | admin key | tenants, keys, own numbers, vault rotation |

**Goals.** (1) What the three products need (messages, media, templates,
OTP, Embedded Signup, inbox, live events) over HTTP, with the library's
rules intact: E.164 with `+`, BSUID keys, no blind send retries, secrets
never logged, ownership checked before the vault is read, 401 before the
body on a bad signature. (2) Safe by default: loopback binds, refuses to
start on a blank secret, no CORS, no raw Graph passthrough. (3) Horizontal
scaling with one database as the only dependency (Postgres by default;
the backend is swappable, §8). (4) A versioned contract
generated clients can rely on.

**Non-goals.** A public end-user API. A UI. A consent registry (callers
own it, as with the library). Behaviour the library lacks: the service
adds none of its own, and what it needs goes into the library first
(token expiry signals, dead-lettering and a call-aware window are
decided that way: `OPEN_QUESTIONS.md` #8, #30, #32). A Graph proxy:
`MessageContent::Raw` and `Client::request_url` are not exposed.

**Lifted on 2026-09-26.** Parity with Zaileys on Meta's Cloud API
([parity.md](../parity.md)) requires what this section used to leave
out, so these are now goals: paced broadcast, scheduled messages, the
bot framework (commands, middleware, plugins, markdown replies), and
the modules that were to come on demand (analytics, commerce, groups,
calling, Flows, QR codes, In-App Signup, the Marketing Messages API,
block users, the partner APIs), with the send union's remaining types.
They live in library crates behind ports (`meta-whatsapp-bot`, D28;
jobs as a typed store on `KvStore`, D29), and the service exposes them
as thin routes over those modules (M5, [roadmap.md](../roadmap.md)).

**Decision for owner (D1): tenancy per deployment.** (a) One deployment per
tenant; (b) one multi-tenant deployment per Meta app; (c) one per product.
Every merchant onboarded by Embedded Signup delivers to the **app's single
callback URL**, and template and account webhooks ignore per-WABA overrides,
so (a) needs a router in front and cannot share one vault.
*Recommendation: (b)*, the store being one tenant when it shares the app;
separate deployments only for separate apps or environments.

## 2. Architecture

### 2.1 The crate

`crates/meta-whatsapp-server`, binary `meta-whatsapp-server`, `publish = false`, a workspace
member (so `just ci` covers it); as built through M1 (§8 splits it into
a core, API adapters and backends, which depend on the facade and on
each other, never the library on them). It depends only on the `meta-whatsapp-rs` facade
(`postgres`, `axum`, `typst`; axum and sqlx through its re-exports, OQ
#29): the API an outside integrator has, which proves the facade suffices.
[architecture.md](../architecture.md)'s "nothing depends on `meta-whatsapp-rs`"
becomes "no library crate depends on `meta-whatsapp-rs`; binaries may" (L6). New
dependencies (OpenAPI generation, `tower-http`, a Prometheus exporter, a
rate limiter, `clap`) must pass `cargo deny`.

```text
               public listener (:8080)                      internal listener (:8081)
Meta ─HTTPS─► ingress ─► GET|POST /webhooks/meta        Medusa / CMS ─► /v1/…            (API key)
                             │                           operator     ─► /v1/admin, /metrics, /readyz
                             ▼
     WebhookHandler: signature (401 before the body), 3 MiB, DedupGuard (Postgres KvStore)
                             │ each event, in order
                             ├─► InboxSink  ─► ConversationStore
                             └─► OutboxSink ─► wa_server_events ── NOTIFY ──┐
            ┌──────────────────────────────┬────────────────────────────────┘
            ▼                              ▼                                ▼
  SSE hub (each replica LISTENs)   GET /v1/events (poll)    webhooks-out workers (SKIP LOCKED)

API call ─► key → tenant ─► tenant owns number/WABA? ─► TokenVault ─► client.with_token ─► meta-whatsapp-client
```

Reused unchanged: `meta_whatsapp_rs::webhooks::router`, `DedupGuard`, `InboxSink`,
`Inbox`, `TokenVault`, `EmbeddedSignup`, `SignupSessions`, `OtpService`, the
endpoint modules, `meta_whatsapp_rs::typst::Renderer`, the Postgres stores. Not used:
`meta_whatsapp_rs::webhooks::sse` and `BroadcastSink` (the service needs resume and
cross-replica fan-out, [§4.5](#45-live-updates-sse)).

### 2.2 Configuration and storage

Environment variables, with a `<NAME>_FILE` variant for every secret; an
optional TOML file (`WA_SERVER_CONFIG`) for non-secrets. The list is in
[§7.2](#72-environment). **The service refuses to start** on a blank or
missing app secret or verify token (closing OQ #16 for itself), a missing
vault key or a pepper under 32 bytes with Postgres, memory storage outside
`WA_SERVER_ENV=development`, identical public and internal binds, a
plain-`http` `WA_GRAPH_ENDPOINT` outside development (every token
travels to it), a vault key id used twice (`WA_VAULT_KEY_ID`'s and
`WA_VAULT_PREVIOUS_KEYS`': records sealed with one of the two keys
could never be opened), or Solution Partner mode without its
credentials.

Postgres is required outside development (memory stores and a throwaway
vault key only there). Library tables keep their `wa_` prefix; the
service's use `wa_server_` and their own migration history:

| Table | Holds |
| --- | --- |
| `wa_server_tenants` | id, name, status, settings (OTP sender and template, limits) |
| `wa_server_api_keys` | key id, SHA-256 of the secret, kind, tenant(s), scopes, expiry, revocation, last use |
| `wa_server_wabas` | `waba_id` (unique) → tenant, credit allocation id (Solution Partner) |
| `wa_server_numbers` | `phone_number_id` (unique) → tenant, WABA, status (`connected`, `reconnect_required`, `disconnected`) |
| `wa_server_signup_attempts` | resumable attempts: tenant, WABA, failed step, session info (never the code or PIN) |
| `wa_server_idempotency` | [§5.4](#54-idempotency-keys) |
| `wa_server_events`, `wa_server_event_streams` | the outbox: tenant (null = operator-only), the sequence within its tenant's stream, id, number, WABA, type, JSON and its size; each stream's last sequence and how far it was purged |
| `wa_server_webhook_endpoints`, `…_deliveries` | [§4.4](#44-webhooks-out) |

Vault, OTP challenges, dedup claims and signup sessions stay in the
library's `KvStore` namespaces (`wa.token`, `wa.otp`, `wa.webhook.dedup`,
`wa.es.session`).

### 2.3 The event pipeline

- **Sinks run in sequence, inbox then outbox** (not concurrently as
  `FanoutSink` does): whoever sees an event can already read its history.
- **Routing is an allow-list.** An event naming a phone number goes to the
  tenant owning it; a WABA-level event (template, account, quality) to the
  tenant owning the WABA. `unknown`, `unparsed`, `partner_solution_updated`,
  the types not yet reviewed for tenants and events for unowned numbers
  or WABAs are operator-only rows (metric,
  log with size and digest), never shown to a tenant.
- Inserts are idempotent on the event's key (`WebhookEvent::dedup_key`;
  for the events the library gives none, the signed body and their place
  in it, for an hour). A sink error answers Meta 500 and the batch is
  redelivered (OQ #30 applies unchanged).
- The service adds `number_connected`, `number_disconnected` and
  `number_reconnect_required` (after a 190 on a merchant's call) events.
- `data` is the library's `WebhookEvent` JSON, pinned by snapshot tests
  over Meta's documented examples: a library change that alters it fails
  the server's tests and forces an API-version decision.

How M1c settled what the list above leaves open. Four of these choices
are coordinator's decisions of 2026-09-25, which M1c ships and which
stand under the owner's delegation of 2026-09-26; each is a place to
look in review:
the per-tenant sequences (D21), a deleted tenant's events deleted with
it (D22), the hour keyless events are deduplicated for (D23), and a
deleted tenant taken out of platform keys (D24); their rows in
[§10](#10-decisions-for-the-owner) say how far each is reversible. The
rest follows from the list above and decides nothing for the owner:

- **Types are an allow-list too.** A tenant receives only the event types
  the service reviewed and pinned (`TENANT_EVENT_TYPES` in
  `src/events.rs`); `unknown`, `unparsed`, `partner_solution_updated`, the
  types the library's webhook conformance sweep typed (PR #17:
  Conversation Routing's `standby_observed` and `thread_control_changed`,
  the Marketing Messages API's `user_action_reported`), which are not yet
  reviewed for tenants (M2 makes them tenant-visible: D25), and any type
  a later library adds are
  operator-only rows until the service lists them in `TENANT_EVENT_TYPES`
  (a test reads the library's `WebhookEvent::kind` and fails on an
  unclassified one; promoting a type is additive, the reverse is not).
- **Ownership is by the bindings, number first, since before the
  event.** An event naming a number (an untyped change: the number its
  raw `metadata` names, which the inbox reads a `history` change by) goes
  to the number's tenant only when the WABA it names is the one the
  number is bound under (stale bindings route to nobody); an event naming
  only a WABA goes to the WABA's tenant. And only when Meta dated the
  event (the message's, status's, call's own time, else the entry's) no
  earlier than the second that WABA's binding began: a WABA moved from
  one tenant to another does not bring the first one's retried events to
  the second. History and contact syncs (the past, on purpose) and errors
  (no date) route by the current binding. The inbox records an event only
  when a tenant owns it, so an unowned number's messages never wait in
  the inbox for whoever binds it later; the outbox records every event.
  The Postgres insert checks the routing again, in its own transaction:
  it keeps the tenant only while the binding the event was routed by (the
  number under the WABA the event names, else the WABA) still names that
  tenant and, for an event Meta dated, began no later than the event's
  second. It reads that binding under a `FOR KEY SHARE` lock, so an
  unbinding (and with it a deletion of the tenant) waits for the insert
  to commit. So an event routed just before its WABA moved to another
  tenant, or before its tenant was deleted and created again under the
  same id and bound again, is operator-only. An undated event (errors,
  syncs) has only the tenant's id to go by: in that last race it reaches
  the tenant created again. The memory outbox (development) does not
  check again.
- **Replays go to nobody, under the same id.** An event Meta dated before
  what the dedup lease remembers (7 days and an hour) is operator-only.
  An event's id is derived from it (HMAC-SHA256 of its outbox key, under
  a key derived from the first app secret, `WA_APP_SECRET`): recorded
  again, it keeps its id, and receivers deduplicate on it. Two limits:
  after `WA_APP_SECRET` is rotated, an event recorded again gets another
  id (which only matters for an event recorded again after its row was
  purged: before that, the stored row deduplicates it), and a keyless
  event recorded again after its hour (below) is a new occurrence with an
  id of its own. Undated events (errors, syncs) cannot be told from a
  replay.
- **Each tenant has its own sequence** (security review L2: global
  sequences showed every tenant the platform's volume and timing; D21, a
  coordinator's decision). Sequences increase, with gaps (a
  duplicate that lost a race draws one), and a tenant created again
  under a deleted one's id goes on after the deleted one's last. `sequence`, `after`,
  `next_after`, `410` and `422` are the tenant's; operator-only rows have
  a stream of their own. An insert draws its sequence from its stream's
  row of `wa_server_event_streams`, locked until it commits, so a
  stream's events commit in order and a poll that saw sequence `n` saw
  every event of the tenant before it; other tenants' inserts do not
  wait. A lock waited for over 2 s answers Meta `503`.
- **Polling.** `next_after` is the last event's sequence when more follow,
  else the tenant's newest sequence. `410 cursor_expired` is a cursor
  below what was purged; a cursor above the tenant's newest sequence (a
  restored database) is `422` on `after`. A page stops before the event
  that would take its `data` past 8 MiB (the first always comes), chosen
  from stored sizes before any data is read. Housekeeping purges a prefix
  of each stream past `WA_SERVER_OUTBOX_RETENTION` (7 days by default,
  D10), on one replica at a time, and the library's expired key/value
  rows with it.
- **Restoring the database** reuses sequences. A point-in-time restore
  rolls each stream's `last_sequence` back with it, and the events
  recorded after the restore draw the sequences again. The `422` on
  `after` catches only a consumer whose cursor is ahead of its stream
  when it polls; one that polls after new events have passed its cursor
  gets no error and misses the events drawn at the sequences it had
  already seen. So sequences are never reused *within one database's
  history*, and a restore is an operational event: the operator tells
  integrators to resynchronise and reset their cursors (poll without
  `after`), as after `410`. The service builds no mechanism for it.
- **Keyless events** (`error_reported`, `unparsed`: no dedup key in the
  library) are keyed by the SHA-256 of the signed body and their position
  among its keyless events, and deduplicated for an hour only
  (`KEYLESS_DEDUP_WINDOW`; D23, a coordinator's decision of 2026-09-25).
  Meta documents a retry of a failed delivery at once, then with
  decreasing frequency over 7 days, and asks receivers to deduplicate
  (`webhooks/create-webhook-endpoint`); that a redelivery carries the
  same bytes is this design's assumption, not Meta's statement (were the
  bytes to differ, its keyless events would be recorded again, as new
  ids). On that assumption a batch redelivered within the hour records
  them once; the library itself never deduplicates them, since the same
  error legitimately recurs and carries no date. An identical body after
  the hour is recorded again, as a new occurrence with its own id and
  sequence (the earlier row stays). The trade-off: an outage longer than
  an hour (the database answering `500`, say, while Meta keeps retrying)
  records the batch's keyless events twice, as new ids, against a
  recurring error recorded once for the whole retention. The hour is
  measured on the service's clock at both ends (a row holds its key until
  `dedup_until`). Keys also cover the business number.
- **Deleting a tenant** deletes its events (D22, a coordinator's decision
  touching D10: the outbox's foreign key cascades) and records its stream
  purged in the same transaction (a tenant created later with the same id
  polls none of them, its sequences go on after them, an old cursor is
  `410`), and takes the id out of every platform key's allowed tenants
  (D24, a coordinator's decision: a tenant created again under a
  recycled id must not inherit another's access). No route or command
  edits a key's allowed tenants, so a tenant created again needs a new
  platform key (a `*` key allows it at once).
- **The library's handler, the service's route.** `POST /webhooks/meta`
  calls the library's `WebhookHandler` (3 MiB, parsing, the dedup lease;
  the service checks the signature first) rather than mounting its
  `router`: the router discards the delivery report the service counts
  duplicates from. It answers as the router does, bare statuses: `401`,
  `413`, `503`, `500`, and `408` for a body slower than 15 s.
- **Intake is bounded.** A replica reads at most 64 deliveries at once
  (the next is `503` before its body is read: well-formed signatures cost
  nothing to forge, and bodies are buffered until checked) and records at
  most 4 at once, below its pool of 10 connections, so API key lookups
  always find one (`503` after 10 s without a turn). The public listener
  takes 256 connections. Refused deliveries write at most one warning a
  minute per reason; the metric counts every one. Not done: one
  transaction per delivery instead of per event, which the library's
  per-event lease does not allow without re-implementing its handler. The
  residual risk: a large delivery holds its recording turn through one
  commit per event, each of which may wait up to 2 s for a lock, so a
  few such deliveries can hold every turn for many seconds (other
  deliveries answer `503` after 10 s without a turn, and Meta retries);
  the request deadline (55 s) and the dedup lease (60 s) bound it, and a
  delivery cut there is redelivered, its recorded events counted as
  duplicates.

### 2.4 Several replicas

| Concern | Behaviour |
| --- | --- |
| vault, OTP, signup sessions, dedup, inbox, outbox, keys | shared in Postgres; expiry by the database clock (NTP) |
| dedup lease (60 s) | a retry meeting a live lease on another replica gets 503 and Meta returns; a crashed replica's lease expires; the sink path (two inserts) stays far below 60 s |
| outbox inserts | each tenant's in turn (its stream's row, locked to the commit); tenants in parallel; at most 4 deliveries recording per replica |
| SSE | per replica, fed from the outbox by `LISTEN/NOTIFY`; `Last-Event-ID` resumes on any replica |
| webhooks-out | workers on every replica claim rows with `FOR UPDATE SKIP LOCKED` and a lease; no leader |
| rate limits | token buckets per replica (limit ÷ replicas); a shared limiter only if needed |
| API key cache | none in v1: every request reads its key, so a revocation or suspension holds on the next request, on every replica (a cache of at most 30 s, purged by a revocation `NOTIFY`, if the key reads ever cost too much) |
| housekeeping (`purge_expired`, outbox and idempotency purges) | any replica, under an advisory lock |
| migrations | at start, under the library's lock and a service advisory lock; expand-then-contract so rolling deploys can mix versions. One exception predates the service: the library's migration 3 (lossless message content) converts in one step and needs older writers stopped first, then `migrate` run once from a one-off job (`docs/guides/production.md`); it runs before the service's first deploy, so no service rollout crosses it |

## 3. Tenancy and authentication

### 3.1 The model

```text
tenant (a merchant, or the platform's store) 1──n WABA 1──n phone number
  ├─ API keys, webhook endpoints, OTP settings     └─ business token in TokenVault, by WABA
  └─ id: the integrator's (e.g. the CMS merchant id), 1–64 of [A-Za-z0-9._:-],
     immutable (it is the OTP namespace: changing it invalidates codes in flight)
```

### 3.2 Credentials

| Kind | Sent as | Acts as | For |
| --- | --- | --- | --- |
| tenant key | `Authorization: Bearer wak_…` | its tenant | Medusa; any backend that is one tenant |
| platform key | the same, plus `WA-Tenant: <id>` | the named tenant, if among its allowed ones (`*` or a list) | the CMS backend, for its merchants |
| admin key | the same | `/v1/admin` only | the operator |

Keys carry scopes (`send`, `media`, `templates`, `inbox`, `events`,
`webhooks`, `signup`, `otp`, `numbers`), look like `wak_<key id>_<32 random
bytes, base62>` (the prefix lets secret scanners find leaks) and are stored
as the key id plus SHA-256 of the secret, compared in constant time (the
examples' scheme; random 256-bit secrets need no slow hash). The digest is
not peppered: whoever can write the keys table can plant a key, a limit
accepted with the database's own access control. Shown once;
several active per tenant; rotate by create, deploy, revoke. The first admin
key comes from the CLI (`meta-whatsapp-server admin create-admin-key`), not the
environment.

**Decision for owner (D2): credential model.** (a) Tenant keys only: the
CMS keeps one key per merchant, minted at onboarding; (b) platform key plus
header only; (c) both. A platform key is as powerful as every tenant it may
name, but the per-tenant checks still stop a CMS bug from crossing tenants.
*Recommendation: (c).*

### 3.3 Authorization order

Every tenant route, as middleware plus typed extractors (`OwnedNumber`,
`OwnedWaba`) that are the only way a handler obtains a token:

1. Authenticate the key before reading the body, else `401`.
2. Resolve the tenant (the key's, or `WA-Tenant` within the platform key's
   set), else 403 (`tenant_suspended` for a suspended one).
3. Check the scope, else `403 forbidden`.
4. **Ownership**: the path's `{pn}` or `{waba_id}` must be bound to that
   tenant, else `404 not_found`, the answer for a number that does not
   exist, so tenants cannot probe each other.
5. Only now read the vault: no entry is `409 number_not_connected`; an
   expired token, or a number marked after a 190, `409
   reconnect_required`. Then `client.with_token(token)`.

### 3.4 How numbers get bound

- **Embedded Signup** ([§4.6](#46-embedded-signup)): when `onboard`
  succeeds, or fails at a resumable step with the token stored, the
  verified `waba_id` and every number in `Onboarded::phone_number_ids` are
  bound to the calling tenant, in one transaction with the D4 check.
- **Admin attach** (the platform's own WABA): the admin gives the WABA id
  and a system user token; the service lists the WABA's numbers from Meta
  with it before `TokenVault::store` (which trusts its input). Ids are
  never bound on a caller's word. It binds (D4, atomically) **before** it
  stores the token: storing first would overwrite the owner's token
  before D4 refused. Then it subscribes the app to the WABA's webhooks
  (`POST /{waba_id}/subscribed_apps`, idempotent), as onboarding does
  after storing: disconnecting unsubscribes, so attaching must subscribe.
  A refused subscription leaves the WABA attached, and the answer says
  so (`step: subscribe_app`, `resumable: true`): repeating the attach
  finishes it. A `190` there is a stored token's: `409
  reconnect_required`, the numbers marked so, not the `422` on `token`
  of a token refused before anything was bound.
- **Disconnect**: `DELETE /v1/wabas/{waba_id}` unsubscribes the app with
  the merchant's token, deletes the vault entry and the bindings. An
  `account_updated` with `PARTNER_APP_UNINSTALLED` or `ACCOUNT_DELETED`
  does the same and emits `number_disconnected`. A tenant whose token no
  longer works (`409 number_not_connected` or `reconnect_required`)
  cannot disconnect: the operator's path is the admin unbind.
- **Admin unbind** (`DELETE /v1/admin/wabas/{waba_id}/binding`, D4): with
  the stored token, if usable, unsubscribe the app, at best (Meta refusing
  does not stop it); then delete the vault entry, then the bindings. A
  token no tenant can reach serves nothing and widens what a database and
  vault key compromise exposes.

**Decision for owner (D3): browser access.** (a) Never browser-facing: the
CMS relays signup calls and live events (an SSE relay per open inbox, or
webhooks-out into its own realtime channel); (b) short-lived number-scoped
stream tokens plus a CORS allow-list, so browsers open SSE directly: one
hop fewer, but the service is exposed beyond `/webhooks/meta`.
*Recommendation: (a) in v1*; (b) can be added later without breaking
anything.

**Decision for owner (D4): one WABA, several tenants (OQ #6).** (a) Refuse
(`409 waba_owned_by_another_tenant`), checked on the claimed id before
`redeem` and on the verified id after `onboard` (by then the vault holds the
second merchant's token, also valid for that WABA; the binding stays);
(b) move the binding to the newest tenant (today's "last onboarding
wins"); (c) share it (one vault token per WABA: both act with the last one
stored). *Recommendation: (a)*, with an admin unbind.

### Coexistence (merchants keeping the WhatsApp Business app)

A merchant may onboard the number they already use in the WhatsApp Business
app (Embedded Signup `featureType: whatsapp_business_app_onboarding`). It is
the same tenant and the same inbox as any other onboarding; three streams
share one conversation per customer:

| Stream | Source | Where it lands |
| --- | --- | --- |
| customer → merchant | `messages` webhooks | inbox, inbound |
| merchant's replies from the phone app | `smb_message_echoes` webhooks | inbox, outbound (never opens or extends the 24 h window) |
| merchant's replies from the platform | `POST …/messages` / inbox reply (API, billed, window-bound) | inbox, outbound |
| contacts and past chats | one-time `smb_app_data` sync → `smb_app_state_sync`, `history` | inbox (history import), contact list |

Rules the service implements:

- **Sync automatically (D7).** Right after a coexistence onboarding the
  service triggers the contacts sync, then the history sync — Meta allows each
  once, within 24 hours, after which only offboarding and a new signup
  recovers. A failed trigger is retried within the window and surfaced to the
  operator; a declined history share (error `2593109`) is recorded, not
  retried.
- **Subscribe the extra webhook fields** `history`, `smb_app_state_sync`,
  `smb_message_echoes` on the Meta app (a startup check warns if they are
  missing).
- **Offboarding is the merchant's.** They disconnect from the phone app
  (Settings → Account → Business Platform); the service marks the number
  disconnected on the `account_update` offboarded event, stops sending, keeps
  history, and restores it on the reconnected event.
- **Tell merchants what changes**: fixed 20 messages/s; disappearing
  messages, view-once, live location and broadcast lists turn off in the app;
  companion devices are unlinked (re-link supported ones); groups, calls,
  catalog and profile editing are not available through the API for that
  number; phone-app messages stay free, API messages are billed.

## 4. The HTTP API

### 4.1 Conventions

- JSON under `/v1`, snake_case; Meta ids as strings (they overflow JS
  numbers); RFC 3339 UTC times; lists take `?limit=` (≤ 100) and an opaque
  `cursor` and answer `{"data", "next_cursor"}` (inbox cursors wrap the
  store's exclusive ones: no repeats, no gaps), except `GET /v1/events`,
  which takes `after` and answers `next_after`, a sequence of the
  tenant's ([§2.3](#23-the-event-pipeline)).
- **Phone numbers are E.164 with `+` everywhere**; digits-only is `422` on
  the field (Meta would read it as local to the sender's country).
  Recipients: `{"phone"}`, `{"user_id"}` (BSUID), both, or `{"group_id"}`.
- `X-Request-Id` echoed and logged; `Idempotency-Key` on sends
  ([§5.4](#54-idempotency-keys)). Body limits: 64 KiB, 16 KiB for signup
  completion, 100 MiB for uploads. Request DTOs reject unknown fields;
  responses may gain fields, which clients ignore.
- `{pn}` is a phone number id; `{contact}` (BSUID, `wa_id` or group id) and
  `{message_id}` are percent-encoded. Every tenant route may also answer
  `401`, `403`, `404` (not owned), `409 number_not_connected` /
  `reconnect_required`, `422 invalid_request`, `429 too_many_requests`.

### 4.2 Resources

**Numbers and profile** (scope `numbers`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/wabas`, `GET /v1/numbers` | the tenant's WABAs; its numbers with connection status | → `{data: [...]}` |
| `GET /v1/numbers/{pn}` | live details: display number, verified name, quality, name status, throughput | → object |
| `GET`, `PATCH /v1/numbers/{pn}/profile` | business profile: about, address, description, email, websites, vertical | partial profile → profile |
| `DELETE /v1/wabas/{waba_id}` | disconnect ([§3.4](#34-how-numbers-get-bound)); if unsubscribing fails, nothing is deleted and the answer is Meta's error's code and status ([§5.2](#52-codes-and-statuses): `502` when Meta fails, `403` or `409` when it refuses, `504` on a timeout); a tenant whose token no longer works asks the operator for the admin unbind | → 204 |

**Messages and media** (scopes `send`, `media`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/numbers/{pn}/messages` | send free-form (Meta enforces the window), template or reaction ([§4.3](#43-message-content)) | `{to, type, <type>: {…}, reply_to?, callback_data?}` → `202 {message_id, contacts}` | 409 `customer_service_window_closed`, `marketing_opted_out`; 422 `template_*`; 429; 502/504 with `may_have_been_sent` |
| `POST /v1/numbers/{pn}/messages/{message_id}/read` | blue ticks; optional typing indicator (never replayed) | `{typing_indicator?}` → 204 | |
| `POST /v1/numbers/{pn}/media` | upload, type and size checked first | multipart `file`, `type` → `201 {media_id}` | 422 `type`, 413 |
| `GET /v1/numbers/{pn}/media/{media_id}` | download, SHA-256 verified before the first byte; `?max_bytes=` (default and cap 16 MiB), larger with `?stream=true` | → bytes, `X-WA-SHA256` | 413 `media_too_large`, 502 `integrity`; *as built in M1b*: 422 `media_id` (not digits), 404 for a media id not the number's (asked with `phone_number_id={pn}`; Meta's refusal, 4xx, answered like a missing id) |
| `DELETE /v1/numbers/{pn}/media/{media_id}` | delete | → 204 | *as built in M1b*: looked up first, with `phone_number_id={pn}` (`DELETE /{id}` deletes whatever node an id names): 422 `media_id`, 404 for a media id not the number's or a node that is not that media, nothing deleted |
| `POST /v1/numbers/{pn}/documents` (M4) | render `invoice`, `receipt` or `voucher` from its JSON input (`date` required: the renderer has no clock), upload | `{template, input, date, format}` → `201 {media_id, filename, mime_type}` | 422 on the input |

**Templates** (scope `templates`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `GET /v1/wabas/{waba_id}/templates` | list, `?status=&name=&cursor=`, cached 60 s per WABA (Meta allows 200 management calls an hour per WABA) | → page of `{id, name, language, status, category, components}` | |
| `GET /v1/wabas/{waba_id}/templates/{id}` | one | → template | *as built in M1b*: 422 `id` (not digits); the id's name read, then the id looked for in the WABA's own list of that name (5 pages of 100 at most): 404 for another WABA's template, or one past those pages |
| `POST /v1/wabas/{waba_id}/templates` | create from Meta's JSON shape (`TemplateDefinition` deserializes it), validated locally | → `201 {id, status, category}` | 422 `template_rejected`; 409 `template_limit_reached`; *as built in M1b*: 422 `invalid_request` on a key `TemplateDefinition` would not send (never dropped; the shapes it cannot carry are listed in the guide's Templates section) |
| `POST /v1/wabas/{waba_id}/templates/authentication` (M3) | copy-code, one-tap or zero-tap, several languages | → 201 | |
| `DELETE /v1/wabas/{waba_id}/templates?name=[&id=]` | every language of a name, or one | → 204 | *as built in M1b*: with an id, looked for in the WABA's own list of that name first: 404 when it is not there, nothing deleted |

Review results arrive as `template_status_updated` events.

**Inbox** (scope `inbox`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/numbers/{pn}/conversations` | newest activity first | → page of `{contact, last_message_at, last_inbound_at, last_text, unread, window}` |
| `GET /v1/numbers/{pn}/conversations/{contact}/messages` | history, newest first | → page of `{id, direction, kind, text, payload, status, timestamp, status_at, error}` |
| `GET /v1/numbers/{pn}/conversations/{contact}/window` | the 24-hour window (calls unseen: OQ #32) | → `{open, closes_at}` (`null`: templates only) |
| `POST /v1/numbers/{pn}/conversations/{contact}/messages` | reply as the merchant to the conversation's contact; free-form refused locally outside the window (`409`, nothing sent), templates exempt; recorded `accepted` | content → `202 {message_id}` |
| `POST /v1/numbers/{pn}/conversations/{contact}/read` | reset unread; `notify_customer: true` also marks the latest inbound message read on Meta | → 204 |

**Events and webhook endpoints** (scopes `events`, `webhooks`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/events` | poll the tenant's events after a sequence of its own, `?after=&types=&phone_number_id=&limit=` (`types`: `KnownEventType`s, comma-separated or repeated; `limit` ≤ 100, 50 by default); a page stops before 8 MiB of `data` (the first event always comes); `410 cursor_expired` below what was purged (retention, or a deleted tenant of the same id); `422` on `after` past the tenant's newest sequence, on `types` for a type a tenant never receives | → `{data: [envelope], next_after}` |
| `GET /v1/events/{id}` | one event in full (for truncated deliveries) | → envelope |
| `GET /v1/events/stream`, `GET /v1/numbers/{pn}/events/stream` | SSE for the tenant, or one number ([§4.5](#45-live-updates-sse)); `429 too_many_streams` | `text/event-stream` |
| `POST /v1/webhook-endpoints` | forward events to a URL ([§4.4](#44-webhooks-out)); `422 url` outside the allowed destinations | `{url, types, tenants?}` → `201 {id, secret}` (shown once) |
| `GET`, `PATCH`, `DELETE /v1/webhook-endpoints[/{id}]` | list, read, change URL, types or enabled, delete | |
| `POST /v1/webhook-endpoints/{id}/rotate-secret`, `…/test` | new secret (the old one also signs for 24 h); send a `ping` | → `{secret}`; the delivery |
| `GET /v1/webhook-endpoints/{id}/deliveries`, `POST …/{delivery_id}/retry` | attempts (status, duration, next attempt; no bodies); redeliver a failed one | page; 202 |

`tenants` (a list or `"*"`) is for platform keys; a tenant key's endpoint
receives its own tenant's events.

**Embedded Signup and OTP** (scopes `signup`, `otp`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/signup/sessions` | bind an attempt (15 min) to the tenant; what `FB.init` and `FB.login` need | `{coexistence?}` → `{state, expires_at, app_id, config_id, graph_api_version, launch_options}` | |
| `POST /v1/signup/complete` | redeem for this tenant, then onboard | `{state, code, event, pin?}` → `{status: connected \| cancelled, waba_id, phone_number_ids, steps_completed, needs_coexistence_sync, credit_allocation_id?}` | 403 `stale_attempt`; 409 `waba_owned_by_another_tenant`; 502 `onboarding_failed` (`step`, `resumable`) |
| `GET /v1/signup/attempts` | the tenant's resumable attempts | → `{data: [{waba_id, failed_step, updated_at}]}` | |
| `POST /v1/signup/resume` | rerun the steps after the stored token | `{waba_id, pin?}` → like `complete` | 404 `nothing_to_resume`; 502 `onboarding_failed` |
| `POST /v1/numbers/{pn}/coexistence-sync` | start the contacts and history sync, once, within 24 h (D7) | `{types}` → 202 | 409 `sync_not_allowed` |
| `POST /v1/otp/issue` | send a code with the tenant's authentication template | `{phone, purpose?}` → `{status: sent, challenge_id, expires_at}` or `{status: cooling_down \| rate_limited, retry_after_seconds}` | 422 `phone`, `recipient_not_supported`; 502/504 with `may_have_been_sent: true` |
| `POST /v1/otp/verify` | check the typed code; always counts as an attempt | `{phone, purpose?, code}` → `{result: verified \| invalid \| expired \| too_many_attempts \| not_found, attempts_left?}` | 422 |

The signup state travels in bodies only (URLs reach access logs). OTP
outcomes are `200` with a status, as the library returns them as `Ok`:
a caller's generic error handling can never swallow `invalid`.

**Admin and operations**

| Method and path | Does |
| --- | --- |
| `POST`, `GET`, `PATCH`, `DELETE /v1/admin/tenants[/{id}]` | create, list, suspend, configure (OTP sender and template, limits), delete (disconnects every WABA) |
| `…/tenants/{id}/keys[/{key_id}]`, `/v1/admin/platform-keys[/{key_id}]` | mint, list, revoke keys; platform keys with their allowed tenants |
| `POST /v1/admin/tenants/{id}/wabas`; `GET /v1/admin/wabas/{waba_id}`; `DELETE /v1/admin/wabas/{waba_id}/binding` | attach an own WABA, verified with Meta and subscribed; which tenant holds a WABA, and its numbers; unbind (D4: token deleted too, [§3.4](#34-how-numbers-get-bound)) |
| `POST /v1/admin/vault/rotate` | re-encrypt every WABA's token under the active key, walking `wa_server_wabas` (the vault cannot list itself), within the request deadline: a walk cut there answers `504 timeout` and is repeated (idempotent), and `meta-whatsapp-server vault rotate` has no deadline. It walks bound WABAs only, which holds every vault record in M1a; M3 keeps records past a binding (credit ledgers of offboarded WABAs, revocation markers), and the walk must cover those too (`TokenVault::rotate` on each such WABA, `rotate_business` on each marker) before an operator may drop an old key |
| `GET /livez`, `/readyz`, `/metrics`, `/v1/openapi.json`, `/v1/version` | internal listener, no key; `version` reports server, meta-whatsapp-rs revision, Graph and API versions; `/v1/openapi.json` is served by the axum API adapter, while the CrateStack adapter's contract is its `.cstack` schema ([§8](#8-modular-architecture-and-client-sdks)) |

The public listener serves `GET|POST /webhooks/meta` and `GET /livez`,
nothing else.

### 4.3 Message content

A union owned by the service, named after Meta's Cloud API message object
(so Meta's pages describe it) and mapped onto `meta-whatsapp-client` builders, which
`OutboundMessage::validate` checks before any request (`OutboundMessage` is
serialize-only, so no library change). Types: `text`; `image`, `video`,
`audio`, `document`, `sticker` by `id` or `https://` `link` (Meta fetches
it, never the service); `location`, `contacts`, `reaction`; `template`
(Meta's object, deserialized into `TemplateMessage`); `interactive`
`button`, `list`, `cta_url`. Anything else, `MessageContent::Raw` and Direct
Send included, is `422 unsupported_message_type`. A streamed download
**aborts the connection** on a digest mismatch, so unverified bytes never
arrive as a complete body.

### 4.4 Webhooks-out

For backends such as Medusa that prefer not to hold a stream open.

```json
{"id": "evt_3f9c0a6e1b2d4c58a7e9f0b1c2d3e4f5", "sequence": 18342, "type": "message_received", "api_version": "v1",
 "tenant_id": "merchant-42", "phone_number_id": "106540352242922", "waba_id": "102290129340398",
 "received_at": "2026-09-24T10:00:01Z", "truncated": false,
 "data": {"event": "message_received", "…": "the meta-whatsapp-rs WebhookEvent JSON"}}
```

- **Signature: [Standard Webhooks](https://www.standardwebhooks.com/)**
  (`webhook-id`, `webhook-timestamp`, `webhook-signature: v1,<base64
  HMAC-SHA256(secret, "{id}.{timestamp}.{body}")>`, secrets `whsec_…`), with
  verifiers for TypeScript and most stacks. Receivers reject timestamps over
  5 minutes off, which Meta's signature cannot offer. Rotation sends both
  signatures for 24 h. Secrets are stored encrypted under
  `WA_SERVER_DATA_KEY` (signing needs them).
- **Delivery**: `POST`, 10 s timeout, 2xx is success, no redirects, at most
  1 KiB of the answer read. **At least once**: backoff with jitter from
  10 s, at most 1 h a step, for 72 h, then `failed` (retryable through the
  API); an endpoint failing the whole window is disabled. Unordered:
  receivers deduplicate on `id` (the same event keeps its id, except
  after an app secret rotation; an error or a body that is not a webhook
  seen again after an hour is a new occurrence, with a new id) and order
  on `sequence`, which is each tenant's own ([§2.3](#23-the-event-pipeline)).
- **Destinations** only within `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`
  (hosts, CIDRs), HTTPS outside development; the address is resolved,
  checked and pinned per attempt (no DNS rebinding). Private ranges pass
  only when listed (Medusa in the same cluster usually is).
- `data` over 256 KiB (a history sync) is left out (`truncated: true`);
  `GET /v1/events/{id}` returns it.

### 4.5 Live updates (SSE)

- Frames `event: whatsapp`, `id: <sequence>` (the tenant's own,
  [§2.3](#23-the-event-pipeline)), `data: <envelope>`; a
  keepalive comment every 15 s; `event: lagged` when a slow client lost
  events (reload history, or poll from its last sequence). Reconnecting with
  `Last-Event-ID` replays from the outbox, on any replica, within retention.
- Each replica holds one `LISTEN` connection; `NOTIFY` carries only the
  tenant's id and the event's sequence in that tenant's stream (sequences
  are per tenant, [§2.3](#23-the-event-pipeline); well under Postgres's
  8,000-byte payload limit). New rows are
  read once and shared (`Arc`) into channels per (tenant, number), created
  with their first subscriber: a stream never receives, or clones, another
  tenant's events, so OQ #31's cost does not arise.
- Limits: 20 streams per tenant, 1,000 per replica. Operator-only rows are
  never streamed. `EventSource` cannot send `Authorization`: browsers go
  through the CMS (D3).

### 4.6 Embedded Signup

```text
merchant's browser       CMS backend                     meta-whatsapp-server                Meta
"Connect" ────────────► POST /connect ─────────────────► POST /v1/signup/sessions (WA-Tenant: m42)
          ◄─ launch data ◄────────────────────────────── {state, app_id, config_id, launch_options}
FB.login(…) ──────────────────────────────────────────────────────────────────────────────► popup
          ◄─ code (30 s, single use) + WA_EMBEDDED_SIGNUP event ◄─────────────────────────────┘
POST /connect/callback ► POST /v1/signup/complete ─────► redeem(state, m42), onboard ─────────► exchange, verify, store,
  {state, code, event, pin}                              bind WABA + numbers to m42             subscribe, [credit line], register
```

Kept from the `embedded_signup` example: the tenant comes from the
credential, never the body (`SignupSessions::redeem` does not burn the state
on a mismatch); inputs are parsed before `redeem`; `onboard` is never
retried; a failure after the token is stored saves a resumable attempt
(session info only; `resume` uses a placeholder code until L4); errors carry
step, kind and `resumable`, never Meta's text; the page gets only the app
id, configuration id, Graph API version and `LaunchOptions`.
The service launches **Embedded Signup v4 only**: Meta retires v2 and v3,
including their public previews, on 2026-10-15
(`embedded-signup/onboarding-customers-as-a-solution-partner`, banner).

| | Tech Provider (`WA_ONBOARDING_MODE=tech_provider`) | Solution Partner (`solution_partner`) |
| --- | --- | --- |
| Steps | exchange code, `debug_token`, verify assets, store token, subscribe app, register number | the same, plus, between `subscribe_app` and `register_phone` (Meta's documented order): adding the partner's system user to the merchant's WABA (`POST /{waba_id}/assigned_users`, a prerequisite of the one-call method) and sharing the partner's extended credit line (`POST /{extended_credit_line_id}/whatsapp_credit_sharing_and_attach` with `waba_id`, `waba_currency`) with the **partner's system user token**, never the merchant's; the returned allocation config id is stored on the WABA. Meta's newer two-call method (share with the system token, attach with the merchant's token) is supported by the library as an option |
| Who pays Meta | the merchant, after adding a payment method in WhatsApp Manager | the partner's credit line |
| Extra settings | — | `WA_PARTNER_SYSTEM_TOKEN`, `WA_PARTNER_SYSTEM_USER_ID`, `WA_CREDIT_LINE_ID`, `WA_WABA_CURRENCY` (default; one of AUD, EUR, GBP, IDR, INR, USD; a signup may override it). A credit line cannot be changed once attached to a WABA |
| Library | implemented | in flight (L3): the step's name, resume behaviour and error kinds come from it |

The mode is per deployment (per Meta app), not per tenant.

**Decision for owner (D5): onboarding mode in production (OQ #3).** (a) Tech
Provider: merchants pay Meta; (b) Solution Partner: the platform pays
through its credit line and bills merchants (needs partner status and a
default currency). Both are supported from M3. *Recommendation: (a) first*,
switching by configuration once partner status and billing exist.

**Decision for owner (D6): two-step PIN (OQ #4).** (a) Typed by the merchant
per attempt, never stored (today's behaviour); (b) generated and stored
encrypted by the service. *Recommendation: (a)*: one leak must not expose
every number's PIN. *Decided on 2026-09-26 under the owner's
delegation: (a) ([§10](#10-decisions-for-the-owner)).*

**Decision for owner (D7): coexistence sync (OQ #7)**, due within 24 h of
onboarding a WhatsApp Business app number. (a) An explicit endpoint; (b)
automatic after onboarding; (c) the endpoint plus a per-tenant setting for
(b). *Recommendation: (c)*, automatic by default: 24 h is easy to miss. The
inbox still records neither echoes nor history (OQ #17).

### 4.7 OTP

- One `OtpService` per (tenant, sending number), cached, with
  `OtpConfig::namespace` = the tenant id: codes, cooldowns and issue limits
  never cross tenants, even on a shared number.
- Per-tenant settings (admin API): sending number, approved authentication
  template and language, `OtpConfig` (TTL = the template's expiry; defaults
  per OQ #13). `phone` is strict E.164; a BSUID alone is refused (131062).
- The code is in no response, log or error; OTP routes drop Meta's text
  and `details` (a template error could quote a parameter: the code).
  Per-IP throttling stays the caller's (the service sees only the backend).

**Decision for owner (D8): which number sends a tenant's codes.** (a) One
platform number for all (the user sees the platform's name); (b) each
merchant's own number (the user sees the merchant; each needs an approved
authentication template); (c) per tenant, defaulting to (a).
*Recommendation: (c).* *Decided on 2026-09-26 under the owner's
delegation: (c) ([§10](#10-decisions-for-the-owner)).*

## 5. Error model

### 5.1 Body

```json
{"error": {"code": "customer_service_window_closed",
  "message": "More than 24 hours since the customer's last message: send a template.",
  "retryable": false, "may_have_been_sent": false, "field": null, "step": null, "resumable": null,
  "graph": {"code": 131047, "subcode": null, "fbtrace_id": "AbCdEf", "details": "…"},
  "request_id": "req_01J8Z6Q4M3"}}
```

`code` is stable (codes only grow within `v1`; an unknown one is handled
by its status class). `message` is the service's sentence for the code,
never Meta's message or an input value. `graph` appears when Meta answered
with an error; `details` is Meta's text, not the service's: dropped on OTP
and signup routes, and elsewhere opt-in per route, without control or
format characters or line separators (they could forge a log line), at
most 512 characters.

### 5.2 Codes and statuses

Meta-derived codes are the snake_case `ErrorKind` name
(`ErrorKind::TemplateParameterMismatch` → `template_parameter_mismatch`),
defined once in the library by `ErrorKind::as_str()` and pinned by a test
(L1): `ErrorKind` is non-exhaustive, so a service-side mapping would turn
each new kind into `unknown` silently.

| HTTP | Codes | Meaning |
| --- | --- | --- |
| 422 | `invalid_request` (local validation, with `field`), `invalid_parameter`, `unsupported_message_type`, `recipient_not_supported`, `undeliverable`, `template_parameter_mismatch`, `template_not_found`, `template_text_too_long`, `template_policy_violation`, `template_rejected`, `idempotency_key_reused` | fix the request; nothing was sent |
| 409 | `customer_service_window_closed`, `marketing_opted_out`, `blocked_by_business`, `experiment_holdout`, `template_paused`, `template_disabled`, `template_syncing`, `template_unavailable`, `template_limit_reached`, `flow_unavailable`, `registration`, `two_step_verification`, `sync_not_allowed`, `duplicate_onboarding`, `number_not_connected`, `reconnect_required` (also Meta's `authentication` on a stored token: a merchant's, or an attached WABA's once stored), `waba_owned_by_another_tenant` (also a phone number another tenant holds), `tenant_exists`, `idempotency_in_progress`, `outcome_unknown` | a state must change first |
| 403 | `permission`, `account_restricted`, `country_restricted`, `payment`, `feature_not_available`, `marketing_not_allowed`; `forbidden`, `tenant_suspended`, `stale_attempt` | not allowed, by Meta or the service |
| 429 | `rate_limited`, `pair_rate_limited`, `spam_rate_limited`, `ecosystem_engagement_limit`, `classification_limit_reached`, `too_many_requests`, `too_many_streams` | `Retry-After` when known; `retryable` says whether waiting helps (false for 131048, 131049) |
| 404, 410, 413 | `not_found`, `nothing_to_resume`; `cursor_expired`; `payload_too_large`, `media_too_large` | |
| 502 | `service_unavailable`, `unknown`, `upstream` (non-Graph answer), `integrity`, `media_download_failed`, `media_upload_failed`, `onboarding_failed` | Meta or the network failed |
| 504 | `timeout` | no answer in time: a send may have gone out |
| 503, 500 | `storage_unavailable`, `shutting_down`; `internal` (configuration, an undecryptable vault record) | |
| 401, 405 | `unauthenticated`; `method_not_allowed` | no valid key (step 1 of [§3.3](#33-authorization-order), before anything else); the path exists, not with this method |

### 5.3 Was it sent?

Every error from a sending route carries `may_have_been_sent`
(`Error::may_have_been_sent`) and `retryable` (`Error::is_retryable`). The
rule the docs and the SDK teach: resend only when `may_have_been_sent` is
`false`; otherwise reconcile through `status_updated` events, whose
`data.status.biz_opaque_callback_data` is your `callback_data` (Meta's
name for it; after a `504` it is all you have) and `data.status.id` the
message id a `202` answers, or repeat with the same `Idempotency-Key`,
which never sends twice. The service adds no send retries of its own.

### 5.4 Idempotency keys

- `Idempotency-Key` on sends, inbox replies, OTP issue, uploads and
  template creation, scoped to the tenant; stored with a hash of (method,
  route, body), the state and the final answer.
- A request inserts the key `in_progress` (unique), runs, stores the answer.
  A repeat with the same body gets it back (`Idempotent-Replayed: true`); a
  different body is `422 idempotency_key_reused`; a concurrent one `409
  idempotency_in_progress`.
- **Released when the outcome proves nothing was sent**
  (`may_have_been_sent` is false: validation, 4xx, throttling), so the
  caller may retry with it; otherwise kept, success or not: a key never
  sends twice. After a crash the key reads as unknown once its lease (twice
  the Graph timeout) ends: repeats get `409 outcome_unknown` with
  `may_have_been_sent: true`, never a new send.
- Kept 24 h (`WA_SERVER_IDEMPOTENCY_TTL`). Callers derive keys from their
  own records (`order:1234:shipped`) and set `callback_data` to the same.

## 6. Security

| Threat | Control |
| --- | --- |
| Forged Meta deliveries | signature over raw bytes with any of N app secrets; missing or malformed header `401` before the body is read; 3 MiB; at most 64 read at once per replica (`503` before the body), 15 s to send one (`408`), refusals logged once a minute; the public listener serves nothing else; Meta's IP ranges or mTLS at the ingress (below) |
| Replayed Meta bodies | dedup for 7 days and an hour (errors and bodies that are not webhooks: an hour, D23); events Meta dated before that go to nobody; an event keeps its id when recorded again, until `WA_APP_SECRET` is rotated; bodies never logged |
| A WABA or number moving between tenants | events Meta dated before the binding began go to nobody, inbox included; the previous tenant's inbox rows stay under the number (M2's inbox reads must filter by binding epoch, or D10 decides a purge on unbind) |
| A tenant reading or sending as another | ownership before the vault ([§3.3](#33-authorization-order)); foreign numbers are `404`; extractors are the only path to a token. *As built in M1b*: one token may reach several tenants' WABAs (the platform's system user token attached to each), so an id in a path is checked to be the path's number's or WABA's own: media with Meta's `phone_number_id`, templates through the WABA's own list; another's is `404` like a missing one |
| A stolen platform key | limited to its tenants and scopes; internal network only; revocation effective across replicas at once |
| A stolen database dump | tokens encrypted (vault key elsewhere), API keys hashed, OTP codes and numbers only as HMACs (pepper elsewhere), webhook secrets encrypted (data key elsewhere); the inbox history, the event outbox (`wa_server_events`: message texts, vCards, orders, Flow answers, BSUIDs, phone numbers, coexistence history; operator-only rows keep whole raw bodies and parse error texts) and the answers idempotency records keep for 24 h (a send's recipient: phone number, `wa_id` or BSUID) are readable, so database encryption at rest is the operator's, and the inbox's and the outbox's retention is D10 |
| Signup attributed to the wrong merchant | state bound to the tenant, redeemed for the credential's tenant; ids verified with Meta; D4 |
| OTP brute force, cross-tenant codes | the library's limits, per-tenant rate limits, namespace = tenant |
| SSRF | no URL fetching (media by id, through the library's host allow-list); webhooks-out allow-list, no redirects, pinned address |
| Exhaustion | per-route body limits, per-tenant rate limits, stream caps, timeouts, bounded media and rendering concurrency |

**Secrets** come from the environment or `<NAME>_FILE`; never the TOML
file, the image, the logs, or the database they protect.

| Secret | Protects | Rotation |
| --- | --- | --- |
| `WA_APP_SECRET` (+ `WA_APP_SECRET_PREVIOUS`) | Meta signatures, code exchange, app token, the key event ids are derived with | both listed while rolling out; an event recorded again after the rotation (its row purged) gets a new id |
| `WA_VERIFY_TOKEN` | Meta's subscription check | with the App Dashboard |
| `WA_VAULT_KEY`, `WA_VAULT_KEY_ID`, `WA_VAULT_PREVIOUS_KEYS` | merchants' tokens | `VaultKeys::with_previous`, `POST /v1/admin/vault/rotate`, drop the old |
| `WA_OTP_PEPPER` | OTP hashes | invalidates codes in flight |
| `WA_SERVER_DATA_KEY` (+ previous) | webhook signing secrets | like the vault key |
| `WA_PARTNER_SYSTEM_TOKEN` | credit-line sharing | new token, deploy, revoke |

**Never logged or echoed**: API keys and `Authorization`, Meta tokens, the
app secret, the verify token, signup codes and states, PINs, OTP codes,
webhook secrets, bodies, query strings, signature headers, `WebhookEvent`'s
`Debug`; Meta's error messages go to `debug` logs only. Request logs carry
the route template, not the raw path (contacts identify customers).

**Rate limits** per tenant and replica, overridable by the admin: sends
20/s (burst 40), reads 50/s, template management 2/s, OTP issue 5/s, 20
streams; `429` with `Retry-After`. Meta's limits (80 messages/s per number,
portfolio limits) still apply; pacing campaigns is the caller's job.
*As built in M1b*: token buckets keyed by tenant and route class, checked
after the scope and before ownership; the bursts this paragraph does not
state (reads, template management) are one second's worth; read
receipts, media and profile writes count as sends; the operator sets
them per deployment (`WA_SERVER_RATE_*`, [§7.2](#72-environment)), and
per-tenant overrides wait for the tenant settings (M3).

**CORS**: none (D3). **TLS** terminates at the ingress: valid certificate,
body limit of at least 3 MiB, no body rewriting or decompression, a timeout
above the slowest sink, request buffering (so slow clients never reach the
service), and only Meta's webhook IP ranges, or mutual TLS with Meta's
client certificate, admitted to `/webhooks/meta`. The app secret signs
every tenant's deliveries: whoever holds it can forge any tenant's
events. So from M3, a webhook that would do something destructive (a
`PARTNER_APP_UNINSTALLED` disconnecting a WABA, a credit line revoked) is
confirmed with Graph first, not acted on by its signature alone. The internal listener is reachable only from the
private network (a `NetworkPolicy` admitting the two backends). Egress:
`graph.facebook.com`, `lookaside.fbsbx.com`, allowed webhook destinations.

## 7. Operations

### 7.1 Docker image

- Multi-stage: `rust:1.98.1` (the pinned toolchain), then
  `gcr.io/distroless/cc-debian12:nonroot` (glibc, no shell, non-root). Not
  `distroless/static`: a static musl build of aws-lc-rs needs extra tooling
  and musl's allocator is slower; revisit if size matters more.
- The image sets both binds to `0.0.0.0` explicitly (loopback is
  unreachable in a container); the binary's default stays loopback.
- `HEALTHCHECK` runs `meta-whatsapp-server healthcheck` (no curl). Read-only root file
  system (fonts bundled). amd64 and arm64, SBOM and provenance attestations.

**Decision for owner (D9): image name and registry.** (a) Public on GHCR
next to the public repository; (b) a private registry. The name follows the
crate names, settled when OQ #1 closed (`meta-whatsapp-*`, 2026-09-25).
*Recommendation: (a)*, named after the binary, `meta-whatsapp-server`.
*Decided on 2026-09-26 under the owner's delegation: (a)
([§10](#10-decisions-for-the-owner)).*

### 7.2 Environment

| Variable | Default | What |
| --- | --- | --- |
| `DATABASE_URL` | required outside development | Postgres |
| `WA_SERVER_ENV` | `production` | `development` allows memory storage, a throwaway vault key, a plain-`http` `WA_GRAPH_ENDPOINT`, plain-HTTP webhook targets |
| `WA_SERVER_PUBLIC_BIND`, `WA_SERVER_INTERNAL_BIND` | `127.0.0.1:8080`, `127.0.0.1:8081` | must differ |
| `WA_APP_ID`, `WA_APP_SECRET`, `WA_VERIFY_TOKEN`, `WA_ES_CONFIG_ID` | — | the Meta app |
| `WA_VAULT_KEY*`, `WA_OTP_PEPPER`, `WA_SERVER_DATA_KEY*` | — | [§6](#6-security); `WA_VAULT_KEY_ID` defaults to `k1`, `WA_VAULT_PREVIOUS_KEYS` is `<id>:<base64 of 32 bytes>`, comma-separated, each id once (the active key's included: a repeated id refuses the start) |
| `WA_ONBOARDING_MODE` | `tech_provider` | `solution_partner` needs `WA_PARTNER_SYSTEM_TOKEN`, `WA_PARTNER_SYSTEM_USER_ID`, `WA_CREDIT_LINE_ID`, `WA_WABA_CURRENCY` |
| `WA_GRAPH_API_VERSION`, `WA_GRAPH_ENDPOINT` | `ApiVersion::DEFAULT` (v25.0), Graph | the version is also handed to the signup page; the endpoint serves proxies and test stubs, and must be `https` outside development (every token travels to it) |
| `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS` | none | hosts and CIDRs for webhooks-out |
| `WA_SERVER_OUTBOX_RETENTION`, `…_IDEMPOTENCY_TTL`, `…_WEBHOOK_RETRY_WINDOW` | 7 d, 24 h, 72 h | |
| `WA_SERVER_MEDIA_MAX_BYTES`, `WA_SERVER_SHUTDOWN_GRACE`, `WA_SERVER_MIGRATE` | 100 MiB, 25 s, `auto` | `skip` when a job runs `meta-whatsapp-server migrate` |
| `WA_SERVER_MEDIA_CONCURRENCY` (M1b) | 4 | uploads and whole-file downloads held in memory at once per replica (section 6's "bounded media concurrency"; the next is `429`); one tenant holds half at most (the service's default, not the design's) |
| `WA_SERVER_MEDIA_STREAMS` (M1b) | 16 | streamed downloads at once per replica, one tenant holding half at most (the service's defaults, not the design's) |
| `WA_SERVER_RATE_SEND`, `…_READ`, `…_TEMPLATES`, each with `…_BURST` (M1b) | 20 and 40, 50 and 50, 2 and 2 | section 6's rates, per tenant and replica; the bursts of reads and template management are the service's defaults (section 6 states none) |
| `RUST_LOG`, `WA_SERVER_LOG_FORMAT`, `OTEL_EXPORTER_OTLP_ENDPOINT` | `info`, `json`, unset | |

### 7.3 Start, observability, shutdown

- `meta-whatsapp-server serve` validates the configuration, runs `postgres::migrate`
  and the service's migrations (unless `skip`), opens both listeners.
  Also: `migrate`, `openapi`, `healthcheck`, `admin …`, `vault rotate`.
- Logs: `tracing` JSON, a span per request (request id, route template,
  tenant, status, duration); library events keep their levels
  ([production guide](../guides/production.md)); optional OpenTelemetry.
- Prometheus metrics, labelled by route template, event type, code and
  outcome, never by id, number or contact (`tenant` only on request):
  requests and latency; Meta webhooks by outcome (a run of `in_flight`
  means sinks outlast the lease); events by type (a rising `unknown`: Meta
  shipped a field worth typing); Graph errors by code; outbox lag; pending
  webhooks-out; open streams; OTP outcomes; idempotent replays.
- `SIGTERM`: `/readyz` fails, listeners stop accepting, open requests get
  `WA_SERVER_SHUTDOWN_GRACE`, streams end with `event: shutdown` (clients
  resume elsewhere), workers finish or release their lease. A Meta delivery
  cut mid-way is redelivered after its lease expires.

### 7.4 Backups

| State | Lose it and | Backup |
| --- | --- | --- |
| vault **and the vault key** | every merchant reconnects; a backup without the key is useless | database PITR; the key backed up in the secret manager, separately |
| tenants, key hashes, bindings, webhook endpoints (+ data key) | integrations stop until recreated | PITR |
| inbox history, outbox and deliveries | history; events never delivered to the integrator | PITR (subject to D10) |
| idempotency records (24 h) | a repeat after a restore could send twice | PITR |
| dedup claims (7 d) | Meta's retries delivered twice (sinks are idempotent) | PITR, not critical |
| OTP challenges, signup sessions | codes and attempts in flight fail | not needed |

**Decision for owner (D10): retention and erasure.** The service stores
customers' messages and identifiers. Choose retention for inbox history
(keep, or purge after N days), the outbox (7 days proposed; M1c ships that
as the default of `WA_SERVER_OUTBOX_RETENTION` until this is decided) and
delivery logs (30 days proposed), whether an erasure endpoint (one contact
on one number) is required, and what happens to a number's inbox history
when the number moves to another tenant (M2 hides it by binding epoch; a
purge on unbind would be erasure); erasure needs a `ConversationStore`
port change (L5). M1c deletes a deleted tenant's outbox events with it
(they could otherwise reach a tenant created again under its id): D22, a
coordinator's decision of 2026-09-25, reversible, which this decision
may revisit (keeping them for an erasure request or an audit would need
another way to keep them from the new tenant).
*Recommendation*: configurable, keeping history by default; erasure in M2
if the platform's privacy obligations require it. A legal and product call.
*Decided on 2026-09-26 under the owner's delegation, swappable:
retention is configurable per store (the outbox 7 days by default, the
inbox kept by default), erasure comes through L5 (a `ConversationStore`
erase with conformance cases), and a moved number is hidden by M2's
binding epoch ([§10](#10-decisions-for-the-owner)). The legal half stays
with whoever deploys: which retention and which erasure requests a
deployment's privacy obligations require.*

### 7.5 Versioning

Within `/v1`, changes are additive; a breaking change is `/v2`, served
beside `/v1` for a deprecation period; webhook endpoints keep their
`api_version`. The OpenAPI document is committed (`crates/meta-whatsapp-server/openapi/v1.json`)
and stays with the axum API adapter while that adapter is built (D18):
CI fails when the generated one differs. From the first release on,
`oasdiff` checks breaking changes against the last release's document;
nothing runs it yet, since there is no release to compare with. From the
default flip ([§8](#8-modular-architecture-and-client-sdks)) the committed
`api.cstack` schema is the primary contract: CrateStack's check mode fails on a stale
generated client, and `cratestack diff` checks breaking changes against the
last release. Image, API contract (spec, then schema) and the TypeScript
client share one semver; `/v1/version` adds the meta-whatsapp-rs revision.

## 8. Modular architecture and client SDKs

> Rewritten on 2026-09-26 under the owner's delegation (D26). M1 is built
> as one crate on axum with a committed OpenAPI document; this section is
> the target, reached by the steps in [roadmap.md](../roadmap.md) (§1,
> "The service's modular split"), each of which keeps `/v1` served and
> `openapi/v1.json` byte-identical until the default flips.

The owner's direction (2026-09-26): CrateStack (the owner's schema-first
framework, [cratestack/cratestack](https://github.com/cratestack/cratestack))
for the API **and the models** is the default, and every choice,
CrateStack included, must be swappable by an integrator: another API
layer, another database such as MongoDB. CrateStack cannot be what swaps
databases: its own decisions rule out a shared backend trait (its ADR
0013) and call a document store a new design (ADR 0016), and
`db = Postgres` is its only sqlx backend. So the swapping lives in our
ports, and CrateStack is one adapter on each side of them.

### 8.1 A framework-free core with ports

`meta-whatsapp-server-core` holds the service's logic and no framework:
no axum, http, utoipa, sqlx or CrateStack type. It depends on the facade
(`default-features = false`). It holds the model (tenants, keys,
bindings), the key format and digests, event routing and the event type
lists, outbox keys and event ids, polling (cursors, `410`, `422`), the
idempotency engine (§5.4), the rate limiter, the §5 error model as data
(a code and a status as a number), authorization as services (a
credential to a caller, a tenant and a scope; ownership to
`OwnedNumber` or `OwnedWaba`, which stay the only path to a vault
token), and the operations (messages, media as a byte stream,
templates, numbers, admin, events).

| Port | Does | Replaces (as built in M1) |
| --- | --- | --- |
| `RecordStore` | tenants, keys, WABA and number bindings | the `Store` trait, idempotency aside |
| `IdempotencyRecords` | claim, complete, release, purge (§5.4) | the idempotency half of `Store` |
| `Outbox` | insert with the routing checked again, page, purge (§2.3) | the `EventStore` trait |
| `LeaderLock` | `try_exclusive(name)`: one replica at a time | advisory locks in housekeeping |
| `Janitor` | expired key/value rows and the other purges | the direct `PostgresKvStore::purge_expired` call |
| `SchemaMigrator` | the store's migrations, under its own lock | the Postgres migrations |
| `EventNotifier` (M2) | wake SSE and webhooks-out on a new event | Postgres `NOTIFY`; a memory broadcast; MongoDB change streams |
| `KvStore`, `ConversationStore`, `Clock`, `HttpTransport` | the library's ports ([architecture.md](../architecture.md#ports-meta-whatsapp-core)) | — |

The store and events suites move into
`meta_whatsapp_server_core::conformance` (a feature, like
`meta_whatsapp_adapters::store::conformance`), with the invariants that
cross ports, which a new backend cannot skip: deleting a tenant purges
its stream; a binding changed during an insert leaves the event
operator-only; each tenant's events commit in order.

### 8.2 The backend bundle is the unit of swapping

`RecordStore` and `Outbox` are not independent: the Postgres outbox
insert reads the binding tables again under a lock, and deleting a
tenant writes the stream table. So one factory, `Backend`, returns every
port over one database, with its kind and capabilities, and ports from
two databases are never mixed. It replaces the optional Postgres pool
the service uses today as its "memory?" flag.

| Backend | Database | Status |
| --- | --- | --- |
| `…-store-cratestack` (the default) | Postgres: CrateStack models for the records tables (tenants, API keys, WABAs, numbers); the outbox, idempotency, locks, janitor and migrations reuse store-postgres's SQL on the same pool, and the library's stores are `PostgresKvStore` and `PostgresConversationStore` on it | planned |
| `…-store-postgres` | Postgres through sqlx: today's code and migrations | built; kept and conformance-tested |
| `…-store-memory` | memory, development only | built; kept |
| `…-store-mongodb` | MongoDB, a replica set | later: the library's MongoDB adapters first, then an `Outbox` spike against the conformance suite before the port shapes freeze |

### 8.3 API adapters

`…-server-http` (axum) is shared by every adapter: the listeners,
telemetry, request ids, authentication before the body is read, rate
limits, the §5 renderer, and the routes CrateStack cannot express
(`/webhooks/meta`, media upload and download, SSE, the operations
routes). Both CrateStack and our routes use axum 0.8, so their routers
merge.

| Adapter | Contract | Status |
| --- | --- | --- |
| `…-api-cratestack` (the default) | `api.cstack`: procedures, types and enums, REST (D19) | planned |
| `…-api-axum` | today's `/v1` resource routes and `openapi/v1.json` | built; kept and conformance-tested |

The binary composes one adapter and one backend: Cargo features choose
which are compiled in (`api-cratestack` and `store-cratestack` by
default; `api-axum`, `store-postgres` and `store-memory` optional), and
configuration picks among them. The authorization (M1.3), error (M1.5)
and idempotency (M1.4) suites run against every adapter, enumerated
from its contract so a new route cannot skip them.

### 8.4 Two CrateStack schemas

- **`api.cstack`**, the contract: `datasource { provider = "none" }`,
  compiled with `cratestack-api` (`db = None`). It names no database,
  so the CrateStack API runs over any backend, MongoDB included.
- **`store.cstack`**, the records tables' models only (`@@internal`,
  `db = Postgres`, `cratestack-pg`), for `store-cratestack`, with a live
  drift test decoding every model against the tables our migrations
  create.

One `db = Postgres` schema would tie the API crate to sqlx and its pool:
CrateStack allows one database-owning schema per crate, and a
`db = None` crate and a `db = Postgres` crate may share a binary.

### 8.5 What never crosses a port

- sqlx types (pools, rows, transactions).
- CrateStack types: its error, context and transaction types, generated
  models, its `Json<T>`, chrono times.
- axum and http types, and utoipa derives, in the core.
- A Meta token, anywhere but behind `OwnedNumber` or `OwnedWaba`.

### 8.6 Where CrateStack forces a compromise

- URLs become procedures (`POST /[v1/]$procs/<name>`: no `GET`, no path
  parameters).
- Two error shapes until D16 lands upstream: our outer layer and our
  custom routes answer §5, the generated routes CrateStack's own (a flat
  `{code, message, details}` with its fixed codes and statuses, no 410,
  413, 502 or 504). So the CrateStack adapter is not the default before
  D16 (roadmap S9).
- Authentication sits in an outer layer: CrateStack's auth hook receives
  a body it has already buffered, and §3.3 answers `401` before the body.
- Ownership `404`s move inside the procedures.
- Our idempotency middleware (§5.4 releases a key when nothing was sent;
  CrateStack's keeps every outcome) and our per-route body limits stay.
- About 40% of the HTTP surface stays custom axum (§8.3).
- The models cover the records tables only: the outbox insert, the
  idempotency claim, the purges and the migrations need what models
  cannot express (`FOR KEY SHARE`, conditional upserts, advisory locks,
  the database's clock, `json` rather than `jsonb`, which keeps U+0000).
- chrono and a system principal enter the service's dependency graph,
  and `ring` is linked beside aws-lc-rs: sqlx picks it for Postgres TLS
  in those builds until upstream U2 (D20).

### 8.7 Client SDKs

- **The primary contract is the CrateStack schema** and the clients
  generated from it (D18): TypeScript for Medusa and the CMS, speaking
  JSON on the wire rather than CBOR (D19), Dart and Rust as needed.
  `openapi/v1.json` stays committed and tested with `api-axum` while
  that adapter is built.
- A thin hand-written layer over the generated TypeScript client adds
  the credential and `WA-Tenant` headers, `Idempotency-Key`, a typed
  error from the §5 body, an SSE reader that resumes, media, and
  `verifyWebhook()`. It lives in `clients/typescript`; at the default
  flip the server skills' TypeScript is type-checked against it,
  replacing today's openapi-typescript gate.

What CrateStack does not do today, from the fit studies (0.12 on
2026-09-25, 0.13 on 2026-09-26): domain error bodies (D16); a `404` from
`@authorize`; authenticating before the body is read; resource-shaped
URLs; idempotency that releases a key when nothing was sent; resumable
SSE; multipart and streamed media; per-procedure body limits. The bug
by which its generated clients ignored `@api_version` is fixed in 0.13.0.
Each gap is an upstream change in CrateStack, or stays a custom route
in the service.

**Decision for owner (D11): publishing the client.** (a) Public npm under
the organization's scope; (b) GitHub Packages (consumers need a token even
for a public repository); (c) vendored by consumers. *Recommendation: (a)*,
matching the public, MIT-licensed repository. *Decided on 2026-09-26
under the owner's delegation: (a); the publish workflow is prepared,
and publishing needs the owner's token ([§10](#10-decisions-for-the-owner)).*

**Decision for owner (D12): a Medusa module or plugin.** (a) None: Medusa's
developers call the client from their own module; (b) a Medusa v2 plugin in
its own repository: a notification provider (order templates), an auth
provider (WhatsApp OTP), a route turning webhooks-out into Medusa events;
(c) (a) now, (b) as a fifth milestone after the first integration.
*Recommendation: (c)*: before one integration exists, a plugin guesses at
Medusa's workflows. *Decided on 2026-09-26 under the owner's delegation:
(c), after the first integration ([§10](#10-decisions-for-the-owner)).*

## 9. Delivery plan

Each milestone goes through the project's pipeline (implement, adversarial
review with distinct lenses, remediate, re-run the original failure, `just
ci` on the final head) and names its companion docs and skills in the PR.
Tests use `ScriptedTransport` (method, path, token, exact JSON,
`remaining() == 0`); live tests are `live_*`, and `just test-live` gains
`-p meta-whatsapp-server` under `META_WHATSAPP_RS_REQUIRE_LIVE=1`.

| # | Library change (own PR, own parity) | When | Kind |
| --- | --- | --- | --- |
| L1 | `ErrorKind::as_str()`, stable snake_case, pinned by a test (with `ErrorKind::ALL`) | done (M1a) | additive |
| L2 | `OtpConfig::namespace` required | done (#4) | breaking |
| L3 | Solution Partner credit-line step in onboarding | done (#5: `onboard_with_approval`, `offboard`, credit ledger) | additive |
| L4 | a code-less `OnboardingRequest` for `resume` (OQ #10) | decided (OQ #10, 2026-09-26): [roadmap.md](../roadmap.md) L4 | additive |
| L5 | `ConversationStore` erasure, and retention per store | decided (D10, 2026-09-26): [roadmap.md](../roadmap.md) L5, before M2's retention settings | port change |
| L6 | [architecture.md](../architecture.md): dependency rule for binaries, a "Service" section | done (M1a) | docs |

| | Scope | Docs and skills it adds |
| --- | --- | --- |
| **M1** skeleton, auth, messages and templates, webhooks in | the crate, fail-closed configuration, both listeners, storage and migrations, tenants, keys, admin API and CLI bootstrap, admin attach, the authorization order, messages, media, templates (list, get, create, delete), `/webhooks/meta` into inbox and outbox, `GET /v1/events`, errors (L1), idempotency, rate limits, health, metrics, tracing, the committed spec | `docs/guides/server.md` (run, configure, tenants, keys, first send); a README section "Not writing Rust? Run the service"; L6; a `docs/coverage.md` row; skills `meta-whatsapp-rs-server` (hub for HTTP callers: deploy, credentials, errors, idempotency, routing) and `meta-whatsapp-rs-server-send` (messages, templates, media); the skills gate below |
| **M2** inbox, live updates, webhooks out | inbox routes (their reads filter by the number's binding epoch: a number moved to another tenant shows it nothing from before its binding, the inbox half of security review M3, unless D10 purges on unbind), SSE (`LISTEN/NOTIFY`, `Last-Event-ID`), `GET /v1/events/{id}`, webhook endpoints, dispatcher, retries, destination allow-list, the service's number events, and the three types PR #17 typed made tenant-visible (D25: `standby_observed`, `thread_control_changed`, `user_action_reported` into `TENANT_EVENT_TYPES`, `KnownEventType` growing, additive) with the inbox's answer to `OPEN_QUESTIONS.md` #44, retention per store (D10), the received-media exemption (`OPEN_QUESTIONS.md` #43) | `server.md` inbox and events; skill `meta-whatsapp-rs-server-inbox` (inbox API, relaying live events to the CMS's browsers, receiving and verifying webhooks-out) |
| **M3** Embedded Signup in both modes, OTP | signup routes, persisted attempts, disconnection, coexistence sync, authentication templates, OTP and its per-tenant settings; needs L2 (and L3 for partner mode). Vault rotation extended to the records partner mode keeps past a binding (credit ledgers that outlive their token, revocation markers: the library's `rotate_business`), without which a rotation's empty `failed` no longer means the old key is unused | `server.md` onboarding and OTP, and its key rotation advice ("drop the old key once `failed` is empty", caveated since M1a) made true again; skills `meta-whatsapp-rs-server-onboarding` (the CMS connect flow through the service: page, relay, PIN, resume, both modes) and `meta-whatsapp-rs-server-otp` |
| **M4** packaging: Docker image, TypeScript client, docs | the image and its CI (D9), a Compose file, `clients/typescript` generated by CrateStack and published (D11), the documents route, the deployment guide. Re-scoped on 2026-09-26: the move onto CrateStack is the modular split's ([§8](#8-modular-architecture-and-client-sdks), [roadmap.md](../roadmap.md) S1–S9; the default flips once D16 lands upstream), no longer this milestone's | `server.md` deployment (Docker, Compose, Kubernetes notes); a `docs/guides/README.md` row; skill `meta-whatsapp-rs-server-typescript` (install, calls, errors, idempotency, SSE, webhook verification in Medusa or any Node backend); `meta-whatsapp-rs-production` points to the service |
| **M5** the modules parity requires (added 2026-09-26) | routes over the library modules §1 once left "on demand", one PR per family ([roadmap.md](../roadmap.md) M5a–M5l): the send union's other types (pin, request contact info, Direct Send, interactive carousels, voice call, location request, address, call permission request, Flow, product messages), template edit, library, migration, comparison and unpausing, numbers and profile settings, WABA management, Flows, calling, groups, commerce, QR codes, analytics, block users, the Marketing Messages API and CTWA, In-App Signup (enabled once the owner answers `OPEN_QUESTIONS.md` #26), partner APIs, conversation routing, and the bot and broadcast APIs over `meta-whatsapp-bot`. Each route checks the object an id names against the path's number or WABA ([architecture.md § Service](../architecture.md#service-meta-whatsapp-server)); M1.3's table covers it by construction | a `server.md` section and a server skill (or a section of one) per family |

**M1 ships in three parts**, each its own pull request: **M1a** the
crate, configuration, listeners, storage and migrations, tenants, keys,
the admin API (attach, unbind, vault rotation) and CLI bootstrap, the
authorization order, the numbers and profile routes, errors (L1),
health, metrics, tracing, the committed spec and the skills gate; **M1b**
messages, media, templates, idempotency keys and rate limits; **M1c**
`/webhooks/meta` into inbox and outbox and `GET /v1/events`. M1a meets
M1.1, M1.3, M1.5 and M1.6, and M1.7's parallel migrations and its log
capture over every M1a route (the M1a routes stand in for a send and a
webhook). **M1b meets M1.4** (`tests/messages.rs`, and on Postgres
`tests/live_postgres.rs`), **M1.3 and M1.5 over its routes** (the tests
iterate the committed document) **and M1.7's send part** (the capture
exercises every route, sends with text, a phone number, a BSUID and a
contact card included). **M1c meets M1.2 and M1.7's webhook half** (two
instances on one database deduplicating a webhook, the capture of
webhooks and of `GET /v1/events`), and keeps M1.1, M1.3, M1.5 and M1.6
over its route. With all three, M1 is complete (M1.1 on the merged head).

Acceptance tests. "Decisive" names the guard whose removal must make the
test fail.

| # | Test |
| --- | --- |
| M1.1 | `just ci` exits 0 with the crate included (lint, doc, deny with the new dependencies) |
| M1.2 | A body signed with `meta_whatsapp_rs::webhooks::sign` is `200` and one outbox row for the owning tenant; no signature is `401` without the body being polled; the same body twice is one row; 3 MiB + 1 byte is `413`; `unknown` and `unparsed` are operator-only. Decisive: routing an unowned number's event to a tenant |
| M1.3 | Table-driven over every `{pn}` and `{waba_id}` route in the spec (a new route cannot skip it): tenant B's key on A's number is `404`, and a counting vault wrapper records zero reads. Decisive: step 4 of [§3.3](#33-authorization-order) |
| M1.4 | Sends carry the merchant's vault token; a digits-only `to.phone` is refused before any request; a scripted timeout is `504` with `may_have_been_sent: true` and the same `Idempotency-Key` replays it with no second request; a scripted 131047 is `409` and releases the key |
| M1.5 | Every `ErrorKind` maps to a code and status (iterating L1's list); a sentinel in a scripted Graph error's message, title and user texts reaches no response; `details` only where [§5.1](#51-body) allows, bounded |
| M1.6 | One test per start-up refusal ([§2.2](#22-configuration-and-storage)); the generated spec equals the committed one |
| M1.7 | Live: two instances on one database deduplicate the same webhook; parallel migrations succeed. Captured `tracing` output of a send and a webhook holds no token, secret, key, message text, phone number or contact |
| M2.1 | On Postgres: inbound webhook → conversation list → history → reply in the window (recorded `accepted`) → a status webhook moves it to `delivered`; a free-form reply outside the window is `409` with zero requests |
| M2.2 | Live, two replicas on one database: a webhook into A reaches a stream on B within 2 s; `Last-Event-ID` replays exactly the missed events; a stream on tenant B's number sees none of A's |
| M2.3 | No `unknown` event reaches any stream, poll or endpoint. Decisive: the routing allow-list |
| M2.4 | Webhooks-out: Standard Webhooks test vectors; retry schedule under a fake clock; redirects not followed; a destination outside the allow-list refused at creation and at delivery (a resolver stub that turns private on the second lookup); two signatures during rotation; over 256 KiB delivered truncated and fetchable only by its tenant |
| M2.5 | 200 open streams at 100 events/s with bounded memory |
| M2.6 | D25: Meta's examples of `standby_observed`, `thread_control_changed` and `user_action_reported` reach their number's tenant (poll and stream) and no other; `KnownEventType` gains them and the v1 document stays additive. Decisive: taking one out of `TENANT_EVENT_TYPES` |
| M3.1 | The happy path over HTTP with Meta's documented responses binds the verified WABA and all its numbers; no response contains the token |
| M3.2 | Another tenant's state is `403 stale_attempt` and the rightful tenant still completes; a malformed PIN is `422` and the state survives |
| M3.3 | A scripted 133005 at registration is `502 onboarding_failed` (`register_phone`, resumable); a new process on the same database resumes with a corrected PIN; another tenant's resume is `404` |
| M3.4 | Partner mode: the credit-line request carries the partner's system token (asserted header), never the merchant's; missing partner settings refuse the start. D4 is enforced **before** any credit call, through the library's post-verification gate: a second tenant onboarding a bound WABA gets `409`, nothing is stored, subscribed or shared, binding unchanged. A business whose line was revoked is not re-funded by `resume` or a new signup without an explicit operator action. Offboarding revokes first and deletes second: a CMS disconnect, `PARTNER_APP_UNINSTALLED` and `PARTNER_REMOVED` (in any order) end with the line revoked (from the stored or the webhook's owner business id) before the vault entry and bindings go; tests replay both orders |
| M3.5 | OTP: every outcome; tenant A's code verifies at no other tenant on the same number (decisive: the namespace); logs hold neither code nor number; a sentinel in a scripted Graph error on issue reaches no response |
| M4.1 | The image builds for both architectures, runs non-root on a read-only file system, has no shell; `meta-whatsapp-server healthcheck` works in it |
| M4.2 | A Compose smoke test in CI (Postgres, the image, a Graph stub via `WA_GRAPH_ENDPOINT`): CLI admin key, tenant, attach, send, a signed Meta webhook, a webhooks-out delivery verified at a stub receiver |
| M4.3 | The client is generated from the committed `.cstack` schema (CrateStack's check mode fails on drift); `tsc --noEmit` passes on it and on every TypeScript excerpt of the server skills; a Node test verifies a real delivery with `verifyWebhook()`; a breaking change within `v1` fails `cratestack diff` against the last release; the invoice fixture renders byte-identically |

**The skills gate for HTTP callers (M1).** `crates/meta-whatsapp-rs/tests/skills.rs`
assumes Rust (no TypeScript fences; backticked names must exist in
`crates/`), so server skills would fail it or pass unchecked if
allow-listed. M1 extends it for `skills/meta-whatsapp-rs-server*`: a `ts` fence must be
an excerpt of the skill's `examples/*.ts`, type-checked by a new `just
skills-ts` (tsc against the generated client, Node pinned) inside `just
ci`; backticked routes, schemas and codes are checked against the committed
spec. Stamps, the 160-line limit and hub listing apply unchanged.

**Decision for owner (D13): where the server skills live.** (a) This
repository's `skills/` (`npx skills add vaam-apps/meta-whatsapp-rs -s meta-whatsapp-rs-server …`),
changed in the same PR as the API; (b) a separate skills repository with
its own coverage gate. *Recommendation: (a)*: API, spec and skills change
atomically and are checked against the same commit.

## 10. Decisions for the owner

**From 2026-09-26 the owner delegates these decisions to the
coordinator**, on one condition: the code stays composable, with traits
and interfaces hiding the hard logic, so that an integrator can change
each decision (the owner's examples: MongoDB for the database;
CrateStack for the API and the models, as the default, and CrateStack
itself swappable altogether). So a decision below is taken when it can
be made swappable, and its row says how to swap it. **Legal and
terms-of-service choices, licences among them, and irreversible data
migrations still go to the owner.**

Who decided what: the owner decided D1–D4 and D7 on 2026-09-24, D13–D14
and D15–D18 on 2026-09-25 (the recommended option each time, except D17,
against the recommendation to move before M1b), and D25 on 2026-09-26.
D21–D24 are the coordinator's decisions of 2026-09-25, which M1c ships
and which stand under the delegation. D6, D8–D12, D19, D20's (b) and
(c), and D26–D29 are the coordinator's decisions of 2026-09-26 under the
delegation, as are the changes to D15 (widened), D16 (the gate) and D18
(revised) that day. D20's (a), a licence, is the owner's: roadmap S5
asks for it. D5 is settled as "support both modes, chosen per
deployment"; which mode a production deployment runs follows the platform's
agreements with Meta (partner status, who pays), so that choice stays
the owner's.

| # | Question | Options | Recommendation | Needed by |
| --- | --- | --- | --- | --- |
| D1 | Tenancy per deployment | per tenant / per Meta app / per product | **Decided 2026-09-24: one multi-tenant deployment per Meta app** | M1 |
| D2 | Credentials | tenant keys / platform key + header / both | **Decided 2026-09-24: both** | M1 |
| D3 | Browser access | never (the CMS relays) / stream tokens + CORS | **Decided 2026-09-24: never, in v1** | M2 |
| D4 | One WABA, several tenants (OQ #6) | refuse / move / share | **Decided 2026-09-24: refuse, admin unbind** | M3 |
| D5 | Onboarding mode in production (OQ #3) | Tech Provider / Solution Partner | Tech Provider first, switch by configuration; both are supported, per deployment. Which one a production deployment runs is the owner's (the platform's agreements with Meta) | M3 |
| D6 | Two-step PIN (OQ #4) | per attempt, never stored / generated and stored | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: per attempt, never stored.** The PIN is an input of `onboard`, `resume` and the M3 routes; to swap, an integrator keeps PINs in its own secret store and passes them in | M3 |
| D7 | Coexistence sync (OQ #7) | endpoint / automatic / both, per tenant | **Decided 2026-09-24: automatic** (the service starts the one-time contacts + history sync right after a coexistence onboarding) | M3 |
| D8 | Who sends a tenant's OTP codes | platform number / merchant's / per tenant | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: per tenant, the platform's number by default**; to swap, a tenant's OTP settings name another sending number and its approved template | M3 |
| D9 | Image name and registry | public GHCR / private registry; the name | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: public on GHCR, named `meta-whatsapp-server`** (OQ #1, closed, settled the crate names); to swap, the image workflow's registry and name are its inputs | M4 |
| D10 | Retention and erasure of customers' messages | keep / purge after N days; erasure or not; a number's inbox history when it moves to another tenant (hidden by M2's binding epoch, or purged on unbind) | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: retention is configurable per store.** The outbox keeps 7 days by default (`WA_SERVER_OUTBOX_RETENTION`), the inbox keeps everything by default, erasure (one contact on one number) comes through L5, a `ConversationStore` erase with conformance cases, and a number moved to another tenant is hidden from it by M2's binding epoch, not purged. To swap, each store's retention is a setting and erasure a call. Which retention a deployment sets, and which erasure requests it must honour, follow its privacy obligations: the operator's legal call | M2 |
| D11 | Publishing the TypeScript client | npm / GitHub Packages / vendored | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: public npm** under the organization's scope. The publish workflow is prepared; publishing needs the owner's token. To swap, the workflow's registry is its input | M4 |
| D12 | A Medusa plugin | none / now / after the first integration | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: after the first integration**, in its own repository; nothing here depends on it | after M4 |
| D13 | Where the server skills live | this repository / a separate one | **Decided 2026-09-25: this repository** (under `skills/`, same stamp gate and `npx skills add vaam-apps/meta-whatsapp-rs`) | M1 |
| D14 | Credit line after a merchant unshares (`PARTNER_REMOVED`) | revoke at once (Meta's recommendation) / revoke after a grace period when `disconnection_info` says the coexistence number may reconnect / operator decides | **Decided 2026-09-25: revoke at once** on every `PARTNER_REMOVED` for our solution, coexistence included; a merchant who reconnects re-onboards and is funded again only through the explicit re-share (`reshare_after_revocation`) | M3 |
| D15 | API layer framework | OpenAPI (axum + utoipa) / CrateStack `cratestack-api` (procedures) / `cratestack-pg` (models + policies) / schema for clients only | **Decided 2026-09-25: CrateStack, `cratestack-api`, procedures only. Widened by the coordinator on 2026-09-26 under the owner's delegation, swappable: CrateStack for the API and the models, by default**: `api.cstack` (`db = None`) for the API, and in `store-cratestack` models from `store.cstack` (`db = Postgres`) for the records tables, a hybrid whose outbox, idempotency, locks, janitor and migrations reuse store-postgres's SQL ([§8.4](#84-two-cratestack-schemas)). To swap, the binary's features select `api-axum` and `store-postgres` (or `store-memory`, later `store-mongodb`), and CrateStack is then absent from the build | the flip ([roadmap.md](../roadmap.md) S9) |
| D16 | The §5 error body under CrateStack | add domain errors to CrateStack upstream / rewrite layer in the service / two shapes / results instead of errors | **Decided 2026-09-25: upstream in CrateStack. The coordinator, 2026-09-26: the default API flip is gated on it**; until it lands, the CrateStack adapter answers two shapes (§5 from our outer layer and custom routes, CrateStack's own from its generated routes), which is why it is not the default before then | before the flip |
| D17 | When to move to CrateStack | before M1b / finish M1 first, migrate at M4 | **Decided 2026-09-25: finish M1 on axum + OpenAPI, migrate at M4.** Since 2026-09-26 the move is the modular split's ([§8](#8-modular-architecture-and-client-sdks), [roadmap.md](../roadmap.md) S1–S9) rather than M4's | M1 |
| D18 | Callers that do not use a generated client | generated clients only / an OpenAPI emitter upstream / a hand-kept OpenAPI document | **Decided 2026-09-25: generated clients only. Revised by the coordinator on 2026-09-26 under the owner's delegation, swappable: CrateStack's generated clients are the primary contract, and `openapi/v1.json` stays, committed and tested, with `api-axum` while that adapter is built**; to swap, build `api-axum` and serve its document | the flip |
| D19 | CrateStack transport | REST (JSON, `@status`, `POST /$procs/<name>`) / RPC (batching, subscriptions; CBOR by default in the TS client) | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: REST** (a status per request, `Retry-After`, `Idempotency-Key`), **and the TypeScript client pinned to the JSON wire format, not CBOR**; to swap, the schema's transport and the generator's format option | S6 |
| D20 | CrateStack's dependencies | allow BlueOak-1.0.0 (`minicbor`) in `deny.toml`; accept `ring` beside `aws-lc-rs` (explicit TLS provider at start); accept its `sqlx =0.9.0` pin / change them upstream first | **(a) The owner's: a licence.** Recommended: a `deny.toml` licence exception for BlueOak-1.0.0 scoped to `minicbor` and `minicbor-serde`, which are non-optional dependencies of CrateStack's axum crate (CrateStack's own `deny.toml` allows that licence). Roadmap S5 asks the owner and does not merge without the answer. A separate workspace for the CrateStack crates would keep the exception out of the main `deny.toml`, but the licence would still enter the service's binary; a refusal means no CrateStack in the build, and the defaults stay `api-axum` and `store-postgres`. **(b) and (c): coordinator's decisions 2026-09-26 under the owner's delegation, swappable.** (b) aws-lc-rs installed as rustls's process default at start, for the crates that use the process default; sqlx picks its provider itself, so Postgres TLS runs on `ring` in a build with CrateStack until upstream U2. (c) CrateStack's `sqlx =0.9.0` pin accepted, with an upstream PR relaxing it (U3) | S5 |
| D21 | Event sequences ([§2.3](#23-the-event-pipeline)) | one sequence for the whole outbox / one per tenant | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: one per tenant** (security review L2: a global sequence shows every tenant the platform's volume and timing); `sequence`, `after`, `next_after`, `410` and `422` are the tenant's. Reversible until the first release, or the first deployment with polling consumers: the per-tenant sequence is in the v1 contract (the descriptions of `sequence` and `next_after` in `openapi/v1.json`), consumers store cursors in it, and within `/v1` changes are additive ([§7.5](#75-versioning)), so going back to one sequence then breaks every stored cursor | M1 |
| D22 | A deleted tenant's outbox events (related to D10) | delete them with the tenant / keep them, hidden from a tenant created again under the id | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: delete them** (the outbox's foreign key cascades; the tenant's stream records them purged, so an old cursor is `410`). Reversing it keeps the events of tenants deleted afterwards only: events the cascade already deleted are not recoverable | M1 |
| D23 | Dedup window of keyless events (`error_reported`, `unparsed`, [§2.3](#23-the-event-pipeline)) | 1 h / Meta's whole 7-day retry period / none | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: 1 h** (`KEYLESS_DEDUP_WINDOW`). Failure mode: Meta documents a retry at once, then with decreasing frequency over 7 days, and that receivers must deduplicate; it does not say a redelivery carries the same bytes, which the key assumes. An outage longer than an hour (the database answering `500`, say) records the batch's keyless events twice, as new ids. 7 days would record a recurring identical error once for that long; none would record every redelivery | M1 |
| D24 | A deleted tenant on platform keys | scrubbed from every key's allowed tenants, a tenant created again needing a new key / kept | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: scrubbed** (a tenant created again under a recycled id must not inherit access). No route or command edits a key's allowed tenants, so a tenant created again under the id needs a new platform key; a `*` key allows it at once. Reversing it keeps the allowances of tenants deleted afterwards only: the lists already scrubbed are not restored | M1 |
| D25 | The event types the webhook conformance sweep typed (PR #17: `standby_observed`, `thread_control_changed`, `user_action_reported`, formerly `unknown`) | operator-only / tenant-visible | **Decided 2026-09-26: tenant-visible, in M2** (with the inbox's answer to `OPEN_QUESTIONS.md` #44); M1c ships them operator-only, the rule for a type not yet reviewed | M2 |
| D26 | The service's architecture | one crate / a framework-free core with ports, API adapters and backend bundles | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: the modular service ([§8](#8-modular-architecture-and-client-sdks)).** Defaults: `api-cratestack` and `store-cratestack` (the hybrid), once the owner accepts D20's licence (a). Kept built and conformance-tested: `api-axum` (today's v1 and its OpenAPI document) and `store-postgres` (sqlx); `store-memory` for development; later `store-mongodb`, after the library's MongoDB adapters, with an `Outbox` spike first. To swap, Cargo features choose which adapters are compiled in and configuration picks one of each; a new backend implements the core ports and passes `meta_whatsapp_server_core::conformance` | [roadmap.md](../roadmap.md) S1–S12 |
| D27 | The idempotency fingerprint ([§5.4](#54-idempotency-keys)) | method, route and body (as built) / operation id and canonical input | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: the operation id plus the canonical input**, so one key means the same through every API adapter. The change is pre-release, so no stored key breaks; to swap, the fingerprint is one function in the core | S4 |
| D28 | The bot framework and its plugins | in the facade / a new library crate / in the service; plugins loaded at run time / registered at compile time | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: a new library crate, `meta-whatsapp-bot`, with plugins registered at compile time.** No hot reload: loading Rust code at run time is neither idiomatic nor safe (no stable ABI, and `unsafe` loading, which the workspace forbids), and parity row 87 says so. To swap, a `Bot` is an `EventSink`, so an integrator can put their own dispatcher in its place; the service exposes the bot over HTTP in M5 | B1 |
| D29 | Paced broadcast and scheduled messages | a job-queue port / a typed store on `KvStore` / the caller's | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: in `meta-whatsapp-bot`, with jobs as a typed store on `KvStore`** (a bucketed due-time index, claims by compare-and-swap under a lease), not a new port, per architecture.md's rule that typed stores are built on `KvStore`; broadcasts are paced per number under Meta's throughput. To swap, the pacer and the job store are traits with these as their defaults | B2, B3 |

**Inherited from [OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md).** The
questions the service inherits were decided on 2026-09-26 under the same
delegation, each with its roadmap item: #5 (multi-WABA signups: opt-in),
#8 (no token refresh: the service reports `reconnect_required`, and the
expiry is surfaced before it lapses), #9 (vault key custody and cadence:
rotation is one call), #10 (a code-less resume, L4), #13 (the OTP issue
limit, kept), #30 (dead-lettering a permanently failing event, L21),
#32 (calls reopen the inbox's window, L7). Until an item lands, the
service keeps the library's behaviour and makes it visible to callers.
#33 (message ids unique per store) stays open: its alternative is a
primary-key migration of stored messages, which is the owner's.
