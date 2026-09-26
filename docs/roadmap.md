# Roadmap to parity

The waves that take meta-whatsapp-rs to parity as the owner defined it on
2026-09-26 ([parity.md](parity.md): Meta's Cloud API, plus Zaileys'
framework features). Every partial or gap row of the parity table is in
an item below. Written against `main` at b6fc893 (PR #20).

- Each item is one pull request (a family of routes may take one per
  family, as noted), names its crate, and names its **decisive test**:
  the guard whose removal must make a test fail, as in
  [design §9](design/server.md#9-delivery-plan). A PR is done when its
  decisive test holds, `just ci` exits 0 on its final head, and it has
  updated its rows in [parity.md](parity.md),
  [categories.md](categories.md) and [coverage.md](coverage.md), the
  docs and the skills (AGENTS.md § Companion docs and skills).
- Items keep the design's names where they exist (L4, L5, M2–M4) and
  add S (the service's modular split), U (CrateStack, upstream), B (the
  bot framework), L7 onwards (library batches), M5 (service routes) and
  P (payments).
- The choices behind the items are recorded in
  [design §10](design/server.md#10-decisions-for-the-owner) (D26–D29
  and the rows updated on 2026-09-26) and in
  [OPEN_QUESTIONS.md](../OPEN_QUESTIONS.md). Each is the coordinator's
  under the owner's delegation, swappable; the ones still reserved for
  the owner are named where an item meets them.

## Order

| Wave | Items (in parallel within a wave, at most three building at once) |
| --- | --- |
| 0 | this plan (parity, categories, roadmap, decisions) |
| 1 | S1 (server core), B1 (bot framework) |
| 2 | S2, S3, S4; U1–U3 in the cratestack repository; library batches L7–L23 may start here and run alongside every later wave |
| 3 | S5–S9 (CrateStack adoption, the default flip last, gated on U1); M2 |
| 4 | M3; B2, B3; L5, then B4 |
| 5 | M4; M5 route families |
| 6 | S10–S12 (MongoDB) |
| last | P1, P2 (payments) |

## 1. The service's modular split (design §8, D26)

From the CrateStack fit study of 2026-09-26: the unit of swapping is a
backend bundle and an API adapter over one framework-free core (parity
row 112, and the ground every service milestone builds on). `/v1`
stays served, and `openapi/v1.json` stays byte-identical, until the
default flips (S9).

- [ ] **S1. Extract `meta-whatsapp-server-core`** (new crate, no axum,
  http, utoipa, sqlx or CrateStack). The model, keys, event routing and
  type lists, outbox keys and ids, polling, the idempotency engine, the
  rate limiter, the §5 error model as data, authorization as services;
  the ports `RecordStore`, `IdempotencyRecords`, `Outbox`, `LeaderLock`,
  `Janitor`, `SchemaMigrator` and the `Backend` bundle (replacing
  `Backends.pool` as the "memory?" flag). Implementations stay in the
  server crate. Companion: architecture.md § Service (the server family
  depends on the facade and on each other; the library never on them).
  *Decisive:* `cmp` of `openapi/v1.json` against `main` is equal; every
  server test passes with only its imports changed; `cargo tree -p
  meta-whatsapp-server-core` shows no axum, http, utoipa or sqlx.
- [ ] **S2. Conformance into core** (`meta_whatsapp_server_core::conformance`,
  a feature, like the library's `store::conformance`): the store and
  events suites, plus the cross-port invariants (deleting a tenant
  purges its stream; a binding changed during an insert leaves the
  event operator-only; per-tenant commit order). The memory backend
  implements the binding re-check. *Decisive:* removing the memory
  backend's re-check fails the binding invariant; removing the stream
  purge from `delete_tenant` fails on memory and, live, on Postgres.
- [ ] **S3. The crate split**: `…-store-memory`, `…-store-postgres`,
  `…-server-http` (listeners, auth before the body, the §5 renderer,
  `/webhooks/meta`, media, ops) and `…-api-axum` (today's `/v1` routes
  and `openapi/v1.json`); the binary composes them by Cargo features.
  *Decisive:* `openapi/v1.json` byte-identical; each store crate runs
  the core conformance suite (live for Postgres, with
  `META_WHATSAPP_RS_REQUIRE_LIVE=1`); the binary builds with only
  `api-axum` and `store-memory` enabled.
- [ ] **S4. The idempotency fingerprint is the operation's** (D27,
  `…-server-core`): operation id plus the canonical input, not the
  request path, so a key means the same through every adapter. A
  pre-release change of `/v1`'s semantics, noted in the guide.
  *Decisive:* two spellings of one operation's path (percent-encoded or
  not) replay the same key; putting `uri.path()` back into the
  fingerprint fails that test.
- [ ] **U1. Domain errors in CrateStack** (D16; the owner's cratestack
  repository, a PR there with its own parity declaration): a domain
  error carrying our code, status and body, statuses 410, 413, 502 and
  504 included. *Decisive:* a procedure returning a domain error answers
  the §5 body byte for byte; dropping the status passthrough fails it.
- [ ] **U2. A `tls-aws-lc-rs` feature on `cratestack-sqlx`** (D20,
  upstream). *Decisive:* with the feature on, `cargo tree -e features`
  of a test crate shows no `ring`.
- [ ] **U3. Relax CrateStack's `sqlx = "=0.9.0"` pin to `~0.9`** (D20,
  upstream). *Decisive:* a test workspace resolves a newer 0.9 patch
  release beside CrateStack.
