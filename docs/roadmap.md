# Roadmap to parity

The waves that take meta-whatsapp-rs to parity as the owner defined it on
2026-09-26 ([parity.md](parity.md): Meta's Cloud API, plus Zaileys'
framework features). Every partial or gap row of the parity table is
cited by an item below. Written against `main` at b6fc893 (PR #20).

- Each item is one pull request (S13, one per resource group, says
  otherwise) and names its crate, what it comes
  **after**, and its **decisive test**: the guard whose removal must
  make a test fail, as in
  [design §9](design/server.md#9-delivery-plan). A library item (L) also
  names its **kind**: additive, breaking, or a port change, as design
  §9's table does. A PR is done when its decisive test holds, `just ci`
  exits 0 on its final head, and it has updated its rows in
  [parity.md](parity.md), [categories.md](categories.md) and
  [coverage.md](coverage.md), the docs and the skills (AGENTS.md
  § Companion docs and skills).
- **After** names the items that must have merged first, and any
  answer the owner must have given; "nothing" when there is none. Items
  in one wave run in parallel only where their `after:` lines allow.
- Ids: S (the service's modular split), U (CrateStack, upstream), B (the
  bot framework), L (library batches; L1–L6 are design §9's), M
  (service milestones: M1 shipped, M2–M5 below) and P (payments). A
  letter or digit after an id is one pull request of a family (M2a,
  M5c1). The security reviews' findings (H1, M1–M4, L1–L6 in the code's
  comments) are cited in the docs as SR-H1, SR-M3, SR-L2, so they never
  read as items.
- The choices behind the items are recorded in
  [design §10](design/server.md#10-decisions) (D26–D29 and the rows
  updated on 2026-09-26) and in
  [OPEN_QUESTIONS.md](../OPEN_QUESTIONS.md), under the rule in AGENTS.md
  § Decisions. What stays the owner's is listed in
  [§ Owner touchpoints](#owner-touchpoints).
- **Routes.** Every new route calls a core service, so both API adapters
  stay thin, and lands in both adapters in the same PR once
  `meta-whatsapp-server-api-cratestack` exists (S10). A route that lands
  before S10 goes into `meta-whatsapp-server-api-axum`, and S13 carries it
  over: its groups cover every route that adapter has when S13 starts.
  `api-axum` is permanent, the swap target (design D26).

## Order

| Wave | Items (in parallel only where their `after:` lines allow; at most three building at once) |
| --- | --- |
| 0 | this plan (parity, categories, roadmap, decisions) |
| 1 | S1 (server core), B1 (bot framework), U4 (upstream issues) |
| 2 | S2, S3, S4, S8; U1–U3 in the cratestack repository; L4, L5, L7 (the `ConversationStore` port change and the inbox's use of it, before M2); the library batches L8–L25, which may start here and run alongside every later wave; B1b, B1c (the bot framework's follow-ups) |
| 3 | S5a–S5e, S6, S7, S9 |
| 4 | S10 (waits on the owner's answer to D20 (a)), S11, S12; M2a–M2e |
| 5 | S13–S16 (S16 last, gated on U1's merge); M3a–M3f; B2–B4 |
| 6 | M4; M5a–M5l |
| 7 | S17, S18 (MongoDB) |
| last | P1, P2 (payments) |

## 1. The service's modular split (design §8, D26)

From the CrateStack fit study of 2026-09-26 and the architecture review
of the core's extraction: the unit of swapping is a backend bundle and
an API adapter over one framework-free core. First the core and its
ports are made right for any backend (S1–S6), then the crates split
(S7), then MongoDB (S8, S9, S17, S18) and CrateStack (S10–S16) are
added behind them. `/v1` stays served, and `openapi/v1.json` stays
byte-identical, until the default flips (S16); after it,
`meta-whatsapp-server-api-axum` keeps both, as the swap target. The
CrateStack defaults of D26 hold once the owner accepts D20 (a); until
then, and for good if the owner refuses, the defaults stay `api-axum`
and `store-postgres`.

- [ ] **S1. Extract `meta-whatsapp-server-core`** (new crate, depending
  on the facade with `default-features = false`): the model, keys, event
  routing and type lists, outbox keys and ids, polling, the idempotency
  engine, the rate limiter, the §5 error model as data, authorization as
  services; the ports `RecordStore`, `IdempotencyRecords`, `Outbox`,
  `LeaderLock`, `Janitor`, `SchemaMigrator` and the `Backend` bundle
  (replacing `Backends.pool` as the "memory?" flag). Implementations
  stay in `meta-whatsapp-server`. Row 112 (the bundle). Companions:
  architecture.md § Service and CLAUDE.md's dependency rule (the
  server family's library crates depend on the facade and on each
  other; the library never on them).
  - **After:** nothing.
  - **Decisive:** `cmp` of `openapi/v1.json` against `main` is equal;
    every server test passes with only its imports changed; `cargo tree
    -p meta-whatsapp-server-core` shows no axum, hyper, tower, utoipa,
    sqlx or cratestack (`http` enters only through the library's
    `HttpTransport` port).
- [ ] **S2. The port contracts, right for any backend**
  (`meta-whatsapp-server-core` and both in-tree backends): what Postgres
  guarantees by accident becomes a requirement of the ports, cheap now
  and costly once a third backend exists.
  - `Outbox::insert` must re-check the routing: when the event has a
    tenant, the row keeps it only if, atomically with the insert, the
    binding it was routed by still names that tenant and began no later
    than the event's time; otherwise the row is operator-only. A typed
    `RouteGuard` on `NewEvent` carries what the check needs, so no
    backend redoes the routing rules from strings.
  - Referential rules: `bind_waba` to a missing tenant has its own
    outcome (`BindOutcome::NoSuchTenant`), and `bind_waba` and
    `delete_tenant` on one tenant serialize.
  - Purges take no lock of their own and return a count; housekeeping
    takes one `LeaderLock` turn per round. `LeaderLock` is a lease
    (`try_exclusive(name, lease)`), ending at release, drop or expiry,
    and the work under it tolerates an overlap.
  - One clock all replicas agree on: the database's where it has one,
    else the service's `Clock`, with its skew bounded below the lease;
    the core's `Authorizer` takes an injected `Clock`.
  - A typed "busy" storage error for contention, instead of a downcast.
  - `Backend`'s accessors return handles to the same data on every
    call.
  - The outbox's types carry the id newtypes; listings are in byte
    order.
  - **After:** S1.
  - **Decisive:** on memory and, live, on Postgres: `bind_waba` to a
    missing tenant answers `NoSuchTenant` (memory binds it today); an
    insert whose guarded binding moved is operator-only (removing
    memory's re-check fails it); a backend reporting the typed busy
    error makes the webhook path answer `503`.
- [ ] **S3. Conformance into core** (`meta-whatsapp-server-core`,
  `meta_whatsapp_server_core::conformance`, a feature, like the
  library's `store::conformance`, run over a `&dyn Backend`): the store
  and events suites moved out of the server's tests, the cross-port
  invariants (deleting a tenant purges its stream; a binding changed
  during an insert leaves the row operator-only; per-tenant commit
  order) and S2's rules. Row 112 (conformance in core).
  - **After:** S2.
  - **Decisive:** removing the memory backend's re-check fails the
    binding invariant; removing the stream purge from `delete_tenant`
    fails on memory and, live, on Postgres; a `Backend` whose `kv()`
    returns a new store on each call fails the same-data case.
- [ ] **S4. Composed only through the bundle** (`meta-whatsapp-server`,
  `meta-whatsapp-server-core`): `serve_with(config, Arc<dyn Backend>)`;
  `AppState::from_backend(Arc<dyn Backend>, …)` building the inbound
  pipeline inside; housekeeping and the sweep in core over the bundle;
  the CLI through a backend factory. Row 112 (a backend an integrator
  brings).
  - **After:** S3.
  - **Decisive:** a test serves the API over a `Backend` defined outside
    the server crate, with no edit to `serve.rs`; a doctest that builds
    the state from loose ports fails to compile.
- [ ] **S5a. Admin services into core** (`meta-whatsapp-server-core`;
  S5a–S5e move the operation services out of the axum handlers,
  returning domain values and `ServiceError`, so both API adapters stay
  thin; hardest logic first): deleting a tenant (the offboarding loop),
  attaching a WABA (the D4 check, the capped listing, bind, vault store,
  subscribe), minting keys, allowed tenants, the tenant and key
  listings, revocations, unbinding.
  - **After:** S4.
  - **Decisive:** called with no HTTP layer, attaching a WABA another
    tenant holds is refused with nothing stored, subscribed or shared;
    the admin routes' tests pass unchanged; `openapi/v1.json`
    byte-identical.
- [ ] **S5b. Idempotency and messages into core**
  (`meta-whatsapp-server-core`): `idempotency::run` answering a status,
  a body and whether it replayed; sending (the accepted message, not an
  HTTP status), marking read, the recipient and E.164 rules, over a
  request type free of utoipa.
  - **After:** S4.
  - **Decisive:** called with no HTTP layer, a scripted timeout reports
    `may_have_been_sent` and the same key replays it with no second
    request (M1.4's case); `openapi/v1.json` byte-identical.
- [ ] **S5c. Numbers and templates into core**
  (`meta-whatsapp-server-core`): number details, the profile's read and
  update, disconnecting a WABA; the template list, the lookup in a WABA,
  create, delete, and the template cache.
  - **After:** S4.
  - **Decisive:** called with no HTTP layer, tenant B's caller on A's
    template id is not found with zero vault reads; `openapi/v1.json`
    byte-identical.
- [ ] **S5d. Media into core** (`meta-whatsapp-server-core`): upload,
  download as a framework-free byte stream, delete.
  - **After:** S4.
  - **Decisive:** a download through the core's stream ends before its
    last chunk on a digest mismatch (M1b's rule, without axum); the
    media routes' tests pass unchanged.
- [ ] **S5e. The webhook pipeline into core** (`meta-whatsapp-server-core`,
  or `meta-whatsapp-server-http` for what needs a listener): routing and
  recording, framework-free.
  - **After:** S4.
  - **Decisive:** M1.2's routing cases run against the core pipeline
    with no listener; routing an unowned number's event to a tenant
    fails them.
- [ ] **S6. Adapter-neutral pieces** (`meta-whatsapp-server-core`; D27):
  a rate class per operation id, not per HTTP method; the idempotency
  fingerprint over the operation id and the canonical input, not the
  request path, so a key means the same through every adapter (a
  pre-release change of `/v1`'s semantics, noted in the guide);
  `ErrorCode` constants generated from the code table, and
  `ServiceError::new(ErrorCode)`; the vault's write access kept behind
  an admin capability; unused dependencies dropped; `just features`
  checking the core alone.
  - **After:** S5a, S5b, S5c, S5d, S5e.
  - **Decisive:** two spellings of one operation's path (percent-encoded
    or not) replay the same key, and putting `uri.path()` back into the
    fingerprint fails that test; a read charged by operation id lands in
    the read budget whatever its HTTP method; a misspelled error code
    fails to compile (a `compile_fail` doctest).
- [ ] **S7. The crate split**: `meta-whatsapp-server-store-memory`,
  `meta-whatsapp-server-store-postgres`, `meta-whatsapp-server-http`
  (listeners, auth before the body, the §5 renderer, `/webhooks/meta`,
  media, ops) and `meta-whatsapp-server-api-axum` (today's `/v1` routes
  and `openapi/v1.json`); the binary composes them by Cargo features.
  - **After:** S6.
  - **Decisive:** `openapi/v1.json` byte-identical; each store crate
    runs the core conformance suite (live for Postgres, with
    `META_WHATSAPP_RS_REQUIRE_LIVE=1`); the binary builds with only
    `api-axum` and `store-memory` enabled; live, a Postgres database
    migrated by `main`'s binary starts the split binary with no
    migration run again and no checksum error (the migration history
    and its lock are stable identifiers).
- [ ] **S8. MongoDB in the dev container and CI** (`.devcontainer/`,
  the `just test-live` Compose file, `.github/workflows/ci.yml`): a
  single-node replica set (transactions need one) beside Postgres and
  Redis, and `META_WHATSAPP_RS_TEST_MONGODB_URL`, for S9, S17 and S18.
  - **After:** nothing.
  - **Decisive:** `just test-live` with
    `META_WHATSAPP_RS_REQUIRE_LIVE=1` and no MongoDB URL fails; with it,
    a live smoke test commits a multi-document transaction.
- [ ] **S9. An `Outbox` spike on MongoDB**
  (`meta-whatsapp-server-store-mongodb`, the `Outbox` only, not composed
  into the binary), before the port shapes freeze: per-tenant commit
  order, the routing re-check, the stream purge, against a replica set.
  - **After:** S3, S8.
  - **Decisive:** the spike passes the core conformance suite's outbox
    cases, the cross-port invariants included, live against a replica
    set. If a case cannot pass, the port changes first (memory and
    Postgres with it, through S3's suite), and the spike passes after
    that change.
- [ ] **U1. Domain errors in CrateStack** (D16; the owner's cratestack
  repository, a PR there with its own parity declaration): a domain
  error carrying our code, status and body, statuses 410, 413, 502 and
  504 included.
  - **After:** nothing. The owner merges it (§ Owner touchpoints).
  - **Decisive:** a procedure returning a domain error answers the §5
    body byte for byte; dropping the status passthrough fails it.
- [ ] **U2. A `tls-aws-lc-rs` feature on `cratestack-sqlx`** (D20,
  upstream).
  - **After:** nothing. The owner merges it.
  - **Decisive:** with the feature on, `cargo tree -e features` of a
    test crate shows no `ring`.
- [ ] **U3. Relax CrateStack's `sqlx = "=0.9.0"` pin to `~0.9`** (D20,
  upstream).
  - **After:** nothing. The owner merges it.
  - **Decisive:** a test workspace resolves a newer 0.9 patch release
    beside CrateStack.
- [ ] **U4. Issues for CrateStack's model gaps** (the owner's cratestack
  repository, issues only): `FOR KEY SHARE`, advisory locks,
  `SKIP LOCKED`, conditional and expression upserts, a `json` (not
  `jsonb`) type, the database's clock for timestamps, writable queries,
  and SSE resume by `Last-Event-ID`, each filed as its ADR 0018 says a
  gap in the in-process API is (a bug, not "use sqlx").
  - **After:** nothing.
  - **Decisive:** each issue carries a minimal `.cstack` schema and the
    statement our store needs that it cannot express; design §8.6 links
    every one of them.
- [ ] **S10. CrateStack enters the workspace** (D20; the workspace
  manifest, `deny.toml`, `meta-whatsapp-server`): the `deny.toml`
  licence exception for BlueOak-1.0.0 scoped to `minicbor` and
  `minicbor-serde`, aws-lc-rs installed as rustls's process default at
  start, a minimal `meta-whatsapp-server-api-cratestack` with one
  procedure, not the default. A refusal of D20 (a) means no CrateStack
  in the build: S10–S16 do not happen.
  - **After:** S7, and the owner's yes to D20 (a) (the coordinator asks
    at this step; S10 does not merge without the answer).
  - **Decisive:** `just deny` passes and fails again when the exception
    is removed; a live Postgres test with `sslmode=require` passes in a
    binary that links both providers (sqlx picks `ring` itself until
    U2); a test that builds a rustls client configuration from the
    process default after start-up passes in that binary, and panics
    when the install is removed.
- [ ] **S11. The system principal, a spike**
  (`meta-whatsapp-server-store-cratestack`, one model): CrateStack
  applies its policies to in-process calls too (its ADR 0018), so the
  store calls models as the system principal, with
  `@@allow(..., auth().isSystem())` on every model.
  - **After:** S10.
  - **Decisive:** an in-process `find_many` as the system principal
    returns the rows; the same call under a tenant's context returns
    none; removing `auth().isSystem()` from the model's `@@allow` fails
    the first.
- [ ] **S12. A CrateStack upgrade cadence** (the workspace manifest, a
  scheduled CI job): the `cratestack*` crates pinned to one minor
  (`~0.13`) and to one version between them; a weekly job that opens an
  issue when CrateStack releases a new minor; one upgrade PR per
  release, through the review pipeline.
  - **After:** S10.
  - **Decisive:** a workspace test reading `Cargo.toml` fails when a
    `cratestack*` requirement is not pinned to one minor, or when two
    CrateStack crates are pinned to different versions.
- [ ] **S13. `meta-whatsapp-server-api-cratestack` parity** (one PR per
  resource group: numbers, messages and media, templates, events,
  admin, and each group added before S10): `api.cstack` with
  `db = None`, REST (D19), procedures calling the core's services.
  - **After:** S10.
  - **Decisive:** the authorization (M1.3), error (M1.5) and idempotency
    (M1.4) suites run against both adapters through an enumerator over
    each adapter's contract; a procedure left out of the enumerator
    fails it.
- [ ] **S14. `meta-whatsapp-server-store-cratestack`, the hybrid** (D15):
  `store.cstack` models (`db = Postgres`, `@@internal`) for tenants, API
  keys, WABAs and numbers, called as the system principal; the outbox,
  idempotency, locks, janitor, migrations and the library's Postgres
  stores on the same pool, reusing `meta-whatsapp-server-store-postgres`'s
  SQL.
  - **After:** S9, S10, S11.
  - **Decisive:** the core conformance suite passes live; a model field
    renamed away from our migrations' column fails the live drift test.
- [ ] **S15. The TypeScript client from CrateStack** (`clients/typescript`,
  D18, D19, D11): generated by `@cratestack/cli` pinned to the crate's
  version, JSON on the wire (not CBOR), the hand-written layer (keys,
  `WA-Tenant`, `Idempotency-Key`, the §5 error, a resuming SSE reader,
  `verifyWebhook()`), a drift gate, and the npm publish workflow
  prepared (publishing is the owner's, § Owner touchpoints).
  - **After:** S13.
  - **Decisive:** editing `api.cstack` without regenerating fails the
    gate; a request the client builds carries
    `content-type: application/json`.
- [ ] **S16. The default flip** (the binary's features): `api-cratestack`
  and `store-cratestack` become the default features; the server skills
  and `just skills-ts` move to the generated client, and `just
  skills-ts` keeps generating types from `openapi/v1.json` with
  openapi-typescript, so the swap target's client stays checked; the
  server-skill checks move out of `crates/meta-whatsapp-rs/tests/skills.rs`;
  `openapi/v1.json` stays with `meta-whatsapp-server-api-axum`, which
  is permanent: swapping to it swaps the wire contract (resource URLs)
  and the client (openapi-typescript), as swapping to CrateStack's does
  (procedures, its generated client).
  - **After:** S13, S14, S15, and U1 merged by the owner.
  - **Decisive:** `just ci` with default features; the errors suite
    fails if the CrateStack adapter answers an error in CrateStack's
    own shape; the binary built with `api-axum` still serves
    `openapi/v1.json` byte for byte.
- [ ] **S17. MongoDB library adapters** (`meta-whatsapp-adapters`,
  feature `mongodb`): `KvStore` and `ConversationStore`.
  - **After:** S8, L5.
  - **Decisive:** both library conformance suites pass live against a
    replica set; removing the version check from compare-and-swap fails
    them.
- [ ] **S18. `meta-whatsapp-server-store-mongodb`**, the whole bundle.
  - **After:** S9, S17, M2a–M2e.
  - **Decisive:** the core conformance suite passes live; the service's
    M1–M2 acceptance tests pass on it.

## 2. The bot framework (`meta-whatsapp-bot`, D28, D29)

A new library crate on core, client and webhooks (never adapters or the
facade), re-exported by the facade behind a feature. A `Bot` is an
`EventSink`, so it plugs into `WebhookHandler` like any sink.

- [x] **B1. Commands, middleware, compile-time plugins, markdown
  replies** (`meta-whatsapp-bot`; rows 17, 85–88): a command parser
  (prefixes, aliases, quoted arguments, button and list payloads),
  guards (private or group only, owner and banned lists behind a trait),
  per-user cooldowns as a typed store on `KvStore`, generated help, an
  optional sync to Meta's command menu; `Middleware` with `next`; a
  `Plugin` trait with setup and unload hooks, registered at compile time
  (no hot reload, D28); `markdown::render` to WhatsApp formatting, split
  at 4096 characters on paragraph boundaries. Companions:
  `docs/guides/bots.md`, the skill `meta-whatsapp-rs-bot`, the facade's
  `bot` feature and `just features` checking it alone, architecture.md's
  dependency rule (the bot crate on core, client and webhooks), and
  architecture.md § Stable identifiers for the cooldowns' `KvStore`
  namespace.
  - **After:** nothing.
  - **Decisive:** removing the cooldown check fails a test; removing the
    banned-list guard fails a test; a middleware that does not call
    `next` stops the handler; a 5000-character text splits into exactly
    two messages at a paragraph boundary; a sender with no `wa_id`
    (BSUID only) is served.
  - **Landed:** rows 86–88 done in the library; row 85 partial
    (subcommands and flags: B1b) and row 17 partial (rich replies
    beyond text: B1c). Folder loading and hot reload stay out (D28).
- [ ] **B1b. Subcommands and flags** (`meta-whatsapp-bot`; row 85): a
  command's subcommands (`/order status 42` runs `status` under
  `order`, each with its own guards, usage and help line) and flags in
  its arguments (`--dry-run`, `--limit=5`), read from `Args` next to
  the positional arguments. Companions: the guide's and the skill's
  "not here yet".
  - **After:** B1.
  - **Decisive:** `/order status 42` runs the subcommand with `42` as
    its only argument, and `/order` alone runs the parent; a
    subcommand's own cooldown refuses a second run (removing its guard
    check fails the test); a quoted `"--limit=5"` stays a positional
    argument (removing the quote check fails the test).
- [ ] **B1c. Rich replies beyond text** (`meta-whatsapp-bot`; row 17):
  a reply built from Markdown and a few typed parts, sent as the
  messages Meta documents for them: an image as an image message (by
  link, under the renderer's URL rule), suggestions as reply buttons
  (up to 3, `messages/interactive-reply-buttons-messages`) or a list
  (up to 10 rows, `messages/interactive-list-messages`), product cards
  as a product carousel; the text parts through the Markdown renderer
  as today.
  - **After:** B1.
  - **Decisive:** a reply of a paragraph and an image sends a text
    message and then an image message, in order, with the exact
    requests asserted; three suggestions become buttons and four a list
    (removing the count check fails the test); a `javascript:` image
    URL is never sent.
- [ ] **B2. Paced broadcast** (`meta-whatsapp-bot`; rows 89, 92; D29): a
  per-number rate under Meta's throughput (80 messages a second by
  default), progress, retries only when `Error::may_have_been_sent` is
  false, the pair and per-user marketing limits read from `ErrorKind`;
  typing indicators and group operations throttled through the same
  pacer.
  - **After:** B1.
  - **Decisive:** under a fake clock, 200 sends at a configured 20 a
    second never exceed it; a scripted timeout is never retried
    (removing the `may_have_been_sent` check fails the test).
- [ ] **B3. Durable scheduling** (`meta-whatsapp-bot`; row 90; D29): jobs
  as a typed store on `KvStore` (a bucketed due-time index, claims by
  compare-and-swap with a lease; no new port), send at a time, cancel,
  retry, survive a restart. Companion: architecture.md § Stable
  identifiers for the jobs' namespace.
  - **After:** B1.
  - **Decisive:** two runners on one store send a due job once
    (replacing the compare-and-swap by a plain put fails it); a job
    scheduled before a restart is sent by a new runner on the same
    store.
- [ ] **B4. Retention and auto-delete** (`meta-whatsapp-bot`; row 91): by
  age and a cap per chat, through `ConversationStore`'s erasure.
  - **After:** B1, L5.
  - **Decisive:** removing the age filter fails the test; the erased
    messages are gone from `messages` and `conversations` on memory and,
    live, on Postgres.

## 3. Library gap batches

By crate and topic; each is one PR through the review pipeline, with
`ScriptedTransport` tests asserting method, path, query, token and exact
JSON from Meta's pages. A batch whose pages are not in the mirror
(parity.md's (nm)) starts by reading them.

- [ ] **L4. A code-less `OnboardingRequest` for `resume`**
  (`meta-whatsapp-client`, `client::embedded_signup`; OPEN_QUESTIONS
  #10).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** a new process resumes from stored session info alone;
    sending a placeholder code to Meta fails the test's request
    assertion.
- [ ] **L5. The `ConversationStore` port change** (`meta-whatsapp-core`,
  `meta-whatsapp-adapters`; D10; OPEN_QUESTIONS #32, #44; rows 69, 111):
  one change for everything the port gains before M2, so adapters change
  once. Erasure of one contact on one number, purge by age, retention
  set per store (D10); window events (the calls that reopen the window,
  standby inbound messages) and thread-ownership records, for L7; a
  lookup by message id, scoped to the business number, for L8 and M2; the
  coexistence sync's contacts, for M3b. The lookup works with either
  answer to OPEN_QUESTIONS #33, which stays open: keyed by
  `(phone_number_id, id)`, it finds a row whether ids are unique per
  store (today) or per number.
  - **Kind:** port change.
  - **After:** nothing.
  - **Decisive:** new conformance cases for each part; memory and
    Postgres pass; a Postgres erase that leaves the message bodies fails
    live; the lookup case fails an adapter that finds a message of
    another number.
- [ ] **L7. Window events and thread ownership in the inbox**
  (`meta_whatsapp_rs::inbox`; OPEN_QUESTIONS #32, #44; rows 119, 139):
  `InboxSink` records the calls that reopen the 24-hour window and
  standby messages as window events (never unread), and ownership from
  the handovers; `Inbox::reply` refuses locally when another app owns
  the thread; a caller's explicit override of the local check.
  - **Kind:** additive (on L5's port).
  - **After:** L5.
  - **Decisive:** after a scripted call, `Inbox::reply` sends a
    free-form reply it refused before; after `control_taken`, a reply is
    refused locally with zero requests; each fails when its recording is
    removed.
- [ ] **L8. Message and contact stores** (`meta-whatsapp-adapters`,
  features `redis` and a new `sqlite`; `meta_whatsapp_rs::inbox`; rows
  69, 111): a Redis `ConversationStore`, SQLite adapters for both ports,
  and `InboxSink` keeping the coexistence sync's contacts through L5's
  port. It depends on OPEN_QUESTIONS #33, which stays open: the new
  adapters key messages by `(phone_number_id, id)` and enforce today's
  per-store uniqueness through a separate index, so either answer needs
  no data migration in them (per-number ids drop the index).
  - **Kind:** additive (new adapters, a new feature).
  - **After:** L5.
  - **Decisive:** every adapter passes both conformance suites; a second
    `append` of one id on another number returns `false` on each, as the
    suite says today; the synced contacts are read back from the store.
- [ ] **L9. Phone numbers** (`meta-whatsapp-client`,
  `client::phone_numbers`; rows 66, 129–133, 151): the business
  username calls, deleting a contact book entry, search visibility,
  security notifications and number-change notices, the Official
  Business Account request and status, business compliance
  information, health status on numbers, WABAs and businesses, bot
  details.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON per field from
    `reference/whatsapp-business-phone-number/*`; dropping any one field
    from its body fails its test.
- [ ] **L10a. The parent BSUID accounts API, and the credential host
  allow list** (`meta-whatsapp-client`, `client::GraphRequest` and
  `client::waba`; row 152): the API is served from `api.facebook.com`,
  outside the allow list, so the list widens to that host for that
  endpoint only, through the security review (`wa-security-reviewer`).
  - **Kind:** additive (a security-sensitive change).
  - **After:** nothing.
  - **Decisive:** the endpoint's request reaches `api.facebook.com` with
    the token; any other path on that host is refused before a request
    (removing the path restriction fails the test); the allow list's
    existing tests pass unchanged.
- [ ] **L10b. WABAs, accounts, billing, history** (`meta-whatsapp-client`,
  `client::waba`; rows 90's Meta side, 127, 146–148): WABA creation
  (partner-initiated), system user tokens for client businesses,
  activities, a WABA's solutions, system users, a user's assigned WABAs,
  the billing currency migration intent, message history events, Meta's
  schedules API.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON per endpoint; a non-idempotent creation is
    never retried after a timeout (`RetryPolicy`); a system user token
    never reaches `Debug` (a sentinel test).
- [ ] **L11a. Hosted Embedded Signup** (`meta-whatsapp-client`,
  `client::embedded_signup`; row 121; OPEN_QUESTIONS #11): started by the
  `PARTNER_ADDED` webhook, its business token from
  `system_user_access_tokens` with an `appsecret_proof`, then the
  existing steps (verify, store, subscribe, register).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** a scripted `PARTNER_ADDED` leads to a token request
    carrying the `appsecret_proof` of the app secret (a wrong proof fails
    the test's request assertion), and to the same stored record the
    code flow writes.
- [ ] **L11b. Several WABAs in one signup** (`client::embedded_signup`;
  row 122; OPEN_QUESTIONS #5): opt-in onboarding of every granted WABA,
  each bound and resumable on its own.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** with the option off, a signup granting two WABAs
    onboards only the claimed one; with it on, both, and a failure on
    the second leaves the first stored and resumable.
- [ ] **L11c. Pre-verified numbers** (`client::embedded_signup` and
  `client::waba`; row 123): pools, adding, codes, sharing, partners
  (`reference/business/*pre-verified*`,
  `reference/whatsapp-business-pre-verified-phone-number/*`).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON per endpoint; a pre-verified phone id is
    checked as digits before any request.
- [ ] **L11d. The coexistence sync inside onboarding**
  (`client::embedded_signup`; OPEN_QUESTIONS #7): a step right after a
  coexistence onboarding, on by default, that an option turns off. The
  library already carries the sync call
  (`client::phone_numbers::PhoneNumber::sync_smb_app_data`); this makes
  it a step of onboarding, which M3b's automatic sync (D7) builds on.
  - **Kind:** additive (a new default step; `needs_coexistence_sync()`
    stays).
  - **After:** nothing.
  - **Decisive:** a coexistence onboarding requests the sync exactly
    once; with the option off, none; an onboarding without coexistence
    sends none.
- [ ] **L11e. A token-expiry signal** (`client::embedded_signup`; row 6;
  OPEN_QUESTIONS #8): a configurable lead time before a stored business
  token's `expires_at`.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** under a fake clock, a token expiring inside the lead
    time is reported and one expiring after it is not; removing the
    comparison fails the test.
- [ ] **L12. Partners** (`meta-whatsapp-client`, new client modules; rows
  144, 145): Multi-Partner Solutions (create, accept, reject,
  deactivation, the solution token, an app's solutions and connected
  client businesses) and migration intents.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON per endpoint; the solution token never
    reaches `Debug` (a sentinel test).
- [ ] **L13. Marketing** (`meta-whatsapp-client`, `client::marketing`;
  rows 137, 138, 154; OPEN_QUESTIONS #45): max price (its agreement, the
  partner allow list, duplicating a template at another price), reach
  estimates and CTWA welcome message sequences, and a typed send-time
  parameter for a GIF header (read Meta's send syntax first: no
  mirrored page shows it). Calling the agreement endpoint signs Meta's
  beta agreement, a legal act: the library exposes it as its own
  explicit call, which no other call makes; signing is the deployer's
  act (AGENTS.md § Decisions).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON per endpoint; the 15-entry allow-list cap
    is checked locally; a max-price send makes no agreement request
    (a transport scripted for the send alone fails on any other call).
- [ ] **L14. Call recording and transcription** (`meta-whatsapp-client`,
  `client::calling`; row 135): the per-call `recording` and
  `transcription` objects on connect and accept (`calling/call-recording`,
  `calling/call-transcription`).
  - **Kind:** breaking (a new field on `ConnectCall`, whose fields are
    public; accept's options can come as an additive method).
  - **After:** nothing.
  - **Decisive:** exact JSON from those pages; a connect without the
    options sends neither key.
- [ ] **L15. The thread control API** (`meta-whatsapp-client`, a new
  client module; row 139): pass, take, release.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON from `conversation-routing/thread-control`.
- [ ] **L16. Account model evolution** (`meta-whatsapp-client`, beta;
  row 140).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON from `reference/whatsapp-account-number/*`.
- [ ] **L17. Catalog and product reads** (`meta-whatsapp-client`,
  `client::commerce`; row 103), against Meta's Catalog API, whose docs
  sit outside the WhatsApp docs (not mirrored: read them first).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** a paged product list follows its cursors to the end
    with no repeat.
- [ ] **L18. A typed Flow JSON builder** (`meta-whatsapp-client`,
  `client::flows`; row 63).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** every Flow JSON example in `flows/guides/flowjson`
    round-trips through the builder byte for byte (as JSON values).
- [ ] **L19. Media conversion** (`meta-whatsapp-client`: a
  `MediaConverter` trait; `meta-whatsapp-adapters`: an implementation
  behind an `ffmpeg` feature, off by default; row 48): the
  implementation runs an `ffmpeg` the integrator installs, or calls a
  conversion sidecar the integrator runs; nothing is linked. The
  service's default public image contains no `ffmpeg` (M4 lists what
  the image contains): a deployment that wants conversion adds it
  itself, so no licence question arises.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** a WAV input comes out as Ogg with Opus that
    `Media::upload`'s type check accepts; a missing `ffmpeg` is a typed
    error, not a panic.
- [ ] **L20a. One catch-all shape for open enums** (`meta-whatsapp-core`
  and every crate with an open enum; OPEN_QUESTIONS #27): `Other(String)`,
  generated by one macro in core.
  - **Kind:** breaking, before the first release.
  - **After:** nothing.
  - **Decisive:** an unknown value round-trips unchanged through
    deserialize and serialize for every open enum; a unit `Unknown` left
    anywhere fails it.
- [ ] **L20b. The `#[non_exhaustive]` rule** (`meta-whatsapp-client`,
  `meta-whatsapp-webhooks`, docs/architecture.md; OPEN_QUESTIONS #28): on
  every response struct, webhook payload struct and open enum, never on
  request or builder types; written into architecture.md.
  - **Kind:** breaking, before the first release.
  - **After:** L20a.
  - **Decisive:** a source scan over the crates fails on a response or
    webhook struct, or an open enum, without the attribute; removing it
    from one type fails the scan.
- [ ] **L20c. Ids checked against their documented shape**
  (`meta-whatsapp-core` ids and `client::GraphRequest`; OPEN_QUESTIONS
  #20): digits for numeric ids; whole-segment percent-encoding kept for
  opaque ones (BSUIDs, group ids).
  - **Kind:** breaking (an id of the wrong shape is refused).
  - **After:** nothing.
  - **Decisive:** a numeric id holding `/` is refused before any
    request; a group id holding `/` stays one percent-encoded segment.
- [ ] **L20d. A check for a blank verify token** (`meta-whatsapp-webhooks`,
  `WebhookHandlerBuilder`; OPEN_QUESTIONS #16).
  - **Kind:** additive (the builder stays infallible).
  - **After:** nothing.
  - **Decisive:** the check reports a handler built with a blank verify
    token and passes one built with a real token.
- [ ] **L20e. Product-card carousel templates: two cards at creation**
  (`meta-whatsapp-client`, `client::templates`; OPEN_QUESTIONS #22).
  - **Kind:** breaking (creating one with 3–10 cards is refused).
  - **After:** nothing.
  - **Decisive:** a product-card carousel with three cards is refused
    before any request; a media-card carousel with three is created; a
    send with 10 cards is accepted.
- [ ] **L21a. Dead-lettering a permanently failing event**
  (`meta-whatsapp-webhooks`, `meta-whatsapp-adapters`; OPEN_QUESTIONS
  #30): sink errors classified transient or permanent (unclassified
  stays transient, today's behaviour); a permanent one written to a
  dead-letter typed store on `KvStore` (bounded by count and age, reached
  by L5's erasure, alerted, replayable) before the delivery is
  acknowledged. Companion: architecture.md § Stable identifiers for the
  dead-letter namespace.
  - **Kind:** additive.
  - **After:** L5.
  - **Decisive:** a batch with one permanently failing event delivers
    the others and dead-letters that one (removing the dead-letter write
    makes the handler answer 500 again, and the test fails); an erasure
    of the contact removes their dead-lettered events.
- [ ] **L21b. Live events shared, not cloned** (`meta-whatsapp-webhooks`
  `sse`, `meta-whatsapp-adapters` `BroadcastSink`; OPEN_QUESTIONS #31):
  both over `Arc<WebhookEvent>`.
  - **Kind:** breaking (their types change).
  - **After:** nothing.
  - **Decisive:** ten subscribers share one allocation per event
    (`Arc::ptr_eq` across them); reverting to a clone per subscriber
    fails it.
- [ ] **L21c. `rediss://`** (`meta-whatsapp-adapters`, feature `redis`;
  OPEN_QUESTIONS #19): an explicit aws-lc-rs provider handed to the
  connection, never the process default; if redis-rs cannot take one,
  integrator-built connections stay the way, documented.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** live, with a TLS Redis in the test Compose and
    `META_WHATSAPP_RS_REQUIRE_LIVE=1`, a `rediss://` store connects in a
    binary linking both `ring` and aws-lc-rs with no process default
    installed (the case that panics today); handing the connection the
    process default instead makes that test panic.
- [ ] **L22a. `llms.txt` and a `doctor`** (docs; the service's CLI in
  `meta-whatsapp-server`; row 117): `llms.txt` for the docs, and a
  `doctor` command checking the configuration, the webhook subscription
  fields and the token's reach.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** `doctor` reports each scripted misconfiguration by
    name and exits non-zero; `llms.txt` links only files that exist (a
    test reads it).
- [ ] **L22b. A docs MCP server** (a tool under `tools/`, outside the
  library; row 117): search and read the docs and skills over MCP's
  stdio transport.
  - **Kind:** additive.
  - **After:** L22a.
  - **Decisive:** a scripted MCP client lists the tools, searches
    `Idempotency-Key` and gets the server guide's section; a missing
    page is an MCP error, not a panic.
- [ ] **L23. Template groups and archiving** (`meta-whatsapp-client`,
  `client::templates`; rows 101, 153): template groups' create, read,
  update and delete, after reading the reference Meta's changelog links
  (its guide is not in Meta's page list); archiving and unarchiving in
  bulk, after reading the endpoints `templates/template-management`
  points to (the mirrored `templates/template-archival` shows none).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON; a group id is checked as digits before any
    request.
- [ ] **L24. App deep links on template URL buttons**
  (`meta-whatsapp-client`, `client::templates::Button`; row 155): the
  Android deep link, its fallback URL and the Meta app id of
  `marketing-messages/deep-links`, which the service refuses today
  (coverage row 33).
  - **Kind:** breaking (a new field on `Button::Url`).
  - **After:** nothing.
  - **Decisive:** the page's request example round-trips byte for byte
    (as JSON values); dropping the field makes the service's
    lost-key check refuse it again.
- [ ] **L25. The launch option shape, checked in Meta's Integration
  Helper** (`client::embedded_signup`, OPEN_QUESTIONS #12): a
  verification before production relies on pre-fill; if the helper
  shows another shape, the serializer follows it.
  - **Kind:** additive if the shape holds; breaking if it changes.
  - **After:** nothing.
  - **Decisive:** the pre-fill test's expected JSON is the helper's
    output, recorded with its date; a serializer that departs from it
    fails the test.

## 4. Service milestones

Re-scoped on the modular architecture (design §8, §9). Each item's
routes are `meta-whatsapp-server-core` services exposed through both API
adapters (`meta-whatsapp-server-api-axum`, and
`meta-whatsapp-server-api-cratestack` once S10 exists; streaming routes
through `meta-whatsapp-server-http`), and run against every backend the
conformance suite covers. The acceptance tests M2.1–M5.3 are the
design's (§9).

### M2. Inbox, live updates, webhooks out

- [ ] **M2a. Inbox routes** (rows 91, 111, 119): conversation list,
  history, a reply checked against the window, all filtered by the
  number's binding epoch; retention per store (D10).
  - **After:** S7, L5, L7.
  - **Decisive:** M2.1; removing the binding-epoch filter shows a moved
    number's history to its new tenant, and a test fails.
- [ ] **M2b. Live updates** (row 10): SSE through an `EventNotifier`
  port (Postgres `LISTEN/NOTIFY`, memory broadcast; resume by
  `Last-Event-ID`); `GET /v1/events/{id}`.
  - **After:** S7.
  - **Decisive:** M2.2, M2.3, M2.5.
- [ ] **M2c. Webhooks out** (row 10): endpoints, dispatcher and retries
  (Standard Webhooks, a destination allow list).
  - **After:** M2b.
  - **Decisive:** M2.4.
- [ ] **M2d. The reviewed event types** (rows 84, 137, 139; D25):
  `standby_observed`, `thread_control_changed` and
  `user_action_reported` made tenant-visible, with L7's inbox answer to
  OPEN_QUESTIONS #44; the service's number events.
  - **After:** S7, L7.
  - **Decisive:** M2.6; taking one of the three types out of
    `TENANT_EVENT_TYPES` fails it.
- [ ] **M2e. Received media** (row 45; OPEN_QUESTIONS #43): a media id
  the service recorded as received on the tenant's number is asked from
  Meta without `phone_number_id`; every other id keeps the check; a live
  check against Meta records which behaviour Meta has.
  - **After:** S7.
  - **Decisive:** another tenant's received media id is `404` with zero
    requests (removing the "recorded on this tenant's number" lookup
    fails it); the live check's result is recorded in OPEN_QUESTIONS #43.

### M3. Embedded Signup, OTP, coexistence

- [ ] **M3a. Embedded Signup routes** (rows 120, 142): both partner
  modes, persisted attempts, resume, disconnection, credit lines in
  Solution Partner mode, and vault rotation over the records partner
  mode keeps past a binding. Companion: `docs/guides/production.md`
  gains the vault key cadence of OPEN_QUESTIONS #9 (at least yearly, at
  once on a suspected exposure, the old key dropped only when a rotation
  reports no failure).
  - **After:** S7, L4.
  - **Decisive:** M3.1–M3.4.
- [ ] **M3b. Coexistence** (rows 69, 124): onboarding, the automatic
  sync (D7), the synced contacts stored and readable by their tenant.
  - **After:** M3a, L8, L11d.
  - **Decisive:** a coexistence onboarding over HTTP starts the sync
    exactly once, and a second onboarding of the number does not;
    another tenant's key reads none of the synced contacts (`404`).
- [ ] **M3c. OTP and authentication templates** (row 98): OTP with
  per-tenant settings (the sending number, D8), authentication
  templates.
  - **After:** S7.
  - **Decisive:** M3.5.
- [ ] **M3d. Registration and the PIN** (rows 108, 128): request and
  verify a code, register, deregister; the PIN typed per attempt and
  never stored (D6).
  - **After:** S7.
  - **Decisive:** a sentinel PIN is found in no stored row and no log
    capture after a registration (storing it anywhere fails the test).
- [ ] **M3e. The token-expiry event** (row 6; OPEN_QUESTIONS #8): an
  event and a metric ahead of a stored token's lapse.
  - **After:** M3a, L11e.
  - **Decisive:** a token inside the lead time yields one event for its
    tenant and none for another; the metric counts it.
- [ ] **M3f. Hosted signup, several WABAs, pre-verified numbers** (rows
  121–123).
  - **After:** M3a, L11a, L11b, L11c.
  - **Decisive:** M1.3's table covers the new routes; with the
    multi-WABA option on, a signup granting two WABAs binds both to the
    tenant, and with it off, one.

### M4. Packaging

- [ ] **M4. Packaging** (rows 115, 118): the Docker image (D9: public
  GHCR, `meta-whatsapp-server`), its contents listed and the image's
  third-party licences listed in the image (no `ffmpeg`: L19), a Compose
  smoke test, the default API adapter's TypeScript client published
  (D11: npm; publishing is the owner's), the documents route. The client
  is S15's, generated by CrateStack; if the owner refuses D20 (a), it is
  generated from `openapi/v1.json` by openapi-typescript, the `api-axum`
  client of D26's swap.
  - **After:** M2c, and S15 unless the owner refuses D20 (a).
  - **Decisive:** M4.1–M4.3; the image's contents list matches the
    image (an extra binary fails it).

### M5. Routes for the modules parity requires

One PR per family, each a thin route over an existing library module
and each object id checked to be the path's number's or WABA's own
(architecture.md § Service). For every family: M5.1 and M5.2 over its
routes (M1.3's table covers each new `{pn}` and `{waba_id}` route by
construction; a route whose object id is not checked against the number
or WABA fails the family's own cross-tenant test).

- [ ] **M5a. The send union's other types** (rows 22, 29, 37, 51, 53,
  56, 57, 59, 60, 104): pin, request contact info, Direct Send,
  interactive carousels, voice call, location request, address, call
  permission request, Flow, product messages.
  - **After:** S7.
  - **Decisive:** M5.1, M5.2; each type's documented example is sent
    byte for byte; one left out of the send union fails its test.
- [ ] **M5b. Templates** (rows 95, 96, 99, 100, 101, 153, 155): edit,
  library, migrate, compare, unpause, archive and unarchive, template
  groups, app deep links, each kind of row 99 created through the
  service.
  - **After:** S7, L23, L24.
  - **Decisive:** M5.1, M5.2; creating each kind of row 99 from Meta's
    example passes with no key dropped.
- [ ] **M5c1. Media: resumable upload and conversion** (rows 47, 48):
  resumable upload; conversion on upload only when the deployment
  configures a converter (L19).
  - **After:** S7, L19.
  - **Decisive:** M5.1, M5.2; with no converter configured, an upload
    that needs conversion is refused as today, with zero requests.
- [ ] **M5c2. Profile and display name** (rows 64, 65): the profile
  picture (through a resumable upload handle), the display name change.
  - **After:** M5c1.
  - **Decisive:** M5.1, M5.2.
- [ ] **M5c3. Number settings** (rows 66, 109, 128, 129–133, 151): the
  settings, the username, the messaging limit tier on
  `GET /v1/numbers/{pn}`, the Official Business Account, compliance
  information, health status, search visibility, notifications, the
  contact book, conversational components.
  - **After:** S7, L9.
  - **Decisive:** M5.1, M5.2; `GET /v1/numbers/{pn}` reports the
    messaging limit tier (dropping it from the fields Meta is asked for
    fails the test).
- [ ] **M5c4. WABAs and accounts** (rows 126, 127, 140, 148, 152).
  - **After:** S7, L10a, L10b, L16.
  - **Decisive:** M5.1, M5.2.
- [ ] **M5d. Flows** (rows 61, 62): management and the data endpoint.
  - **After:** S7.
  - **Decisive:** M5.1, M5.2; the data endpoint answers a request
    sealed with Meta's example key, and refuses a bad signature.
- [ ] **M5e. Calling** (rows 74, 75, 134, 135).
  - **After:** S7, L14.
  - **Decisive:** M5.1, M5.2.
- [ ] **M5f. Groups** (rows 76, 77, 79, 81).
  - **After:** S7.
  - **Decisive:** M5.1, M5.2; a group id not in the number's groups is
    `404` with zero vault reads.
- [ ] **M5g. Commerce, QR codes, analytics, block users** (rows 71,
  102, 103, 106, 107).
  - **After:** S7, L17.
  - **Decisive:** M5.1, M5.2.
- [ ] **M5h. Marketing Messages API and CTWA** (rows 136–138, 154). The
  max-price agreement is its own admin route: an explicit, audited
  operator action, off until the deployment enables it; no other route
  signs it (OPEN_QUESTIONS #45).
  - **After:** S7, L13.
  - **Decisive:** M5.1–M5.3: with the setting off, the agreement route
    answers without a request to Meta; with it on, one request and one
    audit event naming the admin key; no other route sends the
    agreement.
- [ ] **M5i. In-App Signup** (row 125): accepting Meta's terms on a
  business's first signup is an explicit, audited operator action, off
  until the deployment enables it, never taken automatically
  (OPEN_QUESTIONS #26: the deployer's act).
  - **After:** S7.
  - **Decisive:** M5.1–M5.3: with the setting off, a signup that would
    accept the terms is refused with zero requests; with it on, the
    acceptance is audited; no request carries the acceptance unless the
    caller asked for it.
- [ ] **M5j. Partner APIs** (rows 143–147).
  - **After:** S7, L10b, L12.
  - **Decisive:** M5.1, M5.2.
- [ ] **M5k. The bot and broadcast APIs** (rows 17, 85–92), over B1–B4.
  - **After:** B1, B2, B3, B4.
  - **Decisive:** M5.1, M5.2; a broadcast over HTTP never exceeds its
    tenant's configured rate under a fake clock.
- [ ] **M5l. Conversation routing** (row 139): the thread control API.
  - **After:** S7, L15.
  - **Decisive:** M5.1, M5.2.

## 5. Payments (last)

- [ ] **P1. A `payments` client module** (`meta-whatsapp-client`; rows
  38, 141): India (UPI, payment links, order details and order status,
  onboarding) and Brazil (Pix, Boleto, payment links, one-click), typed,
  instead of `MessageContent::Raw`; the payment buttons of templates
  (`order_details`, `payment_request`).
  - **After:** nothing.
  - **Decisive:** exact JSON from `payments/payments-in/*` and
    `payments/payments-br/*`; the payments pages' webhook examples join
    the conformance sweep (their manifest status moves from `OutOfScope`
    to `Typed`).
- [ ] **P2. Payment routes in the service** (rows 38, 141).
  - **After:** P1.
  - **Decisive:** M1.3 over the new routes; an order-details send
    carries the tenant's token.

## Done criteria for parity

- Every row of [parity.md](parity.md) that is not n/a says done for the
  library, and done or n/a for the service.
- [categories.md](categories.md) says done for every category, or gives
  the reason a part stays out (an n/a of the parity table).
- `just ci` exits 0 on `main` at that commit, with the live tests
  forced (`META_WHATSAPP_RS_REQUIRE_LIVE=1`).
- The parity-completion report goes to the owner, with everything
  decided on the way that is not an owner touchpoint, and every choice
  still left to the owner: the entries left open in
  [OPEN_QUESTIONS.md](../OPEN_QUESTIONS.md) (on 2026-09-26, #33) and
  design D5's production mode.

## Owner touchpoints

What waits for the owner before parity (AGENTS.md § Decisions);
everything else goes into the parity-completion report.

- **D20 (a)**, the BlueOak-1.0.0 licence of `minicbor`: asked at S10 as
  one yes or no question. It blocks the CrateStack adoption (S10–S16)
  and nothing else: after a refusal, `api-axum` and `store-postgres`
  stay the defaults, and M4 publishes the client openapi-typescript
  generates.
- **The merges of U1–U3** in the owner's cratestack repository. S16's
  default flip waits on U1.
- **Publishing**: to crates.io (parity row 115) and to npm (D11, M4).
  Crate and package names and versions are permanent, and the tokens
  are the owner's.
- **D21, D22 and D24**, confirmed before the first release: after it,
  D21's per-tenant sequences are a released contract, and D22's and
  D24's deletions cannot be undone.
