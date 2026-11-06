# Design: a deployable meta-whatsapp-rs service (`meta-whatsapp-server`)

> **Milestones M1a, M1b and M1c are implemented** in
> `crates/meta-whatsapp-server`, on its framework-free core
> `crates/meta-whatsapp-server-core` (roadmap S1,
> [§8.1](#81-a-framework-free-core-with-ports)) (M1a: the crate, configuration, listeners,
> storage, tenants and keys, the admin API with attach, unbind and vault
> rotation, the numbers routes, errors, operations, the committed OpenAPI
> document; M1b: messages, read receipts, media, templates, idempotency
> keys and rate limits; M1c: `POST /webhooks/meta` into the inbox and the
> event outbox, and `GET /v1/events`; since then, roadmap M2a the inbox
> routes, M2b one event by id and live events ([§4.5](#45-live-updates-sse)),
> and M3c OTP, its per-tenant settings and authentication templates
> ([§4.7](#47-otp)); [§9](#9-delivery-plan) says which
> acceptance tests they meet, [coverage.md](../coverage.md) row 33 what
> is missing); the rest is design. Written against `main` =
> bbf24a3 (2026-09-24), Graph API v25.0; the library changes it assumed have
> since landed on `main` (#4: `OtpConfig::namespace` required, the Intent
> API's result renamed `marketing::OnboardingRequested`; #5: Solution Partner
> onboarding with `onboard_with_approval`, `offboard` and the credit ledger).
> Choices are marked **Decision (Dn)** and listed in
> [§10](#10-decisions). From 2026-09-26 the owner delegates
> them to the coordinator, on condition that each stays swappable
> (§10 says which remain the owner's); the same day, parity with Zaileys
> on Meta's Cloud API ([parity.md](../parity.md)) lifted most of
> [§1](#1-goals-and-non-goals)'s non-goals, and [§8](#8-modular-architecture-and-client-sdks)
> became the modular architecture ([roadmap.md](../roadmap.md) has the
> steps).

The product decision: apps not written in Rust (Medusa, in TypeScript; the
CMS, any stack) use meta-whatsapp-rs through a **service deployed as a Docker image
and called over HTTP**. In short: a binary crate, `crates/meta-whatsapp-server`
(axum), and its framework-free core, `crates/meta-whatsapp-server-core`,
built only on the `meta-whatsapp-rs` facade, productize the runnable examples
and keeps their security rules; one multi-tenant deployment per Meta app;
a public listener serving only Meta's webhook and an internal one for the
API; Postgres for all state, with an event outbox feeding SSE, polling and
signed webhooks; `/v1` REST/JSON, a committed OpenAPI 3.1 spec, a generated
TypeScript client; five milestones (M5, added on 2026-09-26, brings the
modules parity requires). M1 was built as one crate; its core is a crate
of its own since roadmap S1, and §8 splits the rest into API adapters
and swappable backends, with
CrateStack for the API and the models as the default once roadmap S16
flips it (its licence, D20 (a), decided by the owner on 2026-11-02: yes;
CrateStack entered the workspace in S10); axum and sqlx stay built beside
it.

## 1. Goals and non-goals

| Caller | Reaches | Credential ([§3.2](#32-credentials)) | Typical calls |
| --- | --- | --- | --- |
| Medusa backend | internal API | tenant key (the store is a tenant) | order templates, invoices, OTP login, events by webhook |
| CMS backend | internal API | platform key + `WA-Tenant` header | Embedded Signup, inbox, replies, merchants' templates, events |
| Merchant's browser | the CMS only (D3) | the CMS session | — |
| Meta | public listener, `/webhooks/meta` | verify token, `X-Hub-Signature-256` | subscription check, deliveries, and the Flows data endpoint `/webhooks/flows/{waba_id}` (M5d) |
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

**Decision (D1): tenancy per deployment.** (a) One deployment per
tenant; (b) one multi-tenant deployment per Meta app; (c) one per product.
Every merchant onboarded by Embedded Signup delivers to the **app's single
callback URL**, and template and account webhooks ignore per-WABA overrides,
so (a) needs a router in front and cannot share one vault.
*Recommendation: (b)*, the store being one tenant when it shares the app;
separate deployments only for separate apps or environments.

## 2. Architecture

### 2.1 The crates

Six crates, as built through roadmap S7 (S1 extracted the core, S7 split
the rest), all `publish = false` and workspace members (so `just ci`
covers them) but not default ones:

- `crates/meta-whatsapp-server-core`, a library: the domain, the
  authorization order, event routing and polling, the webhook pipeline
  (since roadmap S5e), the idempotency engine, the rate limiter, the §5
  error model as data, and the ports a backend implements
  ([§8.1](#81-a-framework-free-core-with-ports)). It turns on none of
  the facade's adapter features: no axum, no sqlx, no utoipa.
- `crates/meta-whatsapp-server-store-memory` and
  `crates/meta-whatsapp-server-store-postgres`: the two backends
  ([§8.2](#82-the-backend-bundle-is-the-unit-of-swapping)), on the core
  and the facade's `memory` or `postgres` alone (sqlx through its
  re-export, OQ #29); the Postgres one holds the service's migrations
  and their migrator.
- `crates/meta-whatsapp-server-http`: what every API adapter shares
  ([§8.3](#83-api-adapters)): the state and the grant an adapter's routes
  are served with, the listeners, telemetry and metrics, authentication
  before the body, the §5 renderer, the webhook listener (what an HTTP
  body needs before the core's pipeline), media and the operations
  routes. On the facade's `axum`.
- `crates/meta-whatsapp-server-api-axum`: the `/v1` resource routes and
  the OpenAPI document generated from them.
- `crates/meta-whatsapp-server`, binary `meta-whatsapp-server`:
  configuration, `serve` and the command line, composing the others by
  its Cargo features (`api-axum`, `store-memory`, `store-postgres`, all
  three by default; a build without `api-axum` does not compile, as it
  is the only API adapter), with the facade's `reqwest`. The committed
  `openapi/v1.json` stays here, beside the command that prints it.

Beside them, `crates/meta-whatsapp-server-store-mongodb`, a spike
(roadmap S9): the `Outbox` port alone on a MongoDB replica set, through
the `mongodb` driver (which the facade re-exports since S17, as
`adapters::store::mongodb::mongodb`, with the library's MongoDB stores),
composed into no binary ([§8.2](#82-the-backend-bundle-is-the-unit-of-swapping)).

They depend on the `meta-whatsapp-rs` facade and on each other, never on
another crate of the library, and no library crate depends on them: the
API an outside integrator has, which proves the facade suffices.
[architecture.md](../architecture.md)'s "nothing depends on
`meta-whatsapp-rs`" became "no library crate depends on
`meta-whatsapp-rs`; binaries may, and the service's crates do" (L6). No
backend depends on the HTTP crates, and the HTTP crate on no adapter or
backend. New dependencies (OpenAPI generation, `tower-http`, a Prometheus
exporter, a rate limiter, `clap`) must pass `cargo deny`.

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

Reused as the library ships them: `meta_whatsapp_rs::webhooks::router`, `DedupGuard`, `InboxSink`,
`Inbox`, `TokenVault`, `EmbeddedSignup`, `SignupSessions`, `OtpService`, the
endpoint modules, `meta_whatsapp_rs::typst::Renderer`, the Postgres stores. Not used:
`meta_whatsapp_rs::webhooks::sse` and `BroadcastSink` (the service needs resume and
cross-replica fan-out, [§4.5](#45-live-updates-sse)).

`InboxSink` is the library's, with every recording switch on, so the
service's conversation store records what the library records. Since
roadmap L7 that is, besides messages, statuses and the coexistence
feeds: the calls that reopen the window and the customer's standby
messages (window events), thread ownership from handovers and standby
copies, and identity links (an inbound message's phone number to its
BSUID, a number change, `user_id_update`). Every event the service owns
reaches `InboxSink`, the `standby_observed` and `thread_control_changed`
types included, which are tenant events in the outbox since roadmap M2d
([D25](#10-decisions)): the tenant polls the events the inbox recorded
from, and M2a's routes answer what the inbox made of them (a standby copy
opens the window without an unread message; a `control_taken` refuses a
free-form reply `409 thread_owned_elsewhere`, unless the number's
`ReplyChecks` turn the check off). A handover costs up to 16 `identity_links` reads, then, when no
link leads anywhere, one `identities` read and one `contact` read per
identity, all inside the webhook request. No
test of the service depends on these records today: the M1 send route
does not use `Inbox`; M2a's routes will.

### 2.2 Configuration and storage

Environment variables, with a `<NAME>_FILE` variant for every secret; an
optional TOML file (`WA_SERVER_CONFIG`) for non-secrets. The list is in
[§7.2](#72-environment). **The service refuses to start** on a blank or
missing app secret or verify token (in its configuration, before any
handler exists, with the blank test the library's `WebhookHandler::check`
makes), a missing vault key or a pepper under 32 bytes with Postgres, memory storage outside
`WA_SERVER_ENV=development`, identical public and internal binds, a
plain-`http` `WA_GRAPH_ENDPOINT` outside development (every token
travels to it), a vault key id used twice (`WA_VAULT_KEY_ID`'s and
`WA_VAULT_PREVIOUS_KEYS`': records sealed with one of the two keys
could never be opened), or Solution Partner mode without its
credentials.

A shared backend is required outside development: Postgres, the only one
built besides memory (memory stores and a throwaway vault key only in
development; another database is a `Backend` of the core's ports,
[§8.2](#82-the-backend-bundle-is-the-unit-of-swapping)). Library tables keep their `wa_` prefix; the
service's use `wa_server_` and their own migration history:

| Table | Holds |
| --- | --- |
| `wa_server_tenants` | id, name, status, settings (OTP sender and template, limits) |
| `wa_server_api_keys` | key id, SHA-256 of the secret, kind, tenant(s), scopes, expiry, revocation, last use |
| `wa_server_wabas` | `waba_id` (unique) → tenant, credit allocation id (Solution Partner), when the binding began, its generation (attaches) and when the WABA last left another tenant (S2b) |
| `wa_server_waba_tombstones` (S2b) | `waba_id` (unique) → the tenant its last unbinding was from (null once deleted), when, and when it had last left another tenant before |
| `wa_server_numbers` | `phone_number_id` (unique) → tenant, WABA, status (`connected`, `reconnect_required`, `disconnected`), when it came under its tenant and when another tenant last released it (M2h) |
| `wa_server_number_tombstones` (M2h) | `phone_number_id` (unique) → the tenant its last release was from (null once deleted), when, and when it had last been released by another tenant before |
| `wa_server_signup_attempts` | resumable attempts: tenant, WABA, failed step, session info (never the code or PIN); *as built in M3a* (migration 6): one per WABA, its primary key referencing the WABA's binding `ON DELETE CASCADE` (an attempt goes with its binding), the flow's completion (`finish_kind`), `created_at` and `updated_at` |
| `wa_server_vault_records` (M3a) | the token vault's records kept past a binding, which a key rotation walks besides the bindings: `kind` (`waba`: a credit ledger; `business`: a revocation marker) and `id`, kept for good |
| `wa_server_idempotency` | [§5.4](#54-idempotency-keys) |
| `wa_server_events`, `wa_server_event_streams` | the outbox: tenant (null = operator-only), the sequence within its tenant's stream, id, number, WABA, type, JSON and its size; each stream's last sequence and how far it was purged |
| `wa_server_webhook_endpoints`, `…_deliveries` | [§4.4](#44-webhooks-out); *as built in M2c* (migration 9, numbered after M2f's 8): an endpoint's tenant (`ON DELETE CASCADE`), URL, `event_types`, `enabled` and `disabled_reason`, its secret and the previous one sealed under the data key with `previous_secret_until`, its `cursor` in the tenant's stream and `failing_since`; a delivery's endpoint and tenant (both cascading), its event's id, sequence and type (`ping` for a test), `status`, `attempts`, `next_attempt_at`, `give_up_at`, the claim's `lease_until` and `claim`, the last attempt's time, status, error and duration; no body (D105) |

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
  *As built in M2d* (`meta_whatsapp_server_core::number_events`; D80–D85,
  decided by the coordinator): `number_connected` for each number an
  admin attach or an Embedded Signup (completed or resumed) bound, once
  its token is stored (a first attach, or one again: a reconnection);
  `number_disconnected` for each number of a WABA the tenant disconnects
  (`reason: tenant_disconnected`, once Meta unsubscribed the app) or the
  operator unbinds (`operator_unbound`), or Meta's `account_update`
  offboards (`partner_app_uninstalled`, `account_deleted`: M3a's
  `signup::AccountUpdates`), recorded before the binding goes;
  `number_reconnect_required` for each number a `190` marks anew. Their
  `data` is `{event, phone_number_id, waba_id}`, and `reason` on
  `number_disconnected` (more reasons may come within v1); the business's
  ids only. Each is its number's tenant's only, by the outbox's own check
  (the number bound to that tenant under that WABA, its binding begun by
  the second after the binding the event is about began: dated by its
  binding, not by a clock, so the window after a move does not apply),
  and is recorded once per binding generation (the outbox key is its type,
  the number and the binding's epoch). Not built: an event for the
  coexistence offboarding ([§3.4](#34-how-numbers-get-bound), M3b), and
  for a number a refresh drops from its WABA (its binding is gone when
  the refresh lands).
- The service adds `token_expiring` (roadmap M3e; D180–D183): ahead of a stored token's lapse, the tenant
  that holds its WABA gets one event, `{event, waba_id, expires_at}` (ids
  and the expiry, to the second: never a token), once per token and
  expiry. A round every 10 minutes (`meta_whatsapp_server_core::token_expiry`)
  walks the bound WABAs and asks the library's `TokenVault::token_expiry`
  (L11e) for each, reading the record's clear `expires_at`; a token due
  within `WA_SERVER_TOKEN_EXPIRY_LEAD` (7 days) and not yet expired is
  recorded in the outbox under a key made of its type, WABA, tenant and
  expiry, so no marker of its own is stored: a second round, a second
  replica, a token attached again with the same expiry record nothing, and
  a token stored anew (a new expiry) is told again. It is the WABA's
  tenant's only, by the outbox's own check over the WABA's binding. The
  counter is `wa_server_token_expiry_events_total{outcome}`.
- `data` is the library's `WebhookEvent` JSON, pinned by snapshot tests
  over Meta's documented examples: a library change that alters it fails
  the server's tests and forces an API-version decision. A number event's
  is the service's own (above), pinned by the core's
  `number_events::tests::each_number_events_data_is_pinned`.

How M1c settled what the list above leaves open. Four of these choices
are coordinator's decisions of 2026-09-25, which M1c ships and which
stand under the owner's delegation of 2026-09-26; each is a place to
look in review:
the per-tenant sequences (D21), a deleted tenant's events deleted with
it (D22), the hour keyless events are deduplicated for (D23), and a
deleted tenant taken out of platform keys (D24); their rows in
[§10](#10-decisions) say how far each is reversible. The
rest follows from the list above and decides nothing for the owner:

- **Types are an allow-list too.** A tenant receives only the event types
  the service reviewed and pinned (`TENANT_EVENT_TYPES` in
  `crates/meta-whatsapp-server-core/src/events.rs`); `unknown`, `unparsed`, `partner_solution_updated`
  and any type a later library adds are
  operator-only rows until the service lists them in `TENANT_EVENT_TYPES`
  (a test reads the library's `WebhookEvent::kind` and fails on an
  unclassified one; promoting a type is additive, the reverse is not).
  The types the library's webhook conformance sweep typed (PR #17:
  Conversation Routing's `standby_observed` and `thread_control_changed`,
  the Marketing Messages API's `user_action_reported`) were operator-only
  until roadmap M2d made them tenant-visible (D25, acceptance test M2.6),
  routed as every event naming a number is, the binding epoch and D36's
  rules included. `user_action_reported` names neither the user nor the
  message (`marketing-messages/track-click-events`: the action, its time,
  the clicked component, and Meta's `click_id`, `tracking_token` and
  `product_id`); its `data` is served whole, as every type's, and no log
  line holds it (M1.7's capture covers the three types).
- **Ownership is by the bindings, number first, since before the
  event.** An event naming a number (an untyped change: the number its
  raw `metadata` names, which the inbox reads a `history` change by) goes
  to the number's tenant only when the WABA it names is the one the
  number is bound under (stale bindings route to nobody); an event naming
  only a WABA goes to the WABA's tenant. And only when Meta dated the
  event (the message's, status's, call's own time, else the entry's; a
  contact sync's, when its webhook was triggered) no earlier than the
  second that WABA's binding began in, and, when the WABA had a previous
  tenant, in a later second, by a skew margin more
  (`WA_SERVER_MOVE_SKEW_MARGIN`, none by default): a WABA moved from one
  tenant to another does not bring the first one's dated events, which
  Meta retries for up to 7 days, to the second, and after a move an
  event of the binding's own second, which may be either's since Meta
  dates in whole seconds, is operator-only (S2's security review, L3); a
  first onboarding keeps the events of its first second (roadmap S2b).
  Undated events (history syncs, the past on purpose; errors and group
  updates without a date) of a WABA that moved from another tenant are
  operator-only until a window after the move has passed (D36's
  `undated_after_move`, `OperatorOnly` with 7 days and an hour by
  default, `WA_SERVER_UNDATED_AFTER_MOVE`), since the previous holder's
  that Meta redelivers (a history chunk holds message text, an error its
  details) would otherwise reach the new holder; for a WABA no other
  tenant held, and with `CurrentHolder`, they route by the current
  binding. What tells the two apart is the store's record of the WABA's
  last unbinding (its tombstone: [§8.1](#81-a-framework-free-core-with-ports)),
  which counts a tenant deleted and created again under its id as
  another tenant. The inbox records an event only
  when a tenant owns it, so an unowned number's messages never wait in
  the inbox for whoever binds it later, and keeps no synced address book
  contact before roadmap M3b (D38); the outbox records every event.
  Every outbox insert checks the routing again, atomically with the
  insert (a requirement of the core's `Outbox` port since roadmap S2,
  [§8.1](#81-a-framework-free-core-with-ports)): the routing hands the
  insert a typed route guard (`RouteGuard`: the binding the event was
  routed by, the number under the WABA it was bound under, else the WABA;
  the event's date; and the rules after a move), and the insert keeps
  the tenant only while that binding still names that tenant and, read
  then, is one the event may reach by those rules; a row with a tenant
  and no guard is operator-only. On Postgres the insert reads that binding under a `FOR
  KEY SHARE` lock, so an unbinding (and with it a deletion of the tenant)
  waits for the insert to commit; the memory outbox checks under the lock
  of the store whose bindings it reads. So an event routed just before
  its WABA moved to another tenant, or before its tenant was deleted and
  created again under the same id and bound again, is operator-only. An
  undated event (an error, a history chunk, an undated group update)
  has no date to compare: within the window after a move (the tenant
  created again in that last race included) it is operator-only, and
  otherwise it reaches whoever holds the binding when it arrives.
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
- **Each tenant has its own sequence** (security review SR-L2: global
  sequences showed every tenant the platform's volume and timing; D21, a
  coordinator's decision). Sequences increase, with gaps (a
  duplicate that lost a race draws one), and a tenant created again
  under a deleted one's id goes on after the deleted one's last. `sequence`, `after`,
  `next_after`, `410` and `422` are the tenant's; operator-only rows have
  a stream of their own. An insert draws its sequence from its stream's
  row of `wa_server_event_streams`, locked until it commits, so a
  stream's events commit in order and a poll that saw sequence `n` saw
  every event of the tenant before it; other tenants' inserts do not
  wait. A lock waited for over 2 s answers Meta `503`: the outbox reports
  it as the library's typed `StorageError::Busy`, as any backend reports
  its contention (roadmap S2), and the webhook pipeline answers `503` for
  that, whatever the database.
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
| vault, OTP, signup sessions, dedup, inbox, outbox, keys | shared in Postgres; expiry by the database clock (NTP). The rule for any backend (roadmap S2): one clock all replicas agree on, the database's where it has one, else the service's `Clock`, with its skew between replicas below the shortest lease it measures; the core's `Authorizer` reads the service's `Clock` (key and token expiry), the one the webhook pipeline reads |
| dedup lease (60 s) | a retry meeting a live lease on another replica gets 503 and Meta returns; a crashed replica's lease expires; the sink path (two inserts) stays far below 60 s |
| outbox inserts | each tenant's in turn (its stream's row, locked to the commit); tenants in parallel; at most 4 deliveries recording per replica |
| SSE | per replica, fed from the outbox by `LISTEN/NOTIFY`; `Last-Event-ID` resumes on any replica. *As built in M2b*: each replica holds one `LISTEN` connection of its own (outside the pool), a webhook recorded on any replica `NOTIFY`s the deployment's channel (`wa_server_events`) once its outbox row committed, and each stream reads the outbox after its own cursor when woken and every 15 s whatever it heard, so a notification lost delays an event and loses none ([§4.5](#45-live-updates-sse)) |
| webhooks-out | workers on every replica claim rows with `FOR UPDATE SKIP LOCKED` and a lease; no leader. *As built in M2c* (D106–D108, decided): every replica fans the outbox out to each enabled endpoint (its cursor moved from the value it read, in the transaction that records the deliveries, so two replicas record an event once), then claims each tenant's oldest due delivery under a 60 s lease, at most one a tenant a claim, none of an endpoint (4 in flight) or a tenant (8) at its bound on this replica (32 in all; the bounds and the lease are settings, [§7.2](#72-environment)); a settlement names its claim. Woken by the notifier, an attempt's end, and every second. The times compared are the dispatcher's clock's (the service's `Clock`), not the database's: replicas' clocks within the lease of each other |
| rate limits | token buckets per replica (limit ÷ replicas); a shared limiter only if needed |
| API key cache | none in v1: every request reads its key, so a revocation or suspension holds on the next request, on every replica (a cache of at most 30 s, purged by a revocation `NOTIFY`, if the key reads ever cost too much) |
| housekeeping (the outbox, inbox and idempotency purges, the library's expired key/value rows) | any replica, one at a time: each round takes one `LeaderLock` turn named `housekeeping`, a lease of 5 minutes (`HOUSEKEEPING_LEASE`; on Postgres the advisory lock `HOUSEKEEPING_LOCK`, held by a transaction whose session the server ends past the lease), and runs the purges under it (since M2a the inbox's too: `ConversationStore::apply_retention`, the retention the store was built with); a replica without the turn skips that round. The purges take no lock of their own and are safe to run twice at once, so a round that outlives its lease and overlaps the next replica's does no harm (roadmap S2). A replica of the release before S2, whose purges took `HOUSEKEEPING_LOCK` themselves, takes turns with this one during a rolling deploy |
| token-expiry round (M3e) | any replica, one at a time: a `LeaderLock` turn named `token_expiry`, leased 5 minutes, every 10 minutes. The turn only spares the walk: the event is recorded once whatever the lock does (its outbox key), so a round that overlaps another's, or two replicas with clocks a little apart, tell nobody twice. The lead may not exceed `WA_SERVER_OUTBOX_RETENTION` (the start is refused otherwise): the row holding the key must outlive the lapse |
| migrations | at start, through the backend's `SchemaMigrator` (on Postgres, under the library's lock and the service's `MIGRATION_LOCK`); expand-then-contract so rolling deploys can mix versions (migration 5, M2h's, is metadata only: no table rewrite, and a number row a replica of the release before it inserts reads as moved, its cut at its insertion; one it deletes leaves its tombstone through the migration's trigger; migration 7, M2e's, adds `wa_server_events.media_id` without a default and its partial index: no table rewrite, and a row a replica of the release before it inserts records no media id, so its media keeps Meta's `phone_number_id` check). One exception predates the service: the library's migration 3 (lossless message content) converts in one step and needs older writers stopped first, then `migrate` run once from a one-off job (`docs/guides/production.md`); it runs before the service's first deploy, so no service rollout crosses it |

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
`webhooks`, `signup`, `otp`, `numbers`, since roadmap P2 `payments`, and
since roadmap M2f `erasure`, which no other scope grants, and since
roadmap M5d `flows`, which `templates` does not grant, and since roadmap
M5f `groups`, and since roadmap M5e `calls`, and since roadmap M5j `partners`),
look like `wak_<key id>_<32 random
bytes, base62>` (the prefix lets secret scanners find leaks) and are stored
as the key id plus SHA-256 of the secret, compared in constant time (the
examples' scheme; random 256-bit secrets need no slow hash). The digest is
not peppered: whoever can write the keys table can plant a key, a limit
accepted with the database's own access control. Shown once;
several active per tenant; rotate by create, deploy, revoke. The first admin
key comes from the CLI (`meta-whatsapp-server admin create-admin-key`), not the
environment.

**Decision (D2): credential model.** (a) Tenant keys only: the
CMS keeps one key per merchant, minted at onboarding; (b) platform key plus
header only; (c) both. A platform key is as powerful as every tenant it may
name, but the per-tenant checks still stop a CMS bug from crossing tenants.
*Recommendation: (c).*

### 3.3 Authorization order

Every tenant route, through the core's `Authorizer`
(`meta_whatsapp_server_core::authz`), which the axum adapter calls from
its middleware and its typed extractors. Only an `Authorizer` makes a
`Caller`, an `AdminCaller`, an `OwnedNumber` or an `OwnedWaba`; the
owned ones are the only way a handler obtains a token; and each works
only with the `Authorizer` that made it
([§8.1](#81-a-framework-free-core-with-ports)):

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
  cannot disconnect: the operator's path is the admin unbind. *As built
  in M2d*: the disconnect records `number_disconnected` (`reason:
  tenant_disconnected`) for each of the WABA's numbers once Meta
  unsubscribed the app, before it deletes anything (an outbox that fails
  is `503`, nothing deleted; a repeat records each event once), and in
  Solution Partner mode before the revocation too. *As built
  in M3a*: in Solution Partner mode the disconnection revokes the credit
  line between the unsubscription and the deletion (the library's
  `EmbeddedSignup::offboard`, then the binding the capability was made
  from; a revocation that fails deletes nothing, `step:
  revoke_credit_line`). The webhooks are acted on by the core's
  `signup::AccountUpdates` (the pipeline's `AccountActions`, after the
  event is recorded): `PARTNER_APP_UNINSTALLED` of the deployment's own
  app (`partner_app_id` is `WA_APP_ID`) and `ACCOUNT_DELETED` offboard
  (revoke first in Solution Partner mode, from the owner the vault
  recorded, else the webhook's `owner_business_id`; then the token; then
  the bindings), `PARTNER_REMOVED` revokes at once in Solution Partner
  mode (D14) and leaves the token and the bindings; each is confirmed
  with Graph first ([§6](#6-security), D72). Since M2d the offboarding
  records `number_disconnected` for each of the WABA's numbers once
  Graph confirmed, before anything is revoked or deleted (`reason`:
  `partner_app_uninstalled` or `account_deleted`; an outbox that fails
  leaves the merchant whole, and its `Busy` is redelivered), and the
  `account_updated` event reaches the tenant too. A partner disconnection
  deletes only the token and the binding it read before the
  revocation's calls (a WABA moved meanwhile keeps its new holder's). In
  Solution Partner mode a tenant's deletion revokes each of its WABAs'
  lines the same way; the admin unbind revokes nothing (D75).
- **Admin unbind** (`DELETE /v1/admin/wabas/{waba_id}/binding`, D4): with
  the stored token, if usable, unsubscribe the app, at best (Meta refusing
  does not stop it); then delete the vault entry, then the bindings. A
  token no tenant can reach serves nothing and widens what a database and
  vault key compromise exposes. *As built in M2d*: `number_disconnected`
  (`reason: operator_unbound`) for each of the WABA's numbers, for the
  tenant that held it, before the token and the bindings go (with a token
  or without one); and the admin attach records `number_connected` for
  each number it bound once the token is stored, before it subscribes the
  app.

**Decision (D3): browser access.** (a) Never browser-facing: the
CMS relays signup calls and live events (an SSE relay per open inbox, or
webhooks-out into its own realtime channel); (b) short-lived number-scoped
stream tokens plus a CORS allow-list, so browsers open SSE directly: one
hop fewer, but the service is exposed beyond `/webhooks/meta`.
*Recommendation: (a) in v1*; (b) can be added later without breaking
anything.

**Decision (D4): one WABA, several tenants (OQ #6).** (a) Refuse
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
| `GET /v1/numbers/{pn}` | live details: display number, verified name, quality, name status, throughput, *since M5c3* the messaging limit tier (`GET /{pn}?fields=…,messaging_limit_tier`, `NUMBER_FIELDS`) | → object |
| `GET`, `PATCH /v1/numbers/{pn}/profile` | business profile: about, address, description, email, picture URL, websites, vertical; *since M5c2* (D160, D165) a `profile_picture_upload_id` names one of the number's own completed image uploads (`media-uploads`, below) and the service sends its handle as `profile_picture_handle` (never a handle taken from the caller) | partial profile → profile; 404 an upload of another tenant, another number or an unknown one (nothing asked of Meta); 422 `profile_picture_upload_id` (no id, not a JPEG or PNG, not complete) |
| `POST /v1/numbers/{pn}/display-name` (M5c2, D162, D163) | ask Meta to review a new display name (`POST /{pn}?new_display_name=`); the name changes only once approved *and* the number is registered again (`POST …/register`); audited `display_name_change_requested`; `Idempotency-Key` | `{new_display_name}` (trimmed, 1 to 256 characters, no control character) → `202 {new_display_name}`; 422 `new_display_name`; Meta's refusals (its 10 changes per 30 days among them) |
| `GET /v1/numbers/{pn}/display-name` (M5c2, D162) | the display name and the review of the requested one (`GET /{pn}?fields=verified_name,name_status,new_display_name,new_name_status`) | → `{verified_name, name_status, new_display_name, new_name_status}`, each `null` when Meta answers none |
| `GET`, `PATCH /v1/numbers/{pn}/settings` (M5c3, D190, D191) | the number's settings (`GET /{pn}/settings`: local storage, payload encryption's status and its key's fingerprint, **never a key**); one setting changed per call (identity key check, local storage, search visibility, security notifications, number-change notices), each one Meta call; audited `number_setting_changed` with the setting's name | `{identity_key_check \| local_storage \| search_visibility \| security_notifications \| notify_user_change_number}` exactly one → `{setting}`; 422 `body`, `local_storage`, `search_visibility` |
| `PUT`, `DELETE /v1/numbers/{pn}/settings/payload-encryption` (M5c3, D191) | payload encryption on with the business's public key / off; the key in the body only, Meta's texts withheld, never replayed after a timeout; audited `payload_encryption_changed` | `{client_encryption_key}` → 204; 422 `client_encryption_key` (not base64, a private key; never quoted) |
| `GET`, `PUT`, `DELETE /v1/numbers/{pn}/username`, `GET …/username/suggestions` (M5c3, D190) | the business username (Meta's format checked first), adopted or changed (`force_transfer` moves it from another number of the portfolio), deleted; the portfolio's reserved suggestions; audited `username_set`, `username_deleted` | `{username, force_transfer?}` → `{username, status}`; 422 `username` |
| `GET`, `POST /v1/numbers/{pn}/official-business-account` (M5c3, D192) | the status; the application (5 to 10 supporting links), not a legal act: audited `official_business_account_requested`, `Idempotency-Key`, never replayed after a timeout | application → `202 {message, updated_status, tracking_id}`; 422 the field |
| `GET`, `PUT /v1/numbers/{pn}/compliance-info` (M5c3, D192) | India's business compliance information; the declaration is **the legal act** (off until `WA_SERVER_LEGAL_ACTS=on`; audited `compliance_info_set`, never what it says) | declaration → 204; 403 `feature_not_available`; 422 the field |
| `GET /v1/numbers/{pn}/health-status` (M5c3) | `GET /{pn}?fields=health_status` | → `{can_send_message, entities: […]}` |
| `GET`, `PUT /v1/numbers/{pn}/conversational-components` (M5c3, D190) | the welcome message flag, ice breakers (4 of 80 characters) and commands (30; 32 and 256 characters); audited `conversational_components_set` | `{enable_welcome_message?, prompts?, commands?}` → 204; 422 the entry |
| `DELETE /v1/numbers/{pn}/contact-book/{bsuid}` (M5c3, D192) | erase one user's entry from the portfolio's contact book at Meta: irreversible, an explicit erasure under scope `erasure`, not gated by `WA_SERVER_LEGAL_ACTS`, audited `contact_book_entry_deleted` (never the BSUID) | → `{deleted}`; 403 `forbidden`; 422 `bsuid` (a BSUID only) |
| `DELETE /v1/wabas/{waba_id}` | disconnect ([§3.4](#34-how-numbers-get-bound)); in Solution Partner mode the credit line revoked after the unsubscribe and before the token and bindings, only those the capability was made from (M3a; audited `credit_line_revoked`); if unsubscribing fails, nothing is deleted and the answer is Meta's error's code and status ([§5.2](#52-codes-and-statuses): `502` when Meta fails, `403` or `409` when it refuses, `504` on a timeout); a tenant whose token no longer works asks the operator for the admin unbind | → 204 |

**Registration and the two-step PIN** (scope `numbers`; roadmap M3d,
decision D6: the PIN is typed per attempt and never stored)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/numbers/{pn}/request-code` | Meta sends the number a verification code; `Idempotency-Key` | `{code_method: SMS \| VOICE, language}` → `200 {phone_number_id, status: code_requested}` | 409 `registration` (verified already); 429 the number's verification attempts |
| `POST /v1/numbers/{pn}/verify-code` | submit the code (hyphens dropped) | `{code}` → `200 {…, status: code_verified}` | 422 `code`; 429 |
| `POST /v1/numbers/{pn}/register` | register for the Cloud API with the PIN (the current one, or the one to set); `Idempotency-Key`, never over the PIN (D67) | `{pin, data_localization_region?}` → `200 {…, status: registered}` | 422 `pin`; 409 `two_step_verification` (wrong PIN), `registration` (Meta's 72-hour limit, not verified); 429 the number's registration attempts |
| `POST /v1/numbers/{pn}/deregister` | deregister: the number stops sending and receiving until registered again; Meta keeps its history; refused for a coexistence number; audited (`number_deregistered`); `Idempotency-Key` | `{confirm: <pn>}` → `200 {…, status: deregistered}` | 422 `confirm` (D68); 409 `registration`; 429 |
| `POST /v1/numbers/{pn}/two-step-pin` | set the PIN (no call turns it off); audited (`two_step_pin_set`) | `{pin}` → `200 {…, status: pin_set}` | 422 `pin` |

Every one acts with the binding's own token ([§3.3](#33-authorization-order)),
answers Meta's code and trace id but never its texts (no `details`, and
none of them in any log: D69), and reads its body only once ownership
holds, so another tenant's number is `404` with nothing read, counted or
asked. A body that does not parse is `422` on `body`, never quoting it;
the bodies holding a PIN or a code have no `Debug`. Register and
deregister share a per-number budget, the code's request and check
another (D66; [§6](#6-security)). None changes the binding: a
deregistered number stays bound and connected, and its sends fail at
Meta (`409 registration`).

**Messages and media** (scopes `send`, `media`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/numbers/{pn}/messages` | send free-form (Meta enforces the window), template or reaction ([§4.3](#43-message-content)) | `{to, type, <type>: {…}, reply_to?, callback_data?}` → `202 {message_id, contacts}`; *since M5a*: `category?`, `ttl_seconds?`, `direct_send_config?` (Direct Send) | 409 `customer_service_window_closed`, `marketing_opted_out`; 422 `template_*`; 429; 502/504 with `may_have_been_sent`; *since M5a*: 422 `invalid_request` on the path of a key the library would drop, 404 for a `flow_id` or `catalog_id` not the number's WABA's (D102), nothing sent; *since M5b*: 404 for a template naming a `catalog_id` not the number's WABA's (D122), nothing sent |
| `POST /v1/numbers/{pn}/messages/{message_id}/read` | blue ticks; optional typing indicator (never replayed) | `{typing_indicator?}` → 204 | |
| `POST /v1/numbers/{pn}/media` | upload, type and size checked first; *since M5c1* (D129, D130), on a deployment that configures a converter (`WA_SERVER_MEDIA_CONVERTER`), an optional `convert` (`ogg_opus`, `static_sticker`, `animated_sticker`, `jpeg`, `png`) converts the file first and uploads the converted bytes as the format's type, named after the file with the format's extension (`type` is then not read); without a converter the form has no `convert` field (`422 body`, as before) and a type Meta does not take is `422 type` | multipart `file`, `type` (`convert`) → `201 {media_id}` | 422 `type`, `convert`, `file`, 413, 502 `media_conversion_failed` |
| `GET /v1/numbers/{pn}/media/{media_id}` | download, SHA-256 verified before the first byte; `?max_bytes=` (default and cap 16 MiB), larger with `?stream=true` | → bytes, `X-WA-SHA256` | 413 `media_too_large`, 502 `integrity`; *as built in M1b*: 422 `media_id` (not digits), 404 for a media id not the number's (asked with `phone_number_id={pn}`; Meta's refusal, 4xx, answered like a missing id); *since M2e* (OQ #43, D77–D79): a media id the service recorded as received on the number (the outbox row of a `message_received` image, video, audio, document or sticker, or an edit into one, routed to the caller's tenant, received from the binding epoch on) is asked as `GET /{media_id}`, without `phone_number_id`, once the number's binding is read again (`503 storage_unavailable` if it moved), then fetched through the same allow-list, limits and verification; one recorded anywhere else (another number or tenant, an operator-only row, before the binding epoch) is `404` with nothing asked of Meta; an id never recorded keeps the check |
| `DELETE /v1/numbers/{pn}/media/{media_id}` | delete | → 204 | *as built in M1b*: looked up first, with `phone_number_id={pn}` (`DELETE /{id}` deletes whatever node an id names): 422 `media_id`, 404 for a media id not the number's or a node that is not that media, nothing deleted; *since M2e* (D78): a media id recorded as received elsewhere is `404` with nothing asked of Meta, and one received on the number keeps the check |
| `POST /v1/numbers/{pn}/media-uploads` (M5c1, D125–D128) | open a resumable upload (Meta's Resumable Upload API): the file's `file_name`, `file_length` and `file_type` (`application/pdf`, `image/jpeg`, `image/jpg`, `image/png`, `video/mp4`) checked first, `Idempotency-Key` | `{file_name, file_length, file_type}` → `201 {upload_id, file_name, file_length, file_type, file_offset: 0, complete: false}`, the `upload_id` the service's own (`up_` and 32 hex characters), never Meta's session id | 422 `file_name`, `file_type`, `file_length`, 413 `media_too_large`, 409 `resumable_upload_not_configured` |
| `POST /v1/numbers/{pn}/media-uploads/{upload_id}?file_offset=N` (M5c1) | a chunk: the raw bytes from `file_offset` to the end of the declared file at most, one of the tenant's transfer slots held meanwhile, no `Idempotency-Key` (the offset is its identity; a chunk is not replayed after a timeout) | bytes → `200 {…, file_offset, complete, handle?}`: the `handle` (a template's `header_handle`; a profile picture's `profile_picture_handle` too, which the profile's route reads from the record by the upload's id, M5c2, D160) with the chunk that completes the file | 422 `file_offset`, `upload_id`, `body`, 413 `media_too_large`, 404 for an upload of another tenant or number, 504 `timeout` (ask the status) |
| `GET /v1/numbers/{pn}/media-uploads/{upload_id}` (M5c1) | where it stands: the offset Meta holds (to resume from), or the handle from the record once complete, with no request to Meta | → `200` as above | 404, 422 `upload_id` |
| `POST /v1/numbers/{pn}/documents` (M4) | render `invoice`, `receipt` or `voucher` from its JSON input (`date` required: the renderer has no clock), upload | `{template, input, date, format}` → `201 {media_id, filename, mime_type}` | 422 on the input |

**Templates** (scope `templates`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `GET /v1/wabas/{waba_id}/templates` | list, `?status=&name=&cursor=`, cached 60 s per WABA (Meta allows 200 management calls an hour per WABA) | → page of `{id, name, language, status, category, components}` | |
| `GET /v1/wabas/{waba_id}/templates/{id}` | one | → template | *as built in M1b*: 422 `id` (not digits); the id's name read, then the id looked for in the WABA's own list of that name (5 pages of 100 at most): 404 for another WABA's template, or one past those pages |
| `POST /v1/wabas/{waba_id}/templates` | create from Meta's JSON shape (`TemplateDefinition` deserializes it), validated locally | → `201 {id, status, category}` | 422 `template_rejected`; 409 `template_limit_reached`; *as built in M1b*: 422 `invalid_request` on a key `TemplateDefinition` would not send (never dropped; the shapes it cannot carry are listed in the guide's Templates section); *since M3c's review*: 422 `invalid_request` on the path of a `zero_tap_terms_accepted: true` (`components[2].buttons[0].zero_tap_terms_accepted`), wherever it is: it accepts Meta's zero-tap terms, a legal act no tenant's request takes ([§10](#10-decisions), "Legal acts"; it reached Meta from M1b until then) |
| `POST /v1/wabas/{waba_id}/templates/authentication` (M3) | copy-code, one-tap or zero-tap, several languages; *as built in M3c*: Meta's bulk management (`upsert_message_templates`), its fields flattened (`name`, `languages`, `otp_type`, `supported_apps`, `add_security_recommendation`, `code_expiration_minutes`, `message_send_ttl_seconds`), copy code and one-tap only, an `Idempotency-Key` as on a creation | → 201; *as built*: `{data: [{id, status, language}]}` | *as built*: 422 `invalid_request` on `otp_type` for `zero_tap` (accepting Meta's zero-tap terms is a legal act, the owner's: roadmap M3g) |
| `DELETE /v1/wabas/{waba_id}/templates?name=[&id=]` | every language of a name, or one | → 204 | *as built in M1b*: with an id, looked for in the WABA's own list of that name first: 404 when it is not there, nothing deleted |
| `PATCH /v1/wabas/{waba_id}/templates/{id}` (M5b) | edit: Meta's `POST /{TEMPLATE_ID}` body (`category`, `components`, `message_send_ttl_seconds`, …), the library's `TemplateEdit` | Meta's JSON → 204 | the id looked for in the WABA's own list of its name first (as a get): 404, nothing edited; 422 `invalid_request` on a key the library would not send, on `name` (not editable), on a `zero_tap_terms_accepted: true`, on `edit` (nothing to change); not replayed on a timeout (an approved template's edit counts against its limit) |
| `POST /v1/wabas/{waba_id}/templates/{id}/unpause` (M5b) | unpause (`templates/template-pausing`) | → 204 | the same lookup: 404, nothing unpaused |
| `GET /v1/wabas/{waba_id}/templates/{id}/compare?template_ids=&start=&end=` (M5b) | compare two templates (`templates/template-comparison`) | → `{data: [{metric, type, order_by_relative_metric?, number_values?, string_values?}]}` | both ids looked up: 404 for either not the WABA's, nothing compared; 422 on `template_ids` naming `id` itself |
| `POST /v1/wabas/{waba_id}/templates/archive`, `…/unarchive` (M5b) | archive or unarchive 1 to 100 templates (`templates/template-archival`) | `{hsm_ids}` → `{archived_templates, failed_templates}` (`unarchived_templates`) | every id found in one walk of the WABA's list first (`fields=id`, 100 a page, 10 pages at most; an unarchive walks `status=ARCHIVED`; D116): 404, nothing done; Meta's partial failures answered as Meta wrote them |
| `GET`, `POST /v1/wabas/{waba_id}/templates/library` (M5b) | browse the Template Library (`?search=&topic=&usecase=&industry=&language=&name=`, the first page: the page documents no cursor); create a template from an entry (Meta's body, the button inputs an array of objects) | → `{data: [LibraryTemplate]}`; Meta's JSON → `201 {id, status, category}` | the creation takes an `Idempotency-Key`; 422 on a key the library would not send and on a `zero_tap_terms_accepted: true` in a button input |
| `POST /v1/wabas/{waba_id}/templates/migrations` (M5b) | recreate another of the tenant's WABAs' templates here (`templates/template-migration`) | `{source_waba_id, page_number?, count?, template_ids?}` → `{migrated_templates, failed_templates}` | the source opened as the caller's own WABA (another tenant's is 404 before its token is read; D117), its `template_ids` walked with its token: 404, nothing migrated; an `Idempotency-Key` |
| `POST /v1/wabas/{waba_id}/template-groups`; `GET`, `PATCH`, `DELETE /v1/wabas/{waba_id}/template-groups/{group_id}` (M5b) | template groups (`templates/template-groups`): create, read (with the first page of its templates), rename, describe, add or remove templates, delete | Meta's JSON → `201 {id}`; → `TemplateGroup`; Meta's JSON → 204; → 204 | a group read first, acted on only when its `whatsapp_business_account` is the path's WABA (D118): 404, nothing changed; the templates a creation or an update adds walked as an archive's; a creation takes an `Idempotency-Key`; no list of a WABA's groups (no page documents one) |

Review results arrive as `template_status_updated` events.

**Groups** (scope `groups`; *as built in roadmap M5f*, `groups/reference`,
`groups/webhooks` and `analytics`, "Group analytics"), on a number of the
caller's tenant, with the WABA's token. **A group is the number's, or it
does not exist** (the design's M5.2, D150): the service's records of the
number's groups decide it before the token is opened, so another tenant's
group, another number's, an unrecorded one and an id no group has are all
`404 not_found` with the vault unread and Meta not asked.

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/numbers/{pn}/groups` (M5f) | ask Meta to create a group (`POST /{pn}/groups`); asynchronous: Meta's `group_lifecycle_update` event carries the group's id and invite link, and makes the group the number's here | `{subject, description?, join_approval_mode?}` → `202 {request_id}` | 422 on `subject` (1 to 128 characters trimmed), `description` (2,048), `join_approval_mode` (`approval_required` or `auto_approve`); an `Idempotency-Key` (D153) |
| `GET /v1/numbers/{pn}/groups?limit=&after=&before=` (M5f) | the number's active groups (`GET /{pn}/groups`), each recorded as the number's | → `{data: [{id, subject, created_at}], next_after, previous_before}` | 422 on `limit` (1 to 1,024) |
| `GET`, `PATCH`, `DELETE /v1/numbers/{pn}/groups/{group_id}` (M5f) | read (`?fields=`, the six scalar fields by default), change the subject and/or description, delete (the participants and the business removed: Meta documents no way to leave) | → `Group`; `{subject?, description?}` → 204; → 204 | 404 when the number does not hold the group (D150), or Meta no longer has it; 422 on `update` (nothing to change) |
| `GET`, `POST /v1/numbers/{pn}/groups/{group_id}/invite-link` (M5f) | the invite link; replace it, every previous link stopping to work | → `{invite_link}` | the link is a credential: in the answer and nowhere else |
| `GET /v1/numbers/{pn}/groups/{group_id}/participants` (M5f) | the participants (`GET /{group_id}?fields=participants,total_participant_count`) | → `{participants: [{wa_id, user_id, parent_user_id, username}], total_participant_count}` | |
| `POST /v1/numbers/{pn}/groups/{group_id}/participants/remove` (M5f) | remove 1 to 8 participants (`DELETE /{group_id}/participants`: a body on a `DELETE`, so the service takes a `POST`, D152) | `{users: [{phone} or {wa_id} or {user_id}]}` → `{partial}` | 422 on `users[i]…`, never quoting a value; Meta's code and kind, never its text (it may quote a participant) |
| `GET /v1/numbers/{pn}/groups/{group_id}/join-requests?after=&before=` (M5f) | the open join requests | → `{data: [{join_request_id, wa_id, user_id, parent_user_id, username, created_at}], next_after, previous_before}` | |
| `POST /v1/numbers/{pn}/groups/{group_id}/join-requests/approve`, `…/reject` (M5f) | approve (`POST`) or reject (`DELETE /{group_id}/join_requests`) 1 to 100 requests (D154) | `{join_request_ids}` → `{done, failed: [{join_request_id, reasons}], reasons}` | a partial success is a `200` with the failures listed, as codes and kinds |
| `GET /v1/numbers/{pn}/groups/{group_id}/analytics?start=&end=&metrics=&after=` (M5f; parity row 81) | the group's analytics, day by day (`GET /{waba_id}/group_analytics`, the number's WABA, the one group Meta supports), RFC 3339 `start` within 90 days, `metrics` among `SENT`, `DELIVERED`, `READ`, `PARTICIPANTS_JOINED`, `PARTICIPANTS_LEFT` | → `{data: [{start, end, sent, delivered, read, joined, left}], next_after}` | 422 on `start`, `end`, `metrics` |

The changes (a creation, a settings change, a deletion, an invite link's
reset, a removal, an approval, a rejection) are the caller's audit events
by count and outcome (D155). Not offered: pinning (a send, M5a), the
picture (a multipart upload), adding participants (Meta: join through the
invite link), deleting the invite link, leaving (no call), communities
(none documented) (D156).

**Calling** (scope `calls`; *as built in roadmap M5e*, `calling`,
`calling/call-settings`, `calling/user-initiated-calls`,
`calling/business-initiated-calls`, `calling/user-call-permissions`,
`calling/call-recording`, `calling/call-transcription` and
`reference/whatsapp-business-phone-number/calling-api`), on a number of the
caller's tenant, with the WABA's token. Signalling only: WebRTC media is the
integrator's stack, and an SDP is passed through unparsed. **A call is the
number's, or it does not exist** (the design's M5.2, D210): Meta documents
no read of a call by its id, so the service's records of the number's calls
(written by the number's `calls` events and by the call it places, expiring
after a day without an event, D212) decide it before the token is opened,
and another tenant's call, another number's, an unrecorded or expired one
and an id no call has are all `404 not_found` with the vault unread and Meta
not asked. *Decided* (D210 to D219, coordinator, 2026-11-06).

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `GET`, `PATCH /v1/numbers/{pn}/call-settings` (M5e; parity rows 75, 134) | the `calling` object of `GET`, `POST /{pn}/settings`: status, call icon visibility and countries, call hours (replaced as a whole), callback permission, SIP, audio codecs, voicemail; the read adds Meta's restrictions | Meta's object (strict: a key it does not have is 422) → `204`; → the object and `restrictions` | the SIP server's password is never asked for (`include_sip_credentials` is never sent) and never answered (D214); a voicemail announcement's `announcement_media_id` is looked up as the number's own media (`GET /{media_id}?phone_number_id={pn}`) and Meta refusing it is 404, nothing changed (D211); 422 on `update` (nothing to change), on a value the pages do not list and on what the library checks; audited by section name (D216) |
| `GET /v1/numbers/{pn}/call-permissions?phone=&user_id=` (M5e) | whether the number may call a user, and what it may do next (`GET /{pn}/call_permissions`: `user_wa_id` or `recipient`) | → `{status, expiration_time, actions: [{action_name, can_perform_action, limits}]}` | the user is a phone number in E.164 with its `+` and/or a BSUID or parent BSUID (both: Meta uses the phone); 422 on `phone`, `user_id`, `user` (neither) |
| `POST /v1/numbers/{pn}/calls` (M5e; parity rows 134, 135) | place a call (`POST /{pn}/calls`, `action: connect`) with the caller's SDP offer | `{to: {phone?, user_id?}, sdp_offer, biz_opaque_callback_data?, recording?, transcription?}` → `201 {call_id}` | an `Idempotency-Key` (D218); never replayed on a timeout; the call becomes the number's at once; 422 on `to`, `sdp_offer` (32 KiB, D213), `biz_opaque_callback_data` (512), `recording`, `transcription`; Meta's `138006` (no call permission) answered as its code and kind, never its text |
| `POST /v1/numbers/{pn}/calls/{call_id}/pre-accept`, `…/accept` (M5e; parity row 74) | answer a user's call (`action: pre_accept`, `accept`) with the caller's SDP answer; `accept` may add `biz_opaque_callback_data`, `recording`, `transcription` | `{sdp_answer}`; `{sdp_answer, biz_opaque_callback_data?, recording?, transcription?}` → 204 | 404 when the number does not hold the call (D210); never replayed; audited |
| `POST /v1/numbers/{pn}/calls/{call_id}/reject`, `…/terminate` (M5e; parity row 74) | reject a user's call; end a call in either direction (`action: reject`, `terminate`) | → 204 | as above; terminate also after the media ended, for an accurate price |
| `GET /v1/wabas/{waba_id}/analytics/calls?start=&end=&granularity=&…` (M5e; parity row 107) | calls made and received (`GET /{waba_id}?fields=call_analytics.…`), scope `analytics` (D215) | → `{data_points: [{start, end, count, cost, average_duration, phone_number, country, direction}]}` | the range as the other analytics' (D145); `directions` `USER_INITIATED`, `BUSINESS_INITIATED`; `dimensions` `phone`, `direction`, `country` (lowercase); `metric_types` `COUNT`, `COST`, `AVERAGE_DURATION` |

`recording` and `transcription` are **the caller's choice for each call**:
the service sends them when the request sets them and never otherwise, and
decides nothing about the announcement or the consent it asks of both
participants (Meta plays it; a participant who does not consent ends the
call). The audit event of a placed or accepted call says whether either was
enabled (D217). Not offered (D219): the SIP password, an announcement
upload with `use_case=call_voicemail_announcement`, the call permission
request (a message: a send), call buttons and deep links, the reference's
`media_update` action (no page documents it).

**Flows** (scope `flows`, rate class `templates`; *as built in roadmap
M5d*, `flows/guides/flowsapi`, `flows/guides/lifecycle`; D132, D135). Each route is one of the library's `Flows` and `Flow` calls with the
tenant's WABA token, behind the authorization order; a Flow id is the
WABA's, or it does not exist (the design's M5.2, D135):

| Route | What | Request → answer | Errors and rules |
| --- | --- | --- | --- |
| `POST /v1/wabas/{waba_id}/flows` | create a Flow (a draft, or published with it) | `{name, categories[1..8], clone_flow_id?, endpoint_uri?, flow_json? (the object, or Meta's text), publish?}` → `201 {id, validation_errors[]}` | 422 `name`, `categories[i]`, `clone_flow_id`, `endpoint_uri` (`https`, no credentials), `flow_json`, `body`; a `clone_flow_id` is read first and must be the WABA's (404, nothing created); an `Idempotency-Key`; not replayed on a timeout |
| `GET /v1/wabas/{waba_id}/flows?cursor=` | the WABA's Flows | → `{data: [Flow], next_cursor?}` (Meta's default fields) | Meta's list takes a cursor and no page size: `limit` is 422 `query` |
| `GET /v1/wabas/{waba_id}/flows/{flow_id}` | one | → `Flow {id, name, status, categories, validation_errors, json_version, data_api_version, endpoint_uri}` | 404 for another WABA's Flow, an id Meta refuses or a node that is not a Flow |
| `PATCH /v1/wabas/{waba_id}/flows/{flow_id}` | name, categories, endpoint | `{name?, categories?, endpoint_uri?}` → 204 | the Flow read first; 422 for nothing to change; `application_id` (the Meta app connected to a Flow with an endpoint) is not offered: the library's `UpdateFlow` lacks it |
| `PUT /v1/wabas/{waba_id}/flows/{flow_id}/json` | replace the Flow JSON | the Flow JSON itself (an object, at most 2 MiB), sent to Meta as it arrived → `{success, validation_errors[]}` | the Flow read first; Meta's validation errors are the answer, never an error; 413 over 2 MiB |
| `POST /v1/wabas/{waba_id}/flows/{flow_id}/preview` | the web preview link | `{invalidate?}` → `{preview_url, expires_at}` | the Flow read first; the link is a secret (its token), in no log |
| `POST …/flows/{flow_id}/publish`, `…/deprecate`; `DELETE …/flows/{flow_id}` | publish (irreversible: a published Flow cannot be deleted), deprecate, delete a draft | → 204 | the Flow read first; Meta's refusals (outstanding validation errors or publishing checks, a published Flow's deletion) are its `invalid_parameter` with its `details`; audited |

A Flow's read is the only request made for another WABA's Flow. The Flow
JSON's content is Meta's to validate. Not offered, because the library
lacks them: a Flow's `health_status` and its assets.

**The Flows data endpoint** (the public listener; *as built in roadmap
M5d*, `flows/guides/implementingyourflowendpoint`; D132–D134, D136): `POST /webhooks/flows/{waba_id}`, the address a tenant gives as its
Flow's `endpoint_uri`. Meta's contract, not the integrators': not in the
OpenAPI document, no API key. Its order (the core's
`flows_endpoint::FlowsEndpoint::handle`): a missing or malformed
`X-Hub-Signature-256` is `432` before the body is read; the body (at most
256 KiB, 15 s) is read; the signature over the raw body is checked in
constant time against every app secret (`432`), before any record is read,
key loaded or byte parsed; the WABA's tenant (unbound, or suspended: `404`)
and the deployment's Flow key (`WA_SERVER_FLOWS_PRIVATE_KEY`; none: the
route is `404`); decryption (RSA-OAEP, AES-128-GCM; every failure `421`,
one answer for all: Meta's client then downloads the public key again);
then the action: `ping` is answered `{"data":{"status":"active"}}` and an
error notification `{"data":{"acknowledged":true}}` by the service, and a
data exchange (`INIT`, `BACK`, `data_exchange`) by the integrating app
(the `FlowHandler` port: `503` when unavailable or past 5 s, `500` when it
failed); the answer is sealed with the request's key and the flipped IV and
sent as `text/plain` (`200`). At most 32 requests are answered at once on a
replica (`503` before the body is read). The outcome, the WABA, the action
and the screen are logged; a user's `data`, the `flow_token`, the Flow JSON
and the sealed text never are.

**Payments** (scope `payments`; *as built in roadmap P2*, India's pages
`payments/payments-in/onboarding-apis` and `payments/payments-in/pg`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `GET /v1/wabas/{waba_id}/payment-configurations` | the WABA's payment configurations (Meta documents no pages) | → `{data: [PaymentConfiguration]}` | 403 `permission` until the WABA accepted the WhatsApp Payments terms (Meta's `131005`, its link in `details`) |
| `POST /v1/wabas/{waba_id}/payment-configurations` | create one, checked first (a name of 1 to 60 characters, a provider among `upi_vpa`, `razorpay`, `payu`, `zaakpay`); an `Idempotency-Key` | Meta's body → `201 {oauth_url, expiration}` (a gateway's link, a credential for the merchant; `null` for `upi_vpa`, and in a replay: the kept answer has no link) | 422 `invalid_request`; 504 with `may_have_been_sent` |
| `GET`, `DELETE /v1/wabas/{waba_id}/payment-configurations/{configuration_name}` | one, by name; remove it | → `PaymentConfiguration`; → 204 | 404 for a name the WABA has none of (Meta's empty `data`, or its refusal of the name) |
| `POST /v1/wabas/{waba_id}/payment-configurations/{configuration_name}/oauth-link` | a new OAuth link for a gateway configuration; an `Idempotency-Key` | `{redirect_url?}` (optional) → `{oauth_url, expiration}`, the link never kept | |
| `GET /v1/numbers/{pn}/payments/{configuration_name}/{reference_id}` | the payment of an order: its status, attempts and refunds (Meta asks to read it rather than trust the webhook alone); the configuration looked for in the number's own WABA first | → `{data: [Payment]}` | 404 for a configuration not the WABA's, the payment not asked for; 502 `upstream` for Meta's own lookup error |
| `POST /v1/numbers/{pn}/payments/refunds` | refund an order paid through a gateway, after the same configuration check; never replayed; an `Idempotency-Key`, required (`428 idempotency_key_required` without one) | Meta's body (`reference_id`, `speed?`, `payment_config_id`, `amount` with `currency` `INR` and `value`, `offset` as strings or numbers) → `201 {id, status, speed_processed}` | 404 as above, nothing refunded; 504 with `may_have_been_sent` |

The payment messages (`order_details`, `order_status`) are sends
(scope `send`, [§4.3](#43-message-content)), and the payment webhooks
are events: `payment_configuration_updated`, a status of type `payment`
in `status_updated`, Brazil's `payment_method` reply in
`message_received`. A refund, a configuration's creation or removal and
a new OAuth link are the caller's audit events. No route accepts Meta's
terms (AGENTS.md § Decisions): the merchant opens Meta's link. Brazil
documents neither configurations nor lookups. India's checkout button
templates and the third party validation are sends too, read through
the library's types since roadmap P1d; the data endpoint link has no
route yet (roadmap P2b).

**Commerce, QR codes, analytics and the block list** (scopes `commerce`,
`qr_codes`, `analytics`, `contacts`; *as built in roadmap M5g*, D140–D145)

| Method and path | Does | Notable errors |
| --- | --- | --- |
| `GET`, `PATCH /v1/numbers/{pn}/commerce-settings` | the cart and catalog settings; change one or both (204) | 422 `update` for an empty change |
| `GET /v1/wabas/{waba_id}/catalogs`, `…/{catalog_id}`, `…/{catalog_id}/products` | the WABA's catalog, one by id, its products paged (`limit` 1–100, `cursor`, `return_only_approved_products`) | 404 for a catalog not the WABA's (D141) |
| `POST`, `GET /v1/numbers/{pn}/qr-codes` | create (an `Idempotency-Key`; 140 characters, `PNG`/`SVG`); a page (`limit` 1–25) | 422 on Meta's limits |
| `GET`, `PATCH`, `DELETE /v1/numbers/{pn}/qr-codes/{code}` | read, change the message, delete (audited) | 404 for a code not the number's (D144) |
| `GET /v1/wabas/{waba_id}/analytics/{messaging,conversations,pricing,templates}` | the four analytics; RFC 3339 times, comma-separated filters | 422 on a range or a filter outside D145; 404 for a template not the WABA's |
| `GET`, `POST /v1/numbers/{pn}/blocked-users`, `POST …/blocked-users/unblock` | the list; block or unblock 1–1,000 people (`{users: [{phone?, user_id?}]}`), an `Idempotency-Key`; `200` with `{done, refused, error_code}` | 422 on a phone without its `+`, a parent BSUID, a group |

**Marketing Messages API, max price and welcome message sequences** (scope `marketing`; M5h, D230–D235)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `POST /v1/numbers/{pn}/marketing-messages` | a marketing template through `POST /{pn}/marketing_messages` (`Idempotency-Key`, never replayed after a timeout); a catalog its template names is checked to be the WABA's; Meta's texts withheld (they may quote a parameter) | `{to, template, product_policy?, message_activity_sharing?, bid_spec?}` → `202` as a message send; 404 for a foreign catalog; 422 the field |
| `GET /v1/wabas/{waba_id}/marketing/onboarding` | `GET /{waba}?fields=marketing_messages_onboarding_status` and `…owner_business_info` (two reads) | → `{onboarding_status, owner_business: {id, name, terms_status, terms_time}}` |
| `GET`, `PUT /v1/wabas/{waba_id}/marketing/cloud-api-marketing` | the Cloud API marketing switch (`disable_marketing_messages_on_cloud_api`); the change audited `cloud_api_marketing_set` | `{disabled}` → `{disabled}` / 204 |
| `POST /v1/wabas/{waba_id}/marketing/template-duplicates` | the source found among the WABA's templates (D233), then `POST /{waba}/message_templates` with `optimization_spec`; `Idempotency-Key`; audited `marketing_template_duplicated`; never signs the agreement | `{source_template_id, name, optimization_spec}` → `201 {id, status, category, source_template_id}`; 404 a foreign source |
| `GET /v1/wabas/{waba_id}/marketing/reach-estimates` | `GET /{waba}/reachestimate` | `?countries=IN,US&date_interval=L1D\|L7D\|L14D\|L28D` → `{waba_currency, estimates}` |
| `GET`, `PUT`, `DELETE /v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price` | a template's max price read, set (`optimization_spec`) or returned to rate-card pricing; the template found among the WABA's first (D233); audited `template_max_price_set`, `template_max_price_cleared`; never signs the agreement | `{bid_amount, bid_strategy?, bid_country_multiplier_overrides?}` → 204; 404 a foreign template; 422 the field |
| `POST`, `GET /v1/wabas/{waba_id}/welcome-sequences` | a Click-to-WhatsApp welcome message sequence created (`Idempotency-Key`; audited `welcome_sequence_created`; Meta's texts withheld); the WABA's sequences (`limit` 1–100) | `{name, welcome_message_sequence: {text?, autofill_message?, ice_breakers?}}` → `201 {sequence_id}`; `{data}` |
| `GET`, `PATCH`, `DELETE /v1/wabas/{waba_id}/welcome-sequences/{sequence_id}` | one sequence, changed (`welcome_sequence_updated`) or deleted (`welcome_sequence_deleted`), each after the sequence is found through the WABA's own edge (D233) | 404 a foreign sequence; Meta refuses one an active ad uses |

**Conversation routing** (scope `routing`; *as built in roadmap M5l*, D220–D225)

| Method and path | Does | Notable errors |
| --- | --- | --- |
| `GET /v1/numbers/{pn}/routing-roles` | the roles the number's business holds and its standby grant: `{roles, is_standby}` | 502 with Meta's `100` when routing is not enabled on the number |
| `POST /v1/numbers/{pn}/thread-control/pass` | pass the thread with `user` (exactly one of `phone`, `wa_id`, `user_id`) to `target_role` (Meta's six; absent: the escalation partner), `metadata` optional; an `Idempotency-Key`; `200 {request_id}` | 422 `target_role`, `user…`, `metadata`; Meta's code for a pass on an idle thread or not owned |
| `POST …/thread-control/take` | take it (the escalation partner only); `{user, metadata?}` | Meta's `2494191` for any other caller |
| `POST …/thread-control/release` | release it to idle; `{user}` | Meta's code, never its text |

**Inbox** (scope `inbox`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/numbers/{pn}/conversations` | newest activity first; *as built in M2a*: from the number's binding epoch on (a number whose WABA moved from another tenant lists no conversation whose latest message predates its binding, nor, since M2h, a number Meta migrated from another tenant's WABA any from before it came under the tenant, and shows `last_inbound_at` and `unread` only for a customer who wrote since, `unread` counting only what they wrote since: D10 hides, never purges); *since M2g*, each conversation's `window`, the window route's answer (D63) | → page of `{contact, last_message_at, last_inbound_at, last_text, unread, window}`; *as built*: `{data: [{contact, last_message_at, last_inbound_at, last_text, unread, window: {open, closes_at}}], next_cursor}` |
| `GET /v1/numbers/{pn}/conversations/{contact}/messages` | history, newest first; *as built in M2a*: from the binding epoch on, as the list | → page of `{id, direction, kind, text, payload, status, timestamp, status_at, error}` |
| `GET /v1/numbers/{pn}/conversations/{contact}/window` | the 24-hour window, counting the calls and standby messages that reopen it (library roadmap L7, OQ #32); *as built in M2g* (`get_conversation_window`, read class): `open` is what a free-form reply would meet now, the reply's own checks run with nothing sent (on a number with a cut, a window opened since it; then the library's `Inbox::check_reply`: the thread's owner, the window, a trusted handover), so it never disagrees with the reply's `409`; `closes_at` the recorded window, from the binding epoch on (a window the previous holder's customer opened shows `null`, never its time); asks Meta nothing (D62) | → `{open, closes_at}` (`null`: templates only) |
| `POST /v1/numbers/{pn}/conversations/{contact}/messages` | reply as the merchant to the conversation's contact through `Inbox::reply`; free-form refused locally outside the window (`409 customer_service_window_closed`) or while another app owns the thread (`409 thread_owned_elsewhere`), nothing sent; templates exempt; recorded `accepted`; *as built in M2a*: through `Inbox::send` (what `Inbox::reply` calls), so `reply_to` and `callback_data` pass too; the checks per number (`WA_SERVER_INBOX_REPLY_CHECKS`, the library's `ReplyChecks`); on a number with a cut (the binding epoch), free-form needs a window opened since the cut, else `409 customer_service_window_closed` (the previous holder's customer's window is not the new holder's to answer); an `Idempotency-Key` as on a send | content → `202 {message_id}`; *as built*: `ReplyMessage` (a `SendMessage` without `to`) → `202 MessageAccepted` |
| `POST /v1/numbers/{pn}/conversations/{contact}/read` | reset unread; `notify_customer: true` also marks the latest inbound message read on Meta; *as built in M2g* (`mark_conversation_read`, send class): the body optional (none: `notify_customer` false), unknown fields `422 body`; the receipt (`messages/mark-message-as-read`, the number's token) goes first, for the conversation's latest inbound message only (a revoke's tombstone skipped, among its latest 1000 messages), and never for one dated before the binding epoch or older than Meta's 30 days; a receipt Meta refuses resets nothing; an `Idempotency-Key` as on a send (`Fingerprint::mark_conversation_read`), so a repeat of an answered request sends no second receipt; one answered `503` after its receipt went (the reset failing) releases its key, and its repeat sends the receipt again, for the same message or a newer one (D61) | `{notify_customer?}` → 204 |
| `GET /v1/numbers/{pn}/messages/{message_id}` (M2g) | a message of the number's inbox by id (`ConversationStore::message`, never another number's), with its conversation's contact; *as built in M2g* (`get_inbox_message`, scope `inbox`, read class): one dated before the binding epoch is `404 not_found`, the answer for an id the number never had; asks Meta nothing (D60) | → `{contact, message: {id, direction, kind, text, payload, status, timestamp, status_at, error}}` |

**Erasure** (scope `erasure`; *as built in roadmap M2f*, decisions D10,
D30, D31, D86–D91)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/numbers/{pn}/erasures` (`erase_customer`, rate class `send`) | erase a person on the number, only when called (D10, legal acts): the identities named, closed over the inbox's (`Inbox::identities`, D30), read before anything is deleted; a journal entry written first (the hex HMAC-SHA256 of `<phone_number_id>\|<contact>` under `WA_SERVER_ERASURE_KEY` for each identity, and the time; a journal that fails stops it); then the tenant's own outbox rows of the number naming one of them, never an operator-only row (D89; `wa_server_events.contacts`, migration 8, a GIN index; whole rows); then their inbox records on the number (`Inbox::erase_all`, D31); then the audit event `customer_erased` (ids and counts). Asks Meta nothing; the whole authorization order, the vault included, then the binding read again (`503` if it moved), and a number with a history cut (its WABA or itself moved from another tenant) refused whole (`409 erasure_crosses_binding`, D90: the library's `erase_all` has no time bound). No `Idempotency-Key`: a repeat erases nothing more and answers alike | `{confirm: <pn>, contacts: [1..16]}` → `200 {erasure_id, phone_number_id, erased_at}`, nothing of what was found | 404 another tenant's number; 403 `forbidden` (no scope `erasure`); 409 `erasure_not_configured`, `erasure_crosses_binding`, `number_not_connected`, `reconnect_required`; 422 `confirm`, `contacts`, `body` (never quoting a contact); 503 `storage_unavailable` |

**Events and webhook endpoints** (scopes `events`, `webhooks`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/events` | poll the tenant's events after a sequence of its own, `?after=&types=&phone_number_id=&limit=` (`types`: `KnownEventType`s, comma-separated or repeated; `limit` ≤ 100, 50 by default); a page stops before 8 MiB of `data` (the first event always comes); `410 cursor_expired` below what was purged (retention, or a deleted tenant of the same id); `422` on `after` past the tenant's newest sequence, on `types` for a type a tenant never receives | → `{data: [envelope], next_after}` |
| `GET /v1/events/{id}` | one event in full (for truncated deliveries); *as built in M2b*: the tenant's own only, looked up in its stream, so another tenant's id, an operator-only row's, a purged one and an id of no event's shape (`evt_` and 32 hex digits) are `404 not_found` alike, before any detail | → envelope |
| `GET /v1/events/stream`, `GET /v1/numbers/{pn}/events/stream` | SSE for the tenant, or one number ([§4.5](#45-live-updates-sse)); `429 too_many_streams`; *as built in M2b*: `?after=&types=&phone_number_id=` as `GET /v1/events` takes them (`Last-Event-ID` wins over `after`), `410 cursor_expired` and `422` on the cursor before the stream starts, `503 shutting_down` on a stopping replica; the number's stream needs the number to be the tenant's (step 4 of [§3.3](#33-authorization-order), `404` otherwise) and reads neither the vault nor Meta | `text/event-stream` |
| `POST /v1/webhook-endpoints` | forward events to a URL ([§4.4](#44-webhooks-out)); `422 url` outside the allowed destinations; *as built in M2c* (`create_webhook_endpoint`, scope `webhooks`, send class): `types` at least one `KnownEventType`, each once (`422 types`), the URL checked by its text then its host looked up and every address checked (`422 url`), 10 endpoints a tenant (`409 webhook_endpoint_limit_reached`), the endpoint receiving the tenant's events recorded from its creation on; `409 webhooks_not_configured` without a data key (D112) | `{url, types, tenants?}` → `201 {id, secret}` (shown once); *as built*: `{url, types}` → `201` the endpoint and its `secret` (`tenants` not built: an unknown field, `422 body`) |
| `GET`, `PATCH`, `DELETE /v1/webhook-endpoints[/{id}]` | list, read, change URL, types or enabled, delete; *as built* (`list_webhook_endpoints`, `get_webhook_endpoint`, `update_webhook_endpoint`, `delete_webhook_endpoint`): another tenant's id is `404` like a missing one, before the change is checked; a changed URL is checked again; turning `enabled` on clears the service's `disabled_reason` and resumes from the endpoint's cursor, within the outbox's retention; a deletion takes its deliveries | page of `{id, url, types, enabled, disabled_reason, previous_secret_expires_at, created_at, updated_at}` (never a secret) |
| `POST /v1/webhook-endpoints/{id}/rotate-secret`, `…/test` | new secret (the old one also signs for 24 h); send a `ping` | → `{secret}`; the delivery; *as built* (`rotate_webhook_secret`, `test_webhook_endpoint`): `{secret, previous_secret_expires_at}`; a rotation within the 24 h drops the oldest secret; the ping is a delivery due now that the dispatcher attempts once, answered `202` (its outcome in the deliveries) |
| `GET /v1/webhook-endpoints/{id}/deliveries`, `POST …/{delivery_id}/retry` | attempts (status, duration, next attempt; no bodies); redeliver a failed one | page; 202; *as built* (`list_webhook_deliveries`, `retry_webhook_delivery`): newest first, each `{id, event_id, sequence, type, status, attempts, next_attempt_at, last_attempt_at, last_status, last_error, last_duration_ms, give_up_at, created_at}`, settled ones kept 30 days; a retry makes a failed or delivered one pending, due now, for a new window (a pending one is left), the same `webhook-id` |

`tenants` (a list or `"*"`) is for platform keys; a tenant key's endpoint
receives its own tenant's events. *As built in M2c*: not built (D112):
an endpoint is the tenant's its creator acted as (a tenant key's, or the
`WA-Tenant` a platform key named), and receives that tenant's events
only; a platform integrator makes one per tenant.

**The event types** (`KnownEventType`, the core's `TENANT_EVENT_TYPES`;
*as built in M2d*, 34 types): the library's reviewed `WebhookEvent`
kinds, Conversation Routing's `standby_observed` (a copy of a thread the
business observes without owning it: an inbound message, the owner's
send, a status; never to be answered) and `thread_control_changed`
(`control_passed`, `control_taken`) and the Marketing Messages API's
`user_action_reported` (a click on a marketing message) among them since
M2d ([D25](#10-decisions)), each on the polls, the streams and
`GET /v1/events/{id}` of its number's tenant only; and the service's own
`number_connected`, `number_disconnected` and `number_reconnect_required`
and `token_expiring` ([§2.3](#23-the-event-pipeline)). Types only grow within v1
(`EventType` is `anyOf[KnownEventType, string]`): a client defaults on an
unknown one.

**Partner APIs** (scope `partners`; admin key) (roadmap M5j, D250 to D257)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST`, `GET /v1/admin/partners/business-verifications` | submit a customer business with its documents (a legal act), list the submissions | `{client_business_id, documents[{file_name, content_base64}]}` → `201 {verification_attempts, attempts_left}`; a page | `403 feature_not_available`, `409 signup_not_configured`, `422` |
| `POST`, `GET /v1/admin/partners/solutions`; `GET …/{id}`, `…/{id}/partners`; `GET …/client-businesses` | Multi-Partner Solutions of the deployment's app | `201 {solution_id}`; pages; a solution | `409 signup_not_configured`, `422` |
| `POST …/solutions/{id}/accept`, `…/reject`, `…/deactivation-request`, `…/deactivation-request/accept`, `…/deactivation-request/reject` | a solution's state changes (accept, reject and accepting a deactivation are legal acts) | none → `204` | `403 feature_not_available`, `409`, `422` |
| `GET /v1/admin/partners/users/{user_id}/wabas` | WABAs assigned to a system user | a page of `{id, name}` | `409`, `422` |
| `GET /v1/wabas/{waba_id}/business-verification` | the WABA's verification status | `{status, verified}` | |
| `POST`, `GET /v1/wabas/{waba_id}/solution-migration-intents[/{intent_id}]` | tag the WABA for a solution (a legal act), read the intent back if it names the WABA | `{solution_id}` → `201 {id}`; an intent | `403 feature_not_available`, `404` |
| `POST`, `GET /v1/wabas/{waba_id}/billing-migrations[/{migration_id}]`, `POST …/{migration_id}/resume` | a billing currency or payment method migration (start and resume are legal acts); only the WABA's own migration | `{currency, extended_credit_id?}` → `201 {id, status, …}` | `403 feature_not_available`, `404` |

**Embedded Signup and OTP** (scopes `signup`, `otp`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/signup/sessions` | bind an attempt (15 min) to the tenant; what `FB.init` and `FB.login` need; *as built in M3a*: the library's `SignupSessions::start` (`wa.es.session`, unchanged), `launch_options` the library's `LaunchOptions` as JSON (coexistence's `featureType` when asked), `200`, asks Meta nothing, rate class `send` | `{coexistence?}` → `{state, expires_at, app_id, config_id, graph_api_version, launch_options}` | *as built*: 409 `signup_not_configured` (no `WA_APP_ID` or `WA_ES_CONFIG_ID`) |
| `POST /v1/signup/complete` | redeem for this tenant, then onboard; *as built in M3a*: every input checked first (`422` on `state`, `code`, `pin`, `event`, `body`; 16 KiB), D4 on the claimed WABA before the redemption, then the library's `onboard_with_approval`, whose approval binds the verified WABA and all its numbers (D4 atomically) before anything is stored, subscribed or shared; a `CANCEL` answers `cancelled` with its `current_step` and redeems nothing; an attempt that stopped after its token was stored kept (`resumable: true`); audited `signup_completed` | `{state, code, event, pin?}` → `{status: connected \| cancelled, waba_id, phone_number_ids, steps_completed, needs_coexistence_sync, credit_allocation_id?}` | 403 `stale_attempt`; 409 `waba_owned_by_another_tenant`; 502 `onboarding_failed` (`step`, `resumable`) |
| `GET /v1/signup/attempts` | the tenant's resumable attempts; *as built in M3a*: paged as every list, with `created_at` too, rate class `read` | → `{data: [{waba_id, failed_step, updated_at}]}` | |
| `POST /v1/signup/resume` | rerun the steps after the stored token; *as built in M3a*: the tenant's own attempt only (another tenant's is `404`, nothing read), the library's `resume_with_approval` from the stored token, no code (`OnboardingRequest::for_resume`, the kept completion setting the coexistence steps), its approval checking the binding again before any credit call; the attempt dropped once done, kept at its new step otherwise; audited `signup_resumed` | `{waba_id, pin?}` → like `complete` | 404 `nothing_to_resume`; 502 `onboarding_failed` |
| `POST /v1/admin/signup/hosted` (M3f) | the operator claims a remembered `PARTNER_ADDED` (hosted Embedded Signup) for the tenant it names and onboards it, D201 and D202 (admin key; audited `hosted_signup_completed`) | `{tenant_id, waba_id, owner_business_id, pin?}` → like `complete` | `409 signup_not_configured` unless `WA_HOSTED_SIGNUP=on`; `404 not_found` for no such tenant or no arrival for that WABA and business; `403 tenant_suspended`; `409 waba_owned_by_another_tenant` |
| `POST`, `GET /v1/admin/pre-verified-numbers`; `GET`, `DELETE …/{id}`; `POST …/{id}/code`, `POST …/{id}/verify`; `PUT`, `DELETE …/{id}/reservation` (M3f) | the deployment's pool of pre-verified numbers (admin key; D203 to D205), each call Meta's with the pool's token; reserving a number for a tenant asks Meta nothing |
| `POST /v1/numbers/{pn}/coexistence-sync` | start the contacts and history sync, once, within 24 h (D7) | `{types}` → 202 | 409 `sync_not_allowed` |
| `POST /v1/otp/issue` | send a code with the tenant's authentication template; *as built in M3c*: from the sending number of the tenant's OTP settings, else the default OTP tenant's (D8, [§4.7](#47-otp)), an `Idempotency-Key` as on a send (its fingerprint keyed with the pepper), rate class `otp_issue` | `{phone, purpose?}` → `{status: sent, challenge_id, expires_at}` or `{status: cooling_down \| rate_limited, retry_after_seconds}` | 422 `phone`, `recipient_not_supported`; 502/504 with `may_have_been_sent: true`; *as built*: 409 `otp_not_configured` |
| `POST /v1/otp/verify` | check the typed code; always counts as an attempt; *as built in M3c*: asks Meta nothing and reads no token, rate class `otp_verify`; another tenant's code is `not_found`, the namespace being the caller's tenant | `{phone, purpose?, code}` → `{result: verified \| invalid \| expired \| too_many_attempts \| not_found, attempts_left?}` | 422; *as built*: 409 `otp_not_configured` |

The signup state travels in bodies only (URLs reach access logs). OTP
outcomes are `200` with a status, as the library returns them as `Ok`:
a caller's generic error handling can never swallow `invalid`.

**Admin and operations**

| Method and path | Does |
| --- | --- |
| `POST`, `GET`, `PATCH`, `DELETE /v1/admin/tenants[/{id}]` | create, list, suspend, configure (OTP sender and template, limits), delete (disconnects every WABA; in Solution Partner mode each WABA's credit line revoked first, a failed revocation deleting nothing, D75); *as built in M3c*: the OTP settings are a resource of their own, `GET`, `PUT` and `DELETE /v1/admin/tenants/{id}/otp` (the sending number, which must be one of the tenant's, the template and language, the library's limits; audited), and `PATCH` stays name and status; per-tenant rate limits are not settable |
| `…/tenants/{id}/keys[/{key_id}]`, `/v1/admin/platform-keys[/{key_id}]` | mint, list, revoke keys; platform keys with their allowed tenants |
| `POST /v1/admin/tenants/{id}/wabas`; `GET /v1/admin/wabas/{waba_id}`; `DELETE /v1/admin/wabas/{waba_id}/binding` | attach an own WABA, verified with Meta and subscribed; which tenant holds a WABA, and its numbers; unbind (D4: token deleted too, [§3.4](#34-how-numbers-get-bound); no credit line revoked, D75) |
| `POST /v1/admin/vault/rotate` | re-encrypt every WABA's token under the active key, walking `wa_server_wabas` (the vault cannot list itself), within the request deadline: a walk cut there answers `504 timeout` and is repeated (idempotent), and `meta-whatsapp-server vault rotate` has no deadline. It walks bound WABAs only, which holds every vault record in M1a; M3 keeps records past a binding (credit ledgers of offboarded WABAs, revocation markers), and the walk must cover those too (`TokenVault::rotate` on each such WABA, `rotate_business` on each marker) before an operator may drop an old key. *As built in M3a*: it does: the records kept past a binding are remembered in `wa_server_vault_records` when they may be written (a Solution Partner's approval, an offboarding, a revocation; D71) and walked after the bindings, the answer gaining `records` and `failed_records`; the old key may go once `failed` and `failed_records` are both empty |
| `POST /v1/admin/wabas/{waba_id}/credit-line/reshare` (M3a) | fund again a business whose credit line was revoked: the operator's explicit, audited decision (`credit_line_reshared`; D14, D74), the library's `resume_with_approval` with `reshare_after_revocation` from the WABA's stored token; `409 signup_not_configured` outside Solution Partner mode |
| `POST /v1/admin/businesses/{business_id}/max-price-agreement` (M5h, D231) | sign Meta's max price beta agreement for a business portfolio: **a legal act**, the operator's explicit, audited decision, off until `WA_SERVER_LEGAL_ACTS=on` (`403 feature_not_available`, nothing asked of Meta, before the body is read); one `POST /{business}/max_price_agreements` with the operator's own `token` (body only, never stored), never replayed; audited `max_price_agreement_signed` with the admin key's id and the business, never the signer; no other route signs it (M5.3) |
| `GET /livez`, `/readyz`, `/metrics`, `/v1/openapi.json`, `/v1/version` | internal listener, no key; `version` reports server, meta-whatsapp-rs revision, Graph and API versions; `/v1/openapi.json` is served by the axum API adapter, while the CrateStack adapter's contract is its `.cstack` schema ([§8](#8-modular-architecture-and-client-sdks)) |

The public listener serves `GET|POST /webhooks/meta`, `POST /webhooks/flows/{waba_id}` (Meta's Flows data endpoint, roadmap M5d) and `GET /livez`,
nothing else.

### 4.3 Message content

A union owned by the service, named after Meta's Cloud API message object
(so Meta's pages describe it) and mapped onto `meta-whatsapp-client` builders, which
`OutboundMessage::validate` checks before any request (`OutboundMessage` is
serialize-only, so no library change). Types: `text`; `image`, `video`,
`audio`, `document`, `sticker` by `id` or `https://` `link` (Meta fetches
it, never the service); `location`, `contacts`, `reaction`; `template`
(Meta's object, deserialized into `TemplateMessage`); `interactive`
`button`, `list`, `cta_url`, and, since roadmap P2, the payment messages
`order_details` and `order_status` (their `action.parameters` Meta's
object, read into the library's payment types with no key dropped and
checked by their currency's rules; an `order_status`, whose wire names no
currency, by Brazil's, the wider, an India-only refusal left to Meta).
Since roadmap M5a also `pin` (groups only: `pin` with
`expiration_days`, or `unpin`), the `interactive` types
`location_request_message`, `request_contact_info`,
`call_permission_request`, `voice_call`, `flow`, `address_message`,
`carousel` (media cards or product cards), `product`, `product_list` and
`catalog_message`, and Direct Send's `category` (`utility`,
`authentication`, `service`), `ttl_seconds` and `direct_send_config` on
any type. Each is read into the library's own type (`Pin`,
`LocationRequest`, `RequestContactInfo`, `CallPermissionRequest`,
`VoiceCall`, `FlowMessage`, `AddressMessage`, `MediaCarousel`,
`ProductCarousel`, `SingleProduct`, `ProductList`, `CatalogMessage`,
`OutboundMessage::category`), then what the request carries is compared
with what the library would send: a key it would drop is `422
invalid_request` on its path, nothing sent (D100); the values the library
writes itself (an action's `name`, a card's `card_index` and `type`, a
Flow's `flow_message_version`) must be the ones it writes. Direct Send's
rules are the library's, and its category one of Meta's three (D101). A
Flow named by `flow_id` and a catalog named by `catalog_id` are checked
to be the number's WABA's before the send, `404` otherwise (D102; the
design's M5.2 for this family); a `flow_token` reaches no log (D103). The
window: every type but `template` is free-form, and so is a Direct Send
`service` message; a Direct Send `utility` or `authentication` message is
sent by Meta as a template, outside the window too (the library's
`Inbox::send` exempts both, and so does the inbox reply's cut). Anything
else, `MessageContent::Raw` included, is `422 unsupported_message_type`. A
streamed download
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

*As built in roadmap M2c* (the core's `webhooks_out`; D105–D113, decided,
[§10](#10-decisions)):

- **The outbox stays the source of truth.** An endpoint is a cursor in its
  tenant's stream, as a live stream is: the dispatcher pages the outbox
  after it (its types as the filter), records one delivery per event and
  endpoint, and moves the cursor in the same transaction. A delivery holds
  no body: each attempt reads its event in the endpoint's tenant's stream
  (`Outbox::get`), so a delivery carries its tenant's event only, and one
  whose event is gone (past retention, or with its tenant) ends `failed`
  (`event_gone`) with no attempt counted.
- **The body** is the envelope `GET /v1/events/{id}` answers, field for
  field (a test compares them); over 256 KiB, `data: null` and
  `truncated: true`. `webhook-id` is the event's `id`, `webhook-timestamp`
  the attempt's second on the service's clock, `webhook-signature` one
  `v1,` signature per secret, the current one first. A ping's body is
  `{id, type: "ping", api_version, tenant_id, endpoint_id, sent_at}`, its
  `webhook-id` the delivery's id. The Standard Webhooks libraries' own
  known-answer vectors are pinned in `webhooks_out::signature`'s tests.
- **Secrets**: `whsec_` and the base64 of 32 random bytes, shown by the
  creation and the rotation only, sealed with AES-256-GCM under
  `WA_SERVER_DATA_KEY` (`v1.<key id>.<base64>`, the associated data
  binding the endpoint's id and the key id), opened only to sign; no
  `Debug`, log or answer shows one. Without a data key (Postgres and no
  `WA_SERVER_DATA_KEY`) the routes answer `409 webhooks_not_configured`
  and no dispatcher runs; memory storage takes a throwaway key.
- **Destinations**: `https` (plain `http` in development only), no
  credentials in the URL, the host listed by name (`hooks.example.com`,
  `*.example.com`) or each of its addresses inside a listed CIDR (an
  address in the URL inside one); every address the host resolves to must
  pass, an IPv4-mapped, -compatible or NAT64 one judged as the IPv4 address
  it carries; private, loopback, link-local, shared, reserved,
  documentation, 6to4 and Teredo addresses, and every IPv6 address
  outside the global unicast `2000::/3`, only inside a listed CIDR;
  unspecified, broadcast, multicast and the clouds' metadata addresses
  never; a `/0` refuses the start. Checked at the creation (and a
  change), and again at every attempt on a fresh lookup whose checked
  address is the one the attempt connects to (the sender resolves
  nothing): a host that resolves public at creation and private at the
  attempt is refused there, nothing sent (`destination_refused`), and
  retried.
- **Attempts**: TCP to the pinned address, TLS with the URL's host as SNI
  and the certificate checked against it (the platform's store, an
  aws-lc-rs provider), HTTP/1.1, one connection each, closed with the
  attempt when its time limit cuts it, 10 s in all, a 5 s lookup, a 3xx
  the answer (never followed), the answer's head within 16 KiB and at
  most 1 KiB of its body read. 2xx delivers; anything else waits 10 s, doubling to an hour a
  step, a fifth of jitter either way, until `give_up_at` (72 h,
  `WA_SERVER_WEBHOOK_RETRY_WINDOW`), then `failed`; an endpoint failing
  since a window before is disabled (`disabled_reason: failing`). A
  suspended tenant's deliveries wait.
- **Bounds**: per replica, 32 attempts in flight, 4 per endpoint, 8 per
  tenant, a claim's lease 60 s (`WA_SERVER_WEBHOOK_IN_FLIGHT`,
  `…_PER_TENANT`, `…_PER_ENDPOINT`, `WA_SERVER_WEBHOOK_LEASE`): an
  endpoint that never answers holds its own 4, never the others'.

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
  tenant's events, nor filters them (the library's `sse` shares one
  `Arc` per event since roadmap L21b, but every stream filters every
  event of its channel).
- Limits: 20 streams per tenant, 1,000 per replica. Operator-only rows are
  never streamed. `EventSource` cannot send `Authorization`: browsers go
  through the CMS (D3).

**As built (roadmap M2b).** The core's `live::Streams`, over the
backend's `Outbox` and its new `EventNotifier` port
([§8.1](#81-a-framework-free-core-with-ports)); the HTTP crate's
`api::events::stream_routes`, which the axum adapter mounts behind
scope `events` (operations `stream_events` and `stream_number_events`,
rate class `read`, charged once when a stream opens). Where it differs
from the bullets above, as decided in D39–D42 (and D43, proposed by its
review):

- **The outbox is read by each stream, not once per replica** (D39). A
  stream is a cursor in its tenant's stream of sequences: woken by a
  notification of its tenant, or every 15 s whatever it heard (the
  re-poll, `LiveSettings::repoll`), it pages the outbox after its cursor
  with its own filters, exactly as `GET /v1/events` does, and sends what
  it finds. A notification is a wake-up and nothing else: a lost one (a
  replica restarting, a `LISTEN` connection dropped, a notify that
  failed) delays an event by the re-poll at most, a spurious one costs a
  read. The channels are per tenant (a stream is never woken by another
  tenant's events, and the outbox read names its tenant), not per
  (tenant, number): a number's stream reads its tenant's wake-ups and
  filters by number in the read. The cost: a tenant's open streams each
  read once per wake-up (20 reads per event at most, per replica), where
  a shared read would be one.
- **`NOTIFY`** carries `<sequence> <tenant>` on the channel
  `wa_server_events` of the deployment's database (D40), sent by the
  webhook pipeline once the outbox insert committed, for a row it routed
  to a tenant; a failed notify is logged and the delivery still
  succeeds. Each replica's listener is a connection of its own, opened
  with its first stream, which wakes every stream of the replica once it
  listens again after a lost connection (not before: what commits in
  between would wait for the re-poll).
- **Cursors** (D41): `id:` is the event's sequence; `Last-Event-ID`, or
  `?after=`, resumes exactly after it, on any replica; without either, a
  stream starts at the tenant's newest sequence (what comes next only;
  history is `GET /v1/events`). Below what was purged: `410
  cursor_expired`; past the newest sequence: `422` on the cursor's own
  name (`Last-Event-ID` or `after`); both before the stream starts.
- **Bounded buffering** (D42): a stream holds at most 1 MiB of event data
  (one event larger alone) and 64 frames its client has not taken; a
  client that takes nothing for 30 s while its buffer is full is
  dropped: its stream ends with `event: lagged` and a `data:` of
  `{"after": n}`, `n` the last sequence its client took, and what it
  held is dropped. A
  stream whose cursor fell below what housekeeping purged ends the same
  way (its client lost events). Nothing is lost from the outbox by a
  drop: the client resumes from `after`. **A stream gives its place
  among the 20 and the 1,000, and its buffer, back when it ends**, not
  when its connection closes (D43): a client stalled on a full TCP
  window, which never takes the last frame, holds neither. Its
  connection goes too: both listeners close a connection that takes no
  byte of its answer for a minute (`listen::Limits::write_timeout`).
  Past a bound, `429 too_many_streams` carries `Retry-After: 5`.
- **Only while admitted** (D43): before it reads, at most every second
  and at least every re-poll, a stream asks again whether its key would
  still be admitted for its tenant and scope `events` (the key stored,
  not revoked nor expired, the tenant stored and not suspended, a
  platform key still allowed it); once not, it ends with no last frame,
  and its client reconnecting is answered `401` or `403`.
- **Shutdown**: `begin_shutdown` ends every stream with `event:
  shutdown` (`data: {"after": n}`), so the listeners' connections close
  within the grace period, and a stream asked for afterwards is `503
  shutting_down`.
- The limits are settings: `WA_SERVER_STREAMS_PER_TENANT` (20) and
  `WA_SERVER_STREAMS_PER_REPLICA` (1,000) ([§7.2](#72-environment)); the
  heartbeat (15 s), the re-poll (15 s), the buffer, the slow-client
  wait, the re-check (1 s) and the `Retry-After` (5 s) are the service's
  constants (`live::LiveSettings`). The open streams are a metric,
  `wa_server_event_streams_open` ([§7.3](#73-start-observability-shutdown)).
- Decisive tests: M2.2 live on two pools of one database
  (`live_postgres_a_webhook_into_one_replica_reaches_a_stream_on_another`,
  the server's; `conformance::live::an_event_through_one_replica_wakes_a_stream_on_another`,
  the Postgres crate's); M2.3 on the streams through the core's pipeline
  and over HTTP (`no_unknown_or_unparsed_event_reaches_a_stream`); M2.5
  (`many_streams_at_a_steady_rate_hold_bounded_memory`: 200 streams at 100
  events a second, half never read, on memory and live on Postgres); a
  lost notification still delivers (`a_lost_notification_still_delivers`,
  core and HTTP); `Last-Event-ID` resumes exactly after; an outbox whose
  pages leave the tenant out fails the tenant case
  (`removing_the_tenant_filter_on_the_stream_fails_the_tenant_case`); a
  stalled client gives its place and buffer back
  (`a_slow_client_is_dropped_with_its_buffer_bounded`, core;
  `a_slow_client_is_dropped_with_lagged`, HTTP), and its connection is
  closed (`a_client_that_stops_reading_is_closed_after_the_write_timeout`);
  a revoked key or a suspended tenant ends its streams
  (`a_stream_ends_once_its_key_or_tenant_is_no_longer_admitted`, core;
  `a_stream_ends_once_its_key_is_revoked`, HTTP); a killed listener
  listens again (`live_postgres_a_killed_listener_listens_again`).

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
(session info only; `resume` rebuilds the request with
`OnboardingRequest::for_resume`, no code, since L4); errors carry
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
| Library | implemented | implemented (L2, L3): the step's name, resume behaviour and error kinds come from it |

The mode is per deployment (per Meta app), not per tenant.

*As built in M3a*: the core's `signup` module (`Signups`, built by the
HTTP crate's `AppState::from_backend` on the bundle's `KvStore` and the
service's clock, with the deployment's `SignupDeployment`: the app, the
configuration, the mode; `Signups::open` admits the `Caller`) drives the
library's `EmbeddedSignup` with the vault the `Authorizer` holds
(`Authorizer::vault_for_signup`, crate-private, a UI case pins it). Both
modes run `onboard_with_approval` and `resume_with_approval`: the
approval is where the WABA is bound (D4, before anything is stored,
subscribed or shared), in a Solution Partner's flow too, where the
library records it in the credit ledger. A completion confirms the
binding once onboarded (moved meanwhile, the token it stored is taken
back, `503`); a store that failed after the approval bound a WABA no one
held takes the binding back. The PIN is typed per attempt and never
stored (D6); the merchant's token, the PIN, the code and the state are in
no answer, log, error, audit event or row; Meta's texts are withheld from
the logs (`ServiceError::from_library_withholding_texts`, as on the OTP
routes) and its `details` from the answers. Not built: a per-signup
currency (`WA_WABA_CURRENCY` for every merchant), the two-call credit
method (`CreditSharing::ShareThenAttach`), clearing a pending share
(`EmbeddedSignup::clear_pending_share`). Hosted signup, several WABAs per
signup and pre-verified numbers are M3f's, below.

*As built in M3f* (D200 to D205, decided 2026-11-06): three additions, each off
until the deployment turns it on.

- **Several WABAs** (`WA_SIGNUP_EVERY_GRANTED_WABA=on`, D200): the
  completion runs the library's `onboard_wabas_with_approval`. Every WABA
  the event names (`waba_id`, `waba_ids`) is checked against D4 before the
  attempt is redeemed, so one another tenant holds refuses the signup
  (`409 waba_owned_by_another_tenant`) with nothing redeemed, bound or
  asked of Meta. Each granted WABA then has its own approval (bound, D4
  atomically), token record and tail, and a failure on one does not stop
  the next. The answer's `wabas` lists each with `connected`, `held` (a
  grant the vault already held, left alone), `refused` (bound to another
  tenant, which the pre-check could not see because the event did not name
  it: nothing stored) or `failed` (`failed_step`, `resumable`: kept as an
  attempt for `POST /v1/signup/resume`). The call fails only when no WABA
  connected. With the option off the completion binds the primary WABA (the
  event's `waba_id`, else the first of `waba_ids`) and the answer lists the
  others as `not_bound_waba_ids`.
- **Hosted Embedded Signup** (`WA_HOSTED_SIGNUP=on`, D201 and D202): Meta's
  hosted page returns nothing and names no tenant, so the service
  remembers a signed `PARTNER_ADDED` (`wa.es.hosted`, 7 days; nothing
  fetched or bound; a WABA a tenant or the vault holds is skipped) and the
  operator claims it with `POST /v1/admin/signup/hosted`, naming the
  tenant, the WABA and its `owner_business_id`. Order: admin key, inputs,
  hosted on, the tenant (none: `404`; suspended: `403`), D4, the arrival (none or
  another business: `404 not_found`), its atomic claim, then the library's
  `onboard_hosted_with_approval` (the business token from
  `system_user_access_tokens` with the system token and its
  `appsecret_proof`, then the code flow's steps, the binding in the
  approval, the tenant's PIN). A claim that stopped before the token was
  stored is put back; one that stopped after is an attempt the tenant
  resumes (`POST /v1/signup/resume`). The claim is the operator's because
  the two ids it names are no secrets: a tenant route would let one tenant
  take an arrival meant for another's merchant (D202).
- **Pre-verified numbers** (`WA_PARTNER_BUSINESS_ID`, D203 to D205): the
  pool is the deployment's, so its routes are the operator's
  (`/v1/admin/pre-verified-numbers`: add, list, read, request and check a
  code, delete, reserve for a tenant), every call the library's with the
  pool's token. A tenant's launch (`pre_verified_phone_ids`) offers only
  ids reserved for it. No legal act: no endpoint accepts terms.

**Decision (D5): onboarding mode in production (OQ #3).** (a) Tech
Provider: merchants pay Meta; (b) Solution Partner: the platform pays
through its credit line and bills merchants (needs partner status and a
default currency). Both are supported from M3. *Recommendation: (a) first*,
switching by configuration once partner status and billing exist.

**Decision (D6): two-step PIN (OQ #4).** (a) Typed by the merchant
per attempt, never stored (today's behaviour); (b) generated and stored
encrypted by the service. *Recommendation: (a)*: one leak must not expose
every number's PIN. *Decided 2026-09-26 (coordinator, owner's
delegation), swappable: (a) ([§10](#10-decisions)).*

**Decision (D7): coexistence sync (OQ #7)**, due within 24 h of
onboarding a WhatsApp Business app number. (a) An explicit endpoint; (b)
automatic after onboarding; (c) the endpoint plus a per-tenant setting for
(b). *Recommendation: (c)*, automatic by default: 24 h is easy to miss. The
inbox records the echoes and the synced history since OQ #17 closed (M1c
records them for tenants too).

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
- *As built in M3c*: the core's `otp` module (`OtpCodes`, opened for an
  admitted caller) over the library's `OtpService`, built per request
  rather than cached (it holds no connection); the namespace is the
  calling tenant's id. A tenant's settings
  (`wa_server_tenants.settings`, key `otp`, set by
  `PUT /v1/admin/tenants/{id}/otp`) name one of its own numbers, checked
  when they are set and again, through the authorization order, at every
  issue; a tenant without settings borrows those of the deployment's
  default OTP tenant (`WA_SERVER_OTP_DEFAULT_TENANT`, the platform's
  number: D8's default, D45), whose number is opened for that tenant,
  never for the caller's. No settings, no default, or a sending number
  no longer its holder's: `409 otp_not_configured`, nothing read or sent.
  A suspended or deleted default tenant lends nothing: its borrowers'
  issues and checks are `409 otp_not_configured` too (fail closed;
  `doctor` warns, `otp.default_tenant`, when the tenant named is missing,
  suspended, without settings or no longer holding its number).
  The issue limit cannot be turned off over the API. On the default OTP
  tenant's number, a second issue limit is **shared** by every tenant and
  purpose sending from it: per recipient, that tenant's `issue_limit`,
  reserved before the library's call and given back when nothing was
  sent (a cooldown, a tenant's own limit, a refusal before the send);
  past it, `200 rate_limited`. Without it, N borrowing tenants (or N
  purposes) could send one recipient N times the limit, on the
  platform's bill (SMS pumping, harassment); the cost is that one
  tenant's codes to a recipient count against every borrower's, which a
  tenant with its own number avoids. Its records live in the backend's
  key/value store (`wa_server.otp.shared_rate`), keyed by the hex
  HMAC-SHA256, under HMAC-SHA256(`WA_OTP_PEPPER`,
  `meta-whatsapp-server/otp-shared-limit/v1`), of the number's id and
  the recipient's digits: no phone number in a key; both names are
  stored names pending the owner's confirmation (D59). The design's verify
  takes the phone number and purpose (the library's `verify` is keyed
  so), not the challenge id, so another tenant's code answers `200
  not_found`, the namespace's answer.

**Decision (D8): which number sends a tenant's codes.** (a) One
platform number for all (the user sees the platform's name); (b) each
merchant's own number (the user sees the merchant; each needs an approved
authentication template); (c) per tenant, defaulting to (a).
*Recommendation: (c).* *Decided 2026-09-26 (coordinator, owner's
delegation), swappable: (c) ([§10](#10-decisions)).*

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
| 422 | `invalid_request` (local validation, with `field`), `invalid_parameter`, `unsupported_message_type`, `recipient_not_supported`, `undeliverable`, `template_parameter_mismatch`, `template_not_found`, `template_text_too_long`, `template_policy_violation`, `template_rejected`, `sample_template_not_allowed` (Meta's `hello_world` from a number that is not one of its public test numbers), `invalid_cursor` (Meta's `131059`, a groups cursor that expired or is corrupted), `group_participant_not_found` (`131212`), `join_request_not_found` (`131213`), `idempotency_key_reused` | fix the request; nothing was sent |
| 409 | `customer_service_window_closed`, `thread_owned_elsewhere` (Meta's `131070`, a service message without owning the thread, and the inbox's local refusal while another app owns it), `marketing_opted_out`, `blocked_by_business`, `experiment_holdout`, `template_paused`, `template_disabled`, `template_syncing`, `template_unavailable`, `template_limit_reached`, `group_limit_reached` (`131204`, `131211`), `group_creation_disabled` (`131214`), `group_partially_applied` (`131201`, Meta's `206` is a success the routes answer `partial`: this is the code where it comes as an error), `recipient_terms_not_accepted` (`131203`), `calling_not_enabled` (`138000`, `138015`, `138018`), `call_permission_missing` (`138006`: ask the person for a call permission), `call_permission_already_granted` (`138017`), `call_already_ongoing` (`138003`), `flow_unavailable`, `registration`, `two_step_verification`, `sync_not_allowed`, `duplicate_onboarding`, `number_not_connected`, `reconnect_required` (also Meta's `authentication` on a stored token: a merchant's, or an attached WABA's once stored), `waba_owned_by_another_tenant` (also a phone number another tenant holds), `tenant_exists`, `idempotency_in_progress`, `outcome_unknown`, `otp_not_configured` (roadmap M3c: no OTP settings for the tenant and no default OTP tenant, or the sending number is no longer its holder's), `signup_not_configured` (roadmap M3a: no `WA_APP_ID` or `WA_ES_CONFIG_ID`), `erasure_not_configured` (roadmap M2f: no `WA_SERVER_ERASURE_KEY`), `erasure_crosses_binding` (roadmap M2f, D90: the number has a history cut, its WABA or itself moved from another tenant, and the inbox's erasure cannot stop at it), `webhooks_not_configured` (roadmap M2c: no `WA_SERVER_DATA_KEY`), `resumable_upload_not_configured` (roadmap M5c1, D128: no `WA_APP_ID`, so no app to open an upload session under), `webhook_endpoint_limit_reached` (roadmap M2c: 10 endpoints a tenant) | a state must change first |
| 403 | `permission`, `account_restricted`, `country_restricted`, `payment`, `feature_not_available`, `marketing_not_allowed`, `group_suspended` (`131207`), `calling_temporarily_disabled` (`138014`); `forbidden`, `tenant_suspended`, `stale_attempt` | not allowed, by Meta or the service |
| 429 | `rate_limited` (Meta's throttling codes, `ErrorKind::RateLimited`: 4, 80007, 130429, 613 (the call-permission read's limit), and the Graph API's Business Use Case limits 80008, 80009, 80014), `pair_rate_limited`, `group_rate_limited` (`131208`, never retryable: D171), `call_limit_reached` (`138002`, `138005`, `138009`, `138012`, never retryable: D242), `spam_rate_limited`, `ecosystem_engagement_limit`, `classification_limit_reached`, `too_many_requests`, `too_many_streams` | `Retry-After` when known (the service's own limits, `too_many_requests` and `too_many_streams`: Meta's wait is not forwarded); `retryable` says whether waiting helps (false for 131048, 131049) |
| 404, 410, 413 | `not_found`, `nothing_to_resume`; `cursor_expired`; `payload_too_large`, `media_too_large` | |
| 428 | `idempotency_key_required` (a refund without an `Idempotency-Key`, D56) | send the request again with a key; nothing was sent |
| 502 | `service_unavailable`, `unknown`, `upstream` (non-Graph answer), `integrity`, `media_download_failed`, `media_upload_failed`, `onboarding_failed`, `call_connection_failed` (`138004`, `138007`, `138019`, `138020`: the call may have connected, D242), `call_media_timeout` (`138021`-`138023`), `media_conversion_failed` (roadmap M5c1, D131: the deployment's converter is missing, lacks an encoder, ran past its time (retryable), answered another format or was killed; nothing was sent) | Meta or the network failed |
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

- `Idempotency-Key` on sends, inbox replies, OTP issue, uploads,
  template creation, (since M2g) marking a conversation read, since
  roadmap M3d a verification code's request, a registration and a
  deregistration (D67: a registration's canonical input is its number
  and region, never its PIN) and, since roadmap P2, a payment
  configuration's creation, a new OAuth link and a refund (whose kept
  answer has no OAuth link: the link is a credential the records do not
  store; a refund **requires** one, `428 idempotency_key_required`
  without it: D56) and, since roadmap M5c2, a display name's request
  (D163), scoped to the tenant; stored with a hash of the
  operation and its canonical input (the number or WABA the path names,
  decoded, and the body: D27, built in roadmap S6; never the method or
  the path's spelling, so a key means the same through every API
  adapter), the state and the final answer.
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
| A WABA or number moving between tenants | events Meta dated before the binding began go to nobody, inbox included, and so do, after a move from another tenant (not for a first onboarding), those of its first second and its skew margin; undated ones (errors, history chunks, undated group updates) go to nobody for 7 days and an hour after a move (D36, S2b; `CurrentHolder` gives them to the new holder); the previous tenant's inbox rows stay under the number (M2's inbox reads must filter by binding epoch, or D10 decides a purge on unbind) |
| A tenant reading or sending as another | ownership before the vault ([§3.3](#33-authorization-order)); foreign numbers are `404`; the core's `OwnedNumber` and `OwnedWaba` are the only path to a token, and a capability another `Authorizer` made is refused (`403`; the core's security review, SR-H1). *As built in M1b*: one token may reach several tenants' WABAs (the platform's system user token attached to each), so an id in a path is checked to be the path's number's or WABA's own: media with Meta's `phone_number_id`, templates through the WABA's own list; another's is `404` like a missing one. *Since M2e*: a media id the outbox recorded as received on another number, or on the number before its binding epoch (another tenant's row, an operator-only one), is `404` with nothing asked of Meta, download or deletion, whatever Meta's check would answer (OQ #43: Meta documents the check for uploaded media only); only the caller's own number's, routed to its tenant, is asked without the check |
| A stolen platform key | limited to its tenants and scopes; internal network only; revocation effective across replicas at once |
| Erasing another tenant's customer, or one the request did not name | the scope `erasure` (no other scope grants it), ownership before anything is read (another tenant's number is `404`), the binding read again before anything is deleted (`503` if the number moved), the number's id confirmed in the body; the outbox's rows are the tenant's own only, never the number's operator-only ones nor another tenant's stream, and a number with a history cut is refused whole, so a previous holder's hidden inbox records are never reached (roadmap M2f, D86–D91) |
| An erasure that names the customer, or forgets it | the identities in the body only (never a path, which an ingress logs); no `Debug` on the request; the answer, the audit event, errors, logs and metrics name ids and counts only; the journal holds keyed HMACs (`WA_SERVER_ERASURE_KEY`), written before anything is deleted, for the replay after a restore |
| A stolen database dump | tokens encrypted (vault key elsewhere), API keys hashed, OTP codes and numbers only as HMACs (pepper elsewhere), webhook secrets encrypted (data key elsewhere); the inbox history, the event outbox (`wa_server_events`: message texts, vCards, orders, Flow answers, BSUIDs, phone numbers, coexistence history; operator-only rows keep whole raw bodies and parse error texts) and the answers idempotency records keep for 24 h (a send's recipient: phone number, `wa_id` or BSUID) are readable, so database encryption at rest is the operator's, and the inbox's and the outbox's retention is D10 |
| Signup attributed to the wrong merchant | state bound to the tenant, redeemed for the credential's tenant; ids verified with Meta; D4 |
| OTP brute force, cross-tenant codes | the library's limits, per-tenant rate limits, namespace = tenant |
| SSRF | no URL fetching (media by id, through the library's host allow-list; *since M2e* a received media id's URL too, the same path, with the number's token: a URL off Meta's media host is `502 upstream`, the token sent nowhere); webhooks-out allow-list, no redirects, pinned address; *as built in M2c*: the list checked at creation and at every attempt on a fresh lookup, every address of the host, the checked one the only one connected to; non-public addresses only inside a listed CIDR, metadata, unspecified and multicast never; 10 s, 1 KiB of the answer, no pool ([§4.4](#44-webhooks-out)) |
| Exhaustion | per-route body limits, per-tenant rate limits, stream caps, timeouts, bounded media and rendering concurrency |

**Secrets** come from the environment or `<NAME>_FILE`; never the TOML
file, the image, the logs, or the database they protect.

| Secret | Protects | Rotation |
| --- | --- | --- |
| `WA_APP_SECRET` (+ `WA_APP_SECRET_PREVIOUS`) | Meta signatures, code exchange, app token, the key event ids are derived with | both listed while rolling out; an event recorded again after the rotation (its row purged) gets a new id |
| `WA_VERIFY_TOKEN` | Meta's subscription check | with the App Dashboard |
| `WA_VAULT_KEY`, `WA_VAULT_KEY_ID`, `WA_VAULT_PREVIOUS_KEYS` | merchants' tokens | `VaultKeys::with_previous`, `POST /v1/admin/vault/rotate`, drop the old |
| `WA_OTP_PEPPER` | OTP hashes | invalidates codes in flight |
| `WA_SERVER_DATA_KEY` (+ previous) | webhook signing secrets | like the vault key; *as built in M2c*: `WA_SERVER_DATA_PREVIOUS_KEYS` open what they sealed, new seals use the active key; no walk re-seals the endpoints yet, so an old key stays listed while an endpoint sealed under it remains (a gap, roadmap M2i) |
| `WA_PARTNER_SYSTEM_TOKEN` | credit-line sharing | new token, deploy, revoke |
| `WA_SERVER_ERASURE_KEY` (M2f) | the erasure journal's HMACs (nothing in the database: a replay after a restore needs it) | a new key journals what follows; keep every old one as long as the backups a journal entry it made may be replayed over |

**Never logged or echoed**: API keys and `Authorization`, Meta tokens, the
app secret, the verify token, signup codes and states, PINs, OTP codes,
webhook secrets, bodies, query strings, signature headers, `WebhookEvent`'s
`Debug`; Meta's error messages go to `debug` logs only. Request logs carry
the route template, not the raw path (contacts identify customers).

**Rate limits** per tenant and replica, overridable by the admin: sends
20/s (burst 40), reads 50/s, template management 2/s, OTP issue 5/s, 20
streams; `429` with `Retry-After`; *as built in M3c*: OTP issue is a
class of its own (`otp_issue`, 5/s, a burst of 5) and so is checking a
code (`otp_verify`, 20/s, a burst of 20: the service's default, D46),
per tenant, beside the library's limits per recipient. Meta's limits (80 messages/s per number,
portfolio limits) still apply; pacing campaigns is the caller's job.
*As built in M1b*: token buckets keyed by tenant and route class, checked
after the scope and before ownership; the bursts this paragraph does not
state (reads, template management) are one second's worth; read
receipts, media and profile writes count as sends; the operator sets
them per deployment (`WA_SERVER_RATE_*`, [§7.2](#72-environment)), and
per-tenant overrides wait for the tenant settings (M3).
*As built in M3d* (D66): a number's registration attempts are also
counted **per number**, shared by every replica (on the bundle's
`KvStore`, `wa.server.registration`), after ownership and the local
checks and before Meta: registering and deregistering 5 in a fixed
window of 72 hours, counted together (Meta allows 10 of each in a
moving 72 hours, locks the number for 72 hours on the 11th, `133016`, and locks it after too many
wrong PINs, `133008`), a verification code's request and check 10 an
hour; past it `429 too_many_requests` with `Retry-After` the rest of the
window (`WA_SERVER_REGISTRATION_*`, `WA_SERVER_VERIFICATION_*`).

**CORS**: none (D3). **TLS** terminates at the ingress: valid certificate,
body limit of at least 3 MiB, no body rewriting or decompression, a timeout
above the slowest sink, request buffering (so slow clients never reach the
service), and only Meta's webhook IP ranges, or mutual TLS with Meta's
client certificate, admitted to `/webhooks/meta`. The app secret signs
every tenant's deliveries: whoever holds it can forge any tenant's
events. So from M3, a webhook that would do something destructive (a
`PARTNER_APP_UNINSTALLED` disconnecting a WABA, a credit line revoked) is
confirmed with Graph first, not acted on by its signature alone. *As
built in M3a* (D72): `debug_token` on the WABA's stored token, under the
app token; one Meta still reports valid and granted the WABA contradicts
the webhook, which is recorded and not acted on (`warn`, target `audit`).
A WABA with no stored token leaves nothing to confirm with: what the
library then acts on is its own records (a revocation finds the line's
records for the business, or none). Meta documents no read that confirms
either event, and that a removed or uninstalled merchant's token stops
being valid or granted is this design's assumption: a deployment where it
does not hold sets `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION=signature`. The internal listener is reachable only from the
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

**Decision (D9): image name and registry.** (a) Public on GHCR
next to the public repository; (b) a private registry. The name follows the
crate names, settled when OQ #1 closed (`meta-whatsapp-*`, 2026-09-25).
*Recommendation: (a)*, named after the binary, `meta-whatsapp-server`.
*Decided 2026-09-26 (coordinator, owner's delegation), swappable: (a)
([§10](#10-decisions)).* The image lists its contents and carries the
third-party licences of what it contains (M4); it contains no `ffmpeg`
(roadmap L19).

### 7.2 Environment

| Variable | Default | What |
| --- | --- | --- |
| `DATABASE_URL` | required outside development | Postgres |
| `WA_SERVER_ENV` | `production` | `development` allows memory storage, a throwaway vault key, a plain-`http` `WA_GRAPH_ENDPOINT`, plain-HTTP webhook targets |
| `WA_SERVER_PUBLIC_BIND`, `WA_SERVER_INTERNAL_BIND` | `127.0.0.1:8080`, `127.0.0.1:8081` | must differ |
| `WA_APP_ID`, `WA_APP_SECRET`, `WA_VERIFY_TOKEN`, `WA_ES_CONFIG_ID` | — | the Meta app |
| `WA_VAULT_KEY*`, `WA_OTP_PEPPER`, `WA_SERVER_DATA_KEY*` | — | [§6](#6-security); `WA_VAULT_KEY_ID` defaults to `k1`, `WA_VAULT_PREVIOUS_KEYS` is `<id>:<base64 of 32 bytes>`, comma-separated, each id once (the active key's included: a repeated id refuses the start) |
| `WA_ONBOARDING_MODE` | `tech_provider` | `solution_partner` needs `WA_PARTNER_SYSTEM_TOKEN`, `WA_PARTNER_SYSTEM_USER_ID`, `WA_CREDIT_LINE_ID`, `WA_WABA_CURRENCY` |
| `WA_SIGNUP_EVERY_GRANTED_WABA`, `WA_HOSTED_SIGNUP` (M3f) | `off`, `off` | D200, D201: `on` binds every WABA a signup grants; `on` handles hosted Embedded Signup (needs `WA_SYSTEM_TOKEN` or Solution Partner mode). Names pending the owner's confirmation before the first release |
| `WA_SYSTEM_TOKEN`, `WA_PARTNER_BUSINESS_ID` (M3f) | the partner's `WA_PARTNER_SYSTEM_TOKEN`, unset | D201, D204: the deployment's system user token (`business_management`), which fetches hosted business tokens and manages the pool; the deployment's own business portfolio, whose pre-verified pool the admin routes manage (unset: they answer `409 signup_not_configured`). Names pending the owner's confirmation before the first release |
| `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION` (M3a) | `graph` | D72: an `account_update` that offboards a merchant or revokes their line confirmed with Graph first (`graph`), or acted on by its signature alone (`signature`) |
| `WA_GRAPH_API_VERSION`, `WA_GRAPH_ENDPOINT` | `ApiVersion::DEFAULT` (v25.0), Graph | the version is also handed to the signup page; the endpoint serves proxies and test stubs, and must be `https` outside development (every token travels to it) |
| `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS` | none | hosts and CIDRs for webhooks-out; *as built in M2c*: comma-separated names, `*.`-prefixed names and CIDRs (not `/0`); empty, every destination is refused ([§4.4](#44-webhooks-out)) |
| `WA_SERVER_WEBHOOK_IN_FLIGHT`, `…_IN_FLIGHT_PER_TENANT`, `…_IN_FLIGHT_PER_ENDPOINT` (M2c) | 32, 8, 4 | D108: webhook attempts in flight at once on a replica, in all, of one tenant, to one endpoint |
| `WA_SERVER_WEBHOOK_LEASE` (M2c) | `60s` | D108: how long a replica's claim holds a delivery; at least `30s`; replicas' clocks must agree within it (D107) |
| `WA_SERVER_DATA_KEY_ID`, `WA_SERVER_DATA_PREVIOUS_KEYS` (M2c) | `k1`, none | the data key's id (1 to 32 of `[A-Za-z0-9_-]`) and the previous keys, `<id>:<base64 of 32 bytes>` comma-separated, each id once; `WA_SERVER_DATA_KEY` unset: a throwaway key on memory storage, no webhooks out on Postgres |
| `WA_SERVER_OUTBOX_RETENTION`, `…_IDEMPOTENCY_TTL`, `…_WEBHOOK_RETRY_WINDOW` | 7 d, 24 h, 72 h | |
| `WA_SERVER_TOKEN_EXPIRY_LEAD` (M3e, D180) | `7d` | how long before a stored token's expiry its tenant is told (`token_expiring`); a positive duration no longer than `WA_SERVER_OUTBOX_RETENTION`, or `off`. Pending the owner's confirmation before the first release |
| `WA_SERVER_MEDIA_MAX_BYTES`, `WA_SERVER_SHUTDOWN_GRACE`, `WA_SERVER_MIGRATE` | 100 MiB, 25 s, `auto` | `skip` when a job runs `meta-whatsapp-server migrate` |
| `WA_SERVER_INBOX_RETENTION` (M2a) | `keep` | D10's inbox retention: the inbox store built `with_retention` from it (`keep`, or a positive duration), applied by housekeeping (`apply_retention`); a backend whose inbox applies another is refused at start |
| `WA_SERVER_INBOX_REPLY_CHECKS` (M2a) | every check | the library's `ReplyChecks` per number, `<phone_number_id>:<checks>` comma-separated (`all`, `none`, or `window`, `trust_handover`, `thread_owner` joined by `+`); a number not listed keeps every check |
| `WA_SERVER_LEGAL_ACTS` (M5c3, D192; M5h, D231) | `off` | `on` lets tenants take the legal act their keys allow (the compliance information declaration) and the operator take the max price agreement's (`POST /v1/admin/businesses/{business_id}/max-price-agreement`); off, each is `403 feature_not_available` and asks Meta nothing (the Official Business Account application and the contact book erasure are not gated by it); anything else refuses the start |
| `WA_SERVER_MEDIA_CONVERTER`, `WA_SERVER_MEDIA_CONVERTER_PROGRAM` (M5c1, D130) | `none`; `ffmpeg` from `PATH` | `none`, or `ffmpeg`: the library's `FfmpegConverter` (a build with the `ffmpeg` feature, over an `ffmpeg` the deployment installs; the image carries none), run with its guards and a 20 s limit; the program only with `ffmpeg`. `WA_APP_ID` also opens resumable upload sessions (unset: `409 resumable_upload_not_configured`). Pending the owner's confirmation before the first release |
| `WA_SERVER_MEDIA_CONCURRENCY` (M1b) | 4 | uploads and whole-file downloads held in memory at once per replica (section 6's "bounded media concurrency"; the next is `429`); one tenant holds half at most (the service's default, not the design's) |
| `WA_SERVER_MEDIA_STREAMS` (M1b) | 16 | streamed downloads at once per replica, one tenant holding half at most (the service's defaults, not the design's) |
| `WA_SERVER_RATE_SEND`, `…_READ`, `…_TEMPLATES`, each with `…_BURST` (M1b) | 20 and 40, 50 and 50, 2 and 2 | section 6's rates, per tenant and replica; the bursts of reads and template management are the service's defaults (section 6 states none) |
| `WA_SERVER_RATE_OTP_ISSUE`, `…_OTP_VERIFY`, each with `…_BURST` (M3c) | 5 and 5, 20 and 20 | section 6's OTP issue rate and the service's default for checks (D46), per tenant and replica |
| `WA_SERVER_ERASURE_KEY` (M2f) | unset: erasure off | at least 32 bytes, from `_FILE` or the environment: the key of the erasure journal's HMACs; unset, `POST /v1/numbers/{pn}/erasures` is `409 erasure_not_configured`, but on memory storage in development (a throwaway key); a shorter one refuses the start (`Invalid`) |
| `WA_SERVER_FLOWS_PRIVATE_KEY` (M5d) | unset: the Flows data endpoint off | D133: the deployment's Flow private key, an unencrypted 2048-bit RSA key in PEM (PKCS#8 or PKCS#1), from `_FILE` or the environment; unset, `POST /webhooks/flows/{waba_id}` is `404`; a value that is not such a key refuses the start (`Invalid`, never quoting it). Its public half is the business public key of each number a Flow is sent from |
| `WA_SERVER_OTP_DEFAULT_TENANT` (M3c) | unset | the tenant whose OTP settings a tenant without its own borrows: the platform's number (D8's default, D45); unset, such a tenant's OTP requests are `409 otp_not_configured` |
| `WA_SERVER_UNDATED_AFTER_MOVE` (S2b) | `operator_only` | D36's `undated_after_move`: an undated event (an error, a history chunk, an undated group update) of a WABA that moved from another tenant is operator-only within the window after the move; `current_holder` gives it to the holder, the behaviour before S2b. A WABA no other tenant held is never concerned |
| `WA_SERVER_UNDATED_AFTER_MOVE_WINDOW` (S2b) | `169h` (7 days and an hour) | the window, from the WABA's last unbinding from another tenant; a positive duration, with `operator_only` only (refused with `current_holder`) |
| `WA_SERVER_MOVE_SKEW_MARGIN` (S2b) | `0s` | after a move only: an event Meta dated must be this much past the second its binding began in, and more, to reach the holder (Meta's clock ahead of the database's) |
| `WA_SERVER_REGISTRATION_ATTEMPTS`, `…_WINDOW` (M3d) | 5, `72h` | D66: a number's registrations and deregistrations per fixed window, shared by every replica |
| `WA_SERVER_VERIFICATION_ATTEMPTS`, `…_WINDOW` (M3d) | 10, `1h` | D66: a number's verification codes requested and checked, the same way |
| `WA_SERVER_STREAMS_PER_TENANT` (M2b) | 20 | live event streams one tenant holds open at once on a replica, its number streams included ([§4.5](#45-live-updates-sse)); the next is `429 too_many_streams` |
| `WA_SERVER_STREAMS_PER_REPLICA` (M2b) | 1000 | live event streams open at once on a replica, every tenant's |
| `RUST_LOG`, `WA_SERVER_LOG_FORMAT`, `OTEL_EXPORTER_OTLP_ENDPOINT` | `info`, `json`, unset | |

### 7.3 Start, observability, shutdown

- `meta-whatsapp-server serve` validates the configuration, runs the
  backend's `SchemaMigrator` (on Postgres, the library's `postgres::migrate`
  and the service's migrations; unless `skip`), opens both listeners.
  Also: `migrate`, `openapi`, `healthcheck`, `admin …`, `vault rotate`,
  `doctor` (roadmap L22a: the configuration, migrations, webhook
  subscription and tokens checked read-only, non-zero on a failure).
- Logs: `tracing` JSON, a span per request (request id, route template,
  tenant, status, duration); library events keep their levels
  ([production guide](../guides/production.md)); optional OpenTelemetry.
- Prometheus metrics, labelled by route template, event type, code and
  outcome, never by id, number or contact (`tenant` only on request):
  requests and latency; Meta webhooks by outcome (a run of `in_flight`
  means sinks outlast the lease); events by type (a rising `unknown`: Meta
  shipped a field worth typing); Graph errors by code; outbox lag; pending
  webhooks-out; open streams; OTP outcomes; idempotent replays;
  token-expiry events by outcome (`recorded`, `failed`; M3e).
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

**Decision (D10): retention and erasure.** The service stores
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
if the platform's privacy obligations require it.
*Decided 2026-09-26 (coordinator, owner's delegation), swappable:
retention is configurable per store (the outbox 7 days by default, the
inbox kept by default), erasure comes through L5 (a person on one
number: `identities` and `erase_all`, D30; their group messages
redacted by default, D31; with conformance cases) and reaches the
service in M2f (a route, the outbox's rows, an erasure journal), and a
moved number is hidden by M2's binding epoch, not purged
([§10](#10-decisions)).* *Built in M2f*: `POST
/v1/numbers/{pn}/erasures` ([§4.2](#42-resources), D86–D91). Its journal
lives outside the database (the service's log, target `erasure_journal`,
which the operator ships elsewhere): after a restore, the operator
computes the same HMAC over every identity the restored tables hold and
erases again those an entry lists; the service replays nothing itself. Which retention and which erasure requests a
deployment's privacy obligations require is a legal call, the
deployer's (§10, legal acts).

### 7.5 Versioning

Within `/v1`, changes are additive; a breaking change is `/v2`, served
beside `/v1` for a deprecation period; webhook endpoints keep their
`api_version`. The OpenAPI document is committed
(`crates/meta-whatsapp-server/openapi/v1.json`) and stays with the axum
API adapter, which is permanent (D18, D26): CI fails when the generated
one differs. From the first release on, `oasdiff` checks breaking
changes against the last release's document; nothing runs it yet, since
there is no release to compare with. From the default flip
([§8](#8-modular-architecture-and-client-sdks)) the committed
`api.cstack` schema is the primary contract: CrateStack's check mode
fails on a stale generated client, and `cratestack diff` checks breaking
changes against the last release. Each adapter's contract is versioned
on its own: a deployment that swaps the API adapter swaps the contract
its callers use (procedures or resource URLs) and their client
(CrateStack's generated one, or one from openapi-typescript). Image, API
contract (spec, then schema) and the TypeScript client share one semver;
`/v1/version` adds the meta-whatsapp-rs revision.

## 8. Modular architecture and client SDKs

> Rewritten on 2026-09-26 under the owner's delegation (D26). M1 is built
> as one crate on axum with a committed OpenAPI document; this section is
> the target, reached by the steps in [roadmap.md](../roadmap.md) (§1,
> "The service's modular split"), each of which keeps `/v1` served and
> `openapi/v1.json` byte-identical until the default flips.

The owner's direction (2026-09-26): CrateStack (the owner's schema-first
framework, [cratestack/cratestack](https://github.com/cratestack/cratestack))
for the API **and the models** is the default (D26; D20 (a), its
licence, decided by the owner on 2026-11-02: yes; the default flips in
roadmap S16), and every choice,
CrateStack included, must be swappable by an integrator: another API
layer, another database such as MongoDB. CrateStack cannot be what swaps
databases: its own decisions rule out a shared backend trait (its ADR
0013) and call a document store a new design (ADR 0016), and
`db = Postgres` is its only sqlx backend. So the swapping lives in our
ports, and CrateStack is one adapter on each side of them.

### 8.1 A framework-free core with ports

`meta-whatsapp-server-core` holds the service's logic and no framework:
no axum, hyper, tower, utoipa, sqlx or CrateStack type. It depends on
the facade (`default-features = false`), so `http`'s types enter only
through the library's `HttpTransport` port. It holds the model (tenants,
keys, bindings), the key format and digests, event routing and the event
type lists, outbox keys and event ids, polling (cursors, `410`, `422`),
the idempotency engine (§5.4), the rate limiter, the §5 error model as
data (a code and a status as a number), authorization as services (a
credential to a caller, a tenant and a scope; ownership to `OwnedNumber`
or `OwnedWaba`, which stay the only path to a vault token), and the
operations (messages, media as a byte stream, templates, numbers, admin,
events).

| Port | Does | Replaces (as built in M1) |
| --- | --- | --- |
| `RecordStore` | tenants, keys, WABA and number bindings | the `Store` trait, idempotency aside |
| `IdempotencyRecords` | claim, complete, release, purge (§5.4) | the idempotency half of `Store` |
| `Outbox` | insert with the routing checked again, page, purge (§2.3) | the `EventStore` trait |
| `LeaderLock` | a lease, `try_exclusive(name, lease)`: one replica at a time, ending at release, drop or expiry | advisory locks in housekeeping |
| `Janitor` | expired key/value rows and the other purges | the direct `PostgresKvStore::purge_expired` call |
| `SchemaMigrator` | the store's migrations, under its own lock | the Postgres migrations |
| `EventNotifier` (M2; built in M2b) | wake SSE and webhooks-out on a new event: a wake-up only, the outbox stays the source of truth | Postgres `NOTIFY`; a memory broadcast; MongoDB change streams |
| `KvStore`, `ConversationStore`, `Clock`, `HttpTransport` | the library's ports ([architecture.md](../architecture.md#ports-meta-whatsapp-core)) | — |

The store and events suites move into
`meta_whatsapp_server_core::conformance` (a feature, like
`meta_whatsapp_adapters::store::conformance`), with the invariants that
cross ports, which a new backend cannot skip: deleting a tenant purges
its stream; a binding changed during an insert leaves the event
operator-only; each tenant's events commit in order.

What Postgres guarantees by accident is a requirement of the ports, so a
MongoDB or CrateStack-models backend cannot meet the traits' text
without it ([roadmap.md](../roadmap.md) S2): the outbox re-checks the
routing atomically with the insert (a typed route guard on each new
event); binding a WABA to a missing tenant has its own outcome, and
binding and deleting one tenant serialize; purges take no lock of their
own, and `LeaderLock` is a lease; all replicas read one clock (the
database's where it has one, else the service's `Clock`); contention is
a typed error; a bundle's accessors return the same data on every call;
listings are in byte order.

**As built (roadmap S1, S2, S2b, S3, S4, S5a, S5b, S5c, S5d, S5e, S6, S7, M2b).** `crates/meta-whatsapp-server-core`
exists, with the model, the keys, event routing and polling, the
idempotency engine, the rate limiter, the error model, authorization,
housekeeping (over a `Backend`, since S4), the admin operations
(since S5a), the message operations and the idempotency engine's
run (since S5b), the number and template operations (since S5c), the
media operations (since S5d), the webhook pipeline (since S5e), and
the pieces every API adapter shares made adapter-neutral (since S6);
since S7 the rest of the service is the crates of [§2.1](#21-the-crates).
What it holds today, where it differs from the target above:

- **The ports** are `RecordStore`, `IdempotencyRecords`, `Outbox`,
  `EventNotifier` (since M2b), `LeaderLock`, `Janitor` and
  `SchemaMigrator`, and their contracts carry S2's requirements, which
  both in-tree backends meet:
  - `WebhookStore` (roadmap M2c, the core's `webhooks_out::store`, in
    the bundle as `Backend::webhooks`, required): endpoints, their
    cursors, deliveries, claims and settlements. Its rules, held by
    `conformance::webhooks_out`: an endpoint is its tenant's through every
    call and goes with it; a creation counts the tenant's endpoints
    atomically; a fan-out moves the cursor from the value it read, with
    its deliveries, once; a claim is exclusive until its lease ends, one
    delivery a tenant a claim, never a busy endpoint's or tenant's, a
    disabled endpoint's or a suspended tenant's; a settlement names its
    claim; the instants compared are the caller's. Memory:
    `MemoryWebhookStore`, under its store's lock; Postgres:
    `PgWebhookStore` (`FOR UPDATE` on the endpoint for a fan-out, `FOR
    UPDATE SKIP LOCKED` for a claim).
  - `EventNotifier` (roadmap M2b, the core's `notify`), object-safe:
    `notify(&Wakeup)` tells every replica that a tenant's stream gained
    an event (a `Wakeup` is the tenant and the event's sequence, nothing
    of the event), and `subscribe(&TenantId)` answers a `Subscription`
    to that tenant's wake-ups on this replica, ready when it returns.
    Three rules, held by the conformance suite `conformance::live`:
    every replica hears a notify (within 2 s), only the tenant's own
    subscriptions are woken, and a notification may be lost or arrive
    twice (a backend that knows it lost some wakes every subscription:
    `Hub::wake_all`). A replica's wake-ups go through one `Hub`, a
    channel per tenant made by its first subscription and dropped with
    its last. Memory: `LocalNotifier`, the process's hub. Postgres:
    `PgNotifier`, `NOTIFY` on `EVENTS_CHANNEL` and one `LISTEN`
    connection per replica feeding its hub. `Outbox::get(tenant, id)`
    reads one event in the tenant's stream only (a default that pages,
    which the in-tree backends answer by an index).
  - `Outbox::insert` must check the routing again, atomically with the
    insert: `NewEvent` carries a `RouteGuard` (a `GuardedBinding`, the
    number under the WABA it was bound under or the WABA, and the
    event's date as `not_after`), which the core's routing sets
    (`events::owner` returns the tenant and the guard, `outbox_row`
    copies it); a row keeps its tenant only while the guarded binding
    names it and is one the event may reach (`RouteGuard::admits`, which
    the routing applies too, and the Postgres insert writes in SQL from
    the bounds the core computes, `RouteGuard::dated_bounds`): a dated
    event's binding began by its second (`attached_at <
    to_timestamp(n + 1)`), or, when the WABA had a previous tenant, in a
    second before it by the skew margin (`attached_at <
    to_timestamp(n - margin)`: an event of a moved binding's own second
    is operator-only); an undated event reaches a binding with a previous
    tenant only once the window after the move passed (the guard's
    `AfterMove::undated_moved_by`, the event's arrival less the window,
    D36). A tenant without a guard is operator-only (§2.3). The memory
    outbox belongs to its `MemoryStore` and checks under its lock.
  - **Moves are recorded** (roadmap S2b). Every unbind (`unbind_waba`,
    `unbind_waba_if`) leaves, in the same step, the WABA's tombstone
    (`model::WabaTombstone`: the tenant it left, when, and when the WABA
    had last left another tenant before), which replaces its previous
    one; a new binding reads it into
    `WabaBinding::previous_tenant_unbound_at` (`WabaTombstone::previous_for`:
    the tombstone's time when another tenant left, else the time it
    carried), and a refresh keeps it. Deleting a tenant makes its
    tombstones a deleted tenant's (Postgres: `ON DELETE SET NULL`), so a
    tenant created again under its id binds the WABA as a move. On
    Postgres, migration 4 adds the columns `wa_server_wabas.generation`
    and `previous_tenant_unbound_at` and the table
    `wa_server_waba_tombstones`, additively: its defaults give the rows
    written before it generation 1 and a previous tenant recorded as
    unknown (`UNRECORDED_MOVE`, the Unix epoch), so they keep S2's rules
    (an event of their own second operator-only; their undated events
    the holder's), and the same for the rows a replica of the previous
    release writes during the rolling deploy. An unbind made by an
    earlier release, before the upgrade or during its rolling deploy,
    leaves no tombstone, so the WABA's next binding reads as a first
    onboarding: a move whose unbind came less than the window before the
    deploy is not protected (the guide: no moves between tenants in the
    7 days and an hour before the deploy, nor during it). A new binding
    on Postgres begins no earlier than the unbinding it follows (its
    `attached_at` moved past the transaction's start and the tombstone's
    time: the sabotage review's race). The new names are stable
    identifiers, pending the owner's confirmation.
  - Referential rules: `bind_waba` to a missing tenant answers
    `BindOutcome::NoSuchTenant`, and `bind_waba` and `delete_tenant` on
    one tenant serialize (Postgres: the tenant's row, `FOR KEY SHARE`
    against `FOR UPDATE`; memory: one lock).
  - Purges take no lock of their own, return a count, and are safe on
    two replicas at once; a housekeeping round (the core's
    `housekeeping::round`, over a `Backend`) takes one `LeaderLock` turn
    for all three.
  - Times: one clock all replicas agree on, the database's where it has
    one (Postgres: `now()`), else the service's `Clock`, its skew below
    the shortest lease it measures. The core's `Authorizer` takes an
    injected `Clock` (`with_clock`); the service hands it the webhook
    pipeline's. The memory stores read the system clock (one process).
  - Contention is the library's typed `StorageError::Busy` (on Postgres,
    a lock timeout, a serialization failure or a deadlock); the webhook
    pipeline answers Meta `503` for it, whatever the backend.
  - The outbox's types carry `PhoneNumberId` and `WabaId`; listings are
    in byte order of their ids.
  - A capability's writes are conditioned on what it was made from
    (below): `unbind_waba_if` and `set_waba_status_if` take a
    `BindingEpoch` (the WABA, its tenant, when the binding began, and
    since S2b its generation, which every attach to the tenant holding
    it moves) and do nothing once the WABA was unbound since, or
    refreshed since.
  - **The port rule between the bindings and the vault**, which no
    transaction spans (the `RecordStore` module's docs, and the
    library's `TokenVault`'s): the vault is written for a WABA only by
    its current holder; an attach binds, stores, then confirms
    (`Authorizer::store_token`: it reads its binding, stores with
    `TokenVault::store_versioned`, reads the binding again, and, when
    the WABA moved meanwhile, takes back exactly its own write with
    `delete_if_unchanged`, which never deletes a token stored after it,
    and answers `503`); every unbind deletes the token before the
    binding. A capability reads the binding, then the vault, then the
    binding again, and trusts what the vault answered only if the
    binding held throughout: with the rule, no token outlives its
    binding, so the next holder's binding never meets the previous
    holder's token. Re-encryptions are compare-and-swaps. What the
    confirmation narrows and does not close: another tenant's attach
    whose binding and store both land between an attach's read of its
    binding and its own store (two operator actions within
    microseconds) has its token overwritten, then taken back with the
    first attach's, so its binding is left with no token (`409
    number_not_connected` until it attaches again), and a capability of
    its own made between that store and the take-back carries the first
    attach's token; a re-encryption in that window leaves the take-back
    nothing to delete. Closing it needs the vault record to name the
    binding it was stored under, a change of the stored format (the
    owner's).

  Their failures are the library's `StorageError`; no port names a
  driver's, a framework's or an API toolkit's type, and an HTTP method
  is its name.
- **`Backend`** gives every port over one database, and the library's
  `KvStore` and `ConversationStore`, with its `kind()` (memory,
  Postgres, or another by name) and `close()`. Each accessor returns a
  handle to the same data on every call: what one call's handle writes,
  the next reads. The service has two, `MemoryBackend` and `PgBackend`,
  crates of their own since S7 (`meta-whatsapp-server-store-memory`,
  `meta-whatsapp-server-store-postgres`).
- **The service is composed from a `Backend` alone** (S4), so a backend
  an integrator brings is served as the in-tree ones are, with nothing
  of the server changed for it. `AppState::from_backend(Arc<dyn
  Backend>, VaultKeys, Client, Inbound, Metrics, Settings)` is the
  state's only constructor: the vault is built on the bundle's
  `KvStore`, and the webhook pipeline on its `KvStore` (dedup leases),
  `ConversationStore` (the inbox), `Outbox` and records (routing);
  `Inbound` holds what deliveries are checked with (the app secrets,
  the verify token) and the service's clock, no store. No constructor
  takes a port on its own, and the HTTP crate's UI tests pin it, the server's before S7 (loose
  ports do not compile into a state). `serve::serve_with(config,
  Arc<dyn Backend>)` (`serve_until` with a shutdown future of the
  caller's) migrates it unless `WA_SERVER_MIGRATE=skip`, mints the
  development admin key when it is process-local, serves both
  listeners, runs housekeeping over it (the core's `housekeeping::run`:
  each round under its `LeaderLock`, purging its `Outbox`, its
  idempotency records and what its `Janitor` sweeps) and closes it once
  the listeners stopped; `config.storage` is not read there. The
  command line opens every backend through a `serve::BackendFactory`
  (the in-tree `BuiltinBackends` by default; `cli::run_with` runs the
  same commands on an integrator's), given the configuration's
  `Storage` (memory, or `DATABASE_URL`): `serve` through
  `serve::serve_from(config, factory)`, the other commands through
  `serve::open`, which migrates it per `WA_SERVER_MIGRATE`. The
  server's `tests/composed.rs` serves the API over real listeners on a
  `Backend` defined in the test, and checks that every store the
  service uses is the bundle's, and that `WA_SERVER_MIGRATE=skip` is
  honoured.
- **`LeaderLock`** is a lease, `try_exclusive(name, lease)`: the turn
  for a name, ended by its release, its drop or its lease's expiry, with
  best-effort exclusion (work under a turn tolerates an overlap).
  Housekeeping's is `HOUSEKEEPING_LEASE`, 5 minutes. Locks are named by
  strings (`HOUSEKEEPING`, `"housekeeping"`); a backend maps a name to
  whatever its database locks. On Postgres the key is the first eight
  bytes of SHA-256(`meta-whatsapp-server/<name>`), big-endian, as an
  `i64` (`lock_key`), held by a transaction whose
  `idle_in_transaction_session_timeout` is the lease (the server ends
  the session, and the lock, past it): so the `housekeeping` turn takes
  `HOUSEKEEPING_LOCK`, the key the purges of the release before S2 took
  in their own transactions, and `MIGRATION_LOCK` is
  `lock_key("migrate")`. Both keys are stable identifiers
  ([architecture.md](../architecture.md#stable-identifiers)): replicas
  of two releases must take the same ones, and do. Memory leases on the
  process's monotonic clock.
- **The admin operations are services** (S5a):
  `meta_whatsapp_server_core::admin::Admin`, opened with
  `Admin::new(&Authorizer, &AdminCaller)` only once that `Authorizer`
  admits the admin (another's is `403 forbidden`, nothing read), acts on
  that `Authorizer`'s records, vault and tokenless client, so a service
  writes the records the order reads and reaches the vault only through
  the capability methods. Each operation takes the request's values as
  it carried them (ids, labels, an RFC 3339 expiry; `TenantsInput`,
  `KeyRequest`), validates them in the order, and on the fields, of
  §4.2's answers, and answers a domain value (`Tenant`, a `Listing`,
  `Minted`, `Attached`, `HeldWaba`, `VaultRotation`) or a
  `ServiceError`: the tenants (create, list, read, rename or suspend,
  delete with its WABAs disconnected first, a moved WABA skipped, five
  rounds), the tenant and platform keys (mint, list, revoke), attaching
  a WABA (D4 on the claimed id before Meta is asked; the numbers Meta
  lists with the given token, at most 1,000; bind, D4 again atomically;
  `Authorizer::store_token`; subscribe, `step: subscribe_app` and
  `resumable` on failure, its `190` marking the numbers), a binding's
  read, the admin unbind (unsubscribe at best with a usable token, then
  forget; without one, `forget_for_admin`) and the vault's rotation.
  The operator's changes are audited by the core (`audit`, target
  `audit`, fields unchanged), and failed Graph calls are told to an
  `observe::Observer` (the server's `Metrics`: the graph error counter),
  the core naming no metrics library. What the command line does on the
  database directly, without an admin key (`mint`, `allowed_tenants`,
  `is_valid_name`), takes no capability. The axum handlers read the
  request, call the operation and render its answer; the conformance
  suite `admin` holds the services to D4 over a backend's bundle, with
  no HTTP layer (a WABA another tenant holds is refused with nothing
  stored, subscribed or shared), on memory and, live, on Postgres.
- **Idempotent operations run under the core's engine, and the messages
  are services** (S5b). `idempotency::run(&Keeper, &Authorizer,
  &Caller, key, &Fingerprint, operation)` claims the key for the
  `Caller`'s tenant (a `Caller` its `Authorizer` admits: another's is
  `403 forbidden`, before the key is read), runs the operation, and
  answers an `idempotency::Answer` (a status, a body and whether it was
  replayed) or a `ServiceError`, settling the key as §5.4 says: the
  answer kept byte for byte, a failure kept as the adapter renders it
  (the `Keeper`'s `RenderError`, the axum one's §5.1 body) when it may
  have been sent, the key released when it proves nothing was, a repeat
  answered from the record (its outcome told to the `Observer`, whose
  new `idempotency` method the server's `Metrics` counts), a cut request
  kept as a `504 timeout`. The `Keeper` holds the records, the lease and
  the expiry; the fingerprint is §5.4's, the operation and its
  canonical input since S6 (below).
  `messages` holds the send request as the request carried it, free of
  utoipa (`SendMessage` and its objects: the axum adapter keeps its own
  types for the OpenAPI document and converts them, every field named on
  both sides), the checks before any request (`refuse_unsupported` for
  a JSON body, `message` for the library's `OutboundMessage`, the
  recipient and its strict E.164 rule, `recipient` and `is_e164`), and
  the `Messages` service on an `OwnedNumber` (opened with
  `Messages::new`, which refuses, `403 forbidden`, a number another
  `Authorizer` made: `Authorizer::admit_number`): `send` answers the
  accepted message (`messages::Accepted`), not a status, and
  `mark_read` marks a received message read, with the typing indicator
  when asked; a failed Graph call is told to the `Observer` and marks
  the numbers after a `190` as every route does. Meta objects a request
  carries are read with no key dropped by `request::meta_object`. The
  axum handlers parse, convert, call and render (`202`, `204`,
  `Idempotent-Replayed: true` on a replay); their `idempotency::run`
  stays crate-private, over the service's records. The conformance
  suite `messages` runs M1.4's case with no HTTP layer, over a backend's
  records, vault and idempotency records: a digits-only phone refused
  before any request, a scripted timeout `may_have_been_sent` and its
  key replaying it with no second request, a `131047` releasing its key,
  another `Authorizer`'s caller refused.
- **The numbers and the templates are services** (S5c). `numbers`
  holds `Numbers` on an `OwnedNumber` (`Numbers::new`, which admits
  it: `Authorizer::admit_number`): `details` (`numbers::NumberDetails`,
  Meta's fields `NUMBER_FIELDS`), `profile` and `update_profile` (a
  `numbers::ProfileChange`, as the request carried it: an empty one only
  reads, `numbers::BusinessProfile`); and `Waba` on an `OwnedWaba`
  (`Waba::new`, which admits it: the new `Authorizer::admit_waba`), whose
  `disconnect` asks `still_bound`, unsubscribes the app, then forgets
  the token and the bindings (`503` for a WABA that moved, nothing
  deleted when Meta refuses). `templates` holds the cache
  (`TemplateCache`, per WABA, query and tenant, bounded, its TTL the
  adapter's: the service's 60 seconds, one per replica), the checks
  before any request (the filters, the name, the id, by
  `request::graph_id`, which moved into the core too; and
  `templates::definition` for a creation, no key dropped, every
  documented limit), and `Templates` on an `OwnedWaba`
  for a `Caller` (`Templates::new` admits both and opens only on a WABA
  of the caller's own tenant, else `404 not_found`): `list` (cached),
  `get` (the bare id asked for its name only, then looked for in the
  WABA's own list of that name: another WABA's is `404`), `create` and
  `delete` (each dropping the WABA's cached pages; a deletion audited
  as the tenant's, `template_deleted` or `templates_deleted`). Their
  answers are domain values free of utoipa, which the axum adapter
  converts into its OpenAPI types field by field; failed Graph calls
  are told to the `Observer`, a refused template id by Meta's code
  though it is answered `404`. The conformance suites `templates` and
  `numbers` hold them with no HTTP layer: tenant B's caller on tenant
  A's template id is not found with zero vault reads (A's WABA refused
  at step 4, A's capability refused by `Templates::new`, A's id not in
  B's WABA, nothing deleted), and the services send Meta's documented
  requests. The tenant's listings (`GET /v1/wabas`, `GET /v1/numbers`)
  stay the axum handlers' reads of the records for the admitted caller.
- **The media are services** (S5d). `media` holds `Media` on an
  `OwnedNumber` (`Media::new`, which admits it:
  `Authorizer::admit_number`), over the adapter's transfer slots
  (`media::MediaSlots`: the core's `ratelimit::Slots` for uploads and
  whole downloads, and for streams, each taken from the number's
  tenant's share, `429` otherwise): `transfer_slot` (an upload's, taken
  before its body is read), `upload` (a `media::Upload`, which only
  `Upload::new` makes: a type of Meta's table, without a control
  character, `checked_type`; a file within its kind's size; the file
  name made inert, `filename`), `download` and `delete`. A download runs
  within `media::DownloadLimits` (only `parse` and `new` make them: 1 to
  16 MiB whole, `MAX_WHOLE_DOWNLOAD`, or to the adapter's cap streamed):
  the media id checked (`request::graph_id`), a slot, the lookup with
  `phone_number_id={pn}` (another node or a refused id `404`, without
  Meta's code), the reported size (`413` before the download), Meta's
  digest (`502 integrity` when unusable), the library's download (its
  host allow-list keeps the token to `lookaside.fbsbx.com`: another
  URL is `502 upstream`, nothing sent), then the body verified by the
  library's `VerifiedBody`: whole (`DownloadBody::Whole`, read to
  `max_bytes`, a mismatch `502 integrity` with no byte answered), or as
  `media::MediaStream` (`DownloadBody::Stream`), a `futures::Stream` of
  the library's `Bytes` and no HTTP framework's type, holding one chunk
  back: on a mismatch it yields `StreamStopped::Integrity` instead of
  the last chunk (`TooLarge` past `max_bytes`, `Failed` on a failure),
  logged at `warn`, and it holds its slot until dropped. M1b's rule is
  therefore the core's: the axum adapter renders the stream as the
  body, whose error aborts the connection, so the last chunk never
  goes out and the body never completes. The answer (`media::Downloaded`)
  carries the content type to answer (Meta's when it is a plain MIME
  type) and Meta's digest; the adapter adds `X-WA-SHA256`, `nosniff`,
  `Content-Disposition: attachment` and the sandboxing CSP. A deletion
  looks the id up first, as the number's media, and deletes only that.
  Failed Graph calls are told to the `Observer`, a `190` marks the
  WABA's numbers, Meta's `details` are kept; the module's three log
  lines keep the target `meta_whatsapp_server::api::media`. The axum
  adapter keeps the multipart reading of an upload (the framing budget,
  the per-kind cap, `media::upload_cap`, applied as the file streams
  in), the upload's `Idempotency-Key` under `idempotency::run`
  (`Idempotent-Replayed` on a replay), the OpenAPI types, and the
  conversion of its query into `DownloadLimits`. The conformance suite
  `media` holds it with no HTTP layer, over a transport whose downloads
  arrive in the chunks the case gives: a tampered file streamed yields
  its first two chunks of three, then the error; a file still arriving
  is passed on with one chunk held; past `max_bytes`, nothing; no failure
  or odd chunking (empty chunks, a connection cut mid-file, one chunk,
  none) yields the last chunk unverified, and every failure gives its
  slot back. *Since M2e* (OQ #43, D77–D79) `Media::new` also takes the
  bundle's `Outbox`, and before Meta is asked a download or a deletion
  reads where the id was recorded as received (`Media::received`, over
  `Outbox::received_media`: the rows whose `NewEvent::received_media`
  was the id, kept at insert, on Postgres `wa_server_events.media_id`
  and its partial index, migration 7): `media::Received::Here` (a row of
  this number routed to its tenant, received from `history_from` on) is
  downloaded unscoped once `OwnedNumber::still_held`, `Elsewhere` is
  `404` with no request (one `info` line, no id), `Never` keeps the
  check; a deletion is never unscoped. The suites hold it: `outbox`'s
  `received_media` (the id kept at insert, every stream searched, a
  redelivery adding no row, gone with its tenant) and `media`'s
  `a_media_id_received_elsewhere_is_not_found_with_no_request`.
- **The webhook pipeline is the core's** (S5e). `inbound` holds
  `Inbound` (the app secrets, the verify token and the service's clock,
  no store: the server re-exports it) and `Pipeline`, built on a
  `Backend` alone (`Pipeline::new(Inbound, &dyn Backend, Arc<dyn
  Observer>)`: the dedup leases in its `KvStore`, the inbox in its
  `ConversationStore`, the events in its `Outbox`, routed by its
  `RecordStore`). `Pipeline::deliver(signature, body)` takes the body as
  read and its `X-Hub-Signature-256` value, checks the signature against
  every app secret before anything else (`Refused::Unauthenticated`,
  `401`: no turn waited for, no store touched, no line logged), takes a
  turn to record (`MAX_DELIVERIES_RECORDING`, 4, within `RECORDING_WAIT`,
  10 s, else `Refused::Busy`, `503`), runs the library's
  `WebhookHandler` (3 MiB, `Refused::TooLarge`; the parse, a signed
  body that is not a webhook becoming one `unparsed` event, acknowledged;
  each event under its dedup lease, `Refused::InFlight` while another
  request holds one), and hands each event to its sink: routed by the
  records (`events::route`, the allow-list above: never by what the
  payload says of a tenant), the inbox when a tenant owns it, then the
  outbox row (a busy outbox `Refused::Busy`, a failed store
  `Refused::Failed`, `500`, the event's lease released). It answers
  `Delivered` (the library's report: `200`) only once every event is
  recorded or was already. `Refused::status` and `outcome` are the
  answer and its metric label as data; what the pipeline counts is told
  to the `Observer`, whose new methods `webhook_event`,
  `webhook_duplicates` and `webhook_failure` the server's `Metrics`
  counts as before (`wa_server_webhook_events_total`,
  `_duplicate_events_total`, `_sink_failures_total`), and its log lines
  keep the target `meta_whatsapp_server::events` (`inbound::LOG_TARGET`)
  and their fields. The sink is private to the core (a UI case pins it):
  an event reaches the stores only through `deliver`, after its
  signature. The axum adapter keeps what an HTTP body needs: the
  signature header's shape before a byte is read (`401`), its 64 places
  (`503`), the body read (3 MiB, `413`; 15 s, `408`), the refusals'
  log, at most one line a minute per reason, the outcome's count
  (`wa_server_webhook_deliveries_total`) and the subscription handshake
  (`GET`, the library's `verify_subscription` with the pipeline's verify
  token). Since S7 that adapter half is `meta-whatsapp-server-http`'s,
  shared by every API adapter (before, the server's). The conformance
  suite `webhooks` holds the pipeline with no listener, over a view of
  the backend that records what reaches its key/value store and its
  outbox: M1.2's routing cases (an unowned number's event, a number
  under another WABA than Meta says, a payload naming a tenant, a replay,
  an unbound WABA: operator-only; each tenant polls exactly its own, the
  inbox only an owned number's message), an event dated before its
  binding or seen past the replay window operator-only (each check on
  its own), a number moved while its event records giving it to neither
  tenant, the signature before any store or turn, at most
  `MAX_DELIVERIES_RECORDING` deliveries recording (another waits, with
  no lease claimed; its `503 busy` after `RECORDING_WAIT` is a case on a
  paused clock, outside `run`), each event once under its dedup lease,
  the answer only once every event is recorded (the inbox written
  first), and `unknown` and `unparsed` acknowledged as operator-only
  rows.
- **`ServiceError`** is the §5 error model as data: a code of `CODES`
  (§5.2), its status as a number, `retryable`, `may_have_been_sent`, the
  field for `invalid`, the step and `resumable` of a multi-step call, and
  Meta's code, subcode, trace id and, where a route opts in, bounded
  `details`. An API adapter renders it (the axum one: `ApiError`, the
  §5.1 body), and the error's code table is §5.2's, read by the
  core's and the service's tests. Since S6 a code is an `ErrorCode`
  constant (`ErrorCode::NOT_FOUND`), generated with `CODES` and
  `ErrorCode::ALL` from one table (`codes!` in `error.rs`, which those
  tests hold equal to §5.2), and `ServiceError::new` takes one: a
  misspelled code does not compile (a `compile_fail` doctest).
- **Capabilities are branded with their issuer.** `Authorizer::new` is
  public, since an API adapter in another crate builds one, so anyone
  can build an `Authorizer` over records of their own and have it make a
  `Caller` or an `AdminCaller`. Each `Authorizer` therefore has an
  identity of its own (an allocation made in `new`, compared by
  address), which every `Caller`, `AdminCaller`, `OwnedNumber` and
  `OwnedWaba` it makes carries, and every core method taking one
  (ownership, opening a WABA for an admin, storing, rotating or deleting
  vault tokens, marking numbers after a `190`) refuses one another
  `Authorizer` made: `403 forbidden`, logged at `warn`, before it reads
  or writes anything (the core's security review, SR-H1). What an
  adapter does with a capability on its own asks the `Authorizer`
  nothing, and a forged `Caller` names any tenant: so an adapter admits
  a capability it took from outside its own code (`Authorizer::admit`,
  `admit_admin`) before it acts on it, and keeps what acts on one
  private to its crate. The axum adapter's `Caller` and `AdminCaller`
  extractors admit what they find in a request's extensions, and its
  handlers and `idempotency::run` are crate-private (a second review
  handed two public handlers forged capabilities and minted a real
  platform key). The brand therefore holds for the core's methods, the
  server's extractors and every handler, which only the routers reach,
  behind their guards. The `Authorizer` also refuses a Graph client
  built with a token (SR-L4), and what must not compile outside the
  service's crates (the vault's methods, a capability of one's own, the
  `Authorizer`'s records, since S5a; in
  the HTTP crate (the server's before S7) `AppState::store` and `authz`,
  a `Grant` made from a state and `idempotency::run`, in the API adapter
  the handlers) is pinned by UI tests with the compiler's errors.
  Deleting an opened WABA's token and binding, and marking its numbers
  `reconnect_required` after a `190`, are conditioned on what the
  capability was made from (SR-L2, S2): an `OwnedNumber` and an
  `OwnedWaba` keep the WABA's `BindingEpoch`, and an `OwnedWaba` the
  version of the vault record its token came from (the library's
  `TokenVault::get_versioned`). `OwnedWaba::forget` deletes the token
  only if that record is unchanged (`TokenVault::delete_if_unchanged`),
  then the binding only if unchanged (`unbind_waba_if`), and answers
  whether both went; the server answers `503 storage_unavailable`
  (retryable) when they did not. `OwnedNumber::failed` and
  `OwnedWaba::failed` mark through `set_waba_status_if`. So a WABA
  unbound and attached again since (the same tenant or another, a new
  token) keeps its binding, its token and its numbers' status. What a
  capability is made from is consistent: `Authorizer::open` and
  `owned_number` read the WABA's binding again once its token is read,
  and make the capability only while its epoch is unchanged (else `503
  storage_unavailable`, retryable), whatever the vault answered (no
  token included: a new holder bound, its token not stored yet), so a
  WABA moved between the two reads never pairs one holder's binding
  with the next holder's token, and the admin unbind never takes a new
  holder's missing token for its own. The admin's tenant deletion makes
  its capabilities from bindings it listed earlier: a WABA it cannot
  open is read again and skipped when it moved, and the next round
  lists what the tenant holds now. Before calling `unsubscribe_app` with
  a capability's token (the tenant's disconnection, the admin unbind, a
  tenant's deletion), the route asks `OwnedWaba::still_bound`: moved
  since the capability was made, the call is not made (`503`, or
  skipped by the deletion), as it would remove the new holder's
  subscription. The call's own window stays: `forget` then finds the
  binding moved, deletes nothing, and logs at `warn` (the WABA's id)
  that the new holder's subscription may be gone (S2's security review,
  L1). A binding refreshed for its own tenant without an unbind (the
  operator attaching the WABA again with a new token, as a reconnect or
  a token rotation does) is another generation since S2b
  (`WabaBinding::generation`, a column on Postgres), so a `190`
  answered, after the refresh, to a capability made before it (the old
  token revoked meanwhile) marks nothing; `attached_at` does not move
  (the route guard and the API read it as when the binding began, and
  moving it would send a tenant's in-flight events to nobody). A
  capability made while a refresh lands is refused as one made while
  its WABA moves is (`503`, retryable), and so is an attach whose
  confirmation finds another attach of the same tenant landed after its
  binding (its write kept, a token of the binding's own tenant: taking
  it back could leave the binding with none when it had overwritten the
  other attach's, which answered success; the sabotage review of S2b).
  The admin's unbind of a WABA without a usable token, and the admin's
  own `190` right after attaching, are not a capability's and stay
  unconditioned.
- **OTP is the core's** (M3c). `otp` holds `OtpCodes`, built by the
  HTTP crate's `AppState::from_backend` on the bundle's `KvStore` (the
  library's `wa.otp` and `wa.otp.rate`, unchanged) and the service's
  clock, with the deployment's `OtpDeployment` (the pepper's `OtpKeys`
  and the default OTP tenant); `OtpCodes::open` admits the `Caller`, and
  its `Otp` service issues and verifies through the library's
  `OtpService` under the caller's tenant as the namespace. The sending
  number is opened by `Authorizer::otp_sender`, crate-private (a UI case
  pins it): steps 4 and 5 for the tenant whose settings name the number
  (the caller's, or the default OTP tenant's), never a request's value.
  The settings are a `RecordStore` port change, `otp_settings` and
  `set_otp_settings` (a stored layout of the core's,
  `OtpSettings::to_stored`), set by the admin services
  (`Admin::set_otp_settings`, the number checked to be the tenant's).
  The idempotency fingerprint of an issue is keyed
  (`Fingerprint::issue_otp`, D47). The conformance suite `otp` holds a
  backend to the settings' record and runs M3.5's namespace case with no
  HTTP layer.
- **Embedded Signup is the core's** (M3a). `signup` holds `Signups`
  (the library's `SignupSessions` on the bundle's `KvStore`, the
  deployment's `SignupDeployment`), opened for an admitted `Caller`
  (`Signup`: `start`, `complete`, `attempts`, `resume`), and
  `AccountUpdates`, the webhook pipeline's `AccountActions`
  (`Pipeline::with_account_actions`), which offboards a merchant on
  `account_update` once the event is recorded (routed while the WABA is
  still bound); `Signups::offboard` is a Solution Partner's
  disconnection (`numbers::Waba::offboarded_by`), and
  `Signups::reshare_credit_line` the operator's re-funding. The attempts
  and the vault's records kept past a binding are a `RecordStore` port
  change (`save_signup_attempt`, `signup_attempt`, `signup_attempts`,
  `delete_signup_attempt`, `set_credit_allocation_if`,
  `record_vault_entry`, `vault_entries`: both in-tree backends, the
  Postgres one in migration 6), held by the conformance suite `signup`,
  which also runs acceptance test M3.4's D4 case and a rotation reaching
  an offboarded WABA's ledger with no HTTP layer.
- **Adapter-neutral pieces** (S6). The tenant operations are named by
  id (`operation::Operation`: the HTTP adapter's `operationId`s, with
  their scope), and what the core decides about a request follows its
  operation, not how an adapter spells it. A request's rate class is its
  operation's (`RouteClass::of(Operation)`), charged with
  `RateLimiter::charge(tenant, operation)`, the only public way to spend
  a budget (the axum adapter resolves a request to its operation by its
  method and route template, from its OpenAPI document; `HEAD` is its
  `GET`'s). The idempotency fingerprint (D27) is the operation's id and
  its canonical input, defined once in the core
  (`idempotency::Fingerprint::send_message`, `upload_media`,
  `create_template`): the path parameter as a typed id, decoded, and the
  JSON body canonical (object keys sorted, whitespace ignored) or the
  checked upload's type, file name and SHA-256; no method and no path,
  so two spellings of one path (percent-encoded or not) replay one key
  and every adapter derives the same fingerprint. For one TTL after the
  upgrade the axum adapter also matches a record stored under the
  fingerprint of before (`Fingerprint::or_legacy` with `legacy_v1_json`
  or `legacy_v1_upload`: method, path as sent, body), so a key claimed
  before S6 keeps replaying (D27); a claim stores the new fingerprint.
  The `Authorizer`'s
  admin writes to the vault (`store_token`, `rotate_vault`,
  `forget_for_admin`, and `graph_failed_for_admin` after an attach's
  `190`) are private to the core, reached only through `admin::Admin`,
  opened by `Admin::new`'s admission, and each checks the admin again;
  UI cases pin that they, and a budget charged by class, do not compile
  outside the core.
- **Conformance** is the core's (S3): `meta_whatsapp_server_core::conformance`,
  behind the feature `conformance`, runs over a `Backend`, one empty
  backend per suite from a factory (`conformance::run`). Its suites are
  `records` (the record and idempotency ports, the referential rules,
  byte order), `outbox` (streams, pages, dedup, purges), `invariants`
  (what crosses ports: every accessor hands out the same data, deleting
  a tenant purges its stream, the `RouteGuard` cases, each tenant's
  inserts commit in sequence order), `capabilities` (SR-L2, the
  `failed` race, M1; since S6 the vault's admin writes, each refusing
  another `Authorizer`'s admin), `races` (S2's races), `admin` (S5a: the admin
  services' D4 refusal, with nothing stored, subscribed or shared; the
  core's own `tests/admin.rs` runs it on a memory backend of its own,
  with no server crate), `messages` (S5b: M1.4's case through the
  message services and the idempotency engine; the core's own
  `tests/messages.rs` runs it on that memory backend, with idempotency
  records of its own), and `templates` and `numbers` (S5c: another
  tenant's template id not found with zero vault reads, and Meta's
  documented requests; the core's `tests/templates.rs` and
  `tests/numbers.rs` run them on that memory backend), and `media` (S5d:
  a download through the core's stream ends before its last chunk on a
  digest mismatch, M1b's rule without axum; Meta's documented upload
  and deletion; the core's `tests/media.rs` runs it on that memory
  backend, `tests/media_logs.rs` pins its log lines' target), and
  `webhooks` (S5e: M1.2's routing cases through the core's webhook
  pipeline with no listener, an unowned number's event operator-only,
  a stale binding's or a replay's too, a move while recording; the
  signature before any store; the turns to record; dedup, the answer
  after the recording; the core's `tests/webhooks.rs` runs it on that memory
  backend, which gains an outbox and an inbox, and
  `tests/webhooks_logs.rs` pins its log lines' target). Each backend
  runs it in its own crate since S7: the memory one in
  `meta-whatsapp-server-store-memory`'s `tests/conformance.rs` (the
  service's `tests/store.rs` before), the Postgres one live in
  `meta-whatsapp-server-store-postgres`'s `tests/live_postgres.rs`; the
  memory crate's tests also show what it refuses: a backend whose `kv()`
  is a new store on each call, an outbox that ignores the route guard
  (S2's security review, L4), and outboxes that bend the guard's date
  (L3), each through `conformance::run` or `invariants::run`, not only
  the case. The Postgres-only tests (locks held across sessions, two
  purges at once, a deadlocked insert) are the Postgres crate's: they
  reach into Postgres's own SQL. The live tests over the whole service on
  Postgres stay the binary's.
- **The crate split** (S7, [§2.1](#21-the-crates)). The backends, the
  HTTP layer (`meta-whatsapp-server-http`) and the axum API adapter
  (`meta-whatsapp-server-api-axum`) are crates of their own, which the
  binary composes by its features `api-axum`, `store-memory` and
  `store-postgres` (all default; `just features` builds `api-axum` with
  each backend alone). `openapi/v1.json` is byte-identical, generated by
  the adapter and committed beside the binary, whose `openapi` command
  prints it. An API adapter's handlers are another crate's than the
  state's, so the state's records, its authorization order and the
  idempotency engine reach them through a `Grant`
  (`meta_whatsapp_server_http::state::Grant`), which
  `AppState::from_backend` hands only to the state's composer, beside
  the state: whoever composes the state holds the backend already, and
  a handler or a router given only the state still reaches none of them
  (`AppState`'s accessors stay crate-private; a UI case pins that no
  crate makes a `Grant` from a state). The adapter's router is served
  with the grant as its state; the HTTP crate's handlers take the
  `AppState` it gives. The state no longer holds the backend (its
  composer closes it). The migrations moved to the Postgres crate byte
  for byte: sqlx knows a migration by its version and checksum in
  `wa_server_sqlx_migrations`, under `MIGRATION_LOCK`, all unchanged, and
  a live test migrates a database as `main`'s binary did and starts the
  split binary on it with no migration run again
  (`live_postgres_a_database_mains_binary_migrated_starts_this_one_with_no_migration`).
  Every moved module's log lines keep their target
  (`meta_whatsapp_server::<module>`, explicit since S7; the service's
  `tests/logs.rs` checks no line a request logs carries a new crate's
  path, and reads every log line of the new crates' sources: each names
  its module's `LOG_TARGET`, pinned to the path it had), and the
  metrics' names and labels are unchanged. The binary re-exports the
  moved modules under their old paths.

### 8.2 The backend bundle is the unit of swapping

`RecordStore` and `Outbox` are not independent: every outbox insert
reads the bindings again, atomically with the insert (on Postgres under
a lock; roadmap S2), and deleting a tenant writes the stream table. So one factory, `Backend`, returns every
port over one database, with its kind and capabilities, and ports from
two databases are never mixed. It replaced (roadmap S1) the optional
Postgres pool the service used as its "memory?" flag: that is
`BackendKind::is_process_local` now.

| Backend | Database | Status |
| --- | --- | --- |
| `meta-whatsapp-server-store-cratestack` (the default once roadmap S16 flips it, D20 (a) being decided by the owner on 2026-11-02: yes; until then `meta-whatsapp-server-store-postgres`) | Postgres: CrateStack models for the records tables (tenants, API keys, WABAs, numbers); the outbox, idempotency, locks, janitor and migrations reuse store-postgres's SQL on the same pool, and the library's stores are `PostgresKvStore` and `PostgresConversationStore` on it | a spike crate since S11 (one model, `WaProbe`, called as the system principal under `@@allow(..., auth().isSystem())`, composed into no binary; its names pending the owner's confirmation before the first release); the hybrid is S14 |
| `meta-whatsapp-server-store-postgres` | Postgres through sqlx: today's code and migrations | built (a crate since S7); kept and conformance-tested, live |
| `meta-whatsapp-server-store-memory` | memory, development only | built (a crate since S7); kept and conformance-tested |
| `meta-whatsapp-server-store-mongodb` | MongoDB, a replica set | a spike (roadmap S9): its `MongoOutbox` implements the `Outbox` alone, each write a multi-document transaction (the insert writes the guarded binding's document, so an unbinding conflicts with it, and draws its sequence from its stream's document, so a stream commits in order), and passes the conformance suites live, as a bundle's outbox beside the memory backend's other ports, its bindings a projection the records' owner writes (the test fixture until S18); not composed into the binary. No port changed. Then the library's MongoDB adapters (S17) and the whole bundle (S18) |

### 8.3 API adapters

`meta-whatsapp-server-http` (axum) is shared by every adapter: the listeners,
telemetry, request ids, authentication before the body is read, rate
limits, the §5 renderer, and the routes CrateStack cannot express
(`/webhooks/meta`, media upload and download, SSE, the operations
routes). Both CrateStack and our routes use axum 0.8, so their routers
merge. *As built in S7*: a crate, with the state and its `Grant` (what
an adapter's handlers reach beyond the state; §8.1), the guards and
extractors, the internal listener's layers around an adapter's routes
(`api::internal_router_with`), the public router, and the media and
operations routes as documented route groups (`api::media::routes`,
`api::ops::routes`) an adapter merges into its router and its document;
SSE since roadmap M2b: `api::events::stream_routes`, a route group an
adapter mounts behind its `events` scope, over the core's
`live::Streams`, with the event envelope (`api::events::EventEnvelope`,
which moved here from the axum adapter, its schema unchanged).

| Adapter | Contract | Status |
| --- | --- | --- |
| `meta-whatsapp-server-api-cratestack` (the default once roadmap S16 flips it, D20 (a) being decided by the owner on 2026-11-02: yes; until then `meta-whatsapp-server-api-axum`) | `api.cstack`: procedures, types and enums, REST (D19) | a crate since S10, one procedure (`version`, as `GET /v1/version`), compiled in by the binary's `api-cratestack` feature (not a default) and not served; the rest of the routes, and serving them, in S13 |
| `meta-whatsapp-server-api-axum` | today's `/v1` resource routes and `openapi/v1.json` | built (a crate since S7); permanent, the swap target: every new route lands in it too, and it stays conformance-tested |

The binary composes one adapter and one backend: Cargo features choose
which are compiled in (`api-cratestack` and `store-cratestack` by
default once S16 flips them, D20 (a) being decided by the owner on
2026-11-02: yes; `api-axum`,
`store-postgres` and `store-memory` otherwise), and configuration picks
among them. Every route is a core service, so both adapters stay thin,
and every new route lands in both (D26) from roadmap S13 on, when the
CrateStack adapter is first served (coordinator's decision 2026-11-02,
roadmap § Routes). Swapping the API adapter swaps
the wire contract and the client with it: procedures and CrateStack's
generated client, or resource URLs and a client from openapi-typescript.
The authorization (M1.3), error (M1.5) and idempotency (M1.4) suites run
against every adapter, enumerated from its contract so a new route
cannot skip them. *As built in S7*: the features are `api-axum`,
`store-memory` and `store-postgres`, all three by default; with one
API adapter there is nothing to pick, and the storage configuration
(`DATABASE_URL`, or memory in development) picks the backend, a build
without it refusing that storage at start, naming the feature. *As
built in S10*: a fourth feature, `api-cratestack`, not a default (its
name pending the owner's confirmation before the first release),
compiles in `meta-whatsapp-server-api-cratestack`, re-exported as
`meta_whatsapp_server::api_cratestack`; the internal listener still
serves `api-axum` alone, so nothing picks an adapter yet (S13 adds that
setting), and `openapi/v1.json` is unchanged. The adapter's schema is
`api.cstack` with one procedure, `version` (`POST /$procs/version`,
`@allow(true)`, JSON only, a 1 KiB body bound of its own), answering
`GET /v1/version`'s fields; `cratestack-api` 0.17 (`~0.17.2`, no
default features but `codec-json`) is its only CrateStack dependency.
The procedure, the `router(ApiVersion)` signature, the 1 KiB bound, the
re-export without serving and the pin are coordinator's decisions of
2026-11-02 under the owner's delegation, each swappable (roadmap S10,
**Decided**).

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
- axum, hyper and tower types, and utoipa derives, in the core; `http`
  types only as the library's `HttpTransport` port carries them.
- A Meta token, anywhere but behind `OwnedNumber` or `OwnedWaba`.

### 8.6 Where CrateStack forces a compromise

- URLs become procedures (`POST /[v1/]$procs/<name>`: no `GET`, no path
  parameters).
- Two error shapes until D16 lands upstream: our outer layer and our
  custom routes answer §5, the generated routes CrateStack's own (a flat
  `{code, message, details}` with its fixed codes and statuses, no 410,
  413, 502 or 504). So the CrateStack adapter is not the default before
  D16 (roadmap S16).
- Authentication sits in an outer layer: CrateStack's auth hook receives
  a body it has already buffered, and §3.3 answers `401` before the body.
- Ownership `404`s move inside the procedures.
- Our idempotency middleware (§5.4 releases a key when nothing was sent;
  CrateStack's keeps every outcome) and our per-route body limits stay.
- About 40% of the HTTP surface stays custom axum (§8.3).
- The models cover the records tables only: the outbox insert, the
  idempotency claim, the purges and the migrations need what models
  cannot express (`FOR KEY SHARE`, conditional upserts, advisory locks,
  the database's clock, `json` rather than `jsonb`, which keeps U+0000);
  each gap is an issue upstream (roadmap U4).
- chrono and a system principal enter the service's dependency graph,
  and `ring` is linked beside aws-lc-rs: sqlx picks it for Postgres TLS
  in those builds until upstream U2 (D20). *As built in S10*: with
  `cratestack-api` alone (`db = None`) no CrateStack crate brings sqlx,
  so sqlx keeps aws-lc-rs; `ring` enters through CrateStack's HTTP
  client (`cratestack-client-rust`, rustls with `ring`), which makes
  rustls' process default ambiguous (`ClientConfig::builder()` panics)
  and, once constructed, installs `ring` as that default if nothing did
  first. The binary installs aws-lc-rs before any command runs
  (`meta_whatsapp_server::tls`). sqlx names aws-lc-rs itself but builds
  its `sslmode=verify-ca` verifier from the process default, so without
  the install a connection in that mode panics in such a build. sqlx's
  `ring` arrives with
  `cratestack-pg` (S14), until U2.

### 8.7 Client SDKs

- **The primary contract is the CrateStack schema** and the clients
  generated from it (D18): TypeScript for Medusa and the CMS, speaking
  JSON on the wire rather than CBOR (D19), Dart and Rust as needed.
  `openapi/v1.json` stays committed and tested with `api-axum`, which is
  permanent (D26): a deployment that swaps to it swaps the contract too,
  and its callers use a client generated by openapi-typescript.
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

**Decision (D11): publishing the client.** (a) Public npm under
the organization's scope; (b) GitHub Packages (consumers need a token even
for a public repository); (c) vendored by consumers. *Recommendation: (a)*,
matching the public, MIT-licensed repository. *Decided 2026-09-26
(coordinator, owner's delegation), swappable: (a); the publish workflow
is prepared, and publishing is the owner's ([§10](#10-decisions)).*

**Decision (D12): a Medusa module or plugin.** (a) None: Medusa's
developers call the client from their own module; (b) a Medusa v2 plugin in
its own repository: a notification provider (order templates), an auth
provider (WhatsApp OTP), a route turning webhooks-out into Medusa events;
(c) (a) now, (b) as a fifth milestone after the first integration.
*Recommendation: (c)*: before one integration exists, a plugin guesses at
Medusa's workflows. *Decided 2026-09-26 (coordinator, owner's delegation),
swappable: (c), after the first integration ([§10](#10-decisions)).*

## 9. Delivery plan

Each milestone goes through the project's pipeline (implement, adversarial
review with distinct lenses, remediate, re-run the original failure, `just
ci` on the final head) and names its companion docs and skills in the PR.
Tests use `ScriptedTransport` (method, path, token, exact JSON,
`remaining() == 0`); live tests are `live_*`, and `just test-live` gains
`-p meta-whatsapp-server` under `META_WHATSAPP_RS_REQUIRE_LIVE=1`. M1
shipped as three pull requests (below); M2, M3 and M5 ship as the
[roadmap](../roadmap.md)'s items M2a–M2h, M3a–M3f and M5a–M5l, and the
library changes from L7 on are the roadmap's too.

| # | Library change (own PR, own parity) | When | Kind |
| --- | --- | --- | --- |
| L1 | `ErrorKind::as_str()`, stable snake_case, pinned by a test (with `ErrorKind::ALL`) | done (M1a) | additive |
| L2 | `OtpConfig::namespace` required | done (#4) | breaking |
| L3 | Solution Partner credit-line step in onboarding | done (#5: `onboard_with_approval`, `offboard`, credit ledger) | additive |
| L4 | a code-less `OnboardingRequest` for `resume` | done ([roadmap.md](../roadmap.md) L4: `OnboardingRequest::for_resume`) | additive, plus one breaking type change (`OnboardingRequest::code` became an `Option`) |
| L5 | the `ConversationStore` port change: erasure and retention per store (D10), window events and thread ownership (OQ #32, #44), a lookup by message id, the coexistence contacts; one change so adapters change once | decided (2026-09-26): [roadmap.md](../roadmap.md) L5, before M2 | port change |
| L6 | [architecture.md](../architecture.md): dependency rule for binaries, a "Service" section | done (M1a) | docs |

| | Scope | Docs and skills it adds |
| --- | --- | --- |
| **M1** skeleton, auth, messages and templates, webhooks in | the crate, fail-closed configuration, both listeners, storage and migrations, tenants, keys, admin API and CLI bootstrap, admin attach, the authorization order, messages, media, templates (list, get, create, delete), `/webhooks/meta` into inbox and outbox, `GET /v1/events`, errors (L1), idempotency, rate limits, health, metrics, tracing, the committed spec | `docs/guides/server.md` (run, configure, tenants, keys, first send); a README section "Not writing Rust? Run the service"; L6; a `docs/coverage.md` row; skills `meta-whatsapp-rs-server` (hub for HTTP callers: deploy, credentials, errors, idempotency, routing) and `meta-whatsapp-rs-server-send` (messages, templates, media); the skills gate below |
| **M2** inbox, live updates, webhooks out | inbox routes (their reads filter by the number's binding epoch: a number moved to another tenant shows it nothing from before its binding, the inbox half of security review SR-M3; D10 hides, never purges), SSE (an `EventNotifier` port: `LISTEN/NOTIFY` on Postgres; `Last-Event-ID`), `GET /v1/events/{id}`, webhook endpoints, dispatcher, retries, destination allow-list, the service's number events, and the three types PR #17 typed made tenant-visible (D25: `standby_observed`, `thread_control_changed`, `user_action_reported` into `TENANT_EVENT_TYPES`, `KnownEventType` growing, additive) with the inbox's answer to `OPEN_QUESTIONS.md` #44, retention per store (D10), the received-media exemption (`OPEN_QUESTIONS.md` #43) | `server.md` inbox and events; skill `meta-whatsapp-rs-server-inbox` (inbox API, relaying live events to the CMS's browsers, receiving and verifying webhooks-out) |
| **M3** Embedded Signup in both modes, OTP | signup routes, persisted attempts, disconnection, coexistence sync, authentication templates, OTP and its per-tenant settings; needs L2 (and L3 for partner mode). Vault rotation extended to the records partner mode keeps past a binding (credit ledgers that outlive their token, revocation markers: the library's `rotate_business`), without which a rotation's empty `failed` no longer means the old key is unused | `server.md` onboarding and OTP, and its key rotation advice ("drop the old key once `failed` is empty", caveated since M1a) made true again; `docs/guides/production.md` gains the vault key cadence of `OPEN_QUESTIONS.md` #9; skills `meta-whatsapp-rs-server-onboarding` (the CMS connect flow through the service: page, relay, PIN, resume, both modes) and `meta-whatsapp-rs-server-otp` |
| **M4** packaging: Docker image, TypeScript client, docs | the image and its CI (D9), its contents listed and the image's third-party licences listed in the image (no `ffmpeg`: conversion is an optional feature or a sidecar the deployer adds, roadmap L19), a Compose file, `clients/typescript` generated by CrateStack (D20 (a) decided by the owner on 2026-11-02: yes; a refusal would have meant one from `openapi/v1.json` by openapi-typescript) and published (D11), the documents route, the deployment guide. Re-scoped on 2026-09-26: the move onto CrateStack is the modular split's ([§8](#8-modular-architecture-and-client-sdks), [roadmap.md](../roadmap.md) S1–S16; the default flips once D16 lands upstream), no longer this milestone's | `server.md` deployment (Docker, Compose, Kubernetes notes); a `docs/guides/README.md` row; skill `meta-whatsapp-rs-server-typescript` (install, calls, errors, idempotency, SSE, webhook verification in Medusa or any Node backend); `meta-whatsapp-rs-production` points to the service |
| **M5** the modules parity requires (added 2026-09-26) | routes over the library modules §1 once left "on demand", one PR per family ([roadmap.md](../roadmap.md) M5a–M5l): the send union's other types (pin, request contact info, Direct Send, interactive carousels, voice call, location request, address, call permission request, Flow, product messages), template edit, library, migration, comparison and unpausing, numbers and profile settings, WABA management, Flows, calling, groups, commerce, QR codes, analytics, block users, the Marketing Messages API and CTWA (the max-price agreement an explicit, audited operator action, off by default: `OPEN_QUESTIONS.md` #45), In-App Signup (accepting Meta's terms an explicit, audited operator action, off by default: `OPEN_QUESTIONS.md` #26), partner APIs, conversation routing, and the bot and broadcast APIs over `meta-whatsapp-bot`. Each route checks the object an id names against the path's number or WABA ([architecture.md § Service](../architecture.md#service-meta-whatsapp-server)); M1.3's table covers it by construction | a `server.md` section and a server skill (or a section of one) per family |

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
| M2.4 | Webhooks-out: Standard Webhooks test vectors; retry schedule under a fake clock; redirects not followed; a destination outside the allow-list refused at creation and at delivery (a resolver stub that turns private on the second lookup); two signatures during rotation; over 256 KiB delivered truncated and fetchable only by its tenant. *As built in M2c*: the core's `conformance::webhooks_out` on memory and live on Postgres (the libraries' own vectors in `webhooks_out::signature`; `the_retry_schedule_under_a_fake_clock`; `a_destination_is_refused_at_creation_and_at_delivery`, the stub's second lookup private, nothing sent; `a_rotation_signs_twice_for_24_hours`; `a_large_event_is_delivered_truncated`), the HTTP crate's sender (`an_attempt_goes_to_the_pinned_address_and_follows_no_redirect`), and over HTTP `tests/webhooks_out.rs` |
| M2.5 | 200 open streams at 100 events/s with bounded memory |
| M2.6 | D25: Meta's examples of `standby_observed`, `thread_control_changed` and `user_action_reported` reach their number's tenant (poll and stream) and no other; `KnownEventType` gains them and the v1 document stays additive. Decisive: taking one out of `TENANT_EVENT_TYPES`. *As built in M2d*: `conformance::webhooks::the_reviewed_types_reach_their_numbers_tenant_and_no_other` and `conformance::live::the_reviewed_types_reach_their_numbers_tenants_streams_only` (every backend, Postgres live), over HTTP `tests/reviewed_events.rs` (poll, by id, both streams, `types`; the document's `KnownEventType`) |
| M3.1 | The happy path over HTTP with Meta's documented responses binds the verified WABA and all its numbers; no response contains the token |
| M3.2 | Another tenant's state is `403 stale_attempt` and the rightful tenant still completes; a malformed PIN is `422` and the state survives |
| M3.3 | A scripted 133005 at registration is `502 onboarding_failed` (`register_phone`, resumable); a new process on the same database resumes with a corrected PIN; another tenant's resume is `404` |
| M3.4 | Partner mode: the credit-line request carries the partner's system token (asserted header), never the merchant's; missing partner settings refuse the start. D4 is enforced **before** any credit call, through the library's post-verification gate: a second tenant onboarding a bound WABA gets `409`, nothing is stored, subscribed or shared, binding unchanged. A business whose line was revoked is not re-funded by `resume` or a new signup without an explicit operator action. Offboarding revokes first and deletes second: a CMS disconnect, `PARTNER_APP_UNINSTALLED` and `PARTNER_REMOVED` (in any order) end with the line revoked (from the stored or the webhook's owner business id) before the vault entry and bindings go; tests replay both orders |
| M3.5 | OTP: every outcome; tenant A's code verifies at no other tenant on the same number (decisive: the namespace); logs hold neither code nor number; a sentinel in a scripted Graph error on issue reaches no response |
| M4.1 | The image builds for both architectures, runs non-root on a read-only file system, has no shell; `meta-whatsapp-server healthcheck` works in it |
| M4.2 | A Compose smoke test in CI (Postgres, the image, a Graph stub via `WA_GRAPH_ENDPOINT`): CLI admin key, tenant, attach, send, a signed Meta webhook, a webhooks-out delivery verified at a stub receiver |
| M4.3 | The client is generated from the committed `.cstack` schema (CrateStack's check mode fails on drift); `tsc --noEmit` passes on it and on every TypeScript excerpt of the server skills; a Node test verifies a real delivery with `verifyWebhook()`; a breaking change within `v1` fails `cratestack diff` against the last release; the invoice fixture renders byte-identically. D20 (a) was decided by the owner on 2026-11-02: yes, so the fallback it would have needed (the client generated from `openapi/v1.json` by openapi-typescript, a stale client failing the same drift gate, `oasdiff` in `cratestack diff`'s place) is not built |
| M5.1 | M1.3's table covers every M5 route by construction: tenant B's key on A's number or WABA is `404`, with zero vault reads |
| M5.2 | Each family checks the object an id names against the path's number or WABA (a group, a Flow, a QR code, a call): a route that skips it fails the family's cross-tenant test |
| M5.3 | The legal acts (the max-price agreement, M5h; the In-App Signup terms, M5i) are explicit operator actions: with the deployment's setting off, the route makes no request to Meta; with it on, one request and one audit event naming the admin key; no other route performs them |

**The skills gate for HTTP callers (M1).** `crates/meta-whatsapp-rs/tests/skills.rs`
assumes Rust (no TypeScript fences; backticked names must exist in
`crates/`), so server skills would fail it or pass unchecked if
allow-listed. M1 extends it for `skills/meta-whatsapp-rs-server*`: a `ts` fence must be
an excerpt of the skill's `examples/*.ts`, type-checked by a new `just
skills-ts` (tsc against the generated client, Node pinned) inside `just
ci`; backticked routes, schemas and codes are checked against the committed
spec. Stamps, the 160-line limit and hub listing apply unchanged.

**Decision (D13): where the server skills live.** (a) This
repository's `skills/` (`npx skills add vaam-apps/meta-whatsapp-rs -s meta-whatsapp-rs-server …`),
changed in the same PR as the API; (b) a separate skills repository with
its own coverage gate. *Recommendation: (a)*: API, spec and skills change
atomically and are checked against the same commit.

## 10. Decisions

**From 2026-09-26 the owner delegates these decisions to the
coordinator**, on one condition: the code stays composable, with traits
and interfaces hiding the hard logic, so that an integrator can change
each decision (the owner's examples: MongoDB for the database;
CrateStack for the API and the models, as the default, and CrateStack
itself swappable altogether). The rule is AGENTS.md's § Decisions: a
decision is taken when it ships swappable, or names the roadmap item
that adds the swap, and its row says how to swap it. **The owner keeps,
even when the choice would ship swappable, legal and terms-of-service
choices (licences among them) and what cannot be undone: irreversible
deletion or data migrations, released contracts and the stable
identifiers.**

**Legal acts.** The code decides nothing legal. Where Meta ties a legal
act to an API call (accepting the In-App Signup terms, signing the
max-price beta agreement), the library exposes it as an explicit call
that no other call makes, and the service as an explicit, audited
operator action, off until the deployment enables it and never taken
automatically. Performing it is the deployer's decision, for their own
deployment (`OPEN_QUESTIONS.md` #26, #45); the same holds for the legal
half of D10. Zero-tap authentication templates are one (Meta's
`zero_tap_terms_accepted`, the creator's acceptance of its zero-tap
terms): the service refuses them on both template routes, `otp_type:
zero_tap` on `POST /v1/wabas/{waba_id}/templates/authentication` (M3c)
and any `zero_tap_terms_accepted: true` on `POST
/v1/wabas/{waba_id}/templates` (M3c's review; passed through to Meta
from M1b until then), and they stay the owner's (roadmap M3g, §
Owner touchpoints).

Who decided what: the owner decided D1–D4 and D7 on 2026-09-24, D13–D14
and D15–D18 on 2026-09-25 (the recommended option each time, except D17,
against the recommendation to move before M1b), and D25 on 2026-09-26.
D21–D24 are the coordinator's decisions of 2026-09-25, which M1c ships
and which stand under the delegation; the owner confirms D21, D22 and
D24 before the first release (roadmap § Owner touchpoints), since each
cannot be undone after it. D6, D8–D12, D19, D20's (b) and (c), and
D26–D35 are the coordinator's decisions of 2026-09-26 under the
delegation (D30–D35, with the changes to D10 that day, came out of the
reviews of roadmap L5), as are the changes to D15 (widened), D16 (the
gate) and D18 (revised) that day. D36 is the coordinator's of 2026-09-27
(amended with roadmap S2b's build on 2026-11-01), D37 of 2026-10-26, D38
of 2026-11-01 (roadmap L8), D39–D43 of 2026-11-01 (roadmap M2b, D43 on
its review; see below), D45–D49 of 2026-11-01 (roadmap M3c), D50–D58 of
2026-11-01 (roadmap P2, D56 amended on 2026-11-02), D59 of 2026-11-01 on
M3c's review, D60–D63 of 2026-11-02 (roadmap M2g) and D65–D69 of
2026-11-02 (roadmap M3d), each under the delegation; the namespace
`wa.server.registration` and its record layout (D66) wait for the
owner's confirmation, and a scope of deregistration's own (D68) stays
the owner's (roadmap § Owner touchpoints). D80–D85 are the coordinator's of
2026-11-02 (roadmap M2d, D81's signup and offboarding order amended on
its review), under the delegation; the number events' type names, their
`reason` values and their outbox key's label (D80, D83) are new stored
names, which wait for the owner's confirmation (roadmap § Owner
touchpoints). D20's (a), a licence, is the
owner's, decided on 2026-11-02 (yes: roadmap S10 adds the scoped
exception). D5 is settled as "support both modes, chosen per
deployment"; which mode a production deployment runs follows the
platform's agreements with Meta (partner status, who pays), so that
choice stays the owner's.

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
| D10 | Retention and erasure of customers' messages | keep / purge after N days; erasure or not; a number's inbox history when it moves to another tenant (hidden by M2's binding epoch, or purged on unbind) | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: retention is configurable per store.** The outbox keeps 7 days by default (`WA_SERVER_OUTBOX_RETENTION`), the inbox keeps everything by default, erasure (a person on one number: every key `identities` connects, D30; their group messages redacted by default, D31) comes through L5, `ConversationStore::erase_all` with conformance cases, and reaches the service in M2f (a route, the outbox's rows, an erasure journal), and a number moved to another tenant is hidden from it by M2's binding epoch, not purged. *Built in M2f*: `POST /v1/numbers/{pn}/erasures`, scope `erasure`, decided as D86–D91 (narrow by default: the tenant's own outbox rows, and no number with a history cut). To swap, each store's retention is a setting and erasure a call. *Built in M2a*: the inbox's retention is `WA_SERVER_INBOX_RETENTION` (`keep` by default), built into the store and applied by housekeeping; the binding epoch is `server_core::authz::history_from` (a WABA that moved from another tenant shows its holder nothing dated before its binding began), which the inbox routes apply; and since M2h a number another tenant released (Meta migrated it from that tenant's WABA, neither WABA moving) shows nothing dated before it came under its tenant, the later of the two cuts counting, by each number's own move record (Postgres migration 5, `wa_server_number_tombstones`; a number bound before it reads as moved). Which retention a deployment sets, and which erasure requests it must honour, follow its privacy obligations: the deployer's legal call (legal acts, above) | M2 |
| D11 | Publishing the TypeScript client | npm / GitHub Packages / vendored | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: public npm** under the organization's scope. The publish workflow is prepared; publishing needs the owner's token. To swap, the workflow's registry is its input | M4 |
| D12 | A Medusa plugin | none / now / after the first integration | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: after the first integration**, in its own repository; nothing here depends on it | after M4 |
| D13 | Where the server skills live | this repository / a separate one | **Decided 2026-09-25: this repository** (under `skills/`, same stamp gate and `npx skills add vaam-apps/meta-whatsapp-rs`) | M1 |
| D14 | Credit line after a merchant unshares (`PARTNER_REMOVED`) | revoke at once (Meta's recommendation) / revoke after a grace period when `disconnection_info` says the coexistence number may reconnect / operator decides | **Decided 2026-09-25: revoke at once** on every `PARTNER_REMOVED` for our solution, coexistence included; a merchant who reconnects re-onboards and is funded again only through the explicit re-share (`reshare_after_revocation`) | M3 |
| D15 | API layer framework | OpenAPI (axum + utoipa) / CrateStack `cratestack-api` (procedures) / `cratestack-pg` (models + policies) / schema for clients only | **Decided 2026-09-25: CrateStack, `cratestack-api`, procedures only. Widened by the coordinator on 2026-09-26 under the owner's delegation, swappable: CrateStack for the API and the models, by default**: `api.cstack` (`db = None`) for the API, and in `store-cratestack` models from `store.cstack` (`db = Postgres`) for the records tables, a hybrid whose outbox, idempotency, locks, janitor and migrations reuse store-postgres's SQL ([§8.4](#84-two-cratestack-schemas)). To swap, the binary's features select `api-axum` and `store-postgres` (or `store-memory`, later `store-mongodb`), and CrateStack is then absent from the build | the flip ([roadmap.md](../roadmap.md) S16) |
| D16 | The §5 error body under CrateStack | add domain errors to CrateStack upstream / rewrite layer in the service / two shapes / results instead of errors | **Decided 2026-09-25: upstream in CrateStack. The coordinator, 2026-09-26: the default API flip is gated on it**; until it lands, the CrateStack adapter answers two shapes (§5 from our outer layer and custom routes, CrateStack's own from its generated routes), which is why it is not the default before then | before the flip |
| D17 | When to move to CrateStack | before M1b / finish M1 first, migrate at M4 | **Decided 2026-09-25: finish M1 on axum + OpenAPI, migrate at M4.** Since 2026-09-26 the move is the modular split's ([§8](#8-modular-architecture-and-client-sdks), [roadmap.md](../roadmap.md) S1–S16) rather than M4's | M1 |
| D18 | Callers that do not use a generated client | generated clients only / an OpenAPI emitter upstream / a hand-kept OpenAPI document | **Decided 2026-09-25: generated clients only. Revised by the coordinator on 2026-09-26 under the owner's delegation, swappable: CrateStack's generated clients are the primary contract, and `openapi/v1.json` stays, committed and tested, with `api-axum`, the permanent swap target (D26)**; to swap, build `api-axum`: its callers then use resource URLs and a client from openapi-typescript | the flip |
| D19 | CrateStack transport | REST (JSON, `@status`, `POST /$procs/<name>`) / RPC (batching, subscriptions; CBOR by default in the TS client) | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: REST** (a status per request, `Retry-After`, `Idempotency-Key`), **and the TypeScript client pinned to the JSON wire format, not CBOR**; to swap, the schema's transport and the generator's format option | S13 |
| D20 | CrateStack's dependencies | allow BlueOak-1.0.0 (`minicbor`) in `deny.toml`; accept `ring` beside `aws-lc-rs` (explicit TLS provider at start); accept its `sqlx =0.9.0` pin / change them upstream first | **(a) The owner's: a licence. (a) decided by the owner on 2026-11-02: yes.** A `deny.toml` licence exception for BlueOak-1.0.0 scoped to `minicbor` and `minicbor-serde`, which are non-optional dependencies of CrateStack's axum crate (CrateStack's own `deny.toml` allows that licence); roadmap S10 adds it when CrateStack enters the workspace, and no `deny.toml` before it allows BlueOak-1.0.0. A separate workspace for the CrateStack crates would keep the exception out of the main `deny.toml`, but the licence would still enter the service's binary; a refusal means no CrateStack in the build, and the defaults stay `api-axum` and `store-postgres`. **(b) and (c): coordinator's decisions 2026-09-26 under the owner's delegation, swappable.** (b) aws-lc-rs installed as rustls's process default at start, for the crates that use the process default; sqlx picks its provider itself, so Postgres TLS runs on `ring` in a build with CrateStack until upstream U2; to swap, the binary's start-up installs another provider, and U2's feature chooses sqlx's. (c) CrateStack's `sqlx =0.9.0` pin accepted, with an upstream PR relaxing it (U3); to swap, a build without CrateStack's crates has no pin, and U3 lifts it for the rest. *As built in S10*: (a) two `[[licenses.exceptions]]` entries in `deny.toml`, `minicbor` and `minicbor-serde`, BlueOak-1.0.0 each, and no `allow` entry; (b) `meta_whatsapp_server::tls::install_default_provider`, which `cli::run_with` calls before every command and which keeps a provider already installed (an integrator's swap); (c) does not apply yet, `cratestack-api` bringing no sqlx (S14's `cratestack-pg` does). **(b) as built: coordinator's decision 2026-11-02 under the owner's delegation, swappable**: the install at the top of `cli::run_with` only, keeping a provider already installed (the swap: install another before the command line runs); a direct `serve::serve*` caller calls `tls::install_default_provider()` first, as the guide and the `serve` functions' rustdoc say. sqlx reads the process default in one place, `sslmode=verify-ca` (its certificate verifier), so in a build with both providers the install is also what keeps a database connection from panicking (roadmap S10's `tests/tls_verify_ca.rs`) | S10 |
| D21 | Event sequences ([§2.3](#23-the-event-pipeline)) | one sequence for the whole outbox / one per tenant | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: one per tenant** (security review SR-L2: a global sequence shows every tenant the platform's volume and timing); `sequence`, `after`, `next_after`, `410` and `422` are the tenant's. Reversible until the first release, or the first deployment with polling consumers: the per-tenant sequence is in the v1 contract (the descriptions of `sequence` and `next_after` in `openapi/v1.json`), consumers store cursors in it, and within `/v1` changes are additive ([§7.5](#75-versioning)), so going back to one sequence then breaks every stored cursor: irreversible after the first release, so the owner confirms it before then | M1 |
| D22 | A deleted tenant's outbox events (related to D10) | delete them with the tenant / keep them, hidden from a tenant created again under the id | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: delete them** (the outbox's foreign key cascades; the tenant's stream records them purged, so an old cursor is `410`). Reversing it keeps the events of tenants deleted afterwards only: events the cascade already deleted are not recoverable, an irreversible deletion, so the owner confirms it before the first release | M1 |
| D23 | Dedup window of keyless events (`error_reported`, `unparsed`, [§2.3](#23-the-event-pipeline)) | 1 h / Meta's whole 7-day retry period / none | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: 1 h** (`KEYLESS_DEDUP_WINDOW`). Failure mode: Meta documents a retry at once, then with decreasing frequency over 7 days, and that receivers must deduplicate; it does not say a redelivery carries the same bytes, which the key assumes. An outage longer than an hour (the database answering `500`, say) records the batch's keyless events twice, as new ids. 7 days would record a recurring identical error once for that long; none would record every redelivery | M1 |
| D24 | A deleted tenant on platform keys | scrubbed from every key's allowed tenants, a tenant created again needing a new key / kept | **Coordinator's decision 2026-09-25, standing under the owner's delegation of 2026-09-26: scrubbed** (a tenant created again under a recycled id must not inherit access). No route or command edits a key's allowed tenants, so a tenant created again under the id needs a new platform key; a `*` key allows it at once. Reversing it keeps the allowances of tenants deleted afterwards only: the lists already scrubbed are not restored. The owner confirms it before the first release | M1 |
| D25 | The event types the webhook conformance sweep typed (PR #17: `standby_observed`, `thread_control_changed`, `user_action_reported`, formerly `unknown`) | operator-only / tenant-visible | **Decided 2026-09-26: tenant-visible, in M2** (with the inbox's answer to `OPEN_QUESTIONS.md` #44); M1c ships them operator-only, the rule for a type not yet reviewed. **Built in roadmap M2d**: in `TENANT_EVENT_TYPES` and `KnownEventType` (additive), routed by their number as every event naming one (acceptance test M2.6) | M2 |
| D26 | The service's architecture | one crate / a framework-free core with ports, API adapters and backend bundles | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: the modular service ([§8](#8-modular-architecture-and-client-sdks)).** Defaults: `api-cratestack` and `store-cratestack` (the hybrid), once roadmap S16 flips them (D20's licence (a) decided by the owner on 2026-11-02: yes). Kept built and conformance-tested: `api-axum` (today's v1 and its OpenAPI document), permanent: the swap target, where every new route lands too, through core services that keep both adapters thin; and `store-postgres` (sqlx); `store-memory` for development; later `store-mongodb`, after the library's MongoDB adapters, with an `Outbox` spike first. Swapping the API adapter swaps the wire contract and the client: procedures with CrateStack's generated client, resource URLs with openapi-typescript. To swap, Cargo features choose which adapters are compiled in and configuration picks one of each; a new backend implements the core ports and passes `meta_whatsapp_server_core::conformance` | [roadmap.md](../roadmap.md) S1–S18 |
| D27 | The idempotency fingerprint ([§5.4](#54-idempotency-keys)) | method, route and body (as built until S6) / operation id and canonical input | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: the operation id plus the canonical input**, so one key means the same through every API adapter. **Built in S6**: `idempotency::Fingerprint`'s constructors, one per idempotent operation (the path parameter as a decoded id, the canonical JSON body or the upload's type, file name and digest). **Decided 2026-10-31 (coordinator, under the owner's delegation):** "canonical input" is the request's JSON as it came, canonicalized (object keys sorted, whitespace ignored), not the parsed request type re-serialized; a request's rate class is its operation's, charged with `RateLimiter::charge(&TenantId, Operation)` (a bare tenant id, not an admitted `Caller`: a forged one could only spend a tenant's per-replica budget); the vault's admin writes, `graph_failed_for_admin` among them, are crate-private behind `Admin`, and `authz::rotate_vault` stays public for the command line. **The window (decided the same day):** a deployment exists, and a duplicate customer message is real harm, so for one `WA_SERVER_IDEMPOTENCY_TTL` (24 h by default) after the upgrade a repeat also matches a record stored under the fingerprint of before (the request's method, its path as sent and its canonical body, or the upload's parts: `Fingerprint::legacy_v1_json`, `legacy_v1_upload`, pinned to main's known answers, added by the HTTP adapter with `Fingerprint::or_legacy`); a claim always stores the new fingerprint. A repeat of an old key under another spelling of its path, and an upgraded replica's key repeated on an old one during a rolling deploy, are `422 idempotency_key_reused`, never a second send. The legacy fingerprint goes after the first release (roadmap M2a, which carries the line since S7 landed before the release). To swap, change those constructors, the one place every adapter's fingerprint comes from | S6 |
| D28 | The bot framework and its plugins | in the facade / a new library crate / in the service; plugins loaded at run time / registered at compile time | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: a new library crate, `meta-whatsapp-bot`, with plugins registered at compile time.** No hot reload: loading Rust code at run time is neither idiomatic nor safe (no stable ABI, and `unsafe` loading, which the workspace forbids), and parity row 87 says so. To swap, a `Bot` is an `EventSink`, so an integrator can put their own dispatcher in its place; the service exposes the bot over HTTP in M5 | B1 |
| D29 | Paced broadcast and scheduled messages | a job-queue port / a typed store on `KvStore` / the caller's | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: in `meta-whatsapp-bot`, with jobs as a typed store on `KvStore`** (a bucketed due-time index, claims by compare-and-swap under a lease), not a new port, per architecture.md's rule that typed stores are built on `KvStore`; broadcasts are paced per number under Meta's throughput. To swap, the pacer and the job store are traits with these as their defaults. **Amended 2026-11-01 (coordinator, under the owner's delegation):** until B3b the job store was the concrete `KvJobs`, swappable through its `KvStore` adapter. **Closed 2026-11-01 by roadmap B3b:** the job store is the `JobStore` trait the runner takes (`Arc<dyn JobStore>`), `KvJobs` its default and `MemoryJobs` a second implementation, both held to `jobs::conformance::run` | B2, B3, B3b |
| D30 | Erasing a person who has several keys on a number (a history thread under their phone number, live messages under their BSUID, an earlier BSUID; the privacy review of L5, M3) | one key per call, the caller finding the others / the store finds a person's keys and erases them in one step, on one number / across numbers | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: the port widens in L5** (so adapters change once): `ConversationStore::identities` (the closure over the synced contacts and the identity links, read only), `IdentityLink` (`link_identity`, `identity_links`; the inbox records them in L7), and `erase_all(phone_number_id, ids)`, one step, whose one-key case is `erase`. Never across numbers: a `wa_id` is the same on every number, so an erasure across them would delete other businesses' customers. To swap, a caller passes its own ids to `erase_all` (the closure is a separate read), or an adapter of its own implements the port | L5 |
| D31 | An erased person's messages in someone else's conversation (a group's; review M2) | delete them / redact them in place | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: redact by default, delete as an option**: `ErasureMode::Redact` keeps the message's place and time and replaces its content, payload and sender with the `erased` marker, so the other participants' history keeps its shape; `ErasureMode::Delete` removes it. Either way the group's summary never keeps the erased text. The message id stays under `Redact` (a `wamid` encodes the sender's phone number), which the docs say. To swap, `with_erasure_mode` on the store (a setting, like retention) | L5 |
| D32 | A synced contact's removal (`smb_app_state_sync` `remove`), whose webhooks arrive out of order (the sabotage review of L5) | delete the row, so a late older `add` stores the contact again / keep the removal, its key and time only, so an older `add` is refused | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: keep the removal**, nothing of the contact but its key and time; `erase_all` deletes it with the key's records and `purge_before` once it is older than the cutoff. To swap, a store's retention bounds how long removals are kept, and an adapter of its own may keep them elsewhere (the conformance suite checks the refusal) | L5 |
| D33 | Ordering an erasure and a purge against appends on Postgres (the sabotage review of L5: an append in flight left without its summary; purges and erasures deadlocking) | row locks alone / serializable transactions / two transaction-level advisory locks | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: two advisory locks**, the number lock (`append`, `append_synced` shared; `erase_all` exclusive) and the purge lock (`purge_before` exclusive; `erase_all` shared), in the two-key form with the table's object id as class. Their SQL is a stable identifier ([architecture.md](../architecture.md#stable-identifiers)), pinned by `the_locks_are_pinned`: replicas of two releases must take the same ones. The cost, documented: an erasure makes the appends of its business number wait. To swap, the locks live in the Postgres adapter only; another adapter orders itself its own way, behind the same port and conformance suite | L5 |
| D34 | Two thread-ownership records of the same second (Meta's timestamps are seconds; the sabotage review of L5) | the last one stored wins / a tie-break on content | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: the last one stored wins, documented** on `set_thread_owner`: a tie-break on content would be deterministic but would pick the wrong record as often as the right one, and the dedup markers keep redeliveries of a delivered event out. To swap, a caller orders the records it stores, or an adapter of its own applies another rule | L5 |
| D35 | Identity links under a retention (the review of L5's remediation: a link is personal data, and `purge_before` keeps it) | purge a link by age, like a contact removal / keep it until an erasure | **Coordinator's decision 2026-09-26 under the owner's delegation, swappable: keep it until an erasure**, documented on `link_identity` and in cms-inbox.md § 8. A link is what lets an erasure reach the person's records under their other identity, and those can be newer than the link (a history thread keyed by a phone number, synced after a link recorded earlier, and synced contacts, which no retention purges): a link purged by age would hide them from the erasure. It holds two identifiers and a time, no content, and `erase_all` deletes it. To swap, a deployment bounds them itself: on Postgres a link is a row of `wa_identity_links` with its time (`ts`), so a `DELETE` of the rows older than its own cutoff, scheduled beside `apply_retention`, purges them (another adapter keeps them its own way) | L5 |
| D36 | Undated tenant-visible events of a WABA's previous holder (errors, history chunks, undated group updates), which Meta redelivers for up to 7 days after a failed delivery, arriving after the WABA moved to another tenant (S2's security review, M2) | the current holder (as built) / operator-only within a window after a move | **Coordinator's decision 2026-09-27 under the owner's delegation, swappable: fail closed by default.** A setting `undated_after_move`: `OperatorOnly { window }` by default, the window 7 days and an hour (Meta's retry window plus the dedup margin), and `CurrentHolder`, the behaviour before it. A first onboarding (no previous tenant) is unaffected, so its history sync reaches it. The same item adds a skew margin setting around `attached_at` for dated events, applied only when a previous tenant exists, and relaxes the same-second rule (strict since S2: an event of its binding's own second is operator-only) to `<=` when the WABA has no previous tenant, so a first onboarding keeps the events of its first second; the tombstone is what tells the two apart. Why: a new holder can be another business, and the operator still sees the events in the operator stream. Not a legal choice. **Built in S2b**: `events::UndatedAfterMove` and `events::MoveRules` (the core; `Inbound::with_move_rules`), the window measured from the WABA's last unbinding from another tenant (its tombstone: a tenant deleted and created again under its id counts as another), checked by the routing and again by the outbox with each insert (`RouteGuard::after_move`); the skew margin defaults to none beyond the strict second. Bindings made before the Postgres migration 4 keep S2's rules (a move before them unknown). *Decided 2026-11-01 (coordinator, owner's delegation), with S2b's build, swappable behind the same settings and the `RecordStore` port*: the window from the unbinding, not the new attach; a margin of `0s`; the epoch (`UNRECORDED_MOVE`), not NULL, for bindings made before migration 4; a deleted and recreated tenant another tenant; the generation from 1 per binding; the undated check in the routing (`events::owner`) and `RouteGuard::admits`, for every undated event; a refresh met by a capability or an attach `503`, retryable; tombstones never purged. The names of migration 4 stay the owner's (stable identifiers). To swap, the settings `WA_SERVER_UNDATED_AFTER_MOVE` (`operator_only`, `current_holder`), `WA_SERVER_UNDATED_AFTER_MOVE_WINDOW` (169h) and `WA_SERVER_MOVE_SKEW_MARGIN` (0s), [§7.2](#72-environment) | S2b |
| D37 | A number under several Messaging Accounts (Meta's account model evolution, phase 1 from H2 2026: a WABA's id now names its Messaging Account, which holds templates, billing and webhook subscriptions, and a number can belong to several, one per partner or billing currency) | one binding per number in a deployment, the sends naming it / several bindings per number, keyed by `(waba_id, phone_number_id)` | **Coordinator's decision 2026-10-26 under the owner's delegation, swappable: one binding per number, as built.** A number stays under exactly one binding (`wa_server_numbers`, keyed by `phone_number_id`): an attach whose number another tenant's binding holds is refused (D4), and one whose number the same tenant holds under another binding moves the number to the new binding, and M5c5 makes the latest attach the account billed: a send from a number names its binding's WABA id as `messaging_account_id` (the library's `OutboundMessage::messaging_account_id`, L16a), which Meta requires when the access token can send from several Messaging Accounts on the number, from phase 2 (H1 2027), so the binding's account is billed whichever ones the token reaches. A tenant's send body cannot name the account itself (the send body's unknown keys are refused). Until M5c5 lands, the service's sends name none and Meta resolves the account from the stored token. Why: it is the rule the schema already has, and nothing about events routed by Messaging Account is published yet. The cost, documented: templates belong to the Messaging Account they were created in (`account-model-evolution`), so after a same-tenant move the number's template sends find only the new account's templates (the tenant creates them there, or migrates them with Meta's Template Migration API, which resets their quality rating). Several bindings per number is a primary-key migration, left to the owner (OPEN_QUESTIONS #46). To swap, the setting `send_messaging_account_id` M5c5 adds (on by default; off sends none, as today) | M5c5 |
| D38 | The coexistence address book (`smb_app_state_sync`) in the service's inbox, which the library's `InboxSink` records by default since roadmap L8 | record it now, readable by no route / record nothing until M3b exposes it | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: record nothing until M3b.** The pipeline builds its `InboxSink` with `meta-whatsapp-server-core`'s `inbound::INBOX_RECORDING`, the library's `RecordingSwitches::ALL` with `contacts(false)`: the `app_state_synced` events still reach their tenant, but no synced contact (names, phone numbers, BSUIDs) is stored where no route reads it and no tenant can ask for it to be erased. Swap: the constant; M3b turns it on with the routes that read the contacts. Decisive: `conformance::webhooks::an_address_book_change_reaches_its_tenant_and_no_inbox` (memory and Postgres backends). | M3 |
| D39 | How a replica's live streams read the outbox ([§4.5](#45-live-updates-sse)) | each stream reads after its own cursor when woken / one read per replica and tenant, shared (`Arc`) into channels per (tenant, number), as §4.5's bullets first said | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: each stream reads, as built.** A stream is a cursor in its tenant's stream, woken by its tenant's notifications or its re-poll, reading with its own filters exactly as `GET /v1/events` does: no catch-up and hand-over between a resuming stream and a shared feed to get wrong, and the tenant named in every read. The cost: up to a tenant's 20 streams each read once per wake-up (coalesced under a burst). A later optimisation, not a roadmap item: one reader per replica and tenant, shared into the streams, behind `live::Streams` (the adapters call `open_stream` and `open_number_stream` only), with no route change, held to the same bounds by M2.5's case (`conformance::live::many_streams_at_a_steady_rate_hold_bounded_memory`). To swap, that reader behind `live::Streams` | M2b |
| D40 | The Postgres notification's channel and payload | one channel per database / one per schema / a setting | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: the channel `wa_server_events` per database, the payload `<sequence> <tenant>`, as built.** A deployment is one database ([§2.2](#22-configuration-and-storage)); deployments that share one (schemas apart) hear each other's wake-ups, which cost them a read each and carry a tenant id and a sequence, nothing of an event. **Both names stay stable identifiers pending the owner's confirmation before the first release** (replicas of two releases must agree during a rolling deploy; docs/architecture.md § Stable identifiers). To swap, `PgNotifier::with_channel` (a deployment sharing its database names its own); the payload's format is `PgNotifier`'s `payload` and `wakeup`, pinned by a test | M2b |
| D41 | Where a stream starts, and an event id's shape | without a cursor: the newest sequence / the oldest retained, as a poll does; `Last-Event-ID` over `?after=`; a malformed event id `404` / `422` | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: as proposed.** Without a cursor a stream starts at the newest sequence (a live stream is for what comes next; history is the poll); `Last-Event-ID` wins over `?after=` (what an `EventSource` sends on its own when it reconnects, newer than the URL's `after`); a malformed event id is `404` (an id of no event's shape names nothing, as a non-numeric `{pn}` is `404`, roadmap L20c). To swap, `live::StreamQuery` (an adapter passes `after: Some(0)` to start at the oldest) and `events::get_event` | M2b |
| D42 | A stream's buffer, a slow client, the re-poll | per stream 1 MiB and 64 frames, a client taking nothing for 30 s dropped with `lagged`, a re-poll every 15 s / other numbers | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: as proposed, 1 MiB and 64 frames per stream, a client taking nothing for 30 s dropped, a re-poll every 15 s.** §7's limits bound the streams (20 per tenant, 1,000 per replica: settings) but not their buffers: 1 MiB per stream bounds 1,000 streams to about 1 GiB at worst (M2.5 measures 200 streams, half never read, under 1 MiB in all); a dropped client loses nothing of the outbox and resumes from `after`; the re-poll at the heartbeat's interval bounds a lost notification's delay to 15 s. To swap, `live::LiveSettings` (the service's `Settings::live`; the two bounds are `WA_SERVER_STREAMS_PER_TENANT` and `WA_SERVER_STREAMS_PER_REPLICA`) | M2b |
| D43 | What M2b's review added to the live streams: when a stream asks again whether its key is admitted, what a dropped stream keeps, the `Retry-After` of `too_many_streams`, a write timeout on the listeners | a check before a read, at most every second and at least every re-poll / at each re-poll only / none (the connection's lifetime); a lagged stream drops what it held at once / delivers it first; `Retry-After: 5` / none; a minute / none | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: the first of each, as M2b's sabotage review proposed.** A stream outlived its key: a revoked or expired key, a suspended or deleted tenant, kept reading for as long as its connection lasted. The check before a read costs two record reads per stream at most once a second (an idle stream's once per re-poll), and a stream ends within a second of a revocation on a busy tenant, a re-poll on an idle one. A stalled client (its TCP window full) never takes a last frame: the stream gives its place and its buffer back when it ends, and the frames it held are dropped then (its client resumes from the last sequence it took, which `lagged` names); the listeners close a connection that takes no byte of its answer for a minute, longer than the slow-client wait, so that stalled connections do not pile up under the connection cap. `Retry-After` is a hint (a slot frees when any stream ends); 5 s is above `EventSource`'s usual reconnect delay. To swap, `LiveSettings::recheck` and `LiveSettings::retry_after`, `Producer::end` (`live.rs`), and `listen::Limits::write_timeout` (`WRITE_TIMEOUT`) | M2b |
| D45 | Who sends the codes of a tenant without OTP settings (D8's "the platform's number by default") | a deployment-wide sending number and template / a default OTP tenant whose own settings are borrowed / none: every tenant configures its own | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: a default OTP tenant whose own settings are borrowed.** `WA_SERVER_OTP_DEFAULT_TENANT` names it; its settings name one of its own numbers, which is opened for that tenant only (the authorization order's steps 4 and 5 on its binding), never for the borrower, so a binding that moves never lends another tenant's token; the namespace stays the caller's. Fail closed (M3c's review): a default tenant suspended, deleted or without settings lends nothing, and its borrowers get `409 otp_not_configured`. Pending the owner's confirmation, as stored names (architecture.md § Stable identifiers): the settings' layout, version 1 under the key `otp` of `wa_server_tenants.settings`. To swap, the setting: unset it (every tenant configures its own), or name another tenant, or give the platform's tenant other settings | M3 |
| D46 | The rate class of checking a code, which section 6 does not name | the read budget / the send budget / a class of its own | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: a class of its own, `otp_verify`, 20/s with a burst of 20**, beside `otp_issue` (5/s, a burst of 5, the design's); brute force stays bounded per recipient by the library's attempts and issue limit. To swap, the environment variables `WA_SERVER_RATE_OTP_VERIFY` and `WA_SERVER_RATE_OTP_VERIFY_BURST` ([§7.2](#72-environment)) | M3 |
| D47 | An OTP issue's idempotency fingerprint, whose body names a phone number | SHA-256 of the canonical input, as every other operation (D27) / keyed with the OTP pepper | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: keyed with the pepper**, an HMAC-SHA256 under HMAC-SHA256(`WA_OTP_PEPPER`, `meta-whatsapp-server/otp-fingerprint/v1`) of the canonical input's SHA-256, since section 6 keeps numbers only as keyed hashes. The label is a stored name, pending the owner's confirmation (architecture.md § Stable identifiers). Rotating the pepper makes a repeat within the TTL `422 idempotency_key_reused`, never a second code. To swap, the fingerprint the API adapter hands the core (`idempotency::Fingerprint::issue_otp`; another adapter passes its own) | M3 |
| D48 | Where the admin sets a tenant's OTP settings | `PATCH /v1/admin/tenants/{id}` (section 4.2) / a resource of their own | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: a resource of their own**, `GET`, `PUT`, `DELETE /v1/admin/tenants/{id}/otp`, replaced whole after their checks, so a tenant's read stays one row and `PATCH` keeps its fields. To swap, the routes, the API adapter's (the core's `admin::Admin::set_otp_settings` and `clear_otp_settings` stay); adding `otp` to `PATCH` later would be additive | M3 |
| D49 | The purpose of an OTP request that names none | required / a default | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: `login`.** A purpose is hashed into the code's store key, so changing the default strands the codes in flight (10 minutes by default), nothing else. To swap, the constant `otp::DEFAULT_PURPOSE` (a caller can always name its own) | M3 |
| D50 | The payment routes' key scope (roadmap P2: a WABA's payment configurations, a payment's lookup and its refund) | a new scope / under `send` or `numbers` | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: a new scope, `payments`.** A refund moves money and a configuration links a gateway account: a key that only sends messages (a CMS's) must not refund, so neither `send` nor `numbers` stands in for it, and no key minted before P2 gains a refund. The payment *messages* (`order_details`, `order_status`) stay sends (`send`), like every message. The stored name `payments` (`wa_server_api_keys.scopes`) stays **pending the owner's confirmation before the first release** (a stable identifier: [architecture § Stable identifiers](../architecture.md#stable-identifiers), roadmap § Owner touchpoints). To swap, `Operation::scope` (core `operation.rs`) maps the seven payment operations to another scope and the router's `tenant(payments_routes(), Scope::Payments)` follows it; keys minted with `payments` are then minted again. Decisive: `the_payment_routes_take_the_payments_scope`, `api::tests::each_tenant_route_is_one_core_operation_with_its_groups_scope` | P2 |
| D51 | A lookup's or a refund's configuration, named under a number's path or in the refund's body, when one token may reach several tenants' WABAs (Meta's pages do not say whether either call checks the configuration against the number) | trust Meta / look it up in the number's own WABA first | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: look it up first.** `Payments::configuration_of_the_waba` asks `GET /{waba_id}/payment_configuration/{name}` of the number's binding's WABA with its vault token; none of that name (Meta's empty `data`, its refusal of the name, or an answer naming another configuration) is `404 not_found`, without Meta's code or text, and nothing is read or refunded. A name, in a path or a body, is 1 to 60 characters, with no control character and no `/` or `\`, and is not `.` or `..`: no segment Meta could read as another path once decoded. A check that fails (Meta's timeout or error on that read) is answered with `may_have_been_sent: false`: nothing was asked, and a refund's `Idempotency-Key` is released. The cost: one Graph read per lookup or refund. What it does not do: tell two WABAs' configurations of the same name apart; the payment itself is resolved by Meta under the number's own path. To swap, the check is one private call in `Payments::lookup` and `Payments::refund` (core `payments.rs`); a cache of the WABA's configuration names would replace it. Decisive: `another_wabas_configuration_is_not_found_and_nothing_is_read_or_refunded`, `a_configuration_meta_answers_under_another_name_is_not_found`, `a_refund_whose_check_failed_was_not_sent_and_frees_its_key` | P2 |
| D52 | The OAuth link Meta answers a configuration's creation and a new link with (a credential: whoever opens it links their gateway account to the configuration), under an `Idempotency-Key` | keep the answer whole / keep it without the link / no key on those routes | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: answered to the caller, never kept.** The idempotency engine keeps a second body when the handler gives one (`idempotency::Answer::keeping`, the HTTP adapter's `Success::json_keeping`): the answer with `oauth_url: null`, so a repeat under the key answers `null` (the caller asks for a new link). The link reaches no log, audit event, error, metric or `Debug` (the library's `PaymentOauthLink` holds a `SecretUrl`). To swap, the API adapter's `link_answer` (`api/payments.rs`) answers through `Success::json`, and the link is then kept in `wa_server_idempotency` for `WA_SERVER_IDEMPOTENCY_TTL`. Decisive: `a_created_configurations_oauth_link_is_answered_never_kept`; M1.7's capture (`tests/common/capture.rs`, the links among the secrets) | P2 |
| D53 | The rules an `order_status` message is checked by, when its wire names no currency (India's pages refuse a `payment` update, a `pending` status, a missing `order` and a `reference_id` over 35 characters, which Brazil's allow) | India's / Brazil's / a currency the request names | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: Brazil's, the wider; the India-only refusals are left to Meta.** The parameters are read into the library's `OrderStatus::brl` with no key dropped, and the body's and footer's limits, the `reference_id`'s alphabet and its 60 characters and an order update's description are checked before any request; an India order's status breaking only India's rules reaches Meta, which refuses it. India's rules would refuse Brazil's documented examples. To swap, `OrderStatusParameters::into_message` (core `messages.rs`) builds India's (`OrderStatus::new`), or picks by a currency the request would gain (additive). Decisive: `payment_interactives_are_read_whole_and_checked` (a `pending` status passes) | P2 |
| D54 | The times in Meta's payment answers (Unix seconds in Meta's JSON) | Meta's numbers / RFC 3339 strings | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: RFC 3339 strings, under Meta's names** (`created_timestamp`, `updated_timestamp`, `expiration`), as every time the service answers; every other field keeps Meta's name too. To swap, the API adapter's views (`api/payments.rs`) write the library's `OffsetDateTime` as Unix seconds: within `/v1` only until the first release ([§7.5](#75-versioning)). Decisive: `every_field_of_metas_answers_is_answered` | P2 |
| D55 | A configuration's `provider_name` | any string / Meta's list | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: Meta's four, `upi_vpa`, `razorpay`, `payu`, `zaakpay`** (`payments/payments-in/onboarding-apis`: "Must be one of"), in any case, sent lower case; another is `422 invalid_request` on `provider_name` before any request. A provider Meta answers otherwise is answered as Meta wrote it. To swap, `payments::PROVIDERS` (core `payments.rs`) gains a name when Meta's page does (with the library's `PaymentProvider`). Decisive: `a_configuration_is_checked_before_any_request` | P2 |
| D56 | A refund's body (`payments/payments-in/pg`, "Step 8", whose example writes `value` and `offset` as strings) | Meta's strings only / strings or numbers | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: Meta's names; `value` and `offset` as strings of ASCII digits or as JSON numbers, sent as Meta's strings.** `currency` is `INR`, the page's one value; `offset` is 100; `value` is a positive integer that fits 64 bits (a negative, fractional, zero or larger amount is refused); `reference_id` is 1 to 35 characters of India's alphabet; `speed` is `instant` or `normal`; `payment_config_id` is a configuration's name (D51). Each is `422 invalid_request` on its field (or `body`) before any request. The amount is not compared with what was paid (look the payment up first); Meta and the gateway refuse more. To swap, `payments::refund_request` (core `payments.rs`) and the API adapter's `RefundAmountPart`. *Amended 2026-11-02, the coordinator's decision under the owner's delegation, swappable: a refund requires an `Idempotency-Key`.* Money moves only under a key: a refund without one is `428 idempotency_key_required` (`may_have_been_sent: false`), before its body is checked and before anything is asked of Meta, so that a caller repeating a refund after a timeout repeats it under the same key and the service never refunds twice (the review of P2: a keyless refund that timed out, repeated by its caller, refunded twice). Two refunds under two keys still both reach Meta: partial refunds are Meta's to allow. To swap, the refund's handler passes its optional key to the idempotency engine as the other routes do, without `payments::refund_key` (core `payments.rs`), and the route's `Idempotency-Key` is documented optional again. Decisive: `a_refund_is_checked_before_any_request`, `a_refunds_amount_is_checked_before_any_request`, `a_refund_without_an_idempotency_key_sends_nothing` | P2 |
| D57 | Meta's error `details` on the payment routes | dropped / kept | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: kept**, bounded, as on the send and template routes: Meta's `131005` carries the link to the WhatsApp Payments terms of service there, which the merchant opens. The service accepts no terms itself and no route calls anything that does (AGENTS.md § Decisions, legal acts): the link only passes through. The text is Meta's, about the caller's own WABA or number (the request ran with its binding's token). To swap, `PaymentConfigurations::failed` and `Payments::failed` (core `payments.rs`) answer `counted` alone. Decisive: `the_terms_of_service_refusal_carries_metas_link_and_nothing_is_accepted`; `tests/errors.rs` (`DETAILS_KEPT`) | P2 |
| D58 | Which payment calls are audited, and with what | none / the changes / every call | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable: the four changes, as the caller's audit events** (target `audit`, message `tenant change`): `payment_refunded`, `payment_configuration_created`, `payment_configuration_deleted`, `payment_oauth_link_regenerated`. Each names the key's public id, the tenant and the WABA, the configuration's name and an `outcome`: `answered`, or `unknown` for a call that may have gone through unanswered (a timeout, `may_have_been_sent`); a refund adds what reconciles it: the number, the order's `reference_id`, the amount (`value`, `offset`, `currency`) and, when Meta answered, its refund `id` and `status`. Never the OAuth link, a token or a phone number; reads, refusals that sent nothing and repeats under an `Idempotency-Key` are not audited. A request cut at its deadline while Meta works is settled `504` by the idempotency engine and not audited (its future is dropped). To swap, `audit::payment_audit` (core `audit.rs`) and its four call sites in `payments.rs`. Decisive: M1.7's capture (`check_audit` in `tests/common/capture.rs`, run by `logs.rs` and, live, `live_logs.rs`: every action in order, a refund answered and one cut by a timeout) | P2 |
| D59 | How many codes one recipient gets from the default OTP tenant's number, which every tenant without settings sends from (M3c's review: the library's issue limit is per namespace, so N tenants, or N purposes, could send one recipient N times it, on the platform's bill) | per tenant and purpose only (as M3c built it) / one limit per (sending number, recipient), shared by every tenant and purpose sending from the default tenant's number | **Coordinator's decision 2026-11-01 under the owner's delegation, swappable (the setting roadmap M3g adds), as M3c's review proposed**: shared, the default tenant's own `issue_limit` per recipient, reserved before the library's call and given back when nothing was sent ([§4.7](#47-otp)); the cost, documented: one tenant's codes count against every borrower's, which a tenant with its own number avoids. Pending the owner's confirmation, as stored names: the key/value namespace `wa_server.otp.shared_rate` and the HMAC label `meta-whatsapp-server/otp-shared-limit/v1`. No setting swaps it yet: a setting for its limit, apart from the default tenant's `issue_limit`, would be its swap | M3 |
| D60 | A message by id on a number (roadmap M2g: the design named the lookup, `ConversationStore::message`, not its route) | `GET /v1/numbers/{pn}/messages/{message_id}` / `GET /v1/numbers/{pn}/conversations/{contact}/messages/{message_id}`; the answer the history's message alone / with its conversation's contact | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: `GET /v1/numbers/{pn}/messages/{message_id}`, scope `inbox`, answering `{contact, message}`, as built.** The number's path, because a caller holding a status event's message id finds its conversation without knowing which of the customer's ids (a BSUID or a `wa_id`) keys it; scope `inbox`, because it reads the inbox and asks Meta nothing, although its siblings under `/messages` are scope `send`. The lookup is the store's on the business number, never another number's (`ConversationStore::message`), and a message dated before the binding epoch is `404`, as a missing one. Within v1, the conversation's path can be added beside it; this one stays. To swap, the API adapter's route (`api::inbox::get_inbox_message`, over the core's `Conversations::message`, which takes the id alone) | M2g |
| D61 | The read route's receipt (roadmap M2g) | the latest inbound message / each unread one; before or after the unread reset; past Meta's 30 days sent anyway / skipped; an `Idempotency-Key` or none | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: one receipt, for the latest inbound message, sent before the reset; none past 30 days or before the cut; an optional `Idempotency-Key`, as built.** Meta marks the earlier messages of a one-to-one conversation read with the latest (`messages/mark-message-as-read`; a group's are marked one by one, a gap the roadmap item records); sending it first means a receipt Meta refuses resets nothing, and the binding is read again between the two, so a number that moved while Meta answered resets nothing either (`503`). A latest inbound message older than Meta's 30 days (`inbox::READ_RECEIPT_MAX_AGE`) or dated before the binding epoch gets none, and the count is reset all the same, so no receipt names a message the holder did not receive. The key is optional, as on the design's other POSTs, and the rate class is the writes' (`send`): a repeated receipt names the same message again on the tenant's own number (Meta documents no effect for a repeat), at most the `send` budget's 20 a second. To swap, `Conversations::mark_read` and the constant | M2g |
| D62 | What the window route's `open` means (roadmap M2g) | the recorded window open now / what a free-form reply would meet now; a refusal's reason in the answer or not | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: what a free-form reply would meet now, with no reason field, as built.** `open` runs the reply's own checks with nothing sent (the cut's, then the library's `Inbox::check_reply`), so the route and the reply's `409` never disagree; `closes_at` is the recorded window from the epoch on. The design names only `{open, closes_at}`; a reason (`customer_service_window_closed` or `thread_owned_elsewhere`) can be added later without a break, as an optional field. To swap, `Conversations::window` | M2g |
| D63 | The conversation list's `window` (roadmap M2g) | the window route's answer for each conversation / the recorded window only | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the window route's full answer for each listed conversation, as built.** A list and the route never disagree; the cost is a few reads of the store per listed conversation (at most 100 a page, under the page's one `read` charge and the request deadline). To swap, `Conversations::list_with_windows`, beside `Conversations::list`, which reads no window (the adapter's `list_conversations` calls one or the other) | M2g |
| D65 | The registration routes ([§4.2](#42-resources)) | `request-code`, `verify-code`, `register`, `deregister`, `two-step-pin` under `/v1/numbers/{pn}`, scope `numbers`, each `200 {phone_number_id, status}` / a scope of their own / the PIN change left out | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the five routes, each answering `200` with a body.** One route per Meta call, Meta's names hyphenated; the PIN change included, as row 128 counts it; a `200` with a body (not `204`) so that a kept answer replays as JSON. D6 applies: the PIN is a field of the request, checked as the library's `TwoStepPin` and dropped with it. Swap: the routes are thin adapters over the core's `registration::Registration`; another adapter names them its own way | M3d |
| D66 | A number's registration attempts | per tenant only (the `send` budget) / per number, per replica / per number, shared on the bundle's `KvStore`, fixed windows: registering and deregistering 5 in 72 hours, a code's request and check 10 an hour | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the shared per-number budgets, as built:** fixed windows, each starting at its first attempt, counted by compare-and-swap on the bundle's `KvStore` (every replica books from the same count), charged after ownership (another tenant, or a caller without a key, is refused before: it cannot spend a number's budget) and after the local checks (a malformed PIN costs nothing), and failing closed (a store that fails, or a record that does not read, refuses the attempt, `503`, before Meta is asked). Meta limits registering and deregistering to 10 requests each per number in a moving 72 hours (`133016` on the 11th, and the number locked for 72 hours); the service counts both in one budget of 5 per window, and a moving 72 hours meets at most two of its windows, so it holds at most 10 of either call, whatever a caller's loop does (`registration::tests::two_back_to_back_windows_hold_at_most_ten`). On one clock: replicas whose clocks disagree shorten a window by their skew. Meta's pages name no limit for the code's request and check: 10 an hour is the service's default. The namespace and the record (`wa.server.registration`, `<budget>:<phone_number_id>`, `{"v":1,"since","count"}`) are stable identifiers **pending the owner's confirmation** (ephemeral: renamed, each count starts again; roadmap § Owner touchpoints). Swap: the four settings (`WA_SERVER_REGISTRATION_ATTEMPTS`, `WA_SERVER_REGISTRATION_WINDOW`, `WA_SERVER_VERIFICATION_ATTEMPTS`, `WA_SERVER_VERIFICATION_WINDOW`, [§7.2](#72-environment)), or another `KvStore` | M3d |
| D67 | Idempotency on the registration routes | none (the design listed them nowhere) / on the request of a code, a registration and a deregistration, the PIN no input of the fingerprint / the same, an HMAC of the PIN in it | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the fingerprint without the PIN, and a wrong PIN releases the key.** A 6-digit PIN's digest is the PIN (a million guesses), and even keyed it is a stored derivative of it: the fingerprint is the number and the region only. The cost: a repeat under a kept key with another PIN replays the first answer; a wrong PIN (`133005`, nothing registered) releases the key, so the corrected PIN reuses it. A replay never sends the repeat's PIN, so its answer says nothing of it. Verifying a code and setting the PIN take no key (a check, and a value set). Swap: `Fingerprint::register_number` | M3d |
| D68 | Deregistration, destructive at Meta | the scope `numbers` alone / the number's id again in the body (`confirm`), audited / an operator-only route or a scope of its own | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the scope `numbers`, the number's id again in the body (`confirm`), and an audit event (`number_deregistered`).** Meta's page: the number stops working with the Cloud API until registered again (with its PIN), its history is kept and its local storage stays on, a coexistence number cannot be deregistered, and deregistering is limited to 10 requests per number in a moving 72 hours (D66's budget counts it with registering). Under an `Idempotency-Key` a replay never asks Meta again. The merchant owns the number, so the operator is not in the way; the confirmation stops a call aimed at the wrong path, and the audit event names the key. A scope of its own would be a new stored name (keys keep their scopes), so it stays the owner's (roadmap § Owner touchpoints). Swap: `registration::confirmed`; an operator-only deployment withholds the route by scope until then | M3d |
| D69 | Meta's texts on the registration routes | `details` and debug logs as other routes / none at all | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: none.** Meta's texts could quote the request (the PIN, the code); the answer keeps Meta's code, subcode and trace id (`OwnedNumber::failed_withholding_texts`, M3c's). The cost: `133008`'s wait, which Meta gives in `details`, is not passed on; a caller waits for the attempt budget's `Retry-After` instead. Swap: `registration::Registration`'s `failed`, which calls `OwnedNumber::failed_withholding_texts`; `OwnedNumber::failed` (texts at `debug`) and `ServiceError::with_details` (`details` answered) are the other routes' | M3d |
| D70 | Where an Embedded Signup attempt that stopped after its token was stored is kept (§2.2's `wa_server_signup_attempts`) | a `RecordStore` port change (a table) / a typed store on the library's `KvStore` (no listing: an index record per tenant) | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the `RecordStore` port change, as built.** One attempt per WABA, its tenant's only, gone with its binding (Postgres: a foreign key, `ON DELETE CASCADE`), the session info and the completion kept, never the code or the PIN. Its stored names (migration 6's tables) stay pending the owner (architecture.md § Stable identifiers). Swap: a backend's `RecordStore` (`save_signup_attempt`, `signup_attempt`, `signup_attempts`, `delete_signup_attempt`; the conformance suite `signup` holds the contract) | M3 |
| D71 | How a key rotation reaches the vault's records kept past a binding (credit ledgers, revocation markers), which the vault cannot list | a list of them in the service's records, written when they may be written / a listing in the library's vault (a stored format change: the owner's) / the operator's by hand | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the list, as built.** `wa_server_vault_records` (`waba` and `business` entries), written at a Solution Partner's approval (the WABA and its owner), at every offboarding and revocation, kept for good; the rotation walks it after the bindings and reports `records` and `failed_records`. Its table name stays pending the owner. Swap: `RecordStore::record_vault_entry` and `vault_entries` (the rotation is `authz::rotate_vault`) | M3 |
| D72 | How a destructive `account_update` is confirmed with Graph (§6) | `debug_token` on the stored token / none (the signature) / another read | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: `debug_token` on the WABA's stored token, the setting as the swap.** The binding and the token are read together (the token while the binding held), the token checked with `debug_token`: still valid and granted the WABA is a contradiction, recorded and not acted on (logged, target `audit`). Meta documents no confirming read, so this is an assumption. What it cannot confirm rests on the signature alone: a WABA with no stored token has nothing to delete, and only its line is revoked (from the vault's ledger, else the webhook's `owner_business_id`), as is a `PARTNER_REMOVED` naming no WABA; a revocation the operator's reshare (D74) undoes, never a deletion. With `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION=signature` the app secret's signature is the only gate: whoever holds the secret can sign a delivery that deletes any merchant's token and revokes their line. The action runs after the event is recorded; a retryable failure fails the delivery (`500`, Meta redelivers and the action runs again from fresh reads: the revocation is safe to repeat and the delete is conditional), any other is logged for the operator. Swap: `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION` (`signup::AccountUpdateConfirmation`) | M3 |
| D73 | Which `account_update` events the service acts on, and how | as below / every partner event / none (the integrator's) | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the events acted on, as built.** `PARTNER_APP_UNINSTALLED` of the deployment's app (`partner_app_id` is `WA_APP_ID`) and `ACCOUNT_DELETED` offboard: revoke first in Solution Partner mode, then the token and the bindings, only those read before the revocation's calls (a WABA moved meanwhile keeps its new holder's, logged); `PARTNER_REMOVED` revokes only, in Solution Partner mode (D14), the token kept; a Multi-Partner Solution's removal is not acted on (the deployment's partner business is no setting yet). Swap: the core's `inbound::AccountActions` (`Pipeline::with_account_actions`) | M3 |
| D74 | The explicit operator action that funds a revoked business again (D14) | an admin route re-sharing at once / a one-time grant a tenant's next signup consumes / none | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the admin reshare route.** `POST /v1/admin/wabas/{waba_id}/credit-line/reshare`, audited (`credit_line_reshared`), from the WABA's stored token (the library's `resume_with_approval` with `reshare_after_revocation`), the binding checked again before any credit call; no tenant route ever opts in. Not a legal act (no terms are accepted): no deployment setting gates it. Swap: the route (the core's `Signups::reshare_credit_line`) | M3 |
| D75 | Whether the admin unbind and a tenant's deletion revoke a Solution Partner's line | no / yes / the deletion only | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: a tenant's deletion revokes; the admin unbind does not.** In Solution Partner mode, deleting a tenant revokes the credit line of each WABA it holds, in the order of `DELETE /v1/wabas/{waba_id}` (unsubscribe, revoke, then the token and the bindings, only those the capability was made from); a revocation that fails deletes nothing and is the answer (`step: revoke_credit_line`), so a deleted tenant's WABAs never stay funded by the partner. Each revocation, a tenant's or an admin's, is an audit event (`credit_line_revoked`). The admin unbind revokes nothing, as built: the operator may be moving the WABA to another tenant (revocation is per business, all its WABAs), and revokes explicitly when that is meant (the library's `EmbeddedSignup::revoke_credit_line`). Swap: `Admin::offboarded_by` (the tenant deletion's handler; without it a deletion revokes nothing), and calling `Signups::offboard` from `Admin::unbind` | M3 |
| D76 | The signup routes' small choices | as below / others | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the small choices, as built.** A missing `WA_APP_ID` or `WA_ES_CONFIG_ID` answers `409 signup_not_configured` (a new code, §5.2) rather than refusing the start (a deployment without signup is valid); `start`, `complete` and `resume` charge the `send` class, the attempts list `read` (§6 names none); `POST /v1/signup/sessions` answers `200`; a `CANCEL` answers `cancelled` with its `current_step`, redeeming nothing; the credit allocation is stored on the binding (`wa_server_wabas.credit_allocation_id`); completions and resumptions are tenant `audit` events (`signup_completed`, `signup_resumed`), the webhooks' actions `audit` events with `origin: webhook`. Swap: each is one place in `server_core::signup` or the API adapter | M3 |
| D77 | Where "recorded as received on the tenant's number" is read (roadmap M2e, OQ #43) | the inbox (`ConversationStore`, keyed by number: a lookup by media id is a library port change) / the outbox's rows (`wa_server_events`, each routed to its tenant by the binding it arrived under), the id kept beside the row at insert / a table of its own | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the outbox, with its retention**: an expand-only `wa_server_events.media_id` column with a partial index on `(media_id, created_at)` (migration 7; on `media_id` alone, before the column has statistics, the planner walks the whole outbox for each lookup, the M2e review measured), filled at insert from `NewEvent::received_media`. The column, its index and the migration's description stay pending the owner's confirmation, as stable identifiers (below). The outbox already holds the tenant the routing gave each event (`RouteGuard`, operator-only after a move's window), which the inbox's rows do not; the column is filled from the event the library typed (the five media types' `{type}.id`, an edit's), and found by one index lookup. Its retention is the outbox's (`WA_SERVER_OUTBOX_RETENTION`, 7 days by default), which is how long Meta keeps a webhook's media id (the library's `media` module, "Lifetimes"); a deployment keeping the outbox shorter falls back, past it, to Meta's check. Rows from before migration 7, and those a previous release's replica inserts, record none (Meta's check). The index is built by a plain `CREATE INDEX` in the migration's transaction: it reads the outbox once and holds its inserts meanwhile (about half a second for a million rows, 900 MB, cached, on Postgres 16; a cold outbox as long as reading it), so webhook deliveries wait, or Meta retries them; `CONCURRENTLY` would need a migration outside a transaction, which the service's migrations are not. To swap, a backend answers `Outbox::received_media` from where it likes (a table of its own, the inbox once its port can find a media id) | M2e |
| D78 | What a media id recorded as received elsewhere answers (roadmap M2e) | Meta's `phone_number_id` check, as for any id / `404` with nothing asked, for a download and a deletion; and whether a deletion of one received here is unscoped | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: `404` with nothing asked, for downloads and deletions alike; a deletion is never unscoped**, an id received here included. Meta documents the check for media uploaded on the number; if it lets a received id through whatever number is named, the check would not stop one tenant reading or deleting another's customer's file when one token reaches both, so the service does not ask (the roadmap's decisive test: zero requests). Unscoping only downloads widens the route to the tenant's own customers' files and nothing else; Meta's `DELETE` keeps its check, as no product needs to delete a received file. The cost, documented: a refusal answers without a round trip to Meta, so its time tells a caller that an id it already holds was received elsewhere in the deployment within the outbox's retention (an id never recorded answers `404` after Meta's check). It tells nothing of the file, its number or its tenant; media ids are not enumerable at the media routes' slots; and the cheap mitigations (asking Meta anyway, a delay) would either break the zero requests this row exists for or not hide a round trip's variance. Accepted, not mitigated. To swap, `Media::download_api` and the one line in `Media::delete` (`Received::Elsewhere`), or answer `Received::Never` from `Media::received` for M1b's rule | M2e |
| D79 | The exemption's cut, and its ports (roadmap M2e) | from the binding epoch on (the inbox's `history_from`) / any row of the tenant on the number; the row's time (`created_at`, the service's receipt) / Meta's timestamp; the binding read again or not; `Outbox::received_media` and `Media::new`'s outbox required / with defaults | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the inbox's cut, on the row's `created_at`, and the binding read again before the unscoped call** (`503 storage_unavailable` otherwise). The cut is the inbox's own test (`inbox::visible`: received at or after the cut), and one row of the tenant on the number within it is enough, whatever other rows name the id (`media::Received::among`). The ports' shape, both required (`Outbox::received_media` with no default, `Media::new` taking the outbox), is as built, decided by the coordinator on 2026-11-02 too (a backend built outside this repository must add the method: CHANGELOG, breaking for it; to swap, a default that answers nothing recorded). A row of the tenant on the number from before a move away and back (the same tenant, another binding) is the previous binding's history, hidden as the inbox hides it; a row routed under the current binding was inserted while it held, after it began, so the receipt time is never before the cut for it. Reading the binding again is the inbox's rule (`OwnedNumber::still_held`): a capability made before its number moved asks Meta nothing. A required method breaks an out-of-tree backend at compile time (CHANGELOG), where a default (no row: M1b's rule) would leave it silently without the refusal; the conformance suites hold either. To swap, the filter in `Received::among`, and a default on `Outbox::received_media` | M2e |
| D80 | The service's number events' `data` (roadmap M2d) | `{event, phone_number_id, waba_id}`, and a `reason` on `number_disconnected` / the binding's times, the status, the numbers' display numbers too / one event per WABA | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: one event per number, its `data` `{event, phone_number_id, waba_id}`, and on `number_disconnected` a `reason` from an open set.** The reasons as built: `tenant_disconnected`, `operator_unbound`, and, since the merge with M3a, `partner_app_uninstalled` and `account_deleted`; open: more may come within v1 (a number a refresh drops, the coexistence offboarding: roadmap M2d's "Left"), and a client defaults on one it does not know. Per number, so the number's stream (`GET /v1/numbers/{pn}/events/stream`) and the `phone_number_id` filter carry them; nothing a client could not read from `GET /v1/numbers` (no display number, no token, no Meta text). Why: a tenant switches on the type and the number, and the business's own ids are all it needs to act. The three type names and the four `reason` values are stored (`wa_server_events.event_type`, `data`): **pending the owner's confirmation before the first release** (architecture § Stable identifiers; roadmap § Owner touchpoints). Swap: `number_events::NumberEvent::data` and `number_events::DisconnectReason` | M2d |
| D81 | When each number event is recorded (roadmap M2d) | after the change / before it / best effort or failing the request | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: at each change of a binding, as built: the attach, the disconnections and the signup fail with `503` when the outbox does; the `190`'s is best effort.** `number_connected` once the admin attach stored the token, before it subscribes the app (its repeat is a new generation, so a retried attach records them again: at least once), and once an Embedded Signup completed or resumed, before a kept attempt is dropped (M2d's review: so that an outbox failing is `503` with the attempt kept and its resume records them, once; a first completion that kept no attempt has nothing to resume, and loses them with its `503`). `number_disconnected` before the token and the bindings go, while the binding still holds the rows for the tenant: the tenant's disconnect once Meta unsubscribed the app (before Solution Partner's revocation), the operator's unbind after its unsubscription at best, and Meta's `account_update` offboarding once Graph confirmed it, before anything is revoked or deleted (M2d's review: an outbox failing then leaves the merchant whole, its `Busy` redelivered by Meta, any other failure the operator's; recorded after the token's deletion, it left the token deleted and the binding kept). `number_reconnect_required` once a `190` marked the numbers, only for those not marked already; best effort, as the mark is: a failure is logged, the call's own answer is unchanged, and the event is lost (a marked number is refused before Meta is asked again; the tenant reads `GET /v1/numbers`). The cost, documented: an event recorded before a change that then fails (`503`, the WABA moved or refreshed meanwhile, a revocation that fails) stays for a binding that stays, and its repeat of the same generation records none again; a number a refresh drops gets none. Why: after the change no binding would hold the row for the tenant, and the outbox is the record a client reconciles against `GET /v1/numbers`, not a ledger of the binding's state. Swap: the calls of `authz::Authorizer::record_number_event` (`admin::Admin::attach_waba`, `Admin::unbind`, `numbers::Waba::disconnect`, the signup's completion and resume, `signup::AccountUpdates`' offboarding) and `Authorizer::with_number_events` (without it none is recorded) | M2d |
| D82 | A number event's audience and date (roadmap M2d) | the outbox's route guard on the number, dated by the clock / dated by its binding / a guard of its own (a port change) | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the outbox's existing `RouteGuard` on the number under the epoch's WABA, dated the second after the binding the event is about began, with no window after a move and no skew margin.** No backend change: the row keeps its tenant only while the number is that tenant's under that WABA and the WABA's binding began by that date, so a binding that moved, or one made again in a later second under a tenant created again with the same id, gets nothing; the binding's own event reaches it at once, after a move too. Dated by the clock, a recreated tenant bound before the event would get its namesake's; D36's window is for Meta's undated redeliveries, not the service's own events. The cost, weighed in M2d's review and accepted: a tenant deleted, created again under its id and bound again within the second its namesake's binding began, with that binding's event recorded only after all of it (a `190`'s, between its conditional mark and its insert, or a concurrent request's), gets it; and the guard compares no generation, so an event of a binding's previous generation recorded after its tenant refreshed it reaches that tenant. Either names only the number and the WABA the receiving tenant holds then; closing them is a guard on the whole epoch, a port change of both backends, not proportionate. Swap: `number_events::NumberEvents::row` | M2d |
| D83 | A number event's outbox key (roadmap M2d) | SHA-256 over a label of its own, the type, the number and the binding's epoch / the library outbox key's tags / per occurrence | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: SHA-256 over the label `meta-whatsapp-server/number-event/v1`, then the type, the number, the WABA, the tenant, the binding's start in Unix nanoseconds and its generation, each NUL-separated; its id derived with the event id key as Meta's events' are.** Once per binding generation: a refusal repeated on the same token is one event, both disconnection paths of one binding are one event, an attach again (a new generation) is another, and a resume (no new generation) is none. Why: a key per occurrence would record a repeat's event twice, and a key without the binding's epoch would drop a reconnection's. Stored (`wa_server_events.dedup_key`) and derived into ids, pinned by a known answer: the label and the layout are **pending the owner's confirmation before the first release** (architecture § Stable identifiers). Swap: `number_events::NumberEvent::outbox_key` | M2d |
| D84 | Where the number event types are listed (roadmap M2d) | in `TENANT_EVENT_TYPES` (and so `KnownEventType` and the `types` filter) / a list of their own beside it | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: in `TENANT_EVENT_TYPES`, its last three, and in `number_events::NUMBER_EVENT_TYPES`.** One list says what a tenant receives; the document's `KnownEventType` is generated from it (six values appended in M2d, additive), and the library's classification test leaves the three out (no webhook carries them; the library naming one fails it). Why: a second list would be a second place for a client's generated types and the `types` filter to disagree with. Swap: the list's last three (and `NUMBER_EVENT_TYPES`, which a unit test keeps equal to them) | M2d |
| D85 | `user_action_reported`'s `data` (roadmap M2d) | the library's JSON whole / without Meta's `click_id` and `tracking_token` | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: whole, as every type's.** Meta's page (`marketing-messages/track-click-events`, read again in M2d's review on 2026-11-02) carries the business number's metadata and, per click, `action_type`, `timestamp` and `marketing_messages_link_click_data` (`click_component`, `product_id`, `click_id`, `tracking_token`): no `wa_id`, `user_id`, contact or message id. `click_id` is appended to the URL the user visits, so it ties the click to a visit of the business's own landing page, which the business sees anyway; `tracking_token` is Meta's opaque token; `product_id` is the business's catalogue's. That is less about a person than `message_received` already serves the same tenant (the customer's number, name and text), so serving it whole is proportionate. No log line holds them (M1.7's capture checks the three reviewed types with sentinels). Swap: `events::event_data` for the type (a filter dropping `click_id` and `tracking_token` there) | M2d |
| D86 | Who may erase a customer (roadmap M2f) | an admin key (`/v1/admin`) / a tenant or platform key with the scope `inbox` / a tenant or platform key with a scope of its own | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: a tenant or platform key with the new scope `erasure`, which no other scope grants; no admin route.** The tenant (the merchant) receives the person's request and is the one to decide (D10, legal acts); a scope of its own keeps an inbox key (a CMS's agents) from erasing; ownership keeps another tenant's number `404`, and a platform key reaches only the tenant it names, within its set. The operator erases through the tenant, or through the library. Not a legal choice: the service erases only when a caller asks. The scope's name is a stored name (`wa_server_api_keys.scopes`), pending the owner (architecture.md § Stable identifiers). To swap, `Operation::scope` for `erase_customer`, or an admin route over the same `Erasures` | M2f |
| D87 | The route's shape (roadmap M2f) | the contact in the path / in the body; one contact / several; counts in the answer / none; an `Idempotency-Key` / none | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: as proposed: `POST /v1/numbers/{pn}/erasures`, `{confirm, contacts}` in the body only, no counts in the answer, no `Idempotency-Key`.** The identities in the body, never the path (an ingress logs paths); `confirm` the number's id again (as D68's deregistration); 1 to 16 contacts, each closed over the inbox's identities; `200 {erasure_id, phone_number_id, erased_at}` whether or not the person was known, so the answer tells the caller nothing of what was stored and a repeat answers alike (the operator's audit event has the counts). No `Idempotency-Key`: nothing is sent, a repeat erases nothing more, and a record would keep a fingerprint of the request for 24 hours. Rate class `send`. To swap, the API adapter's `api::erasure`, the core's `Erasures::erase` unchanged | M2f |
| D88 | The erasure journal (roadmap M2f; production guide § 8) | a table of the service's / a port, its default the service's log / none | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: as proposed: a port, `server_core::erasure::ErasureJournal`, its default `LogJournal`, under the dedicated secret `WA_SERVER_ERASURE_KEY`; without the key `409 erasure_not_configured`, on memory storage a throwaway key.** The entry is written before anything is deleted (its failure stops the erasure, `503`): one `info` event with the target `erasure_journal` and the fields `v` (1), `erasure_id`, `at`, `tenant`, `phone_number_id`, `identities` (the hex HMAC-SHA256 of `<phone_number_id>\|<contact>`, comma-separated). Not a table: a restore brings a table back as it was, without the entries made since, the very ones a replay needs. The key is at least 32 bytes and derived from no other key (rotating the OTP pepper or the data key would strand the journal). The target, the layout, the input's format and the `era_` id are stored names, pending the owner. To swap, `ErasureDeployment::with_journal` | M2f |
| D89 | Which outbox rows an erasure deletes (roadmap M2f) | the tenant's rows naming the person / with the number's operator-only rows / every tenant's rows of the number; whole rows / rewritten ones; found by a column / by a scan of `data` | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: narrow by default: on the number, only the calling tenant's own rows whose `contacts` overlap the identities, deleted whole, found by `wa_server_events.contacts` (`TEXT[]`, migration 8, filled at insert by `NewEvent::contacts`, a partial GIN index).** Changed from the proposal, which also deleted the number's operator-only rows: those are the operator's (events of types no tenant receives, `unknown`, `unparsed` and `partner_solution_updated`; events Meta dated before the binding, and undated ones within a move's window, D36; events of a number no tenant held), and expire with the outbox's retention (`WA_SERVER_OUTBOX_RETENTION`, 7 days by default). Another tenant's stream (a previous holder's) is never the caller's to change either. A row naming others too (a history chunk, a group update) goes with their part: the outbox is a 7-day delivery buffer, the inbox keeps theirs. `contacts` holds every value under `wa_id`, `from`, `to`, `input`, `phone_number`, `recipient_id`, `group_id`, a key ending in `user_id`, `_recipient_id` or `participant_id`, and a history thread's `id`, the business's own numbers left out; not a customer quoted in content, nor an `unparsed` body. A row inserted before migration 8 names nobody. Deleting the operator-only rows too is an irreversible deletion beyond the caller's own data, so it stays the owner's ([roadmap § Owner touchpoints](../roadmap.md#owner-touchpoints)). To swap, `Outbox::erase_contacts` in each backend (the memory backend's stream, Postgres's `ERASE_SQL`), or an operator-only route over it; `outbox::event_contacts` for the keys | M2f |
| D90 | The erasure's order and reach on the inbox (roadmap M2f) | inbox first / outbox first; the binding read again / not; the inbox's erasure over the whole number / from the binding epoch on; the whole authorization order / ownership only | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: narrow by default: the outbox before the inbox, the binding read again first; the inbox's erasure reaches only the records from the calling tenant's binding epoch on, the cut its reads use, and never a previous holder's hidden records of the person; the whole authorization order, the vault included, as proposed.** The order: the binding read again (`OwnedNumber::still_held`: `503` if the number moved), the cut, the identities, the journal, the outbox, the inbox, the audit event; the outbox before the inbox, so that a repeat after a failure between them finds the same identities (the inbox's links are still there). The cut is `authz::history_from`, as the inbox's reads. The library has no erasure from a time on: `ConversationStore::erase_all`, and `Inbox::erase_all`, `Inbox::erase` and `Eraser::erase_all` over it, take a number and identities only, so the narrowest call there is reaches the whole number. The service therefore erases only on a number with no cut, where every record is the tenant's to read, and refuses a number with one (its WABA moved from another tenant, or the number itself, M2h; a number bound before Postgres migration 5 reads as moved) whole: `409 erasure_crosses_binding`, a new code, nothing journaled or erased. The library's API is unchanged in M2f: a time-bounded erasure would be a new `ConversationStore` method, which every adapter (Postgres, SQLite, Redis, memory) must implement with its conformance cases, a library item of its own. The refusal tells the tenant only that the number holds records from before its binding, never anything of a person. The whole authorization order (a lapsed token is `409 reconnect_required`). Reaching a previous holder's records (erasing across the cut) is an irreversible deletion beyond the caller's own data, so it stays the owner's ([roadmap § Owner touchpoints](../roadmap.md#owner-touchpoints)). To swap, `Erasures::erase`'s steps (its cut check), a time-bounded erasure once the library has one, or an operator-only route; a token-less capability (`Authorizer::tenant_number`) for a lapsed token | M2f |
| D91 | The outbox port's new method (roadmap M2f) | required / a default | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: as proposed: `Outbox::erase_contacts` required, with no default** (no composition of the other methods deletes rows): a backend built outside this repository must add it (CHANGELOG, breaking for it), as M2e's `received_media`. To swap, a default method on the port | M2f |
| D100 | How the send union's other types are read (roadmap M5a) | Meta's object passed through (`MessageContent::Raw`) / the service's typed request, mapped onto the library's types, unknown keys `422 body` / the same, and what the request carries compared with what the library would send, a dropped key refused on its path | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the comparison, applied to M5a's types only.** Each type of M5a is read into the library's own type (no type re-implemented: `Pin`, `LocationRequest`, `RequestContactInfo`, `CallPermissionRequest`, `VoiceCall`, `FlowMessage`, `AddressMessage`, `MediaCarousel`, `ProductCarousel`, `SingleProduct`, `ProductList`, `CatalogMessage`), its parameters objects (`flow`, `voice_call`, `address_message`, `catalog_message`) as Meta's JSON, as P2 read the payment messages'; then the request's object, written back under Meta's names, is compared with the library's (`request::dropped_key`, P2's comparison): a header or footer the type has not, another type's action key, a misspelt parameter, a card's media by `id`, an `unpin`'s `expiration_days` are `422 invalid_request` on their path, nothing sent. The values the library writes itself (an action's `name`, a card's `card_index` and `type`, `flow_message_version`) must be the ones it writes; a card's media `link` must be `https://`, as every link the service sends. `InteractiveContent.body` is no longer required by the schema (a `call_permission_request` and a `product` may have none: Meta's pages), and the core requires it of every other type (`422` on `interactive.body`). The types before M5a keep their behaviour (a key of their own action they do not read, a header media's `filename` outside a document, is still left out, as since M1b; only the keys new with M5a are refused on them): extending the comparison to them would turn some of today's `202`s into `422`s, a v1 behaviour change left to a later item (roadmap M5a's "Not done here"). Swap: `messages::interactive`, one match choosing `interactive_before_m5a` or `interactive_compared` per type | M5 |
| D101 | Direct Send in the service (roadmap M5a) | refused (`422 unsupported_message_type`, as before) / the library's rules, any category (`DirectSendCategory::Other` passed through) / the library's rules, Meta's three categories | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the library's rules, Meta's three categories.** `category` (`utility`, `authentication`, `service`), `ttl_seconds` and `direct_send_config.template_name` ride on any type, on sends and inbox replies, checked by `OutboundMessage::validate` (a TTL with `utility` or `authentication` only, 30 to 43,200 or 900 seconds; a business-named template with `utility` only, `^[a-z0-9_]+$`, 512 at most; no `authentication` message to a BSUID alone; 1,024 characters of text and 60 of a header). An unknown category is `422` on `body`, before any request: Meta refuses one (`direct-send/send-utility-and-authentication-messages`: `#100`), and the library's inbox would exempt it from the window as a category it does not know. Which formats Direct Send takes (its supported features page: text, URL and reply buttons, call buttons; authentication: text and a copy-code OTP) is Meta's to answer, as is eligibility (`100`, "requires Direct Send"): the library checks neither, and the list grows in phases. No term is accepted: eligibility is WhatsApp Manager's. The inbox reply's window and cut exempt `utility` and `authentication` (sent as templates), not `service`. Swap: `messages::DirectSendCategoryObject` (its `From` into the library's category) and the three lines of `messages::build` | M5 |
| D102 | The design's M5.2 for the send union: which ids a message names are checked against the number's WABA, and how (roadmap M5a) | none (Meta's) / a Flow's `flow_id` and a product message's `catalog_id`, asked of Meta before each send / the same, cached / every id (groups, quoted and pinned messages too) | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: `flow_id` and `catalog_id`, asked before each send, no cache.** A Flow and a catalog are WABA objects any token that reaches the WABA can name, so a token reaching several tenants' WABAs (a platform's system user) could send another tenant's Flow or products from a tenant's number. A `flow_id` is checked with `GET /{flow_id}` (the library's `Flow::get`): Meta must answer that very id with the number's WABA as its `whatsapp_business_account`; a `catalog_id` must be among `GET /{waba_id}/product_catalogs` (a WABA has one at most). Either otherwise, or Meta refusing the id, is `404 not_found` with nothing sent and neither Meta's code nor its text; a check Meta fails was not sent (`may_have_been_sent: false`, the `Idempotency-Key` released). On an inbox reply the check follows the window's, so a reply the window refuses asks Meta nothing. A `flow_id` must be a Meta id (digits) before it becomes a path segment. Not checked: a `flow_name` (Meta finds it in the sending number's WABA), a group (Meta sends to the number's own groups; the group routes are M5f's), a quoted or pinned message (the number's own conversation), and the catalog a template's product cards name (templates are M5b's). The cost: one more Graph read per Flow-by-id or product send; a cache per WABA is the swap's first step if it matters. The check and the send are two calls, so the object could change WABA between them; weighed and accepted: for the send to name another tenant's object, that object must have been the number's WABA's at the check and moved to the other tenant within one Graph round trip, an act of the WABA's own business (WhatsApp Manager, Commerce Manager) that no request of the tenant can make, so the gap gives the tenant nothing. Swap: `messages::admit_named_object`, called by `Messages::send` and `Conversations::reply` | M5 |
| D103 | A Flow's `flow_token` and the logs (roadmap M5a) | as any field / out of every log: the request's `Debug` shows a parameters object's keys only | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: out of every log.** The token is the business's identifier of a Flow session, which its data endpoint trusts. No line logs a request's body; the core's `ActionParameters` and the axum adapter's `InteractiveParameters` show a parameters object's keys, never its values, so a payment's details or an address stay out too; M1.7's capture sends a Flow (accepted, refused locally, failed at Meta) and finds no token. The token is kept where every sent message is kept (the inbox's record of a reply, readable by the tenant's history route; the idempotency engine keeps a digest of the body, not the body): stores, not logs. The library's `FlowParameters` hides the token from its `Debug` too (M5a's review; CHANGELOG, Fixed), so a library message formatted anywhere shows none. Swap: the two `Debug` impls | M5 |
| D104 | Meta's per-user limits on the new types (roadmap M5a) | counted by the service / left to Meta | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: left to Meta, as the library leaves them.** A call permission request is limited to one in 24 hours and two in 7 days per user (`calling/user-call-permissions`), a pin to the group's admin and three pinned messages (`groups/groups-messaging`), an address message to India's businesses and users (`messages/address-messages`): the library counts none, Meta answers each, and a count of the service's would need a store of its own (a stored name the owner confirms). A refusal of the recipient on an inbox reply (a pin to a conversation that is not a group's, an `authentication` message to a contact known by BSUID alone) is `422` on `contact`, the path's, as the reply has no `to`. Swap: a typed store on the bundle's `KvStore` counting per (number, recipient), as OTP's shared limit does (D59) | M5 |
| D105 | Where webhooks out keep their records (roadmap M2c) | a port of their own in the bundle / rows of the outbox / the library's `KvStore` | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: a `WebhookStore` port, `Backend::webhooks`, required with no default.** Nothing is released, so breaking a backend built outside the repository is acceptable (CHANGELOG, breaking for it, as M2b's notifier was). Two tables, `wa_server_webhook_endpoints` and `wa_server_webhook_deliveries` (migration 9; their names pending the owner's confirmation before the first release), a delivery holding no body (each attempt reads its event from the outbox in the endpoint's tenant's stream). To swap, the port itself: a backend brings its own `WebhookStore` | M2c |
| D106 | How events reach the deliveries (roadmap M2c) | an endpoint as a cursor in its tenant's stream, fanned out by the dispatcher / deliveries written by the webhook pipeline with the outbox row / one queue per event | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: an endpoint is a cursor in its tenant's stream, with no backfill.** The cursor starts at the tenant's newest sequence at the creation and moves with the deliveries it records (`fan_out`, once); the outbox stays the source of truth, as for the live streams; turning an endpoint on resumes from its cursor within the outbox's retention. The cost: each round pages up to 100 events (8 MiB of data at most) per endpoint to learn their ids (an id-only read: roadmap M2l). A backfill is a later option on the creation (a starting sequence within the retention). To swap, `Dispatcher::fan_out` | M2c |
| D107 | Whose clock decides what is due (roadmap M2c) | the dispatcher's (the service's `Clock`) / the database's | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the dispatcher's `Clock` decides what is due.** The service's `Clock` is passed to every store call and signs the timestamps too, so the retry schedule runs under a fake clock on every backend; an exception to §8.1's "the database's where it has one". **Requirement**: replicas' clocks agree within the lease (`WA_SERVER_WEBHOOK_LEASE`, 60 s): a replica whose clock runs ahead by more claims a delivery another still holds, and the event is sent twice (at least once still holds; receivers deduplicate on `webhook-id`). To swap, the database's clock behind the same `WebhookStore` calls: a backend compares with its own `now()` instead of the instant it is passed | M2c |
| D108 | Who sends a delivery, and how many at once (roadmap M2c) | a claim with a lease on every replica / the leader lock | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: a claim with a lease on every replica, and its bounds.** `FOR UPDATE SKIP LOCKED`, a token its settlement names, no leader, each tenant's oldest due delivery a claim; a crash's attempt is claimed again after its lease (at least once). The bounds and the lease are settings (made so by the M2c review): `WA_SERVER_WEBHOOK_IN_FLIGHT` (32 attempts in flight on a replica), `WA_SERVER_WEBHOOK_IN_FLIGHT_PER_TENANT` (8), `WA_SERVER_WEBHOOK_IN_FLIGHT_PER_ENDPOINT` (4) and `WA_SERVER_WEBHOOK_LEASE` (60 s, at least 30 s: an attempt takes up to 16 s). To swap, those settings (`DispatchSettings`), or `WebhookStore::claim` | M2c |
| D109 | What the destination allow-list admits (roadmap M2c) | as below / public addresses for any host / names only | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the rules as proposed, tightened by the M2c review.** Names, `*.` names and CIDRs (`/0` refused); every resolved address must pass; IPv6 forms carrying an IPv4 address judged as it; non-public addresses only inside a listed CIDR (so a loopback receiver needs `127.0.0.0/8` listed), every IPv6 address outside the global unicast `2000::/3` or inside the documentation `3fff::/20` counting as non-public (the review's tightening: the IPv4-translated `::ffff:0:a.b.c.d` passed as public); unspecified, broadcast, multicast and the metadata addresses (`169.254.169.254`, `169.254.170.2`, `100.100.100.200`, `fd00:ec2::254`) never; checked at creation and at every attempt on a fresh lookup, the checked address pinned; one connection per attempt (a pool: roadmap M2j), gone with the attempt when its time limit cuts it, its answer's head within 16 KiB (the review's: a cut TLS attempt's connection outlived it). A listed CIDR admits every host name resolving into it and every port of it, for every tenant, each seeing the status its attempt got: list the receivers' addresses, not a cluster's. To swap, `DestinationPolicy`, the `Resolver` and `Sender` ports | M2c |
| D110 | The secrets and the data key (roadmap M2c) | as below / hashed (impossible: signing needs them) / the token vault | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: an optional data key.** `whsec_` and 32 random bytes; AES-256-GCM under `WA_SERVER_DATA_KEY` (`v1.<key id>.<base64 of nonce and ciphertext>`, associated data `wa-server/webhook-secret/v1`, the endpoint's id and the key id), previous keys open what they sealed. Without the key on Postgres, the routes answer `409 webhooks_not_configured` and no dispatcher runs (a deployment without webhooks out is valid, so the start is not refused); memory storage uses a throwaway key. No re-seal walk yet (roadmap M2i). The stored format and the tag are stable identifiers, pending the owner's confirmation before the first release. To swap, `DataKeys` (`WebhooksOut::keys`) | M2c |
| D111 | Retries, failure and disabling (roadmap M2c) | as below / others | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: the retry schedule and the disabling, the window a setting.** 10 s doubling to an hour, a jitter of a fifth either way (never past the hour), until `give_up_at` (`WA_SERVER_WEBHOOK_RETRY_WINDOW`, 72 h from the delivery's creation or its manual retry); a failure after it is `failed`; an endpoint whose attempts have failed since a window before is disabled (`failing`); a ping is attempted once, a manual retry of it once more; a manual retry makes a failed or delivered delivery pending with a new window (a pending one is left); a 3xx is a failure (never followed); a suspended tenant's deliveries wait. To swap, `WA_SERVER_WEBHOOK_RETRY_WINDOW`, `RetrySchedule`, `Dispatcher::attempt` | M2c |
| D112 | The routes' small choices (roadmap M2c) | as below / others | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: all of them.** `types` required (`KnownEventType`s, at least one); 10 endpoints a tenant (`409 webhook_endpoint_limit_reached`, a new code); `POST …/test` answers `202` with the ping's delivery, attempted by the dispatcher; deliveries listed newest first, settled ones kept 30 days from their settlement (D10's proposal for delivery logs, by housekeeping); `tenants` for platform keys not built (an endpoint is one tenant's; roadmap M2m adds it); reads in the `read` class, the rest `send`; no `Idempotency-Key`; audit lines `webhook_endpoint_created`, `…_updated`, `…_deleted`, `webhook_secret_rotated`. To swap, each is one place in `webhooks_out::endpoints` or the API adapter | M2c |
| D113 | An attempt's wire (roadmap M2c) | as below / reqwest | **Coordinator's decision 2026-11-02 under the owner's delegation, swappable: hyper and rustls (aws-lc-rs) over reqwest.** hyper's HTTP/1.1 client connection over the pinned TCP stream, TLS through tokio-rustls on an explicit aws-lc-rs provider with the platform's trust store (the library's choices), 10 s in all, a 5 s lookup, the answer's head within 16 KiB and 1 KiB of its body read, the connection's task aborted with the attempt, `User-Agent: meta-whatsapp-server/<version>`; no dependency new to the tree. To swap, the `Sender` port (`meta-whatsapp-server-http::webhooks_out::HyperSender`) | M2c |
| D125 | Where a resumable upload session lives, and the id the caller sees (roadmap M5c1) | Meta's session id passed through, ownership left to Meta / the service's own id over a record of tenant, number and Meta's session / a stateless signed id | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: the service's own id (`up_` and 32 hex characters, 128 random bits) over a record on the bundle's `KvStore`** (a typed `JsonStore`, namespace `wa.server.upload`, no port of its own), holding the tenant, the number, Meta's session id, the declared file and, once complete, the handle, for 24 hours from the start. Meta's pages say nothing of who may use a session: one token may reach several tenants' numbers (a platform's system user), and a session id can carry a signature (`?sig=`), so only the record decides, a session of another tenant or number or an expired one is `404 not_found` with nothing asked of Meta, and Meta's id reaches no URL, answer or log. The handle is kept in the record so that a lost answer is recovered from the status. Swap: `uploads::UploadSessions` The namespace `wa.server.upload`, the `up_` shape and the 24 hours are pending the owner's confirmation before the first release. | M5c1 |
| D126 | The routes' shape (roadmap M5c1) | `POST /v1/numbers/{pn}/media/uploads` / `…/media-uploads`; the offset in a header as Meta's / in the query; a JSON chunk / raw bytes | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: `POST /v1/numbers/{pn}/media-uploads` (start), `POST …/media-uploads/{upload_id}?file_offset=N` (a raw `application/octet-stream` chunk) and `GET …/media-uploads/{upload_id}` (status)**, scope `media`; `media-uploads` so that no static segment sits among media ids; the offset a query parameter (the name Meta's guide gives it) so that the body is the bytes alone. Only the start takes an `Idempotency-Key` (it creates the session; a chunk's identity is its offset, and the library does not replay a POST); rate classes `send`, `send`, `read`. Swap: `uploads::routes` The paths and the `file_offset` name are pending the owner's confirmation before the first release. | M5c1 |
| D127 | The bounds, which Meta's Resumable Upload API does not state (roadmap M5c1) | none / the media table's limits per type, the declared length as the total, a chunk within the declared file, a 24 hour session | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: the five file types of the guide; a declared length from 1 byte to the kind's limit in Meta's media table (5 MiB for an image, 16 MiB for a video, 100 MiB for a document: the limits of a template header's media) and `WA_SERVER_MEDIA_MAX_BYTES`; a chunk of at least a byte and no further than the declared file's end (so no sum of chunks passes it), read no further than that; one of the tenant's transfer slots (`WA_SERVER_MEDIA_CONCURRENCY`) held while it is read and sent; a record kept 24 hours.** Meta documents no chunk size, total size, session lifetime or handle lifetime (gaps); the profile picture's own limits are M5c2's. Swap: `uploads::file_limit`, `SESSION_TTL`, `OpenUpload::max_chunk` The bounds are the service's own because Meta documents none. | M5c1 |
| D128 | The Meta app a session is opened under, and when there is none (roadmap M5c1) | `WA_APP_ID` / a setting of its own; an existing code / a new one | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: `WA_APP_ID` (the app the deployment already is), and a new code `resumable_upload_not_configured` (409) when it is unset**, rather than `signup_not_configured`, whose sentence names Embedded Signup. The session is opened with the tenant's WABA token (the library's `Authorization: OAuth`); Meta's guide says "a User access token", so whether Meta accepts a WABA token there is to check live (a gap). Swap: `Settings::app_id` Meta's guide says a User access token; issue #123, a human test, checks live whether a WABA token opens a session. | M5c1 |
| D129 | How an upload asks for conversion (roadmap M5c1) | conversion chosen by the service from the `type` and size / an explicit `convert` naming the format / a route of its own | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: an optional multipart field `convert` (`ogg_opus`, `static_sticker`, `animated_sticker`, `jpeg`, `png`: the library's `TargetFormat`s)**, accepted only when the deployment configures a converter; without one the form is what it was (`convert` is an unknown field, `422 body`; a type Meta does not take is `422 type`; an over-limit image `413`), so an upload that needs conversion is refused as before with zero requests. The caller's `type` is not trusted and, with `convert`, not read (the converter detects the format from the bytes); the file is read up to `WA_SERVER_MEDIA_MAX_BYTES`, the converter's own input bound refusing more (`422 file`). The conversion runs after the `Idempotency-Key` is claimed and the key's input is the source file, so a repeat converts nothing again. Swap: `media::convert_target`, `Media::convert` The `convert` field and its five names are pending the owner's confirmation before the first release. | M5c1 |
| D130 | The converter setting (roadmap M5c1) | none, the integrator codes it / `WA_SERVER_MEDIA_CONVERTER=none\|ffmpeg` and a feature | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: `WA_SERVER_MEDIA_CONVERTER` (`none` by default, or `ffmpeg`) with `WA_SERVER_MEDIA_CONVERTER_PROGRAM`, and the server crate's feature `ffmpeg` (off by default; the image carries no `ffmpeg`, so an operator who wants conversion builds with it and installs one); `ffmpeg` in a build without the feature refuses the start.** The library's `FfmpegConverter` is used unchanged, so its guards apply to untrusted media (fixed arguments, stdin and stdout only, restricted protocols and formats, a PNG's text chunks and a GIF's comments taken out first, bounds on input, pixels, frames, length, output and time, the output checked: L19's review fixed the EXIF and iCCP bombs), with its time limit set to 20 s so that a conversion is stopped by its own limit (`502`, retryable) before the 55 s request deadline, which would keep the key as a timeout that may have been sent. A conversion holds an upload slot throughout, which bounds the processes at `WA_SERVER_MEDIA_CONCURRENCY` per replica. Swap: `Settings::media_converter` takes any `MediaConverter` The setting names, the feature `ffmpeg` and the 20 s limit are pending the owner's confirmation before the first release. The Docker image ships no ffmpeg. | M5c1 |
| D131 | A conversion's errors (roadmap M5c1) | `internal` (the classification before M5c1) / one code for the converter's failures | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: an input the converter refuses (or an empty or too large one) is `422 invalid_request` on `file`; a converter that is missing, lacks an encoder, ran past its time (`retryable`), answered another format or was killed is `502 media_conversion_failed`, new**, with `may_have_been_sent: false` (nothing reached Meta) so the key is released; the converter's diagnostics are in no message (the library's promise), the error's kind is logged. Swap: `ServiceError::classify`'s `Error::Conversion` arms The code names `resumable_upload_not_configured` and `media_conversion_failed` are pending the owner's confirmation before the first release. | M5c1 |
| D115 | The routes of a template's life after its creation (roadmap M5b) | one route per Meta call, under the WABA (`/v1/wabas/{waba_id}/templates/…`, `/v1/wabas/{waba_id}/template-groups/…`) / a `POST /v1/wabas/{waba_id}/templates/{id}` mirroring Meta's edit / an action field on one route | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** one route per call, under the WABA, so the authorization order and the WABA's token apply as for the routes before them; an edit is `PATCH …/templates/{id}` (the library's `TemplateEdit`, Meta's names), an unpause `POST …/{id}/unpause`, a comparison `GET …/{id}/compare`, `POST …/templates/archive` and `…/unarchive` (Meta's `hsm_ids`), `GET` and `POST …/templates/library`, `POST …/templates/migrations`, and the groups `POST …/template-groups`, `GET`, `PATCH`, `DELETE …/template-groups/{group_id}`; the library browsed with the WABA's token (Meta's endpoint is not WABA-scoped, the token is). Bodies are Meta's JSON read into the library's types with no key dropped (D100's comparison, P2's `request::dropped_key`), and a `zero_tap_terms_accepted: true` anywhere (an edit's buttons, a library button input) is refused as on a creation. The changes answer `204` (Meta answers `{"success": true}` or, for an unpause, nothing the page prints); Meta's answers with content are answered under Meta's names, a migration's and an archive's per-template failure reasons as Meta wrote them (they are Meta's data, not error texts: section 5.1 keeps Meta's error texts out). The new operation ids (`edit_template`, `list_library_templates`, `create_library_template`, `migrate_templates`, `compare_templates`, `unpause_template`, `archive_templates`, `unarchive_templates`, `create_template_group`, `get_template_group`, `update_template_group`, `delete_template_group`) are hashed into idempotency fingerprints, ephemeral; with the audit actions of D120 they are **pending the owner's confirmation before the first release**. Swap: the axum adapter's `api::template_lifecycle` and `api::template_groups`, over the core's `templates::Templates` | M5 |
| D116 | How a bulk operation's template ids are checked against the path's WABA (roadmap M5b; the design's M5.2) | none (Meta's) / each id looked up by its name (2 management calls an id) / one walk of the WABA's list | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** one walk of the WABA's list, `GET /{waba_id}/message_templates?fields=id&limit=100` page after page until every id is found, at most 10 pages (1,000 templates, `templates::WALK_PAGES`), never cached; an unarchive walks `status=ARCHIVED`, the templates it acts on. An id not found (another WABA's, none, or past the pages) is `404 not_found` with nothing done, nothing of Meta's code or text. For an archive, an unarchive, a group's creation and the templates an update adds, and a migration's ids (on its source, with its token). Meta's pages do not say an archive, a group or a migration checks a template id against the WABA, and one token may reach several tenants' WABAs; a lookup per id would cost up to 200 management calls for one archive of 100 (Meta allows 200 an hour per WABA), the walk ten at most. The cost: a WABA with more than 1,000 templates cannot act on those past them through the service; the walk's filter by status is not checked against Meta's default list (whether it lists archived templates is undocumented, so the unarchive asks for them). A group's removed templates are not walked: they are in the group, which is the WABA's (D118). Past the cap the request is refused (`404`, as an id not found is), never let through unverified: `the_walk_stops_after_its_pages`. Swap: a lookup per id in place of the walk, in `Templates::all_in_waba`; or `WALK_PAGES` | M5 |
| D117 | A migration's source WABA (roadmap M5b) | any WABA Meta accepts (its rule: the same business) / a WABA of the caller's own tenant | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** a WABA of the caller's own tenant, opened through the authorization order (`Authorizer::owned_waba`: another tenant's, or none, is `404 not_found` before its token is read, and a source whose token is unusable answers as any WABA does, `409 reconnect_required`), its `template_ids` found in its list with its own token; the migration is posted to the destination with the destination's token. Meta's rule alone would let a platform token reaching two tenants' WABAs of one business copy one tenant's templates into another's. The source's binding is read as the caller's current one, with the rebinding check of every capability (`Authorizer::owned_waba`), so a WABA that moved to another tenant, or was unbound, is `404`. Swap: `Templates::migrate` (admit another source there) | M5 |
| D118 | A template group's ownership (roadmap M5b; the design's M5.2) | none (Meta's) / the group read first and its `whatsapp_business_account` compared with the path's WABA | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** read first (`GET /{group_id}`, the library's `Templates::get_group`), then acted on only when its `whatsapp_business_account.id` is the path's WABA; otherwise, or when the answer names no WABA (another kind of node: `DELETE /{id}` deletes whatever an id names), or when Meta refuses the id, `404 not_found` with nothing changed or deleted, nothing of Meta's code or text. The cost: one more Graph read per read-modify (an update, a deletion). The read and the change are two calls; a group moving WABA between them is not something a group does (Meta's pages describe none). The node Meta answers must also be the one asked for (its `id` is the path's `group_id`), else `404`, so an id Meta resolves to another node is not acted on as the group (review of M5b). Remaining gap, accepted: Meta's read takes no `fields` here, so another kind of node of the caller's own WABA that carries a `whatsapp_business_account` and the same `id` could be read as a group; it is the caller's own WABA's, and a delete reaches nothing of another tenant's. Swap: `Templates::owned_group` | M5 |
| D119 | Which of M5b's operations take an `Idempotency-Key` (roadmap M5b; section 5.4) | every POST / those that create something / none | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** those that create something, as a template's creation does: a template from the library (`create_library_template`), a migration (`migrate_templates`), a group (`create_template_group`), each fingerprinted over the WABA's id and the canonical body (`Fingerprint::create_library_template`, `migrate_templates`, `create_template_group`). An edit, an unpause, an archive, an unarchive and a group's update or deletion create nothing and take none (as a deletion), and none is replayed by the library on a timeout (`may_have_been_sent: true`). Reviewed for a replay hazard on the others and found none worth a key: an edit sets the template's components absolutely (a repeat sets the same state; Meta's own edit limit refuses an excess one, `template_rejected`), an unpause, an archive, an unarchive and a group's update or deletion are idempotent by state. The keys are scoped to the tenant and, in the fingerprint, to the operation, the WABA and the canonical body (`the_template_lifecycle_fingerprints_are_per_waba_body_and_operation`); a check that refuses before Meta releases the key. Swap: the handlers' `KeyHeader` and the fingerprint constructors (an edit's would be one more constructor) | M5 |
| D120 | M5b's audit events (roadmap M5b) | none / the changes / every call | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** a change that succeeded is the caller's tenant audit event (`audit::tenant_audit`, with the tenant, the WABA and the key's id): `template_edited`, `template_unpaused`, `templates_archived`, `templates_unarchived`, `templates_migrated`, `template_group_updated`, `template_group_deleted`; creations are not audited, as a template's creation is not (Meta's `template_status_updated` events record them). The names are **pending the owner's confirmation before the first release**. Swap: `Templates::audit`'s callers | M5 |
| D121 | A template's Flow token and the logs (roadmap M5b, the M5a review's finding) | as any field / out of every log, as D103 keeps a Flow message's | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** out of every log: the library's `ParameterAction` (a Flow button's `action`) redacts `flow_token` in `Debug`, and the service's raw `template` object (`SendMessage`, `ReplyMessage`, in the core and in the axum adapter) shows its keys only (`messages::KeysOnly`), so a template's parameters (a customer's data, a one-time passcode) never reach one either. Swap: those `Debug` implementations | M5 |
| D122 | The catalog a template's product parameters name (roadmap M5b; D102 left it to M5b) | none (Meta's) / every `catalog_id` the template as sent carries, asked of Meta before each send | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** D102's check, extended: every `catalog_id` of the template Meta would receive, at any depth (a single-product template's header, a product card carousel's cards, a parameter the library does not type), must be among `GET /{waba_id}/product_catalogs` with the number's token, else `404 not_found` with nothing sent (`may_have_been_sent: false`); on an inbox reply after the window's check, as D102's. One more Graph read per template send naming a catalog. Only the first page of the WABA's catalogs is read: a catalog past it counts as not the WABA's (refused, never let through). Swap: `messages::named_object`'s template arm | M5 |
| D150 | A group's ownership: how a group id is checked against the number's (roadmap M5f; the design's M5.2 and M5.1) | none (Meta's) / the group read first and its owner compared, as a template group's (D118) / the service's own records of the groups a number holds, read before the token is opened | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: records, read before the vault.** A group's id is opaque and says nothing of the tenant that may act on it, one token may reach several tenants' numbers, and a lookup at Meta needs the token, which the item's decisive test forbids opening for a group the number does not hold (`404` with zero vault reads). So the service keeps a record per (number, group): a typed store on the bundle's `KvStore` (namespace `wa.server.group`, key `<pn>.<SHA-256 of the pn and the group id>`, value the tenant and the number: the group's id is not stored), **written** by the webhook pipeline when a `group_lifecycle_update` reports a group created for a tenant's number (an event routed to that tenant, with no `errors`) and by every page of the number's listing (the way to adopt a group created before the service, or whose event was missed), **removed** by the event that reports it deleted. The `OwnedGroup` extractor runs step 4 for the number (`Authorizer::tenant_number`: no token), looks the group up under that number and the caller's tenant, and only then step 5; every other case is `404 not_found`, the same answer, with Meta not asked. Consequences, accepted: a group created through the service is not found until Meta's event arrives (or the number is listed); a group deleted at Meta stays recorded until its event (Meta then refusing it is `404` too, no Meta text); a number moved to another tenant keeps its groups' records under the old tenant until a listing re-records them for the new one (the old tenant no longer holds the number, step 4, and the new one finds a record naming another tenant as none: `a_number_that_moves_tenant_takes_no_group_records_with_it`); a number moved away and back finds the record of whichever tenant last adopted the group, so it finds the group only if that was itself; records are not purged when a WABA is unbound or a tenant is deleted, and `KvStore` cannot list or delete by prefix, so a purge is not cheap (it is M5f3): they are inert, a record holds the tenant id, the number and a hash of the group's id (no customer, no group id), and a tenant check precedes every use. Reviewed: the number in a key is checked to be digits at each writer and at the read (so the key's `.` always separates it from the hash, whatever a caller passes); a record is written only for an event the pipeline routed to the number's holder (`events::route`: the event's number bound under the event's WABA, the event's date since the binding), from the same binding read a moment before, so a number that moves in between may get a record naming the previous tenant, which denies and never grants (the read re-checks the binding and the tenant); a listing records only what Meta lists for the number with the tenant's own token. Swap: `GroupRecords` (a table of the `RecordStore`, or a check by Meta's own list with the token, which gives up the zero-vault-read property) and its two writers (`GroupRecords::on_event`, `Groups::list`), or a purge (M5f3). The namespace `wa.server.group` is **pending the owner's confirmation before the first release** | M5 |
| D151 | The key scope of the group routes (roadmap M5f) | the existing scopes / one new scope | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: one new scope, `groups`**, as M5g's: groups act on people (removing participants, approving joins) and a key that only sends or reads the inbox should not. The scope and the thirteen operation ids (`create_group`, `list_groups`, `get_group`, `update_group`, `delete_group`, `get_group_invite_link`, `reset_group_invite_link`, `list_group_participants`, `remove_group_participants`, `list_group_join_requests`, `approve_group_join_requests`, `reject_group_join_requests`, `get_group_analytics`) are **pending the owner's confirmation before the first release**. Swap: `Operation::scope` | M5 |
| D152 | The routes' shape (roadmap M5f) | one route per Meta call (a body on a `DELETE`) / the service's own | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: under the number** (`/v1/numbers/{pn}/groups…`: Meta's groups hang off the business phone number and a group id is the number's, D150); a removal and a rejection are `POST …/participants/remove` and `POST …/join-requests/reject` (a `DELETE` with a body is dropped by proxies, as M5g's unblock); the participants have a route of their own (the group's participants field) and are not a field of the group's read; a group's analytics (row 81) hang off the group, asked of the number's WABA for that one group (Meta supports exactly one). Swap: the routes | M5 |
| D153 | Which group operations take an `Idempotency-Key` (roadmap M5f; section 5.4) | every POST / those that create something / none | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: the creation only** (`create_group`, fingerprinted over the number's id and the canonical body: `Fingerprint::create_group`): a replay would make a second group, and its answer is only Meta's `request_id`. A settings change sets fields, a deletion, a removal, an approval and a rejection are idempotent by state, and an invite link's reset replaces the link either way; none is replayed by the library on a timeout (`may_have_been_sent`). Swap: the handlers' `KeyHeader` | M5 |
| D154 | The bounds the service adds (roadmap M5f) | Meta's only / plus the service's | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: Meta's documented ones** (a subject of 1 to 128 characters trimmed, a description of at most 2,048, 1 to 8 participants a removal, a listing page of 1 to 1,024, an analytics range reaching back 90 days) **and the service's own where Meta says nothing**: 1 to 100 join requests an approval or a rejection (`MAX_JOIN_REQUESTS_PER_CALL`), an id of at most 512 visible ASCII characters (a group's, a join request's), a participant named by exactly one of `phone` (E.164 with its `+`), `wa_id` (1 to 15 digits, no leading zero: Meta's page documents `user` as "a phone number or a WhatsApp user id", and lists a participant by `wa_id`) or `user_id` (a standard BSUID: the BSUID page documents `user_id` on a removal, "`user` or `user_id`, but not both"). Reviewed against Meta's pages (2026-11-05): each form is documented, so none is dropped; an id never reaches a path unencoded (the library keeps one encoded segment, and the group's id must match a record first), and a join request's id and the cursors travel in a body or an encoded query. Swap: those constants | M5 |
| D155 | The group routes' audit events and logs (roadmap M5f) | none / the changes / every call; who / counts | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: the changes, by count and outcome, never who or which** (`audit::groups_audit`, as M5g's block routes): `group_creation_requested`, `group_settings_updated`, `group_deleted`, `group_invite_link_reset`, `group_participants_removed`, `group_join_requests_approved`, `group_join_requests_rejected`, with the tenant, the WABA, the number's id, the key's id, `users`, `done`, `failed` and `outcome` (`unknown` when the call may have gone through unanswered, or Meta answered `206`). No participant, BSUID, username, join request id, invite link or group id (it embeds the business number) is in any line of the service's own; a failed call answers Meta's code and the service's kind and none of Meta's text, in the answer or any log (`OwnedNumber::failed_withholding_texts`). **A gap of the library's:** its debug line for a Graph request names the path it calls, so a group's id is in that line at `debug` (`tests/logs.rs` pins that it is in no other). Accepted: it is the business's own number, never a customer's (the join request ids and participants travel in bodies, which the library does not log), at `debug`, which a deployment turns on knowingly; the library is not changed. Meta's `1312xx` refusals answer their kind's code (D170, L27) with Meta's code and no text (`failed_withholding_texts`; no route here calls `with_details`). The action names are **pending the owner's confirmation before the first release**. Swap: `Groups::audit` | M5 |
| D156 | What the group routes do not offer (roadmap M5f) | everything the library wraps / what the item names | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: what the item names plus the analytics**, leaving out what Meta's pages do not support or the service has no use for yet: pin and unpin (a send, M5a's), the picture (a multipart upload), adding participants (Meta: not manually), deleting the invite link, and a leave (no call: `docs/parity.md` row 78). The picture is M5f2; pin and unpin, adding participants and deleting the invite link wait for a named later item only if Meta offers the call (none is scheduled); a leave has no call. Each is a route away. Swap: the routes | M5 |
| D160 | How a profile picture's handle is checked against the tenant and number that obtained it (roadmap M5c2; the design's M5.1 and M5.2) | accept Meta's `profile_picture_handle` as the library does, and trust it / accept the handle and look it up in the sessions' records / name the upload (`profile_picture_upload_id`) and read the handle from its record | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** name the upload, never the handle. M5c1's records (`wa.server.upload`) are keyed by the service's upload id and `KvStore` cannot be searched by value, so a handle could not be found among them without a second index of handles, which would be one more stored name holding a bearer value. The profile's change therefore carries `profile_picture_upload_id`, and `Uploads::profile_picture` opens the number's own record (the tenant and the number of the caller's `OwnedNumber`) and reads the handle from it: another tenant's, another number's, an expired and an unknown id are one `404 not_found` with zero requests to Meta, and the token's vault read (step 5) precedes it as on every number route; a handle sent as `profile_picture_handle` is `422 body` (an unknown field), as before. The picture's upload must be complete (its handle kept) and an image of `image/jpeg`, `image/jpg` or `image/png` (`422 profile_picture_upload_id`): Meta's profile pages state no picture format or size, so a PDF or a video as a picture is the service's own refusal, with the Resumable Upload API's file types and limits (D127) as the only bound on size. The handle is read from the record each time, so one upload may set the picture again (Meta documents no single use; a handle Meta refuses as invalid or expired is its `invalid_parameter`, no Meta text). The handle reaches no answer and no log (`ProfilePicture` has no `Debug` of it). Swap: `Uploads::profile_picture`, or accept the handle with a second index (a table of the `RecordStore`) | M5 |
| D161 | The scope and operation ids of the display name and picture routes (roadmap M5c2; a family gets a new scope only when its routes do not fit an existing one) | a new scope (`profile`) / the existing `numbers` | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** no new scope. The business profile's routes already sit under `numbers` (S5c) beside registration and the two-step PIN, which are as much about the business's identity as a display name, and a key that can change the profile and register a number can request a display name: a second scope would split one family and strand today's profile keys from the picture. The picture rides the existing `update_profile`; two new operation ids, `request_display_name_change` (rate class `send`) and `get_display_name` (`read`), are **pending the owner's confirmation before the first release**. Swap: `Operation::scope` | M5 |
| D162 | The display name routes' shape and answers (roadmap M5c2) | one route per Meta call (`POST /{pn}?new_display_name=` and the `GET`) / the service's own: a request route and a status route / a request that registers the number again | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** a request route and a status route under the number: `POST /v1/numbers/{pn}/display-name` answers `202 {new_display_name}` (Meta answers only `{"success": true}`; the review is asynchronous) and `GET /v1/numbers/{pn}/display-name` reads the four fields Meta's page names (`verified_name`, `name_status`, `new_display_name`, `new_name_status`), the latest request's among them. The service does **not** register the number again: Meta says the name applies only once approved (the `phone_number_name_update` webhook, `phone_number_name_updated` in the service's events) and the number re-registered within 14 days, and registering needs the two-step PIN, which the service never holds (D6). The caller registers with the existing `POST …/register` when the event says `APPROVED`; the guide says so. Swap: the routes | M5 |
| D163 | `Idempotency-Key` on the display name request (roadmap M5c2; section 5.4) | none / an optional key | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** an optional key, fingerprinted over the number's id and the canonical body (`Fingerprint::request_display_name_change`). A request starts Meta's review and counts against its 10 changes per 30 days, and the library does not replay it on a timeout (`may_have_been_sent`), so a caller's own retry is the risk: under a key a repeat gets the kept answer and a timed-out request is `409 outcome_unknown`, never a second review. The profile change sets fields to values (the library marks it idempotent) and takes none. Swap: the handler's `KeyHeader` | M5 |
| D164 | The display name request's audit event and the routes' logs (roadmap M5c2) | none / the change / the change with the name | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** the change, with the name's length and never the name (`audit::display_name_audit`, action `display_name_change_requested`, with the tenant, the WABA, the number's id, the key's id, `name_chars` and `outcome`: `answered`, or `unknown` when the call may have gone through unanswered). A display name change starts a review and, once approved and the number registered again, shows to every customer, so who asked and for which number is kept; the name is the business's own to read back from the status route, and keeping it out of the line keeps the audit trail free of text a caller chose. The profile's change is not audited (as before: it sets fields at the business's own profile and destroys nothing). Meta's refusals answer its code and kind with `details` as every number route does; the picture's handle, Meta's session id, the picture's URL and the upload ids are in no log or answer (`tests/logs.rs`'s section `profile`). The action name is **pending the owner's confirmation before the first release**. Swap: `DisplayNames::audit` | M5 |
| D165 | The bounds (roadmap M5c2) | Meta's only / plus the service's | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** Meta's documented ones and the service's own where Meta says nothing. Documented: an address of at most 256 characters, a vertical other than `UNDEFINED` and `NOT_A_BIZ` (the library checks both before any request), 10 display name changes per 30 days and 14 days to register after an approval (Meta's to enforce: the service keeps no count). The service's own: the name trimmed, 1 to 256 characters (the one display name length Meta documents, the in-app signup's), no control character, and no bidi override, embedding or isolate, zero-width space, word joiner, byte-order mark, soft hyphen, invisible operator, tag character or line and paragraph separator (every Unicode format character except the joiners U+200C and U+200D and the direction marks U+200E, U+200F and U+061C, which Persian, Indic and mixed-direction names need: `display_names::ALLOWED_FORMAT_CHARS`); a name of nothing but those is empty. Those characters are how a business name is spoofed (a reversed or invisible-padded name reads as another), and Meta's review is not the only gate. Homoglyphs (a Cyrillic letter for a Latin one) are not refused: telling them from honest names of another script is Meta's review's, and a rule here would refuse real names. The picture an image upload (D160). What customers read in the profile is bounded the same way before any request: `about`, `address` and `description` carry no control character (a newline or tab only in `address` and `description`) and none of the characters above; `email` is empty (clears it) or one `@` between two parts, at most 254 characters, without whitespace; each of `websites` is an `http` or `https` URL with a host and no credentials, at most 2048 characters (not `javascript:`, `data:`, `ftp:` nor `https://good.example@evil.example/`), all `422 invalid_request` on the field. Meta's pages state no length for `about`, `description`, `email` or `websites`, nor a count of websites: Meta judges those. Swap: `display_names::NAME_MAX_CHARS`, `display_names::ALLOWED_FORMAT_CHARS`, `numbers::WEBSITE_MAX_CHARS`, `numbers::EMAIL_MAX_CHARS`, `uploads::PICTURE_TYPES` | M5 |
| D166 | What the profile and display name routes do not offer (roadmap M5c2) | everything the library wraps / what the item names | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** what the item names. Not offered: the profile-node API (`GET`/`POST /{whatsapp-business-profile-id}`: the number's own profile is the one Meta's guide documents, and the node adds only `id` and `verified_name`, which the display name's status gives); a display name change for an Official Business Account (Meta: contact support; the service cannot tell, and Meta refuses); the re-registration (D162); a count of the 30-day window (D165); a picture uploaded in the same call (the multipart upload is M5c1's resumable upload, then this route). Each is a route away | M5 |
| D190 | The settings routes' shape (roadmap M5c3) | one route per Meta call / a resource per family with one setting changed per call | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: accepted as proposed (the exclusions go to M5c6).** A resource per family under the number, scope `numbers` (D161): `settings` (read, and a `PATCH` that takes exactly one setting, as the library sets one value per Meta call and Meta documents no combined call, so a change either went through or did not), `settings/payload-encryption` (`PUT`, `DELETE`), `username` (and `username/suggestions`), `official-business-account`, `compliance-info`, `health-status`, `conversational-components`. Setters that set a value take no `Idempotency-Key` (a repeat is harmless); the application, which creates one, does (D192). Not offered: the number's `webhook_url` and `connection_status` setters (they would let a tenant divert or fake its number's events, which the service owns; Meta documents what neither does), `calling` settings (M5e), bot details (no page says where a bot id comes from), the coexistence data sync. Operation ids `get_number_settings`, `update_number_settings`, `enable_payload_encryption`, `disable_payload_encryption`, `get_username`, `set_username`, `delete_username`, `list_username_suggestions`, `get_official_business_account`, `request_official_business_account`, `get_compliance_info`, `set_compliance_info`, `get_health_status`, `get_conversational_components`, `set_conversational_components`, `delete_contact_book_entry` and the paths: pending the owner's confirmation before the first release. Swap: additive routes in a later v1 minor (M5c6 holds the exclusions) | M5 |
| D191 | Payload encryption's keys (roadmap M5c3) | answer Meta's `cloud_encryption_key` / answer the status and fingerprint only | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: accepted as proposed; Meta's `cloud_encryption_key` is never answered.** The business's key is read into `number_settings::ClientKey` (`Debug` shows a length), checked by the library (base64, not an obvious private key; `422` without quoting it) and sent in the body only; Meta's texts of a failed call are withheld from the answer and from every log (`failed_withholding_texts`); the call is not replayed after a timeout (the library's `post_once`); the settings read answers `status` and the key's fingerprint and **drops Meta's `cloud_encryption_key`** (the task's rule: keys are never in answers). The cost is that an app cannot learn Meta's key from the service; if it must, a route that answers it is a decision of its own. Swap: a later additive field behind its own decision (M5c6 holds it; `SettingsView`) | M5 |
| D192 | Legal acts among the number settings (roadmap M5c3; AGENTS.md, Legal acts) | none / a setting / a scope per act / an operator route | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: one legal act, gated by a setting; the other two calls are not.** Only the India compliance information declaration (`PUT …/compliance-info`, the business's statutory statement of its legal entity and grievance officer) is a legal act: off until the deployment sets `WA_SERVER_LEGAL_ACTS=on` (`Settings::legal_acts`, default off; anything but `off`/`on` refuses the start), it is `403 feature_not_available` (an existing code, no new one) before any request to Meta; on, it is audited (`compliance_info_set`: the tenant, the WABA, the number, the key's id, the outcome; never a value) and never taken by anything else. The Official Business Account application is **not** gated: the library classifies it as an application for Meta's review, not a legal act, and the service follows the library; it stays audited (`official_business_account_requested`) and keeps its `Idempotency-Key`. The contact book erasure is **not** gated either: it is a data-subject erasure the tenant performs explicitly under the separate scope `erasure`, audited (`contact_book_entry_deleted`, never the BSUID) and never automatic, and gating it off by default would block erasure duties (D10, D86). Nothing here accepts terms. Swap: the gate is one predicate over operations, `number_settings::is_legal_act` in the core; adding an operation to the gate is one more arm there (a test lists the set), and a per-tenant record or a scope per act would replace the setting Since roadmap M5h the setting also gates the operator's legal acts (`number_settings::is_legal_admin_act`, the admin twin; D231): one deployment switch for both, accepted by the coordinator on 2026-11-06 (swap: one setting per act); its second meaning is pending the owner's confirmation before the first release. | M5 |
| D193 | The bounds of the number settings (roadmap M5c3) | Meta's only / plus the service's | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: accepted as proposed.** Meta's documented limits are the library's (a username of 3 to 35 characters in Meta's format, 5 to 10 supporting links, an entity name of 2 to 128 characters, emails under 128 characters, the entity type's rules, 4 ice breakers of 80 characters, 30 commands of 32 and 256 characters, unique command names). The service's own, where Meta states none: every link `http` or `https` without credentials (the profile's website rule), short texts of at most 128 characters and free texts of at most 1024, no control, bidi override or zero-width character in any text, an email's shape, a phone number of 5 to 32 digits with an optional `+`, spaces, `-` and parentheses, and a local storage region of two uppercase letters. A BSUID only for the contact book (a phone number or a parent BSUID is refused, nothing quoted). Swap: the bounds are constants in one module (`number_settings`, core) | M5 |
| D200 | Whether a signup binds every WABA it grants (roadmap M3f; OQ #5) | the primary only / every granted WABA / a per-signup choice | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: a deployment setting, `WA_SIGNUP_EVERY_GRANTED_WABA`, off by default.** Off, the primary WABA (the event's `waba_id`, else the first of `waba_ids`) is bound and the answer says so (`waba_id`) and lists the others (`not_bound_waba_ids`); on, the library's `onboard_wabas_with_approval` binds each, D4 pre-checked on every WABA the event names (a refusal binds nothing, before the attempt is redeemed) and again in each WABA's approval (`refused` for a grant the event did not name; `held` for one the vault holds; never taken). The setting's name is **pending the owner's confirmation before the first release**. Swap: the setting (`SignupDeployment::every_granted_waba`); a per-signup choice would be a body field | M3 |
| D201 | How a hosted `PARTNER_ADDED` reaches a tenant (roadmap M3f; OQ #11) | remember it and let a tenant claim it / onboard it at once to a tenant named by an operator rule / the integrator's own webhook | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: remember and claim, off by default.** The signed webhook (the pipeline checks the signature first: an unsigned or forged one is `401` and remembered nowhere) is remembered 7 days (`wa.es.hosted`), asking Meta nothing; the operator's claim (D202) takes it atomically and runs the library's `onboard_hosted_with_approval`, the system token being `WA_SYSTEM_TOKEN` (or the Solution Partner's). Off by default (`WA_HOSTED_SIGNUP`). The setting's and the system token's names (`WA_HOSTED_SIGNUP`, `WA_SYSTEM_TOKEN`) and the stored name `wa.es.hosted` are **pending the owner's confirmation before the first release**. Swap: the setting; `signup::AccountUpdates` (the pipeline's `AccountActions`) for another source of arrivals | M3 |
| D202 | Who may claim an arrival | any tenant naming the WABA and its business / only a tenant the operator assigned / the operator, naming the tenant / a state carried through Meta's page | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: the claim is an operator action, `POST /v1/admin/signup/hosted` (admin key) taking `{tenant_id, waba_id, owner_business_id, pin?}`.** The first draft let any tenant naming both ids claim, changed in review: Meta's hosted page carries no state and the webhook no tenant, and a WABA id and its business id are no secrets (the merchant sees both in WhatsApp Manager), so tenant A could claim an unclaimed arrival for tenant B's merchant and have that merchant's business token stored under A: a cross-tenant takeover. The operator knows which tenant sent which merchant, so it names the tenant (`404 not_found` for none, `403 tenant_suspended`); no tenant key claims (a tenant, a platform or a scoped key is `403`, and the tenant route was never released). Kept: D4 (a WABA bound to another tenant is `409 waba_owned_by_another_tenant`, before the arrival is read), the arrival claimed once (a compare and swap; the loser is `404` and asks Meta nothing), the owner business matched, the put-back of an arrival whose onboarding stopped before its token was stored. Audited as an operator's change (`hosted_signup_completed`, the admin key's id, the tenant, the WABA). The route and the audit action are **pending the owner's confirmation before the first release**. Swap back: a tenant route over the same core call (`Signups::complete_hosted`, one route), or an operator-assigned reservation record like D203's, which a tenant's claim would check | M3 |
| D203 | Who may offer a pre-verified number in a launch | any tenant naming an id / only ids the operator reserved for the tenant | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: reserved ids only.** `PUT /v1/admin/pre-verified-numbers/{id}/reservation` (`wa.es.preverified`, a new stored name, **pending the owner's confirmation before the first release**) ties an id to a tenant; the launch's `pre_verified_phone_ids` checks each, an unreserved id, another tenant's and a malformed one the same `422`. Meta documents tracking who claimed a number as the integrator's duty. Swap: `signup::Signup::start_with` | M3 |
| D204 | Whose token manages the pool | the tenant's / the deployment's system token | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: the deployment's** (`WA_PARTNER_BUSINESS_ID` and `WA_SYSTEM_TOKEN`, or the Solution Partner's system token), so the pool routes are the operator's (admin key) and no merchant token is used. Both settings' names are **pending the owner's confirmation before the first release**. Swap: `SignupDeployment::pre_verified_pool` | M3 |
| D205 | What the pool routes leave out | everything Meta offers / the six calls the flow needs | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: add, list, read, request a code, verify, delete, plus the reservation.** Not offered: sharing a number with a partner and reading who has one (`share_pre_verified_number`, `partners`), which concern Multi-Partner Solutions; the library has them. Deleting a number is an explicit, audited operator action no other call makes. Swap: a route over the library's call | M3 |
| D230 | The scope and routes of the Marketing Messages API, max price and welcome message sequences (roadmap M5h) | the existing scopes (`send`, `templates`, `analytics`) / one new scope / one per family | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable.** One new scope, `marketing`, for every route of the family: a marketing send and max price spend money, and a welcome message sequence is an ad's asset, so a key that sends transactional messages (`send`) or edits templates (`templates`) does not get them, and a key made before M5h has none. A resource per Meta call: the send under the number (`/v1/numbers/{pn}/marketing-messages`), everything else under the WABA (`/v1/wabas/{waba_id}/marketing/…`, `/welcome-sequences`); a creation takes an `Idempotency-Key`, a setting is set by `PUT`. All writes share the `send` rate class (the routes' classes follow their method, as `v1s_routes_keep_their_class` pins). Swappable: the scope is one `Operation::scope` arm each. Pending the owner's confirmation before the first release: the scope, the 14 operation ids and the paths |
| D231 | The max price agreement (roadmap M5h; OPEN_QUESTIONS #45; AGENTS.md, Legal acts) | a tenant route / an admin route gated by `WA_SERVER_LEGAL_ACTS` / no route | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable.** An admin route, `POST /v1/admin/businesses/{business_id}/max-price-agreement`, the operator's explicit act on a business portfolio (the agreement is per portfolio, and no record of the service names one): the admin key's id is the audit event's (`max_price_agreement_signed`: the key, the business and the outcome, `answered` or `unknown` after a timeout; never the signer). Gated by `WA_SERVER_LEGAL_ACTS` through `number_settings::is_legal_admin_act(AdminOperation)`, the admin twin of `is_legal_act` (a tenant `Operation` has a scope and a rate class, this route has neither): off, `403 feature_not_available` before the body is read and before any request; on, exactly one request, with the `token` of the body (a system token or a business token of an admin of the business's Business Suite, used once, stored nowhere), never replayed. The signer's name and email are in no log, audit event, error, `Debug` or answer (Meta's texts, which quote the email, are withheld: the code and the kind are answered). The service reads the state of an agreement nowhere (the page's status read needs the same token in a read request: D235). No other route signs it: a refusal for want of the agreement (`100`, a duplicate's `403`) is Meta's, returned as it is. Swappable: leave the setting off (the default) and nothing signs; the token the signer supplies, used for the one call and stored nowhere, would be replaced by a vault-held token behind its own decision, and `WA_SERVER_LEGAL_ACTS` covering the tenant act (D192) and the admin acts by one setting per act. Accepted as one deployment switch for both meanings (D192's and this one). Pending the owner's confirmation before the first release: the path, the audit action and the setting's extra meaning |
| D232 | The audit events and the logs of the family (roadmap M5h) | none / the change / the change and the sends | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable.** The tenant's changes, by ids and flags (`audit::marketing_audit`): `cloud_api_marketing_set` (the flag), `marketing_template_duplicated` (the source, the duplicate, the price), `template_max_price_set` (the template, the price), `template_max_price_cleared`, `welcome_sequence_created`, `welcome_sequence_updated`, `welcome_sequence_deleted` (the sequence's id), each with the key, the tenant, the WABA and an outcome (`answered`, or `unknown` when it may have gone through). Never a name, a welcome message's text, a template's parameters or a recipient. A send is not audited (no send is). A send's and a sequence's creation and change failures answer Meta's code and the service's kind, never Meta's text, which may quote a parameter or the message, and log none (`failed_withholding_texts`). Pending the owner's confirmation before the first release: the seven action names |
| D233 | A template's and a sequence's ownership (roadmap M5h; the design's M5.2) | none (Meta's) / looked for in the WABA's own first / a store of the ones created through the service | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable.** Looked for through the WABA's own edge, as D116 and D144 do: a template by its name, then among the WABA's templates of that name (`Templates::require_owned`, the lookup every template operation shares), a sequence by the `sequence_id` filter of `GET /{waba}/welcome_message_sequences`, which must answer that very sequence. Another WABA's, or none, is `404`, without Meta's code or text, and only the lookup was sent. A catalog a marketing template names is checked as the send union checks it. Swappable by the lookup (one function) |
| D234 | The bounds of the family (roadmap M5h) | Meta's only / plus the service's | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable.** Meta's documented limits are the library's (a max price of 1 to 2147483647 per 1,000 deliveries, 240 country multipliers in (0, 10] keyed by two capital letters, the one bid strategy, a positive per-message multiplier). The service's own where the pages state none (constants of `marketing`): 240 countries a reach estimate, 128 characters a sequence name, 1,024 a welcome text, prefilled message or quick reply, 20 quick replies, 100 sequences a listing, 256 and 320 characters a signer's name and email |
| D235 | What the family does not offer (roadmap M5h) | everything the library wraps / what the item names | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable.** Not offered, and tracked by M5h2: the agreement's state read (`GET /{business}/max_price_agreements` answers the signer, and needs the operator's token in a read), the partner Intent API (`POST /{business}/onboard_partners_to_mm_lite` emails a business's admins an invitation to accept the terms), the client WABAs listing by onboarding status, the WhatsApp Manager visibility switch, and a GIF header's send-time parameter (no page shows it: a template Meta's object carries it as written). Accepting the Marketing Messages API's Terms of Service is done in WhatsApp Manager by a person with full control of the portfolio (`marketing-messages/onboarding`): no call exists, and the service offers none. Automatic events need no route: `automatic_event_detected` is a tenant event since M2d |
| D170 | How Meta's group error codes map to kinds and to the service's codes (roadmap L27) | one kind per code / existing kinds where they fit, new ones where the answer differs / everything stays `unknown` | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: `ErrorKind::from_code`, one table and one test.** `groups/error-codes` (fetched 2026-11-05) lists `131020`, `131041`, `131059`, `131201`, `131202`, `131204`, `131207`–`131215`; `131203` is in `groups/reference` only, as the example reason a join request cannot be approved. Existing kinds where they fit: `131041` `not_found`, `131202`, `131209`, `131210` `invalid_parameter`, `131215` `feature_not_available`, `131020` `undeliverable` (a message to a group of one member: the closest kind, proposed, not Meta's word). New, named for what the caller does next: `invalid_cursor` (`131059`, 422), `group_participant_not_found` (`131212`, 422), `join_request_not_found` (`131213`, 422), `group_limit_reached` (`131204`, `131211`, 409), `group_creation_disabled` (`131214`, 409), `group_partially_applied` (`131201`, 409), `recipient_terms_not_accepted` (`131203`, 409), `group_suspended` (`131207`, 403), `group_rate_limited` (`131208`, 429). Not on the page, so not classified: `131200`, `131205`, `131206` and the rest of the range stay `unknown`. The group routes answer the code and none of Meta's text, as before (`failed_withholding_texts`). The nine codes are **pending the owner's confirmation before the first release**. **Scope, checked in review:** `from_code` classifies a code whatever endpoint returned it, so `131020`, `131041` and `131059` (the 1310xx messaging range) change for non-group callers too (`Unknown` before). Meta's general error-codes page and the messages, templates and media pages (read 2026-11-05) list none of the three; the webhook fixtures show `131020` only on a group message's failed status. No library caller branches on `Undeliverable`, `NotFound` or `InvalidCursor` (the bot's stops, the pacer, the inbox and the window handling read other kinds), and `object_refused` already took `Unknown` and `NotFound` alike; a send route that gets `131020` or `131041` now answers `422 undeliverable` or `404 not_found` where it answered `502 unknown` (both statuses are declared by the send routes; CHANGELOG, Changed). `131203` rests on `groups/reference` naming it as an example error code of a join request, not on the error-codes page. Swap: a kind is one `from_code` arm and one row of the service's `codes!`; to scope any of these to the group calls, drop its arm and classify in the client's `groups` module, which would first need a hook there for mapping a call's error (none exists today) | L27 |
| D171 | Whether a group error may be retried (roadmap L27) | as the kind's class (`RateLimited` is) / never | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: none is retryable, none is resendable.** `131208` (`429`, too many group operations from the number) is its own kind, `group_rate_limited`, not `rate_limited`: that kind is rejected before processing, so the client would replay a non-idempotent call on a non-429 status, and the page does not say Meta did nothing. It is not even `retryable`, so a read is not looped on it either; Meta gives no wait. Swap: add the kind to `ErrorKind::is_retryable` (a read retries; a POST still needs `is_rejected_before_processing`, which stays false). The test `the_group_error_codes_classify_as_their_pages_say` pins it | L27 |
| D172 | The group reads' "4xx `unknown` is Meta refusing the group" rule (roadmap L27) | drop it once the codes are classified / keep it / narrow it | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: kept, and documented.** The page's `131041` is `not_found` now, so the documented refusal no longer needs the rule; but the page is not Meta's whole behaviour (the M5f tests had Meta answer `100` for a missing group, which no page documents), and the rule is `object_refused`'s, shared with other routes. What changed: a code the page documents is now answered as its own kind (`131059` as `invalid_cursor`, `131215` as `feature_not_available`, `131207` as `group_suspended`), where the rule made each a `404 not_found` on a read. Swap: `Groups::failed_on_group` | L27 |
| D240 | How Meta's calling error codes map to kinds (roadmap L28) | one kind per code / existing kinds where they fit, new ones where the caller's next step differs / everything stays `unknown` | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable** (swap: `ErrorKind::from_code`, one table and one test). `calling/troubleshooting` (fetched 2026-11-06) lists `138000`-`138007`, `138009`, `138012`-`138015`, `138017`-`138023` (not `138008`, `138010`, `138011`, `138016`: they stay `unknown`) and the shared `100`, `613`, `131009`, `131030`, `131044`, `131055`. New kinds, named for what the caller does next: `calling_not_enabled` (`138000`, `138015`, `138018`: change the number's call settings), `calling_temporarily_disabled` (`138014`), `call_permission_missing` (`138006`), `call_permission_already_granted` (`138017`), `call_already_ongoing` (`138003`), `call_limit_reached` (`138002`, `138005`, `138009`, `138012`), `call_connection_failed` (`138004`, `138007`, `138019`, `138020`), `call_media_timeout` (`138021`-`138023`). The eight names are **pending the owner's confirmation before the first release**. Swap: a kind is one `from_code` arm and one row of the service's `codes!`; to merge two (say `calling_temporarily_disabled` into `account_restricted`), change the arm and drop the row | L28 |
| D241 | Which calling codes take an existing kind, and what that changes outside calling (roadmap L28) | an existing kind where it fits / a calling kind for each | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: the existing kinds below.** `100`, `131009` stay `invalid_parameter` (already); `138001` and `131030` are `undeliverable` (the receiver cannot be called; the recipient is outside a public test number's allowed list: the closest kind, not Meta's word); `138013` is `feature_not_available` (business-initiated calling is not available for the number, as `131215` is for groups); `131044` is `payment` (the page calls it "similar to" the messaging eligibility error); `613` is `rate_limited` (the page says 429, "try again later"). **`from_code` classifies whatever endpoint returned the code**: `131030`, `131044` and `613` change for non-calling callers too (`unknown` before; a send route that gets `131030` now answers `422 undeliverable`, one that gets `131044` `403 payment`; CHANGELOG, Changed), and `613` makes a POST replayable by `Error::may_resend` as every `rate_limited` is: if that is wrong for a send, give `613` a kind of its own. **Gap:** the page's `131055` (a Graph call to a SIP-enabled number) is the messages page's `marketing_not_allowed` already and a code cannot tell them apart; it stays `marketing_not_allowed`, pinned in the test. Swap: one arm each; for `613`, give it a kind of its own (a `from_code` arm and a `codes!` row), which makes it neither retryable nor resendable if that is wanted | L28 |
| D242 | Whether a calling error may be retried (roadmap L28) | as the kind's class / never | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: none of the eight kinds is retryable or resendable.** (`613`, `rate_limited`, is, as every `rate_limited`). The limits (`call_limit_reached`) are not `rate_limited`, which is rejected before processing and so replayable: a call is never replayed, and `138012` and `138009` are not time-boxed by the page. `call_connection_failed` carries Meta's 500 for `138004` and `138007`: the library says `may_have_been_sent` for a non-4xx, which is right (the call may have connected), and the service answers `502` with `retryable` false. `138019`-`138023` come in the call-terminate webhook (`calls`), not as a Graph answer; classifying them lets an integrator hand a webhook's `errors[].code` to `ErrorKind::from_code`. Swap: add the kind to `ErrorKind::is_retryable`; `is_rejected_before_processing` should stay false for the connection kinds | L28 |
| D243 | The calling kinds' statuses in the service (roadmap L28) | Meta's (401, 400, 500) / the service's by class | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: by class.** `409` for a state to change first (`calling_not_enabled`, `call_permission_missing`, `call_permission_already_granted`, `call_already_ongoing`), `403` for `calling_temporarily_disabled` (Meta restricts the number), `429` for `call_limit_reached`, `502` for `call_connection_failed` and `call_media_timeout` (Meta or the network failed). Never `401`, which is the service's own (Meta's `401` for `138000` and `138006` is not a missing API key). The calling routes answer the kind and Meta's code in `graph`, and none of Meta's text (it may quote an SDP line); they take no "4xx `unknown` is a refusal" rule. Swap: the status is one `codes!` row | L28 |
| D132 | The routes of Flows' management (roadmap M5d) | one route per Meta call under the WABA (`/v1/wabas/{waba_id}/flows/…`) / Meta's shapes mirrored (`POST /{flow_id}`, `POST /{flow_id}/assets` multipart) / an action field on one route | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: one route per call, under the WABA, and a scope of its own, `flows`.** One route per call, under the WABA, so the authorization order and the WABA's token apply as for the routes before them: `POST` and `GET …/flows`, `GET`, `PATCH`, `DELETE …/flows/{flow_id}`, `PUT …/flows/{flow_id}/json` (the body is the Flow JSON itself, at most 2 MiB, sent to Meta byte for byte so that its validation errors' lines match the caller's text), `POST …/preview` (it mints or revokes a link: not a `GET`), `POST …/publish`, `POST …/deprecate`. The scope is **`flows`**, a new one (one scope per family, roadmap M5g's way: a key that edits templates must not also create, publish, deprecate or delete Flows, and no key minted before it gains the routes; the implementer's draft had taken `templates`), the rate class `templates` (Meta's management calls of a WABA are one budget), the OpenAPI tag `flows`. The changes answer `204`; Meta's validation errors come back in the answer of a creation and of an upload. The new operation ids (`create_flow`, `list_flows`, `get_flow`, `update_flow`, `upload_flow_json`, `preview_flow`, `publish_flow`, `deprecate_flow`, `delete_flow`) are hashed into idempotency fingerprints, ephemeral; with D135's audit actions, and the stored scope name `flows` (`wa_server_api_keys.scopes`), they are **pending the owner's confirmation before the first release**. Not offered, the library lacking them: `application_id` on an update, a Flow's `health_status`, its assets. Swap: the axum adapter's `api::flows` over the core's `flows::Flows`; to map Flows back to `templates`, `Operation::scope` (core `operation.rs`) and the router's `tenant(flows_routes(), Scope::Flows)` change together, and keys minted with `flows` are minted again. Decisive: `a_key_without_the_scope_is_forbidden` (a key with every other scope, `templates` included, is forbidden on the Flow routes; a `flows` key is forbidden on the template routes), `api::tests::each_tenant_route_is_one_core_operation_with_its_groups_scope` | M5 |
| D133 | The Flow private key (roadmap M5d) | the token vault (a per-tenant record) / a deployment key from the environment, behind a port / a key per number | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: one deployment key, behind the port `flows_endpoint::FlowKeys`.** `WA_SERVER_FLOWS_PRIVATE_KEY` (or `_FILE`): an unencrypted 2048-bit RSA key in PEM (PKCS#8 or PKCS#1), parsed at start (a key that is not one refuses the start, naming the variable and never quoting it; a password-protected PEM, which Meta's own `openssl genrsa -des3` makes, is refused); unset, the data endpoint is `404`. The key never reaches `Debug` (the library's `FlowEndpointKey` shows its size), a log, an error or the OpenAPI document. A per-tenant key in the vault was weighed: the vault's records are the library's (`wa.token`, pinned and bound to the WABA), a Flow key record is a new stored kind in the library, and Meta's guide asks a Solution Partner for "an encryption key pair for each Messaging account", so the swap is real: an implementation of `FlowKeys` reading a vault record, once the library has one (a new roadmap item). The public half is what each number's business public key is set to (`BusinessEncryption::set_public_key`, parity row 66, M5c3). A new setting: **pending the owner's confirmation before the first release**. Reviewed: the key is parsed at start and a value that is not one refuses the start (`ConfigError::Invalid`, which never quotes it); `Config`'s `Debug` omits it and the library's `FlowEndpointKey` shows its size only; the library scrubs its PEM and DER copies and the RSA operation is aws-lc's, constant time; the server's own copy of the PEM text (the environment or the `_FILE` read, as for every other secret of the service) is not wiped, and `_FILE`'s permissions are the deployer's (the service checks none, for this key as for the others). One deployment key means a request decrypts whatever WABA its path names: the `422` on `endpoint_uri` (D136's review) stops a Flow made through the service from naming another WABA's address, but a merchant managing its Flow on Meta directly can still point it there, so the handler must accept only a `flow_token` the app issued for that WABA's tenant (the guide says so); a key per tenant closes it in the service. Swap: a per-tenant vault record, an implementation of `FlowKeys` reading it (roadmap M5d2) | M5 |
| D134 | How the integrating app answers a Flow's data exchange (roadmap M5d) | a synchronous callback URL over the webhooks-out machinery / a pluggable trait / the app's own endpoint only | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: a pluggable trait, `flows_endpoint::FlowHandler`, with `NoHandler` in the binary.** (`exchange(FlowCall) -> FlowResponse`, `error_notified`). The service decrypts and seals; the app decides the next screen. A tenant that hosts its own endpoint sets its `endpoint_uri` to it and uses none of this. The binary plugs in `NoHandler`: a health check and an error notification are the service's own (so a published Flow's health check passes), and **every data exchange is `503` until an embedder supplies a handler** (the guide says so in as many words). A synchronous callback was weighed and left as the swap: the webhooks-out `Sender` returns a status only and is built for deliveries that retry for 72 hours, a Flow's answer is wanted in under a second (Meta's health page) and needs a body, a secret per tenant and a destination check that fails closed per request. It is one more implementation of the trait (a new roadmap item, the sender gaining an exchange that returns a bounded body). Nothing here is irreversible: no stored name. Swap: the tenant's callback URL called synchronously, one more implementation of `FlowHandler` (roadmap M5d2) | M5 |
| D135 | M5.2, idempotency and audit for Flows (roadmap M5d) | none (Meta's) / the Flow read first, compared with the path's WABA | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: the Flow read first, zero acting requests for another WABA's.** A Flow named by an id (in a path, or as a creation's `clone_flow_id`) is read first (`GET /{flow_id}`, the library's `Flow::get`) and acted on only when Meta answers that very id with the path's WABA as its `whatsapp_business_account` (D102's check, D118's shape): another WABA's Flow, a different id, a node with no WABA, or an id Meta refuses is `404 not_found` without Meta's code or text, the read the only request made (**zero acting requests**: no update, upload, preview, publish, deprecation or deletion is sent). The creation's idempotency fingerprint is scoped by the path's WABA and the body (`Fingerprint::create_flow`), and an audit event carries the action, the key, the tenant and the WABA, never a Flow's JSON. The creation takes an `Idempotency-Key` (D119's rule: the routes that create something); the changes that succeeded are the caller's tenant audit events: `flow_updated`, `flow_json_uploaded`, `flow_published`, `flow_deprecated`, `flow_deleted` (a creation is not audited, as a template's is not; D120). The names are **pending the owner's confirmation before the first release**. Cost: one more Graph read per Flow request. Swap: `Flows::owned_flow`, `Flows::audit`'s callers | M5 |
| D136 | The data endpoint's route, limits and answers (roadmap M5d) | a path per Flow / per tenant / per WABA; Meta's status for each refusal | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable: the route, with the limits as constants.** `POST /webhooks/flows/{waba_id}` on the public listener, per WABA because Meta's request carries no tenant or Flow id (the tenant is the WABA's binding's, as an event's is); a tenant's Flow cannot name another WABA's address as its endpoint: a creation or a change whose `endpoint_uri` has the path `…/webhooks/flows/<id>` (percent-decoded, any host) with an `<id>` other than the path's WABA is `422` on `endpoint_uri`, before any request (the review's finding: the signature binds the app, not the path, so Meta signs whatever a tenant's Flow points at; `a_flows_endpoint_may_not_be_another_wabas_data_endpoint_address`). A Flow can still be pointed at the address by other means (the Flow Builder, a hand-edited URL), so the app's `flow_token` check stays advised. The signature is Meta's `X-Hub-Signature-256` with the app secret (the active one and the previous while rotating: the same list `/webhooks/meta` verifies against), checked in constant time (the library's `verify_request_signature`, HMAC `verify_slice`) before any record, key or byte is read. Statuses: the library's `EndpointStatus` for a bad signature (`432`, Meta's getting-started samples) and a request that does not decrypt (`421`, Meta's guide), `404` for an unbound WABA, a suspended tenant or no key, `503` and `500` for the app, `413` over 256 KiB, `408` past 15 s, bare, with no body (Meta reads the status); the page of Meta's endpoint error codes (`flows/reference/error-codes`, read in review) lists `421`, `432` and `427` (the Flow token is no longer valid, with an error message the client shows): `427` is not offered, the handler having no way to ask for it (roadmap M5d2). The order is the signature first, so a forged request learns nothing of which WABAs are bound (`404` is for a signed request only), and every request that is signed but does not decrypt gets the one `421`, for the bad padding, the bad tag, the bad IV and the body that is not JSON alike. Limits: 256 KiB (enforced while the body streams, `413`), 32 in flight a replica (a place is held until the answer, released on every path), 15 s for the body (`408`), 5 s for the app: constants, with no setting. A new metric (`wa_server_flow_endpoint_requests_total{outcome}`) and a new public route: **pending the owner's confirmation before the first release**. Not done, decided in review: the request-IV replay guard the library suggests (`KvStore::put_if_absent` on the `initial_vector`). Meta's pages state no timestamp or validity window for a request, so a TTL equal to it does not exist; an unbounded record per exchange is a write on every Flow screen; and Meta's client re-sends after a `421` and may after a `5xx`, so a guard that refuses a repeated IV can turn a retry into a failed screen. What a replay needs is the signed bytes of a request (Meta's, over TLS), and what it gets is the same answer sealed again for the party that holds the request's key; the app's `flow_token` is the defence for the data. Swap: settings (the limits, a replay window) and `FlowsEndpoint`, the HTTP crate's `api::flows` (roadmap M5d2) | M5 |
| D140 | The key scopes of commerce, QR codes, analytics and the block list (roadmap M5g) | the existing scopes / one new scope / one per family | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** one new scope per family: `commerce` (settings and catalog reads), `qr_codes`, `analytics`, `contacts` (the block list). Blocking acts on a person and a QR code is public, so a key that only sends (a CMS's) must gain neither, and no key minted before M5g gains any (D50's reasoning). The stored names stay **pending the owner's confirmation before the first release** (a stable identifier). To swap, `Operation::scope` (core `operation.rs`) and the router's `tenant(…, Scope::…)` in `api/mod.rs`. Decisive: `each_family_takes_its_own_scope`, `api::tests::each_tenant_route_is_one_core_operation_with_its_groups_scope` | M5 |
| D141 | A catalog's ownership (roadmap M5g; the design's M5.2) | none (Meta's) / the catalog among the WABA's own `product_catalogs` / the catalog read by id and its owner compared | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** among the WABA's own `GET /{waba_id}/product_catalogs` (D102's check), asked before each read of a catalog or its products; not among them is `404 not_found`, without Meta's text, and the products edge is never asked for. One token may reach several tenants' WABAs, and a catalog read by id would answer another tenant's. A catalog's own read costs no second call (the list holds the same fields). Swap: `Catalogs::get`. Decisive: `a_catalog_is_the_wabas_or_it_does_not_exist` | M5 |
| D142 | The page sizes of catalog products and of the block list (roadmap M5g) | Meta's (none documented) / a bound of the service's | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** 1 to 100, `Meta's default` without a `limit`. Neither reference documents a maximum; an unbounded `limit` would let a caller ask for an answer of any size. Swap: `commerce::PRODUCTS_PAGE_MAX`, `blocked_users::LIST_PAGE_MAX` | M5 |
| D143 | Blocking and unblocking (roadmap M5g) | `DELETE` with a body, as Meta's page / a `POST` of the service's; audited by who / by count; Meta's texts answered / withheld | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** `POST …/blocked-users/unblock` (a body on a `DELETE` is dropped by proxies); each change an audit event by count (`users_blocked`, `users_unblocked`: people named, blocked, refused; outcome `answered`, or `unknown` for a timeout or Meta's `139100` with an error status, which does not say who), never by person; Meta's texts in no answer and no log (`failed_withholding_texts`, as OTP and registration), the answer carrying each refusal's code and its kind. The same withholding holds for a QR code's creation and change, whose Meta texts may quote the message. Swap: `blocked_users::BlockedUsers::failed`, `qr_codes::QrCodes::failed_with_message`. Decisive: `blocking_is_audited_by_count_and_never_by_person`, the M1.7 capture | M5 |
| D144 | A QR code's ownership (roadmap M5g; the design's M5.2) | none (Meta's) / looked for in the number's own list first / a store of the codes created through the service | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** looked for in the number's own list (`GET /{pn}/message_qrdls?code=…`, the filter the reference documents), then read, changed or deleted only when that list holds exactly it; another number's is `404`. **This costs one request to Meta on a refusal, not zero**: Meta's pages do not say a code is checked against the number in its path, and the service keeps no record of codes (a store would be a migration). Another tenant's number is refused with zero requests (M5.1). Swap: `QrCodes::owned`; the record of created codes is the zero-request alternative, a store of the codes created through the service, which is a migration (a new table, and the codes created before it unknown to it), so it is the owner's to order. Decisive: `a_qr_code_is_the_numbers_or_it_does_not_exist` | M5 |
| D145 | The bounds of analytics (roadmap M5g) | Meta's only / plus the service's | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** Meta's documented look-back (a year for messaging, conversation and pricing; 90 days for templates), a range of at most that, 1 to 10 template ids found among the WABA's own by D116's walk (else `404`, the analytics not asked for), and, the service's own, `HALF_HOUR` at most 31 days, lists at most 50, every filter one of Meta's values (phone numbers digits), since they are written into a field expansion. The clock is the authorizer's. Swap: `analytics::HALF_HOUR_MAX_DAYS`, `MAX_FILTER_ITEMS`. Decisive: `analytics_ranges_and_filters_are_bounded_before_any_request` | M5 |
| D210 | A call's ownership: how a call id is checked against the number's (roadmap M5e; the design's M5.2 and M5.1) | none (Meta's) / the call read first (no page documents a read of a call by its id) / the service's own records of the calls a number holds, read before the token is opened | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: records, read before the vault**, as a group's (D150). A call id is opaque and says nothing of the tenant that may act on it, one token may reach several tenants' numbers, and Meta documents no lookup, so the service keeps a record per (number, call): a typed store on the bundle's `KvStore` (namespace `wa.server.call`, key `<pn>.<SHA-256 of the pn and the call id>`, value the tenant and the number: the call's id is not stored), written by the webhook pipeline for every `calls` event (`call_updated`, `call_status_updated`) routed to the tenant owning the number, before the event is recorded, and by the connect route for the call it places. The `OwnedCall` extractor runs step 4 for the number (no token), looks the call up under the caller's tenant and the number, and only then opens the vault. Another tenant's call, another number's, an unrecorded or expired one and an id no call has are `404 not_found`, with the vault unread and Meta not asked. The swap: a Graph lookup, if Meta ever documents one, behind `CallRecords::owned` |
| D211 | A voicemail announcement's media id (roadmap M5e; the design's M5.2) | none (Meta's) / looked up as the number's own media first / a record of the uploads | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: looked up first.** The settings' `voicemail.audio.default.announcement_media_id` names a media object, and one token may reach several tenants' numbers, so a change that names one first asks `GET /{media_id}?phone_number_id={pn}` (the media API scopes it to the number); Meta refusing the id, or answering for another node, is `404 not_found` without Meta's code, and the settings are not changed. The library validates the rest of the object (call hours, SIP, voicemail) only when it sends, so a change that names a media id and is otherwise invalid has already made the lookup (a read) before its `422` Swap: the lookup in `Calls::update_settings` (`restrict_to_phone_number`), or a record of the uploads (a store, so a migration). |
| D212 | How long a call's record lives | never (as a group's) / a bound of the service's / until a terminate event | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: 24 hours since the call's last event** (`RECORD_TTL`), renewed by each event. A call lasts hours at most (Meta documents no longest), a terminate event does not forget it (a terminate action racing the remote hang-up is Meta's to refuse, not a `404`), and an expiry means no purge is ever needed (a group's records have none, M5f3). A call action on a call whose last event is older is `404` Swap: `calls::RECORD_TTL`, the one constant the records' expiry and every document's "24 hours" follow. |
| D213 | The bounds the service adds, and the secrets in a request | Meta's only / plus the service's | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: the SDP at most 32 KiB, not blank, no NUL** (half the 64 KiB request body limit; Meta documents none), `biz_opaque_callback_data` 512 characters (Meta's), and the rest the library's. An SDP offer or answer carries the endpoints' addresses and ICE credentials: it is passed through unparsed, in no log, error, audit event or `Debug` (the request types print nothing), in the idempotency record as a hash only (the body's fingerprint), and the connect route's stored answer holds the call id only Swap: `calls::SDP_MAX_BYTES`. |
| D214 | A SIP server's password (roadmap M5e) | answered on request (`include_sip_credentials`) / never asked or answered / a separate route | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: never asked, never answered.** The password authenticates the business to Meta (calling/call-settings, "Include SIP user password"); the read never sends `include_sip_credentials`, drops a password Meta sends anyway (the library's type has no serializer for it either), and the document's SIP server has no field for it. The cost: a tenant configuring SIP cannot read the password back through the service (it reads it in WhatsApp Manager, or a future route behind its own scope and audit event) Swap: the `settings(false)` in `Calls::settings` and `without_sip_passwords`; a route that reads the password is roadmap M5e2. |
| D215 | The key scope and the routes of calling (roadmap M5e) | the existing scopes / one new scope; call analytics under the calls scope or the analytics scope | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: one new scope, `calls`**, for the settings, the permission, the call a business places and the four actions (a call carries media negotiation and may be recorded, so `send` does not grant it), under the number (`/v1/numbers/{pn}/call-settings`, `…/call-permissions`, `…/calls`, `…/calls/{call_id}/{pre-accept,accept,reject,terminate}`); the call analytics beside the other analytics, `GET /v1/wabas/{waba_id}/analytics/calls`, in the scope `analytics` (a WABA read, with the WABA's own M5.1 check). The operation ids are the nine of `Operation` (`get_call_settings`, `update_call_settings`, `get_call_permissions`, `connect_call`, `pre_accept_call`, `accept_call`, `reject_call`, `terminate_call`, `get_call_analytics`); rate classes `send` for the writes and the actions, `read` for the reads Swap: `Operation::scope` and the `tenant(calls_routes(), Scope::Calls)` layer. The callee of the permission lookup and of a placed call is named by `phone` and/or `user_id`, as Meta takes them (`user_wa_id` or `recipient`; `to` or `recipient`): no record is keyed by either, and the guide recommends `user_id` (the BSUID) first, since a phone number may be absent (AGENTS.md § Meta's docs). |
| D216 | The calling routes' audit events and logs | none / the changes / every call; who is called, which call | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: the changes and the actions, never who or which.** Six audit actions (seven events, a terminate that may have gone through being `unknown`): `call_settings_updated` (the section names, never their values), `call_connect_requested`, `call_pre_accepted`, `call_accepted`, `call_rejected`, `call_terminated`, each with the tenant, the WABA, the number's id, the key's id, the outcome (`answered`, or `unknown` when the call may have gone through unanswered) and, for a call placed or accepted, whether recording and transcription were enabled. No callee, no call id, no SDP, no SIP host or password. A failed call answers Meta's code and the service's kind (Meta's texts are withheld from the answer and every log: a refusal of an SDP may quote one of its lines) Swap: `audit::calls_audit` and `CallFacts`. |
| D217 | Recording and transcription (roadmap M5e) | a deployment default / the caller's choice per call / forbidden | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: the caller's choice for each call, no default.** The `recording` and `transcription` objects are sent when the request carries them and never otherwise; `purpose` and `announcement_language` are the caller's. The consent Meta asks (an announcement to both participants; a participant who declines ends the call) is Meta's and the integrator's: the code decides nothing legal. The audit event records the enabling. No call accepts terms (calling/* have none) Swap: `calls::Announcement` and the request fields; a deployment default would be a setting, which none is today. |
| D218 | Idempotency and replays on the calling routes (section 5.4) | every POST / those that create something / none | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: the call a business places alone takes an `Idempotency-Key`** (a replay would ring the user twice; the library marks it non-idempotent and the service keeps a timed-out placement under its key as `504`, never repeating it). The four actions are state transitions on a call the service holds and are never replayed (a timeout is `504`, the next step is the caller's to read from the call's events); a settings change sets values and is replayed by the library on a timeout, as the other settings. Meta's calling error codes (138xxx) are not classified by the library (`502 unknown` with Meta's code) Swap: `Fingerprint::connect_call` and the route's `idempotent` call. |
| D219 | What the calling routes do not offer (roadmap M5e) | everything the library wraps / what the item names | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable: what the item names.** Not offered: a SIP password read (D214); the upload of a voicemail announcement with `use_case=call_voicemail_announcement` (the library and the media routes do not send it, so the announcement must be uploaded elsewhere); sending the call permission request (a message: a send); call buttons and deep links (a send / a template); the reference's `media_update` action; a typed transcript document. The recordings and transcripts download through the media route by the media id of the `call_recording_available` and `call_transcription_available` events: whether Meta serves them to `?phone_number_id={pn}` is not run against Meta Swap: roadmap M5e2 names each. |
| D220 | The key scope of the routing routes (roadmap M5l) | the existing scopes (`inbox`, `send`) / one new scope / one per route family | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable:** one new scope, `routing`, for the roles read and the three thread actions. A pass, a take and a release hand a customer's conversation to another responder (or back to nobody), which a key that only sends or reads the inbox (a CMS's) must not do, and no key minted before M5l gains it (D50's reasoning, D140's). The roles read shares it: it tells which actions are open, and a key that cannot act has no use for it. The stored name is **pending the owner's confirmation before the first release** (a stable identifier). Swap: `Operation::scope` (core `operation.rs`) and the router's `tenant(…, Scope::Routing)`. Decisive: `the_routes_take_their_own_scope` | M5 |
| D221 | The routing routes' audit events, logs and Meta's texts (roadmap M5l) | none / the three actions / also the roles read; who and the `metadata` / the number and the role only; Meta's texts answered / withheld | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable:** each action an audit event (`thread_passed`, `thread_taken`, `thread_released`: the key's id, tenant, WABA and number, and for a pass the target role; outcome `answered`, or `unknown` when the call may have gone through unanswered), the roles read none. **Never who**: no phone number, `wa_id`, BSUID or `metadata` in an event, a log line or an answer (`metadata` is free text for the new owner, which a customer's words may fill). Meta's texts are withheld from the three actions' answers and logs (`failed_withholding_texts`, as D143): the thread-control page's errors do not quote the user, but nothing promises it of a later one, and the answer carries Meta's code and the service's kind, which the guide maps. The roles read keeps `details` (it names no user; "Conversation routing is not enabled for this phone number" is what the operator needs). Names pending the owner's confirmation. Swap: `routing::Routing::failed`, `audit::routing_audit`. Decisive: `actions_are_audited_without_the_user_or_the_note`, `a_refusal_answers_metas_code_and_never_its_text`, the M1.7 capture | M5 |
| D222 | What a pass may name, and how a user is addressed (roadmap M5l; the design's M5.2) | an app id / a role of Meta's six / any string; the user by phone only / by BSUID / by exactly one of phone, `wa_id`, BSUID | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable:** **a role, never an app**: Meta's thread control names responders by role (`ai_agent`, `ctwa`, `customer_service`, `escalation`, `marketing`, `utility`), which the business that owns the number configures, so there is no app id in the API to check for digits or bounds, and a pass cannot name another tenant's app. `target_role` is a closed list of the page's six (`routing::TARGET_ROLES`), any other value (case included) `422 target_role` with nothing sent; a role Meta adds later is a service release. A missing `target_role` is Meta's untargeted pass (the escalation partner). The user is the thread's other party, no object of Meta's to compare with the number, so M5.2 has nothing further to look for beyond M5.1 (another tenant's number is `404` before the vault is read): addressed by exactly one of `phone` (E.164 with its `+`), `wa_id` (digits) and `user_id` (a standard BSUID; a parent BSUID and a group are refused), as the group participants are (D152), the BSUID being Meta's preferred identifier and the one the guide and the skill recommend first. **Nothing is stored keyed by the user** (nor by phone number): the only record a request leaves is the `Idempotency-Key`'s (the key, a SHA-256 over the operation, the number and the whole canonical body, and the answer, `request_id` only), and no log line, audit event or outbox event names the user. `metadata` at most 2,000 characters (Meta's bound, counted in characters). Swap: `routing::TARGET_ROLES`, `routing::user`. Decisive: `a_request_naming_no_one_or_something_meta_would_refuse_sends_nothing`, `a_pass_targets_one_of_the_six_roles_or_none` | M5 |
| D223 | `Idempotency-Key` on the thread actions (roadmap M5l; section 5.4) | none / every action / only the pass | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable:** all three, optional as everywhere. None is idempotent at Meta: a replayed pass or take is a second transfer, and a replayed release can release a thread a new inbound message routed back to this app in between (the library's module docs), so the library never retries one after a timeout and the service answers `504 timeout` with `may_have_been_sent`. A caller that cannot tell whether it went through reads the thread's state from the `messaging_handovers` events (or its own record) and decides; with a key, a retry of the same request answers the first answer (the stored `request_id`) and sends nothing, and the same key with another body, number or action is `422 idempotency_key_reused`. The fingerprint is the number and the canonical body per action, so a key is never shared between a pass and a release. Swap: `Fingerprint::pass_thread`, `take_thread`, `release_thread`. Decisive: `an_action_is_keyed_and_never_replayed_after_a_timeout`, `the_thread_action_fingerprints_are_per_number_body_and_operation` | M5 |
| D224 | The caller's own actions and the inbox's thread-owner record (roadmap M5l) | record each successful action in the inbox (`Inbox::record_release`, `record_thread_owner`) / leave it to the events and the caller | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable:** not recorded here; follow-up M5l2. Meta reports a caller's own pass, take and release to nobody (`control_passed` goes to the new owner, `control_taken` to the previous one, a release has no event), and the library says a caller that tracks ownership records its own calls. The record needs the conversation's key, which the inbox sink derives from what it has seen (a BSUID the phone number was linked to, `follow_phone`); the request names the user one way only, so a record under the wrong key would be silent and wrong. Until M5l2, after a pass the inbox may still read this app as the owner: a free-form reply is then sent and Meta refuses it (`131070`, `409 thread_owned_elsewhere`), and the thread is read right again by the next event or message. Swap: M5l2 (a key resolver shared with the sink). Decisive: none yet (the gap is stated in the guide) | M5 |
| D225 | The routes' shape and answers (roadmap M5l) | one route with `action` in the body (Meta's) / one route per action; Meta's answer / `request_id` only | **Coordinator's decision 2026-11-06 under the owner's delegation, swappable:** `GET /v1/numbers/{pn}/routing-roles` and `POST /v1/numbers/{pn}/thread-control/{pass,take,release}`, one route (so one operation, one scope check, one `Idempotency-Key` fingerprint and one OpenAPI body) per action; the body `{user: {phone|wa_id|user_id}, target_role?, metadata?}` (a pass), `{user, metadata?}` (a take), `{user}` (a release: the library sends no `metadata` with one, since Meta forwards it only on a handover event, and a key outside the body is `422`). The answer is `{request_id}`: `messaging_product` is always `whatsapp`. The roles' answer is `{roles, is_standby}` with Meta's names, unordered. **No call here accepts terms:** thread control needs the business to have accepted Meta's Business Agents and Platform Terms of Service in Meta Business Suite, which only it can do; the service neither offers nor performs the act, and Meta's refusal for a business that has not arrives as its code. Names pending the owner's confirmation: the paths, the four operation ids (`get_routing_roles`, `pass_thread`, `take_thread`, `release_thread`), the three audit actions and the schema names. Swap: the route table in `api/mod.rs` | M5 |
| D250 | Which partner calls are legal or irreversible acts (roadmap M5j; AGENTS.md, Legal acts; D192) | none / those the pages tie to terms or an irreversible effect / every state change | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). Seven, on the evidence of Meta's pages (fetched 2026-11-06): a business verification submission (a statement of a customer business's legal identity with its documents, three per customer); accepting a solution (the guide: "solutions cannot be declined once they have been accepted"); rejecting one (the reject reference: "permanent and cannot be undone through this API"); accepting a deactivation request (the solution becomes `DEACTIVATED`; payments must be settled); a solution migration intent (asks a client to move its WABA to another partner; unanswered for 90 days it is accepted for the client); a billing migration's start (clones the WABA) and its resumption (switches billing; the old WABA can no longer send). None of the pages accepts terms or signs an agreement (the guide leaves contracts to the partners), so the gate rests on irreversibility and on the statement, which AGENTS.md reserves; when unsure, gated. Not gated: creating a solution (it binds nobody until accepted), asking for a deactivation (the solution stays usable), rejecting a deactivation request, every read. Off (the default of `WA_SERVER_LEGAL_ACTS`): `403 feature_not_available` before any request to Meta; on: audited. The three of a tenant's WABA are arms of `number_settings::is_legal_act(Operation)`; the four of the operator are no tenant operation, so they are arms of `number_settings::is_legal_admin_act(AdminOperation)` (`SubmitBusinessVerification`, `AcceptPartnerSolution`, `RejectPartnerSolution`, `AcceptPartnerSolutionDeactivation`), the one admin predicate it shares with M5h's max-price agreement (D231). The gate is also an extractor before the body (`LegalActsOn`), so a body-carrying act with the setting off is `403` whatever the body is, never `422`. Swap: one arm each. Decisive (added): `the_gate_is_answered_before_the_body_is_read`, `authorization_is_answered_before_the_gate`. Decisive: `a_legal_act_is_refused_before_any_request_while_the_setting_is_off`, `only_the_declaration_and_the_partner_acts_are_legal_acts`, `four_of_the_operators_actions_are_legal_acts`, `the_admin_legal_acts_are_the_agreement_and_the_partner_acts_and_no_marketing_operation_is_one` | M5 |
| D251 | Who calls what (roadmap M5j) | all admin / all tenant / per call | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). Per call. The operator's (admin key) are what the deployment does as a partner with its own credentials: verification submissions and listing (the partner's system user token, the partner's portfolio in the path), Multi-Partner Solutions (the deployment's app owns or is invited to them) and a system user's assigned WABAs; they reuse the pool's business and system token (D204), so no new setting, and answer `409 signup_not_configured` without them. A tenant's (scope `partners`, the WABA's own token) are what Meta documents with the client's business token: the migration intent and its read, the billing migration (the page asks for the Messaging account's token), and the WABA's verification status (the WABA's `business_verification_status`, the page's alternative to reading the customer's portfolio with its token, which the service has no business id for). Swap: a route's guard and the service it opens | M5 |
| D252 | Business verification documents (roadmap M5j) | multipart / base64 in JSON / a URL the service fetches | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). Base64 in JSON (`content_base64`), up to three, so that the routes stay JSON and typed (the media routes' multipart is a different family); bodies up to 21 MiB on these routes only. The library's checks run before any request (type by extension and by content, 5 MiB each, one to three). A document, its name and Meta's text are in no log, error or audit event (the audit event carries the customer's business id and a count); Meta's texts are withheld on the operator's calls. Swap: the request type and `submit_business_verification` | M5 |
| D253 | The solution token (roadmap M5j) | answered / stored in the vault for a tenant / not served | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). Not served. It is a bearer credential for a client's assets, and the brief's rule is that tokens are in no answer, log, `Debug`, error or audit event. The one use the service has for a client's business token through a solution is hosted Embedded Signup (D201), which already stores it. A route that stored a token for a tenant would be a second onboarding path with its own binding rules. Row 144 of the parity table stays done on that basis, as M5e's never-served SIP password did. Decisive: `the_solution_token_is_not_served` | M5 |
| D254 | Moving numbers and WABAs between partners (roadmap M5j) | all the migration pages / the intent and its read / nothing | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). The solution migration intent and its read. Meta's number migration page says it is not supported on the new account model (the request is rejected), and the rest of the migration pages are Meta Business Suite steps, so no number migration route; the library's `migrate_phone_number` flag stays the library's. The intent read asks Meta with the WABA's token for the fields that prove whose it is (`destination_waba`, `waba`) and answers only an intent that names the path's WABA, else `404`; it cannot be decided before Meta is asked (unlike D210), which is the cost. Swap: `solution_migration_intent` | M5 |
| D255 | Idempotency on the partner routes (roadmap M5j; section 5.4) | every POST / the creating tenant POSTs / none | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). The two creating tenant routes (the solution migration intent, the billing migration's start) take an `Idempotency-Key`, scoped to the tenant, and are never repeated after a timeout (the library's `idempotent(false)`); the resumption is not keyed (it creates nothing; Meta's troubleshooting table lets it be called again on `FAILED_TO_COMPLETE`, which the caller reads first). The operator's changes are admin routes, which carry no `Idempotency-Key` (section 5.4); each is never replayed by the library, and its `504` says it may have been sent. A keyed run for admin routes is accepted for now: a business verification submission is capped at three per customer, so a client's retry after a `504` can burn one. Swap: roadmap M5j2, which adds an `Idempotency-Key` to the operator's creating POSTs, the verification submission and the solution creation first; the tenant's fingerprints are in `idempotency.rs` | M5 |
| D256 | A billing migration's owner (roadmap M5j; the design's M5.2) | ask Meta and trust its token / a record of the migrations started / read the clone's parent | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). A record in the bundle's `KvStore`, namespace `wa.server.billing_migration` (a new stored name, pending the owner's confirmation before the first release; docs/architecture.md): Meta's migration id to the tenant and the WABA, written when the start is answered, never expiring, read before the status or the resumption asks Meta; any other WABA's, tenant's or unknown id is `404` with Meta not asked. Meta's status answer names the clone, not the source, so it cannot prove ownership. A write that fails after Meta answered is reported as `may_have_been_sent`, so a retry reads first. Decisive: `a_billing_migration_is_the_wabas_own_to_read_and_to_resume`, `the_billing_namespace_is_pinned` | M5 |
| D257 | The scope, operations, audit actions and logs of the partner routes (roadmap M5j) | the existing scopes / one new scope | Coordinator's decision 2026-11-06 under the owner's delegation, swappable (roadmap M5j). One new scope, `partners`, for the six tenant operations (`get_business_verification`, `create_solution_migration_intent`, `get_solution_migration_intent`, `create_billing_migration`, `get_billing_migration`, `resume_billing_migration`; rate classes `read` and `send`): a key that manages numbers must not move billing or partner (D50's reasoning). Audit actions: `business_verification_submitted`, `partner_solution_created`, `partner_solution_accepted`, `partner_solution_rejected`, `partner_solution_deactivation_requested`, `partner_solution_deactivation_accepted`, `partner_solution_deactivation_rejected`, `solution_migration_intent_created`, `billing_migration_started`, `billing_migration_resumed`, each with ids and a count or an `outcome` (`answered`, or `unknown` when it may have gone through unanswered), never a document, a name, a token or Meta's text. All of them are pending the owner's confirmation before the first release. Decisive: `check_partner_audit` and the capture's `partners` section in `tests/logs.rs` | M5 |
| D180 | When a stored token is told, and the lead time (roadmap M3e; OPEN_QUESTIONS #8) | every state of the library's `TokenExpiry` / only `ExpiresSoon` (due within the lead, not yet expired) / also `Expired`; a lead per tenant / one setting | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** only `ExpiresSoon`; one setting, `WA_SERVER_TOKEN_EXPIRY_LEAD`, `7d` by default (the library's `DEFAULT_EXPIRY_LEAD`), `off` to turn the task off, at most `WA_SERVER_OUTBOX_RETENTION` (an explicit lead past it is refused at start; unset, the default yields to a shorter retention, so a deployment that set the retention below a week before the setting existed still starts). A token found already expired is not told: nothing after the lapse would stop the same event from recurring once its outbox row is purged, and `409 reconnect_required` already says it. Swap: the setting; `token_expiry::lead_fits_retention`; `DEFAULT_LEAD`. The setting's name and the default are pending the owner's confirmation before the first release. Decisive: `a_token_inside_the_lead_is_told_to_its_tenant_once` | M3 |
| D181 | How "once per token, per expiry" is kept | a marker in the `KvStore` beside the vault / the outbox's idempotency key / a column on the binding (a migration) | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** the outbox key, SHA-256 over a label of its own (`meta-whatsapp-server/token-expiry-event/v1`), the type, the WABA, its tenant and the expiry in Unix seconds. One store and one atomic insert: a marker written before the event can lose it (a crash between), one written after can repeat it, and a column is a migration, which is the owner's. A refreshed token has a new expiry and so a new key; the same token attached again to the same tenant (a new binding generation) has the same one. The cost is that the key lives as long as the outbox row, hence D180's bound. At the bound (lead equal to retention, which is the default pair) the row, made when a round first meets the token (within the lead, at most a round late), outlives the lapse by that lateness only: a replica whose clock runs behind the database's by more than that could, in principle, tell the token once more after the purge; one more identical event, never a lost one. Stored (`wa_server_events.dedup_key`): the label is pending the owner's confirmation. Swap: `TokenExpiring::outbox_key`. Decisive: `the_key_is_pinned`, `a_token_stored_again_is_told_again_and_one_expired_is_not` | M3 |
| D182 | The event's type, data and audience | `token_expiring` or `token_expires_soon` or `token_expired`; `{event, waba_id, expires_at}` / with the time left, the numbers, the tenant; the WABA's tenant by the WABA's binding / by each number | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** `token_expiring`; `{event, waba_id, expires_at}` (RFC 3339, UTC, to the second): ids and the expiry only, no token, no number, no time left (the receiver computes it); the WABA's tenant by the outbox's own check on the WABA's binding, dated by the binding as D82's are. The token belongs to the WABA, so one event per token, not per number. In `TENANT_EVENT_TYPES` (so `KnownEventType` and the `types` filter, the polls, the streams and webhooks out). The type's name is pending the owner's confirmation before the first release. Swap: `TokenExpiring::{data, row}`. Decisive: `the_events_data_is_ids_and_the_expiry_only`, the conformance case | M3 |
| D183 | Where the check runs, and its metric | inside housekeeping's turn / a task of its own / a library `Janitor` step; the metric as a counter by outcome / a gauge of tokens due | **Coordinator's decision 2026-11-05 under the owner's delegation, swappable:** a task of its own (`token_expiry::run`, spawned by `serve` unless `off`) with a `LeaderLock` turn named `token_expiry` every 10 minutes: housekeeping purges, and a walk of every binding's vault record is not a purge, and its setting is its own. A counter `wa_server_token_expiry_events_total{outcome}`, `recorded` (an event recorded, once) or `failed` (a record unreadable, or the outbox failing; retried next round): never labelled by an id, a tenant or a token. A gauge of tokens due would repeat each round. The metric's name is pending the owner's confirmation before the first release. Swap: `Observer::token_expiry`. Decisive: the HTTP case reads `/metrics` | M3 |

**Decided with roadmap M5d (D132–D136)**, by the coordinator on
2026-11-05 under the owner's delegation (AGENTS.md § Decisions), after the
implementer's draft and an adversarial review: each ships swappable as its
row says. One new setting (`WA_SERVER_FLOWS_PRIVATE_KEY`), one new scope
(`flows`), one public route, one metric, nine operation ids and five audit
actions, all **pending the owner's confirmation before the first
release**; no error code and no new stored name in a store besides the
scope. No route takes a legal
act: publishing a Flow is the tenant's, a Flow's data endpoint answers what
the integrating app decides, and the deployment's key is the deployer's
own.

**Decided with roadmap M5b (D115–D122)**, by the coordinator on
2026-11-05 under the owner's delegation (AGENTS.md § Decisions): each
ships swappable as its row says. No new setting, error code or stored name in
a store; the new operation ids and audit actions are pending the owner's
confirmation before the first release. No route takes a legal act:
archiving is the tenant's, undone by an unarchive within 28 days (then
Meta deletes the template, as it does an approved template's deletion
today), and a zero-tap acceptance is refused on every route that could
carry one (roadmap M3g stays the owner's).

**Decided with roadmap M5a (D100–D104)**, by the coordinator on
2026-11-02 under the owner's delegation (AGENTS.md § Decisions), as its
implementer proposed them: each ships swappable as its row says.
No new stored name, setting or error code.
No type of M5a takes a legal act: Direct Send's eligibility is
WhatsApp Manager's, a Flow needs a verified business, calling and
payments are enabled outside these sends, and no request accepts terms.

**Decided with roadmap M2e (D77–D79)**, by the coordinator on
2026-11-02 under the owner's delegation, as its implementer proposed
them: each ships swappable as its row says. Migration 7's names
(`wa_server_events.media_id`, `wa_server_events_media_idx`, the
description `received media`) are stable identifiers, pending the
owner's confirmation before the first release
([architecture.md](../architecture.md#stable-identifiers)). The live
check of which behaviour Meta has (OQ #43) is still to be run.

**Decided with roadmap M2f (D86–D91)**, by the coordinator on
2026-11-02 under the owner's delegation: D86, D87, D88 and D91 as their
implementer proposed them, D89 and D90 changed to narrow by default (an
erasure reaches the calling tenant's own data only: its own outbox rows,
and the inbox only on a number with no history cut); each ships
swappable as its row says. The wider reach of D89 and D90 (the number's
operator-only rows, a previous holder's hidden records) is an
irreversible deletion beyond the caller's own data, so it stays the
owner's open choice ([roadmap § Owner
touchpoints](../roadmap.md#owner-touchpoints)). Migration 8's names and
the erasure's stored names are stable identifiers, pending the owner's
confirmation before the first release
([architecture.md](../architecture.md#stable-identifiers)).

**Proposed with roadmap M5c1 (D125–D131)**, pending the coordinator's
decision under the delegation (AGENTS.md § Decisions): each ships
swappable as its row says. Pending the owner's confirmation before the
first release (architecture.md § Stable identifiers): the `KvStore`
namespace `wa.server.upload`, the operation ids `start_media_upload`,
`upload_media_chunk` and `get_media_upload`, the error codes
`resumable_upload_not_configured` and `media_conversion_failed`, the
path `/v1/numbers/{pn}/media-uploads`, the form field `convert` and its
five values, the settings `WA_SERVER_MEDIA_CONVERTER` and
`WA_SERVER_MEDIA_CONVERTER_PROGRAM`, and the feature `ffmpeg`. No legal
act: opening a session and converting a file accept no terms.

**Decided with roadmap M5f (D150–D156)**, by the coordinator on
2026-11-05 under the owner's delegation, after a security review of the
implementer's proposals (adjusted where its rows say): each ships
swappable as its row says. Pending the owner's confirmation before the
first release (architecture.md § Stable identifiers): the `KvStore`
namespace `wa.server.group`, the scope `groups`, the thirteen operation
ids of D151, the seven audit action names of D155 and the paths under
`/v1/numbers/{pn}/groups`. No new error code or setting; no legal act:
creating, changing and deleting a group and managing its participants
accept no terms.

**Decided with roadmap M5e (D210–D219)**, by the coordinator on
2026-11-06 under the owner's delegation (AGENTS.md § Decisions), after the
implementer's draft and an adversarial review: each ships swappable as its
row says. Pending the owner's confirmation before the first release
(architecture.md § Stable identifiers): the `KvStore` namespace
`wa.server.call`, the scope `calls`, the nine operation ids of D215, the six
audit action names of D216 and the paths under `/v1/numbers/{pn}/call-settings`,
`…/call-permissions` and `…/calls`. No new error code or setting. **Legal
acts: none** in the code: no calling page makes the caller accept terms, and
recording and transcription are per-call choices of the caller (D217); the
consent Meta asks of the participants is not decided here.

**Decided with roadmap M5c2 (D160–D166)**, by the coordinator on
2026-11-05 under the owner's delegation (AGENTS.md § Decisions), after the
implementer's draft and an adversarial review (which added the bidi and
zero-width refusal and the profile's text, email and website bounds to
D165): each ships swappable as its row says. The profile picture is named
by its upload, not its handle (D160); no new scope (D161), the display name's two routes (D162), its optional
`Idempotency-Key` (D163), its audit event (D164), the bounds (D165) and
what is not offered (D166). Pending the owner's confirmation before the
first release (architecture.md § Stable identifiers): the operation ids
`request_display_name_change` and `get_display_name`, the path
`/v1/numbers/{pn}/display-name`, the field `profile_picture_upload_id`
and the audit action `display_name_change_requested`. No new scope, error
code, setting or stored name (the sessions' namespace is M5c1's); no legal
act: asking Meta to review a display name accepts no terms.

**Decided with roadmap M5c3 (D190–D193)**, by the coordinator on
2026-11-06 under the delegation (AGENTS.md § Decisions): each ships
swappable as its row says: the settings routes' shape (D190), payload
encryption's keys (D191), the legal act and `WA_SERVER_LEGAL_ACTS`
(D192, changed from the proposal: one legal act, not three) and the
bounds (D193). Pending the owner's
confirmation before the first release: the setting
`WA_SERVER_LEGAL_ACTS`, the operation ids of D190, the paths and the
audit actions `number_setting_changed`, `payload_encryption_changed`,
`username_set`, `username_deleted`, `official_business_account_requested`,
`compliance_info_set`, `conversational_components_set` and
`contact_book_entry_deleted`. No new scope, error code or stored name.

**Inherited from [OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md).** The
questions the service inherits were decided on 2026-09-26 under the same
delegation, each with its roadmap item: #5 (multi-WABA signups: opt-in,
L11b and M3f), #8 (no token refresh: the service reports
`reconnect_required`, and the expiry is surfaced before it lapses, L11e
and M3e), #9 (vault key custody and cadence: rotation is one call, the
cadence documented with M3a), #13 (the OTP issue limit, kept; M3c
keeps it on, never settable off over the API), #26 (the
In-App Signup terms: an explicit, audited operator action, M5i), #30
(dead-lettering a permanently failing event, L21a), #32 (calls reopen
the inbox's window, L5 and L7), #45 (the max-price agreement: an
explicit, audited operator action, M5h). Until
an item lands, the service keeps the library's behaviour and makes it
visible to callers. #33 (message ids unique per store) stays open: its
alternative is a primary-key migration of stored messages, which is the
owner's; L5 and L8 are shaped to work with either answer.

**Proposed with roadmap M3a (D70–D76)**, pending the coordinator's
decision under the delegation (AGENTS.md § Decisions): each ships
swappable as its row says; D70's and D71's stored names are the owner's
to confirm before the first release (architecture.md § Stable
identifiers). No step of Embedded Signup accepts terms or signs anything
(`embedded-signup/*`, `solution-providers/share-and-revoke-credit-lines`):
no legal act is taken.

**Decided with roadmap M5h (D230–D235)**, by the coordinator on
2026-11-06 under the delegation (AGENTS.md § Decisions): each ships
swappable as its row says. One new scope
(`marketing`), one admin route and fourteen tenant operations on eight
paths, seven tenant audit actions and one admin one
(`max_price_agreement_signed`), all **pending the owner's confirmation
before the first release**; no new error code and no new setting (the
agreement reuses `WA_SERVER_LEGAL_ACTS`). One legal act is taken, the
max price beta agreement, only by an admin key, only with the setting
on, and by no other route (M5.3); accepting the Marketing Messages API's
terms and the other calls that touch a business's terms are not routed.

**Decided with roadmap M3f (D200–D205)**, by the coordinator on
2026-11-06 under the delegation (AGENTS.md § Decisions; D202 changed in
review, from a tenant's claim to the operator's): each ships swappable as
its row says, off until its setting is turned on. The new settings
(`WA_SIGNUP_EVERY_GRANTED_WABA`, `WA_HOSTED_SIGNUP`, `WA_SYSTEM_TOKEN`,
`WA_PARTNER_BUSINESS_ID`), stored names (`wa.es.hosted`,
`wa.es.preverified`), the route `POST /v1/admin/signup/hosted` and the
audit actions (`hosted_signup_arrived`, `hosted_signup_completed`,
`pre_verified_number_added`, `pre_verified_code_requested`,
`pre_verified_code_verified`, `pre_verified_number_deleted`,
`pre_verified_number_reserved`, `pre_verified_number_released`) are pending
the owner's confirmation before the first release. No legal act is taken: no hosted or pre-verified endpoint accepts
terms or signs anything (`embedded-signup/hosted-es`,
`embedded-signup/pre-verified-numbers`).

**Decided with roadmap M2b (D39–D42)**, by the coordinator on
2026-11-01 under the delegation (AGENTS.md § Decisions): each ships
swappable as its row says. D40's channel and payload are a contract
between replicas of two releases during a rolling deploy, so their
names stay stable identifiers, pending the owner's confirmation before
the first release
([architecture.md](../architecture.md#stable-identifiers)). D43 came
out of M2b's sabotage review and was decided the same day, as the
review proposed it.