- [ ] **S5. CrateStack enters the workspace** (D20): the `deny.toml`
  licence exception for BlueOak-1.0.0 scoped to `minicbor` and
  `minicbor-serde` (the owner can refuse it at this step: the swap is a
  separate workspace for the CrateStack crates), aws-lc-rs installed as
  rustls's process default at start, a minimal `…-api-cratestack` with
  one procedure, not the default. *Decisive:* `just deny` passes and
  fails again when the exception is removed; a live Postgres test with
  `sslmode=require` passes in a binary that links both providers, and
  fails when the process default is not installed.
- [ ] **S6. `…-api-cratestack` parity** (one PR per resource group:
  numbers, messages and media, templates, events, admin): `api.cstack`
  with `db = None`, REST (D19), procedures calling core. *Decisive:*
  the authorization (M1.3), error (M1.5) and idempotency (M1.4) suites
  run against both adapters through an enumerator over each adapter's
  contract; a procedure left out of the enumerator fails it.
- [ ] **S7. `…-store-cratestack`, the hybrid** (D15): `store.cstack`
  models (`db = Postgres`, `@@internal`) for tenants, API keys, WABAs
  and numbers; the outbox, idempotency, locks, janitor, migrations and
  the library's Postgres stores on the same pool, reusing
  store-postgres's SQL. *Decisive:* the core conformance suite passes
  live; a model field renamed away from our migrations' column fails
  the live drift test.
- [ ] **S8. The TypeScript client from CrateStack** (`clients/typescript`,
  D18, D19, D11): generated by `@cratestack/cli` pinned to the crate's
  version, JSON on the wire (not CBOR), the hand-written layer (keys,
  `WA-Tenant`, `Idempotency-Key`, the §5 error, a resuming SSE reader,
  `verifyWebhook()`), a drift gate, and the npm publish workflow
  prepared (publishing needs the owner's token). *Decisive:* editing
  `api.cstack` without regenerating fails the gate; a request the
  client builds carries `content-type: application/json`.
- [ ] **S9. The default flip** (gated on U1): `api-cratestack` and
  `store-cratestack` become the binary's default features; the server
  skills and `just skills-ts` move to the generated client; the
  server-skill checks move out of `crates/meta-whatsapp-rs/tests/skills.rs`;
  `openapi/v1.json` stays with `…-api-axum`. *Decisive:* `just ci` with
  default features; the errors suite fails if the CrateStack adapter
  answers an error in CrateStack's own shape.
- [ ] **S10. MongoDB library adapters** (`meta-whatsapp-adapters`,
  feature `mongodb`): `KvStore` and `ConversationStore`. *Decisive:*
  both library conformance suites pass live against a replica set;
  removing the version check from compare-and-swap fails them.
- [ ] **S11. An `Outbox` spike on MongoDB** against the core
  conformance suite, before the port shapes freeze (per-tenant commit
  order, the binding re-check). *Decisive:* the cross-port invariants
  pass, or the spike's report says which port must change.
- [ ] **S12. `…-store-mongodb`**. *Decisive:* the core conformance
  suite passes live; the service's M1–M2 acceptance tests pass on it.

## 2. The bot framework (`meta-whatsapp-bot`, D28, D29)

A new library crate on core, client and webhooks (never adapters or the
facade), re-exported by the facade behind a feature. A `Bot` is an
`EventSink`, so it plugs into `WebhookHandler` like any sink.

- [ ] **B1. Commands, middleware, compile-time plugins, markdown
  replies** (rows 17, 85–88): a command parser (prefixes, aliases,
  quoted arguments, button and list payloads), guards (private or group
  only, owner and banned lists behind a trait), per-user cooldowns as a
  typed store on `KvStore`, generated help, an optional sync to Meta's
  command menu; `Middleware` with `next`; a `Plugin` trait with setup
  and unload hooks, registered at compile time (no hot reload, D28);
  `markdown::render` to WhatsApp formatting, split at 4096 characters on
  paragraph boundaries. Companion: `docs/guides/bots.md`, the skill
  `meta-whatsapp-rs-bot`. *Decisive:* removing the cooldown check fails
  a test; removing the banned-list guard fails a test; a middleware that
  does not call `next` stops the handler; a 5000-character text splits
  into exactly two messages at a paragraph boundary; a sender with no
  `wa_id` (BSUID only) is served.
- [ ] **B2. Paced broadcast** (rows 89, 92; D29): a per-number rate
  under Meta's throughput (80 messages a second by default), progress,
  retries only when `Error::may_have_been_sent` is false, the pair and
  per-user marketing limits read from `ErrorKind`; typing indicators and
  group operations throttled through the same pacer. *Decisive:* under a
  fake clock, 200 sends at a configured 20 a second never exceed it; a
  scripted timeout is never retried (removing the
  `may_have_been_sent` check fails the test).
- [ ] **B3. Durable scheduling** (row 90; D29): jobs as a typed store on
  `KvStore` (a bucketed due-time index, claims by compare-and-swap with
  a lease; no new port), send at a time, cancel, retry, survive a
  restart. *Decisive:* two runners on one store send a due job once
  (replacing the compare-and-swap by a plain put fails it); a job
  scheduled before a restart is sent by a new runner on the same store.
- [ ] **B4. Retention and auto-delete** (row 91; after L5): by age and a
  cap per chat, through `ConversationStore`'s erasure. *Decisive:*
  removing the age filter fails the test; the erased messages are gone
  from `messages` and `conversations` on memory and, live, on Postgres.

## 3. Library gap batches

By crate and topic; each is one PR through the review pipeline, with
`ScriptedTransport` tests asserting method, path, query, token and exact
JSON from Meta's pages. A batch whose pages are not in the mirror
(parity.md's (nm)) starts by reading them.

- [ ] **L4. A code-less `OnboardingRequest` for `resume`** (OPEN_QUESTIONS
  #10; `client::embedded_signup`). *Decisive:* a new process resumes
  from stored session info alone; sending a placeholder code to Meta
  fails the test's request assertion.
- [ ] **L5. `ConversationStore` erasure and retention** (D10; core and
  adapters; a port change): erase one contact on one number, purge by
  age, retention configured per store. *Decisive:* new conformance
  cases; memory and Postgres pass; a Postgres erase that leaves the
  message bodies fails live.
- [ ] **L7. Window events and thread ownership** (OPEN_QUESTIONS #32,
  #44; parity rows 119, 139; core, adapters, `inbox`): calls that reopen
  the 24-hour window and standby messages recorded as window events
  (never unread), ownership tracked from the handovers, a caller's
  explicit override of the local check; lands next to L5 so adapters
  change once. *Decisive:* after a scripted call, `Inbox::reply` sends a
  free-form reply it refused before; after `control_taken`, a reply is
  refused locally with zero requests; each fails when its recording is
  removed.
- [ ] **L8. Message and contact stores** (rows 69, 111): a lookup by
  message id on `ConversationStore`, the contacts of the coexistence
  sync kept, a Redis `ConversationStore`, SQLite adapters for both
  ports. *Decisive:* every adapter passes both conformance suites;
  the lookup case fails an adapter that scans another conversation.
- [ ] **L9. Phone numbers** (rows 66, 129–133, 151;
  `client::phone_numbers`): the business username calls, deleting a
  contact book entry, search visibility, security notifications and
  number-change notices, the Official Business Account request and
  status, business compliance information, health status on numbers,
  WABAs and businesses, bot details. *Decisive:* exact JSON per field
  from `reference/whatsapp-business-phone-number/*`; dropping any one
  field from its body fails its test.
- [ ] **L10. WABAs, accounts, billing, history** (rows 90's Meta side,
  127, 146–148, 152; `client::waba`): the parent BSUID accounts API
  (served from `api.facebook.com`, outside the credential host allow
  list: widening it goes through the security review), WABA creation (partner-initiated),
  system user tokens for client businesses,
  activities, a WABA's solutions, system users, a user's assigned WABAs,
  the billing currency migration intent, message history events, Meta's
  schedules API. *Decisive:* exact JSON per endpoint; a non-idempotent
  creation is never retried after a timeout (`RetryPolicy`).
- [ ] **L11. Onboarding** (rows 6, 121–123; OPEN_QUESTIONS #5, #7, #8,
  #11, #12; `client::embedded_signup`): hosted Embedded Signup (started
  by the `PARTNER_ADDED` webhook, its business token from
  `system_user_access_tokens` with an `appsecret_proof`, then the
  existing steps) and app-only install, opt-in onboarding of every
  granted WABA, the
  pre-verified number endpoints, an opt-out coexistence sync step
  inside onboarding, a token-expiry signal ahead of the lapse.
  *Decisive:* with the multi-WABA option off, a signup granting two
  WABAs onboards only the claimed one; with it on, both, and a failure
  on the second leaves the first stored and resumable.
- [ ] **L12. Partners** (rows 144, 145; new client modules): Multi-Partner
  Solutions (create, accept, reject, deactivation, the solution token,
  an app's solutions and connected client businesses) and migration
  intents. *Decisive:* exact JSON per endpoint; the solution token never
  reaches `Debug` (a sentinel test).
- [ ] **L13. Marketing** (rows 137, 138; `client::marketing`): max price
  (its agreement, the partner allow list, duplicating a template at
  another price), reach estimates and CTWA welcome message sequences. Calling the
  agreement endpoint signs Meta's beta agreement: the library exposes
  it, and whether a deployment calls it is the integrator's (legal)
  decision, like OPEN_QUESTIONS #26. *Decisive:* exact JSON per
  endpoint; the 15-entry allow-list cap is checked locally.
- [ ] **L14. Call recording and transcription** (row 135;
  `client::calling`): the per-call `recording` and `transcription`
  objects on connect and accept (`calling/call-recording`,
  `calling/call-transcription`). *Decisive:* exact JSON from those
  pages; a connect without the options sends neither key.
- [ ] **L15. The thread control API** (row 139; a new client module):
  pass, take, release. *Decisive:* exact JSON from
  `conversation-routing/thread-control`.
- [ ] **L16. Account model evolution** (row 140; client, beta).
  *Decisive:* exact JSON from `reference/whatsapp-account-number/*`.
- [ ] **L17. Catalog and product reads** (row 103; `client::commerce`),
  against Meta's Catalog API, whose docs sit outside the WhatsApp docs
  (not mirrored: read them first). *Decisive:* a paged product list
  follows its cursors to the end with no repeat.
- [ ] **L18. A typed Flow JSON builder** (row 63; `client::flows`).
  *Decisive:* every Flow JSON example in `flows/guides/flowjson`
  round-trips through the builder byte for byte (as JSON values).
- [ ] **L19. Media conversion** (row 48): a `MediaConverter` trait in
  the library and an implementation that runs an `ffmpeg` the
  integrator installs, behind a feature; nothing linked. Shipping
  `ffmpeg` inside the service's image is a licence question for the
  owner. *Decisive:* a WAV input comes out as Ogg with Opus that
  `Media::upload`'s type check accepts; a missing `ffmpeg` is a typed
  error, not a panic.
- [ ] **L20. API conventions** (OPEN_QUESTIONS #16, #20, #22, #27, #28):
  one catch-all shape for open enums (`Other(String)`, one macro in
  core), the `#[non_exhaustive]` rule written into architecture.md and
  applied, ids checked against their documented shape (digits for
  numeric ids; percent-encoding kept for opaque ones), an additive
  `WebhookHandlerBuilder` check for a blank verify token, and exactly
  two cards when a product-card carousel template is created
  (OPEN_QUESTIONS #22). A breaking change, before the first release. *Decisive:* an unknown enum value
  round-trips unchanged through deserialize and serialize for every open
  enum (a unit `Unknown` left anywhere fails it); a numeric id holding
  `/` is refused before any request.
- [ ] **L21. Webhook robustness** (OPEN_QUESTIONS #19, #30, #31;
  `webhooks`, `adapters`): sink errors classified transient or
  permanent, a permanent one written to a dead-letter typed store on
  `KvStore` (bounded, alerted, replayable) before the delivery is
  acknowledged; `BroadcastSink` and `sse` over `Arc<WebhookEvent>`;
  `rediss://` with an explicit aws-lc-rs provider (never the process
  default), or integrator-built connections if redis-rs cannot take
  one. *Decisive:* a batch with one permanently failing event delivers
  the others and dead-letters that one (removing the dead-letter write
  makes the handler answer 500 again, and the test fails); ten
  subscribers share one allocation per event.
- [ ] **L22. Tooling** (row 117): `llms.txt` for the docs and a
  `doctor` check (configuration, webhook subscription fields, token
  reach) in the service's CLI. *Decisive:* `doctor` reports each
  scripted misconfiguration by name and exits non-zero.
- [ ] **L23. Template groups** (row 153; `client::templates`): create,
  read, update and delete, after reading the reference Meta's changelog
  links (its guide is not in Meta's page list). *Decisive:* exact JSON;
  a group id is checked as digits before any request.

## 4. Service milestones

Re-scoped on the modular architecture (design §8, §9): each milestone's
routes call core services and run against every backend the
conformance suite covers. The acceptance tests M2.1–M4.3 are the
design's (§9).

- [ ] **M2. Inbox, live updates, webhooks out** (rows 10, 45, 91, 111,
  119, 137 and 139's events): inbox routes filtered by the number's
  binding epoch; SSE through an `EventNotifier` port (Postgres
  `LISTEN/NOTIFY`, memory broadcast; resume by `Last-Event-ID`);
  `GET /v1/events/{id}`; webhook endpoints, dispatcher and retries
  (Standard Webhooks, a destination allow list); the service's number
  events; D25 (`standby_observed`, `thread_control_changed`,
  `user_action_reported` made tenant-visible) with L7's inbox answer to
  OPEN_QUESTIONS #44; retention per store (D10); the received-media
  exemption (OPEN_QUESTIONS #43) with a live check against Meta.
  *Decisive:* M2.1–M2.6; taking one of the three types out of
  `TENANT_EVENT_TYPES` fails M2.6; removing the binding-epoch filter
  shows a moved number's history to its new tenant, and a test fails.
- [ ] **M3. Embedded Signup, OTP, coexistence** (rows 6, 69, 98, 108,
  120–124, 128, 142): signup routes in both partner modes, persisted
  attempts, disconnection, the coexistence sync (D7), authentication
  templates, OTP with per-tenant settings (the sending number, D8),
  registration and the PIN (D6: typed per attempt, never stored), vault
  rotation over the records partner mode keeps past a binding, the
  token-expiry event. *Decisive:* M3.1–M3.5; a PIN found in any stored
  row or log capture fails the test.
- [ ] **M4. Packaging** (rows 115, 118): the Docker image (D9: public
  GHCR, `meta-whatsapp-server`), a Compose smoke test, the TypeScript
  client published from S8 (D11: npm; the owner's token), the documents
  route. *Decisive:* M4.1–M4.3.
- [ ] **M5. Routes for the modules parity requires**, one PR per family,
  each a thin route over an existing library module and each object id
  checked to be the path's number's or WABA's own (architecture.md
  § Service):
  - [ ] M5a. The send union's other types (rows 22, 29, 37, 51, 53,
    56, 57, 59, 60, 104): pin, request contact info, Direct Send,
    interactive carousels, voice call, location request, address, call
    permission request, Flow, product messages.
  - [ ] M5b. Templates (rows 95, 96, 99, 100, 153): edit, library,
    migrate, compare, unpause, template groups, the shapes refused
    today.
  - [ ] M5c. Numbers, profile, WABAs, accounts (rows 47, 48, 64–66,
    126–133, 140, 148, 151, 152): resumable upload, conversion on upload (L19),
    the profile picture, display name, settings, username, and the
    endpoints L9, L10 and L16 add.
  - [ ] M5d. Flows (rows 61, 62): management and the data endpoint.
  - [ ] M5e. Calling (rows 74, 75, 134, 135).
  - [ ] M5f. Groups (rows 76–79, 81).
  - [ ] M5g. Commerce, QR codes, analytics, block users (rows 71, 102,
    103, 106, 107).
  - [ ] M5h. Marketing Messages API and CTWA (rows 136–138).
  - [ ] M5i. In-App Signup (row 125): built, and enabled per deployment
    only once the owner has answered OPEN_QUESTIONS #26 (accepting
    Meta's terms).
  - [ ] M5j. Partner APIs (rows 143–147).
  - [ ] M5k. The bot and broadcast APIs (rows 17, 85–92), over B1–B4.
  - [ ] M5l. Conversation routing (row 139): the thread control API,
    over L15.
  *Decisive, for every family:* M1.3's table-driven test covers each new
  `{pn}` and `{waba_id}` route by construction (tenant B's key on A's
  number is `404`, with zero vault reads); a route whose object id is
  not checked against the number or WABA fails the family's own
  cross-tenant test.

## 5. Payments (last)

- [ ] **P1. A `payments` client module** (rows 38, 141): India (UPI,
  payment links, order details and order status, onboarding) and
  Brazil (Pix, Boleto, payment links, one-click), typed, instead of
  `MessageContent::Raw`. *Decisive:* exact JSON from
  `payments/payments-in/*` and `payments/payments-br/*`; the payments
  pages' webhook examples join the conformance sweep.
- [ ] **P2. Payment routes in the service.** *Decisive:* M1.3 over the
  new routes; an order-details send carries the tenant's token.

## Done criteria for parity

- Every row of [parity.md](parity.md) that is not n/a says done for the
  library, and done or n/a for the service.
- [categories.md](categories.md) says done for every category, or gives
  the reason a part stays out (an n/a of the parity table).
- `just ci` exits 0 on `main` at that commit, with the live tests
  forced (`META_WHATSAPP_RS_REQUIRE_LIVE=1`).
- Only then is the owner pinged.
