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
  [design §10](design/server.md#10-decisions) (D26–D36 and the rows
  updated on 2026-09-26) and in
  [OPEN_QUESTIONS.md](../OPEN_QUESTIONS.md), under the rule in AGENTS.md
  § Decisions. What stays the owner's is listed in
  [§ Owner touchpoints](#owner-touchpoints).
- **Routes.** Every new route calls a core service, so both API adapters
  stay thin, and lands in both adapters in the same PR from S13 on
  (**Coordinator's decision 2026-11-02 under the owner's delegation,
  swappable**: S10 made `meta-whatsapp-server-api-cratestack` with one
  procedure, served by nothing, so the rule starts when S13 serves it;
  to swap, start it at another item). A route that lands before S13
  goes into `meta-whatsapp-server-api-axum`, and S13 carries it over: its
  groups cover every route that adapter has when S13 starts. `api-axum`
  is permanent, the swap target (design D26).

## Order

| Wave | Items (in parallel only where their `after:` lines allow; at most three building at once) |
| --- | --- |
| 0 | this plan (parity, categories, roadmap, decisions) |
| 1 | S1 (server core), B1 (bot framework), U4 (upstream issues) |
| 2 | S2, S2b, S3, S4, S8; U1–U3 in the cratestack repository; L4, L5, L7 (the `ConversationStore` port change and the inbox's use of it, before M2); the library batches L8–L26, which may start here and run alongside every later wave; B1b, B1c (the bot framework's follow-ups) |
| 3 | S5a–S5e, S6, S7, S9 |
| 4 | S10 (D20 (a): the owner's yes of 2026-11-02), S11, S12; M2a–M2m |
| 5 | S13–S16 (S16 last, gated on U1's merge); M3a–M3g; B2–B4 |
| 6 | M4; M5a–M5l |
| 7 | S17, S18 (MongoDB) |
| last | P1a–P1d, P2, P2b (payments; P1d and P2 done) |

## 1. The service's modular split (design §8, D26)

From the CrateStack fit study of 2026-09-26 and the architecture review
of the core's extraction: the unit of swapping is a backend bundle and
an API adapter over one framework-free core. First the core and its
ports are made right for any backend (S1–S6), then the crates split
(S7), then MongoDB (S8, S9, S17, S18) and CrateStack (S10–S16) are
added behind them. `/v1` stays served, and `openapi/v1.json` stays
byte-identical, until the default flips (S16); after it,
`meta-whatsapp-server-api-axum` keeps both, as the swap target. D20
(a), CrateStack's licence, was decided by the owner on 2026-11-02: yes,
so the CrateStack defaults of D26 hold once S16 flips them; until then
the defaults stay `api-axum` and `store-postgres`.

- [x] **S1. Extract `meta-whatsapp-server-core`** (new crate, depending
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
  - **Landed:** the crate, its ports and the `Backend` bundle, with the
    memory and Postgres implementations in `meta-whatsapp-server`;
    `openapi/v1.json` unchanged. Row 112 stays partial on the service
    (conformance in core: S3; composed from a bundle alone: S4). Its
    security review's fixes landed with it: capabilities (`Caller`,
    `AdminCaller`, `OwnedNumber`, `OwnedWaba`) work only with the
    `Authorizer` that made them (SR-H1: the core's methods refuse
    another's, the server's extractors admit what they take from a
    request, and its handlers are crate-private), the Graph client it
    is given carries no token (SR-L4), `AppState::store` is
    crate-private (SR-M1), `Debug` never prints a kept answer's body
    (SR-L3), and the visibility pins are UI tests with the compiler's
    errors (SR-L1).
    SR-L2, and the same race in `failed`, wait for S2.
- [x] **S2. The port contracts, right for any backend**
  (`meta-whatsapp-server-core` and both in-tree backends): what Postgres
  guarantees by accident becomes a requirement of the ports, cheap now
  and costly once a third backend exists.
  - `Outbox::insert` must re-check the routing: when the event has a
    tenant, the row keeps it only if, atomically with the insert, the
    binding it was routed by still names that tenant and began in a
    second before the event's (its security review's L3: an event of
    the binding's own second may be the previous holder's); otherwise
    the row is operator-only. A typed
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
  - Unbind and vault delete conditioned on the binding and the vault
    record version the capability was made from (SR-L2, from the core's
    security review): `OwnedWaba::forget` deletes whatever the WABA's
    binding and token became since the `OwnedWaba` was made, so a WABA
    unbound and attached again in between (a new token, even another
    tenant's binding) loses them. The unbind and the delete take what the
    capability was made from, and do nothing when either moved.
  - The `reconnect_required` marking conditioned the same way (the
    `failed` race, which the core's second review found):
    `OwnedNumber::failed` and `OwnedWaba::failed` mark the numbers of
    whatever binding the WABA has when Meta's `190` comes back, so a
    capability made before the WABA was unbound and attached again (a
    new token, even another tenant) marks the new binding's numbers. The
    status update takes the binding the capability was made from (its
    tenant and `attached_at`), and marks nothing when it moved.
  - **After:** S1.
  - **Decisive:** on memory and, live, on Postgres: `bind_waba` to a
    missing tenant answers `NoSuchTenant` (memory binds it today); an
    insert whose guarded binding moved is operator-only (removing
    memory's re-check fails it); a backend reporting the typed busy
    error makes the webhook path answer `503`; an `OwnedWaba` made
    before its WABA was attached again forgets neither the new binding
    nor the new token; an `OwnedNumber` or `OwnedWaba` made before its
    WABA was unbound and attached again, to the same tenant and to
    another, whose call Meta then answers `190`, leaves the new
    binding's numbers `connected` in both cases (conditioning the update
    on the tenant alone fails the first, on nothing both).
  - **Landed:** every bullet, in the core's ports and both in-tree
    backends, with the decisive tests on memory and live on Postgres
    (the store, events, backend and capability suites of the server's
    `tests/common`). The route guard is `outbox::RouteGuard` (a
    `GuardedBinding` and the event's date), set by `events::outbox_row`
    from the routing's `Holder`; a tenant without a guard is
    operator-only, and the memory outbox belongs to its `MemoryStore`
    and checks under the store's lock. `BindOutcome::NoSuchTenant`;
    `bind_waba` locks the tenant `FOR KEY SHARE` on Postgres. The purges
    return a count and take no lock; `serve::round` takes one
    `HOUSEKEEPING` turn, leased for `HOUSEKEEPING_LEASE` (5 minutes; on
    Postgres the turn's session is ended past it, and the key is still
    `HOUSEKEEPING_LOCK`). `Authorizer::with_clock`, which the service
    hands the webhook pipeline's clock. `StorageError::Busy` in the
    library (additive). `PhoneNumberId` and `WabaId` in the outbox
    types. SR-L2 and `failed`: a `BindingEpoch` and, for an `OwnedWaba`,
    the vault record's `TokenVersion` (the library's
    `TokenVault::get_versioned` and `delete_if_unchanged`, additive);
    `RecordStore::unbind_waba_if` and `set_waba_status_if`; and a
    capability is made only from a token read while its binding held
    (the binding read again after the vault, `503` when it moved: the
    review found the old holder's binding paired with the new holder's
    token). The review's races (`race_suite`, `racing_a_reattach`, two
    purges at once) run on memory and live on Postgres. Its security
    review's remediation: a capability answers for the vault's read
    (a token, none, a failure) only once its binding is read again
    (`Authorizer::open`: a move before the new holder's token is
    stored is `503`, not `number_not_connected`, on which the admin
    unbind deleted the new holder's binding); a tenant's deletion skips
    a WABA that moved since its listing instead of answering `503`;
    `OwnedWaba::still_bound` is asked before every `unsubscribe_app`
    with a capability's token (the tenant's disconnection, the admin
    unbind, a tenant's deletion), and `forget` warns when it finds the
    binding moved (the Graph call's own window); the port rule is
    written down (`server_core::store`: the vault is written for a WABA
    only by its current holder, an attach binds, stores, then confirms,
    every unbind deletes the token before the binding), and the attach
    follows it (`Authorizer::store_token` reads the binding, stores with
    the library's `TokenVault::store_versioned`, additive, and reads the
    binding again: moved, it takes back exactly its own write with
    `delete_if_unchanged` and answers `503`; the remediation found an
    attach overwriting, with its token, the token of a tenant the WABA
    moved to between its binding and its store); the vault's
    conditional delete links again the numbers a record stored since
    lists (L2); an event of its binding's own second is operator-only
    (L3); a contact sync is dated when its webhook was triggered (M2's
    first half); `StorageError::Busy`'s docs say a step storing after a
    send is not repeated on it. Left, and why: a binding refreshed for
    its own tenant (attached again without an unbind, as the operator's
    reconnect or token rotation does) keeps its epoch, so a `190`
    answered after the refresh to a capability made before it marks its
    numbers again (the vault version covers `forget` there, not
    `failed`; `attached_at` cannot move, the route guard reads it): S2b,
    a bind generation; undated events (errors, history chunks, undated
    group updates) of a WABA's previous holder, which Meta redelivers
    for up to 7 days, reach its new holder after a move to another
    tenant: S2b (D36); a skew margin around `attached_at`: S2b; the
    attach's confirmation narrows a window it does not close: another
    tenant's attach landing whole between an attach's read of its
    binding and its store has its token overwritten, then taken back
    with the first attach's (its binding left with no token until it
    attaches again), and a capability of its own made in between
    carries the first attach's token; closing it needs the vault record
    to name the binding it was stored under, a change of the stored
    format, which is the owner's (planned nowhere yet); a disconnection
    racing a vault rotation deletes nothing and answers `503`
    (retryable); the admin's unbind
    of a WABA without a usable token, and the admin's own `190` right
    after attaching, stay unconditioned (no capability was made from a
    binding); the memory stores read the system clock, not the injected
    one (one process).
- [x] **S2b. Undated events after a move, and the bind generation**
  (`meta-whatsapp-server-core` and both in-tree backends; one PR with
  both schema changes, on both backends, and an additive migration: a
  new migration file, never an edit to one on `main`). S2's security
  review (M2) and sabotage review (the same-tenant refresh). Rows 9 and
  112: event routing (the service's side is partial until this lands,
  as a previous holder's undated events reach a new holder after a
  move), and the ports' contracts.
  - Record when a WABA was last unbound from a different tenant: a
    store field on both backends.
  - A setting `undated_after_move`: `OperatorOnly { window }`, the
    default, with a window of 7 days and an hour (Meta's retry window
    plus the dedup margin); `CurrentHolder`, the behaviour before it.
    Undated tenant-visible events within the window are operator-only.
    A first onboarding (no previous tenant) is unaffected (design D36).
  - A skew margin setting around `attached_at` for dated events,
    applied only when a previous tenant exists (the second half of S2's
    L3; S2 made an event of the binding's own second operator-only).
  - The same-second rule, strict since S2 (an event of its binding's
    own second is operator-only), relaxed to `<=` when the WABA has no
    previous tenant: a first onboarding keeps the events of its first
    second. It lands with the tombstone (the record of the last
    unbinding from a different tenant), which is what tells the two
    apart.
  - A bind generation carried in `BindingEpoch` (a new column,
    incremented on every attach, a same-tenant refresh included), so
    that a capability made before a refresh is told apart from one made
    after it; the route guard keeps comparing `attached_at`.
  - **After:** S2.
  - **Decisive:** after a move A→B, an undated `history_synced`
    redelivered within the window reaches no tenant; with
    `CurrentHolder` it reaches B; a WABA with no previous tenant still
    gets its history; removing the check fails the first case; the
    same for the security review's other undated cases (an error, an
    undated group update); an event of its binding's first second
    reaches the tenant of a first onboarding, and no tenant after a
    move; a `190` from a capability made before a same-tenant refresh
    does not mark the numbers `reconnect_required`.
  - **Landed:** every bullet, in the core and both in-tree backends,
    with the decisive tests on memory and live on Postgres. The
    tombstone is `model::WabaTombstone` (its tenant, `None` once that
    tenant is deleted, when, and the unbound binding's own record),
    written with every unbind in the same step and read by a new
    binding into `WabaBinding::previous_tenant_unbound_at` (the
    unbinding's time when another tenant left, else the time it
    carried; a refresh keeps it), the window measured from it. The
    settings are `events::MoveRules` and `events::UndatedAfterMove`
    (`Inbound::with_move_rules`; the service's
    `WA_SERVER_UNDATED_AFTER_MOVE`, `…_WINDOW`, 169 h, and
    `WA_SERVER_MOVE_SKEW_MARGIN`, 0 s by default). The rules are one
    function, `outbox::RouteGuard::admits`, which the routing
    (`events::owner`, `after_move` its new reason for an undated event
    held back) and the memory outboxes apply, and whose bounds
    (`RouteGuard::dated_bounds`, `AfterMove`) the Postgres insert binds:
    it picks between them by the binding's row, under the same `FOR KEY
    SHARE` lock. The generation is `WabaBinding::generation`, compared by
    `BindingEpoch` in every conditioned write and capability check.
    Postgres: migration 4 (`0004_waba_moves.sql`, additive), whose
    defaults give the rows written before it generation 1 and a move
    recorded as unknown (`UNRECORDED_MOVE`, the Unix epoch), so they
    keep S2's rules; its names are stable identifiers pending the
    owner's confirmation (§ Owner touchpoints). The decisive tests:
    `conformance::webhooks::undated_events_after_a_move_reach_no_tenant_within_the_window`
    (a history chunk, an error and an undated group update: no tenant
    within the window, B under `CurrentHolder` and past it, a first
    onboarding always) and
    `an_event_of_its_bindings_first_second_reaches_a_first_onboarding_only`,
    the service's `an_event_of_its_bindings_own_second_reaches_a_first_onboarding_only`
    over HTTP, and `capabilities::failed_before_a_refresh_marks_nothing`;
    the records suite's `a_move_is_recorded` and
    `a_refresh_moves_the_generation`, and the invariants' guard cases
    (`a_first_onboarding_keeps_its_first_second`,
    `the_skew_margin_applies_after_a_move_only`,
    `an_undated_event_after_a_move_waits_out_the_window`), which the
    memory crate also shows an outbox bending the guard fails. S7's
    continuity test now migrates `main`'s database forward (migrations
    1 to 3 unchanged, 4 added once, none on `serve`) and routes `main`'s
    binding as before. By mutation: the window check removed from
    `RouteGuard::admits` fails its unit test, the webhooks case (a
    history chunk within the window reaches B) and the invariants'
    undated case on memory, and removed from the Postgres insert, that
    case live; the first onboarding's exemption removed, dated, fails
    the unit tests, the first-second cases through the core's pipeline
    and over HTTP, and `a_first_onboarding_keeps_its_first_second` on
    memory and live; undated, the routing suite's error of a first
    onboarding and the invariants' undated case, on memory and live; the
    generation not moved on a refresh fails `a_refresh_moves_the_generation`
    on memory and live, and `failed_before_a_refresh_marks_nothing` run
    alone; the skew margin applied without a previous tenant fails its
    unit test and the margin case on memory and live.
    `openapi/v1.json` and the event data snapshots are unchanged.
    Left, and why: an unbind made by a binary of an earlier release
    leaves no tombstone, before the upgrade as during its rolling
    deploy, so a WABA unbound that way and bound afterwards to another
    tenant reads as a first onboarding, and its previous holder's
    undated events reach the new holder for what remains of the window
    (as before S2b); a move made wholly before migration 4 keeps S2's
    rules, which give them to the new holder too; and a replica of the
    earlier release unbinding after a tombstone was written leaves that
    older tombstone in place, so the window runs from an earlier
    unbinding (the guide says not to move WABAs between tenants in the 7
    days and an hour before the deploy, nor during it); the window runs from the previous tenant's
    unbinding, not from the new attach, so an undated event of the
    interval between them is the new holder's once the window from the
    unbinding passed; a moved WABA's new holder loses its own undated
    events within the window (its history sync among them, when it
    onboarded within it: D36's cost); a binding made before migration 4
    keeps S2's strict first second for good, until it is unbound; the
    window and the margin are per deployment, not per tenant.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each a setting, or behind the
    `RecordStore` port and its conformance suite; recorded in design
    D36): the window runs from the previous tenant's unbinding, not from
    the new attach; the skew margin defaults to `0s`; a binding made
    before migration 4 reads the Unix epoch (`UNRECORDED_MOVE`), not
    NULL, so it keeps S2's rules; a tenant deleted and created again
    under its id counts as another tenant; the generation starts at 1
    for every binding; the undated check sits in `events::owner` and
    `RouteGuard::admits` and covers every undated event; a capability
    or an attach meeting a refresh answers `503 storage_unavailable`,
    retryable; tombstones are never purged (one row per WABA ever
    unbound). **Pending the owner** (stable identifiers): the names of
    migration 4's columns and table, its description, and the epoch
    default's meaning (§ Owner touchpoints).
  - **Sabotage review** (isolation, the migration, the routing rules):
    on Postgres a new binding's `attached_at` was its transaction's
    start, which can precede the previous tenant's unbinding when that
    commits while the bind runs, so an event Meta dated while the
    previous tenant still held the WABA fell in a second the new binding
    began before (the memory backend never did): the bind now moves it
    past `clock_timestamp()` and the tombstone's time, and the race
    suite's `a_binding_racing_an_unbind_records_the_move` (which failed
    3 live runs of 3 before) holds every backend to it. An attach
    meeting a refresh of its own tenant took back the token it stored,
    which could leave the binding with none while the other attach
    answered success: it now keeps it (`503` still,
    `capabilities::an_attach_meeting_a_refresh_keeps_its_tenants_token`).
    A skew margin reaching before the Unix epoch failed every dated
    insert on Postgres (`to_timestamp` out of range, folded at plan
    time): `RouteGuard::dated_bounds` stops at the epoch. Added to the
    suites: undated rounds in the insert-against-a-move race (a tenant
    created again under its id kept out by the window alone),
    `capabilities::a_capability_made_while_its_binding_is_refreshed_is_refused`,
    a WABA moved twice in `records::a_move_is_recorded`. The rolling
    deploy's gap is wider than first written (an unbind by an earlier
    release before the upgrade is not recorded either): Left, above.
- [x] **S3. Conformance into core** (`meta-whatsapp-server-core`,
  `meta_whatsapp_server_core::conformance`, a feature, like the
  library's `store::conformance`, run over a `&dyn Backend`): the store
  and events suites moved out of the server's tests, the cross-port
  invariants (deleting a tenant purges its stream; a binding changed
  during an insert leaves the row operator-only; per-tenant commit
  order) and S2's rules, the `RouteGuard` cases among them (S2's
  security review, L4: a third-party outbox that ignores the guard
  fails open until then, and must fail the suite). Row 112 (conformance
  in core).
  - **After:** S2.
  - **Decisive:** removing the memory backend's re-check fails the
    binding invariant; removing the stream purge from `delete_tenant`
    fails on memory and, live, on Postgres; a `Backend` whose `kv()`
    returns a new store on each call fails the same-data case.
  - **Landed:** `meta_whatsapp_server_core::conformance` behind the
    feature `conformance`: `conformance::run` takes a factory of empty
    backends and runs, each on a backend of its own, `records` (the
    server's store suite: tenants, keys, bindings, idempotency, S2's
    referential rules, byte order), `outbox` (its events suite's
    single-port cases), `invariants` (what crosses ports: its backend
    suite's same-data case, deleting a tenant purges its stream, the
    three `RouteGuard` cases, and each tenant's inserts committed in
    sequence order under eight concurrent writers, a case new on every
    backend, which was a Postgres-only HTTP test), `capabilities` (its
    capability suite, through the core's `Authorizer`) and `races` (its
    race suite and `racing_a_reattach`). The server's
    `tests/common/*_suite.rs` are gone; `tests/store.rs` runs the whole
    of it on memory, and `tests/live_postgres.rs` live on Postgres
    (`live_postgres_backend_passes_the_conformance_suite`, a schema per
    suite). The decisive cases, by mutation on memory: without the
    memory outbox's re-check, `invariants::the_route_guard_is_checked_with_the_insert`
    fails; without `forget_tenant` in `MemoryStore::delete_tenant`,
    `invariants::deleting_a_tenant_purges_its_stream`; with a `kv()`
    made anew per call, `invariants::every_accessor_hands_out_the_same_data`.
    Kept as tests: a backend whose `kv()` is new on each call fails the
    same-data case and the whole suite, an outbox that ignores the route
    guard fails the guard's cases and `invariants::run` (L4), and three
    outboxes that bend the guard's date (dropped, dropped for a number's
    guard, read a second late) each fail the whole suite on the case of
    their own, so that a guard case taken out of `run` fails a test. Its
    sabotage review ran the live half on a Postgres 16 of its own: it
    passes, and there removing the `purged_through` update from
    `delete_tenant` fails `deleting_a_tenant_purges_its_stream`, a
    sequence drawn in a transaction of its own fails the commit-order
    case (on memory too: a row stored after an await), and dropping the
    WABA from a number's guard, or reading a guard's date a second late,
    fails the guard's cases. The Postgres-only tests (locks across
    sessions, two purges at once, a deadlocked insert) stay in the
    server's tests. Left: a case taken out of `run` other than the
    same-data and guard cases (the deletion's purge, commit order, the
    records', outbox's, capabilities' and races' cases) fails no test,
    as no permanent backend breaks only it.
- [x] **S4. Composed only through the bundle** (`meta-whatsapp-server`,
  `meta-whatsapp-server-core`): `serve_with(config, Arc<dyn Backend>)`;
  `AppState::from_backend(Arc<dyn Backend>, …)` building the inbound
  pipeline inside; housekeeping and the sweep in core over the bundle;
  the CLI through a backend factory. Row 112 (a backend an integrator
  brings).
  - **After:** S3.
  - **Decisive:** a test serves the API over a `Backend` defined outside
    the server crate, with no edit to `serve.rs`; a doctest that builds
    the state from loose ports fails to compile.
  - **Landed:** `AppState::from_backend(Arc<dyn Backend>, VaultKeys,
    Client, Inbound, Metrics, Settings)` is the state's only constructor
    (`new` and `with_settings`, which took the stores one by one, are
    gone): it builds the vault on the bundle's `KvStore` and the webhook
    pipeline on its `KvStore` (dedup), `ConversationStore` (the inbox),
    `Outbox` and records; `Inbound` keeps the app secrets, the verify
    token and the clock, and no store. `serve::serve_with(config,
    Arc<dyn Backend>)`, and `serve_until`, the same with a shutdown
    future of the caller's, migrate (unless `WA_SERVER_MIGRATE=skip`),
    serve, run housekeeping and close over the bundle, and read no
    `config.storage`; `serve::serve_from(config, factory)` opens the
    backend a factory gives for `config.storage` and serves it
    (`serve::serve`: the in-tree factory). Housekeeping is the core's,
    `housekeeping::run` and `round` over a `Backend` (`serve::Sweep`, and
    a round without a leader lock, are gone; a zero interval is taken as
    a millisecond, and no round starts once `stop` completed). The
    command line opens every backend through a
    `serve::BackendFactory` (`BuiltinBackends`: Postgres, memory;
    `cli::run_with` takes an integrator's), `serve::open` migrating per
    `WA_SERVER_MIGRATE`. The server's test harness serves its stores
    through a `Backend` of its own too. Decisive: `tests/composed.rs`
    serves the API over real listeners on a `Backend` defined in the test
    (memory's ports in a struct of its own, counting what it is asked),
    with no change to `serve.rs`: the API reads and writes its records,
    the vault rotation re-encrypts a token stored in its `KvStore`, Meta's
    webhook takes its dedup lease there and lands in its inbox and its
    outbox, housekeeping sweeps through its janitor, and it is migrated
    at start and closed at shutdown. The other half is four UI cases in
    the server's `tests/visibility` rather than a doctest (they pin the
    compiler's error, which a `compile_fail` doctest cannot on stable):
    `app_state_from_loose_ports` (a `MemoryStore` is not a `Backend`,
    E0277), `app_state_new` (no `new`, no `with_settings`, E0599),
    `inbound_stores` (the pipeline takes no store, E0061) and
    `app_state_backend` (the state's backend is crate-private, as its
    records are, E0624). By mutation: `serve_with` and `serve_until`
    taking an `Arc<MemoryBackend>` fail `tests/composed.rs` to compile;
    the outbox, the dedup store, the inbox, the vault's store or the
    records taken past the bundle, housekeeping on another backend, the
    migration skipped and the close left out each fail an assertion of
    their own; `impl Backend for MemoryStore` makes
    `app_state_from_loose_ports` compile, and a loose `AppState::new`
    again makes `app_state_new` mismatch. The sabotage review added the
    cases four mutations survived: migrating whatever
    `WA_SERVER_MIGRATE` says (`tests/composed.rs`, a second case),
    `serve_with` alone taking an `Arc<MemoryBackend>` (a compile-only
    use there), housekeeping polling its tick before `stop` and a zero
    interval panicking in the timer (`tests/housekeeping.rs`). It also
    fixed a race older than S4: the `SIGTERM` handler was registered
    only once the listeners were spawned, so a `SIGTERM` as they began
    answering ended the process (signal 15, seen once in
    `binary::development_on_memory_prints_a_one_time_admin_key`); it is
    registered before they are bound (`serve.rs`'s unit tests
    `sigterm_is_caught_before_the_signal_is_awaited` and
    `the_stop_signal_is_made_before_the_listeners_are_bound`).
    `openapi/v1.json` unchanged; the
    server's suite passes on memory and, live, on Postgres. Row 112's
    service side stays partial for S2b, which changes the ports'
    contracts again. Left: `cli::run_with` is tested with the in-tree
    factory only (the binary's tests; the admin commands read the
    process's environment, which a test cannot set); a factory is given
    the configuration's `Storage`, whose database variant is
    `Postgres(DATABASE_URL)`, so another database reads its URL from
    `DATABASE_URL` until the configuration picks among the backends
    compiled in (design §8.3: S7 composes them by Cargo features, and
    S18 is the first to need the choice).
- [x] **S5a. Admin services into core** (`meta-whatsapp-server-core`;
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
  - **Landed:** `meta_whatsapp_server_core::admin`: every `/v1/admin`
    operation is a method of `admin::Admin`, which `Admin::new(&Authorizer,
    &AdminCaller)` opens only once the `Authorizer` admits the admin
    (`403 forbidden` for another's, nothing read), over that
    `Authorizer`'s records, vault and tokenless client (the records
    through a crate-private `Authorizer::records`, so no adapter reaches
    them, and what a service writes is what the order reads; the core's
    UI case `authorizer_records` pins it, E0624, and fails with the
    method made `pub`). The
    operations take the request's values as it carried them (ids,
    labels, an RFC 3339 expiry, `admin::TenantsInput`, `admin::KeyRequest`),
    validate them in the order and on the fields the handlers did, and
    answer domain values (`Tenant`, `Listing`, `admin::Minted`,
    `admin::Attached`, `admin::HeldWaba`, `VaultRotation`) or a
    `ServiceError`: the tenant CRUD, the tenant's deletion (five rounds,
    a moved WABA skipped), the key mints, listings and revocations,
    the attach (D4 on the claimed id before Meta, the listing capped at
    `admin::MAX_WABA_NUMBERS`, bind, `Authorizer::store_token`, subscribe
    with `step` and `resumable`), the binding's read, the unbind
    (`Admin::unbind_waba`, and `Admin::unbind` on a binding as read) and
    the vault's rotation. Their audit events are the core's
    (`server_core::audit`, target `audit`, the same fields; the server's
    `telemetry::audit`, `tenant_audit` and `Subject` re-export them), and
    a failed Graph call is told to an `observe::Observer`, which the
    server's `Metrics` implements (`wa_server_graph_errors_total`, the
    same codes as before, the attach's listing counted before its `422`
    remap). `admin::mint`, `admin::MintError`, `admin::allowed_tenants`
    and `admin::is_valid_name` need no capability: the command line's
    bootstrap uses them on the database directly (and the server's
    `api::admin::mint`, `MintError` and `MAX_WABA_NUMBERS` re-export
    them); `OwnedWaba::forget_or_busy` is the core's too. The handlers
    parse, call and map; `auth::OwnedWaba::unbind_for_admin` calls
    `Admin::unbind`. Decisive: `conformance::admin` (in `conformance::run`)
    attaches a WABA through the service, then has another tenant attach
    it, with a `ScriptedTransport` and no HTTP layer: refused `409
    waba_owned_by_another_tenant` before Meta is asked (no request
    sent), and, for a WABA its holder binds while Meta lists its numbers,
    by the binding's own D4 (the listing the only request: no
    subscription); either way the vault's record is the holder's at the
    same version, the bindings and numbers unchanged, and the claimant
    holds nothing. It runs on a memory backend of its own in the core's
    `tests/admin.rs`, on the service's `MemoryBackend` (`tests/store.rs`)
    and live on Postgres (`tests/live_postgres.rs`). By mutation, each
    fails it on the core's backend and on memory: the check before Meta
    skipped, or moved after the vault store (`502 service_unavailable`:
    a listing sent); the bind moved after the store, alone or with the
    check (the holder's own attach answers `503`: the store's
    confirmation finds no binding); the bind's refusal ignored (`503`,
    the store's confirmation the only guard left; also live on
    Postgres), a mutation the admin routes' HTTP tests do not catch;
    both checks skipped. The admin routes' tests pass unchanged, and
    `openapi/v1.json` is byte-identical (the handlers' documentation and
    annotations untouched). Left: the server's source scan of error
    codes counts fewer (16, its floor lowered from 20; the core's raised
    to 30); an expiry is compared with the system clock, not the
    `Authorizer`'s, as before; the command line's key listing and
    revocation (any kind of key, every page) stay its own, as no route
    does either; the log lines that moved carry the core's module as
    their target (a `RUST_LOG` directive naming the crate still matches
    them, one naming the server's module does not: CHANGELOG). The
    sabotage review pinned by test what no test held and a mutation
    showed: each admin change's own audit event, its fields, and none
    on a refusal or a failure (the service's `tests/admin_audit.rs`);
    Meta's failures counted by code, the attach's listing among them
    (`the_admin_operations_count_metas_failures`); `Admin::new` refusing
    another `Authorizer`'s admin with nothing read (the core's
    `tests/capabilities.rs`); a platform key's scopes; a missing
    tenant's key listing, and its attach answered `404` before Meta is
    asked.
- [x] **S5b. Idempotency and messages into core**
  (`meta-whatsapp-server-core`): `idempotency::run` answering a status,
  a body and whether it replayed; sending (the accepted message, not an
  HTTP status), marking read, the recipient and E.164 rules, over a
  request type free of utoipa.
  - **After:** S4.
  - **Decisive:** called with no HTTP layer, a scripted timeout reports
    `may_have_been_sent` and the same key replays it with no second
    request (M1.4's case); `openapi/v1.json` byte-identical.
  - **Landed:** `meta_whatsapp_server_core::idempotency::run(&Keeper,
    &Authorizer, &Caller, key, &Fingerprint, operation)` runs an
    operation under a key and answers an `idempotency::Answer` (a
    status, a body, and `replayed`) or a `ServiceError`: the claim with
    its lease and expiry, the answer kept byte for byte, a failure kept
    as the adapter renders it when it may have been sent (the `Keeper`'s
    `RenderError`) and the key released when it proves nothing was, a
    repeat's outcome told to the `Observer` (its new method
    `idempotency`, which the server's `Metrics` counts as
    `wa_server_idempotency_total`, as before), and the `504 timeout` a
    cut request keeps (the `Claim`'s drop). It takes the tenant from a
    `Caller` its `Authorizer` admits (another's is `403 forbidden`,
    before the key is read), not from a bare `TenantId`. The fingerprint
    is unchanged (method, path, body: S6 changes it). The messages are
    `meta_whatsapp_server_core::messages`: the send request as the
    request carried it, free of utoipa (`messages::SendMessage` and its
    objects), `refuse_unsupported` (Direct Send and the types the
    service does not send, on a JSON body), `message` (the library's
    `OutboundMessage`, checked in the order and on the fields of
    before), `recipient` and `is_e164` (strict E.164 with its `+`), and
    the `Messages` service on an `OwnedNumber` (opened with
    `Messages::new`, `403 forbidden` for a number another `Authorizer`
    made, `Authorizer::admit_number`; `send`, answering
    `messages::Accepted`, the accepted message, not a status; and
    `mark_read`, with or without the typing indicator), its failed Graph
    calls told to the `Observer` (`wa_server_graph_errors_total`, the
    same codes). `request::meta_object` (a Meta object read with no key
    dropped) moved there too; the server's `api::common::meta_object`
    wraps it for template creation. The server keeps its request types,
    with their OpenAPI schema, reads the body into them and converts
    them into the core's field by field (every field named on both
    sides, so a field on one side only fails to compile, and one
    destructured but dropped is `-D warnings`' unused variable; a test
    reads each of the core's type names into the server's enums); its
    `idempotency::run` (crate-private still, now taking the `Caller`)
    hands the core's the service's records, settings, §5.1 error body
    and metrics, and renders the answer (`Idempotent-Replayed: true` on
    a replay, a kept error's code tagged for the metrics). Decisive:
    `conformance::messages` (in `conformance::run`), with a
    `ScriptedTransport` and no HTTP layer: a digits-only phone number
    refused on `to.phone` before any request; a scripted timeout `504
    timeout` with `may_have_been_sent`, sent once with the vault's
    token, and the same key's repeat its kept answer, `replayed`, byte
    for byte, with no second request (`remaining() == 0`, one request
    recorded); a `131047` `409 customer_service_window_closed` that
    releases its key, the same key then sending and its success
    replayed; another `Authorizer`'s `Caller` for the same key refused
    `403` with nothing sent. It runs on the core's own memory backend
    (`tests/messages.rs`, the admin suite's records moved beside it into
    `tests/memory`, with idempotency records that lease and expire), on
    the service's `MemoryBackend` (`tests/store.rs`) and live on
    Postgres (`tests/live_postgres.rs`). By mutation, on the core's
    backend: the operation run without claiming the key (no lease), and
    a failure that may have been sent released instead of kept, each
    fail it at the replay (a second send, which nothing answers; the
    latter also live on Postgres); every failure kept, a `131047`
    included, fails the released key's send; the caller's admission
    dropped fails its last step (the other authorizer's caller gets the
    tenant's kept answer); a phone number accepted without its `+`
    fails its first. The review (mutation-driven, its own lens) found
    what moved and nothing pinned: the handlers' and the core's
    `Observer` calls dropped, `Metrics`' `idempotency` emptied, the
    replay's code tag dropped, the lease and the TTL swapped, a blank
    message id let through, the caller admitted only with a key, and an
    interactive footer, a contact's birthday or an accepted contact's
    `parent_user_id` dropped in the conversion each passed every test.
    Each now fails one: the suite tells the `Observer`'s outcomes and
    codes, sends keyless for another authorizer's caller, and gains a
    second case,
    `the_services_open_on_their_own_number_and_mark_read_as_meta_documents`
    (a blank id refused before any request, the typing indicator,
    Meta's refusal counted); the service's `tests/messages.rs` gains
    `repeats_and_metas_failures_are_counted`,
    `a_kept_answer_expires_after_the_ttl` and the read route's blank id;
    `api::messages` gains `every_field_a_request_carries_reaches_metas_object`
    and `every_field_of_the_accepted_message_is_answered`. `Messages::new`
    admits its number (`Authorizer::admit_number`, as `Admin::new` its
    admin, `tests/capabilities.rs`:
    `the_message_services_refuse_another_authorizers_number`), so nothing
    is sent with a number another `Authorizer` made. The idempotency
    records stay the `Keeper`'s, not the `Authorizer`'s (as S5a did with
    its `records`): the port's methods take a tenant as given and whoever
    holds the `Backend` reaches them (`Backend::idempotency`), so behind
    the order they would guard nothing, for a new parameter of
    `Authorizer::new` at its eleven callers. Every message,
    idempotency and read-receipt route test passes unchanged, and
    `openapi/v1.json` is byte-identical (the request types, their
    documentation and the handlers' annotations untouched). Left: the
    server's source scan of error codes counts fewer (14, its floor
    lowered from 16; the core's raised to 32); no log line moved (the
    messages and idempotency handlers logged nothing of their own; the
    engine's own lines were already the core's); the server's
    `api::messages::MessageType::ALL` is gone (the core's
    `messages::MessageType::ALL` is the list); media and template
    creation run under the core's `run` too, their operations still the
    server's until S5c and S5d.
- [x] **S5c. Numbers and templates into core**
  (`meta-whatsapp-server-core`): number details, the profile's read and
  update, disconnecting a WABA; the template list, the lookup in a WABA,
  create, delete, and the template cache.
  - **After:** S4.
  - **Decisive:** called with no HTTP layer, tenant B's caller on A's
    template id is not found with zero vault reads; `openapi/v1.json`
    byte-identical.
  - **Landed:** `meta_whatsapp_server_core::numbers`: `Numbers` on an
    `OwnedNumber` (`Numbers::new`, which admits it,
    `Authorizer::admit_number`): `details` (`numbers::NumberDetails`,
    `NUMBER_FIELDS`), `profile` and `update_profile` (a
    `numbers::ProfileChange` as the request carried it, answering
    `numbers::BusinessProfile`; an empty change only reads), and `Waba`
    on an `OwnedWaba` (`Waba::new`, which admits it through the new
    `Authorizer::admit_waba`): `disconnect` (`still_bound`, then Meta
    unsubscribes the app, then the token and bindings go; `503` for a
    WABA that moved, nothing deleted when Meta refuses).
    `meta_whatsapp_server_core::templates`: the `TemplateCache` (per
    tenant, WABA and query, bounded per WABA and in all, least recently
    used first; its TTL the adapter's, the service's 60 seconds, one per
    replica), the checks (the status filter, the name, the id: the core's
    `request::graph_id`, moved from the server; `templates::definition`
    for a creation's body), and `Templates` on an `OwnedWaba` for a
    `Caller` (`Templates::new` admits both and opens only on a WABA of
    that caller's own tenant, else `404 not_found`): `list` (cached),
    `get` (the bare id asked for its name only, then looked for in the
    WABA's own list of that name, at most 5 pages of 100), `create` and
    `delete` (each dropping the WABA's cached pages; a deletion audited
    as the tenant's, target `audit`), answering domain values free of
    utoipa (`Template`, `TemplatePage` with Meta's raw cursor,
    `TemplateCreated`). Failed Graph calls are told to the `Observer`
    (`wa_server_graph_errors_total`, the same codes; a refused template
    id by Meta's code though it is answered `404`). The handlers parse,
    convert (every field named on both sides, no `..`), call and
    render; template creation still runs under `idempotency::run`
    (`Idempotent-Replayed` on a replay). Decisive:
    `conformance::templates::another_tenants_template_id_is_not_found_with_zero_vault_reads`
    (in `conformance::run`), with a `ScriptedTransport` and a vault
    whose every read is recorded, no HTTP layer: B's caller naming A's
    WABA is `404` (step 4) with zero vault reads and no request; B's
    caller handed A's capability is refused by `Templates::new`, `404`,
    zero vault reads; through B's own WABA, A's template id is `404`
    without Meta's code or text (Meta asked with B's token only: the
    name, then B's WABA's list), and Meta refusing the bare id is `404`
    too; a deletion of A's template by id through B's WABA deletes
    nothing (no `DELETE`); another `Authorizer`'s caller or capability
    is `403`; and A's vault record and phone index are never read
    through any of it. It runs on the core's memory backend
    (`tests/templates.rs`, its records now conditioning an unbind and a
    status change on the binding's epoch), on the service's
    `MemoryBackend` (`tests/store.rs`) and live on Postgres
    (`tests/live_postgres.rs`). By mutation: the ownership filter of
    `Authorizer::owned_waba` dropped fails its step 2 on all three
    backends (no earlier suite of `conformance::run` caught it); the
    tenant check of `Templates::new` dropped fails step 3; its
    admission of the caller, or of the WABA, dropped fails step 6; the id
    check in the WABA's list dropped fails step 4; the lookup before a
    deletion by id dropped fails step 5. A second case,
    `the_template_services_send_metas_documented_requests`, pins the
    list's requests, the cache, its invalidation by a creation and a
    deletion, and the checks before any request; `conformance::numbers`
    (`the_number_services_read_change_and_disconnect_as_meta_documents`)
    the number services' requests, the admission of both capabilities,
    the empty change, and a disconnection that deletes nothing until
    Meta unsubscribed the app. The sabotage review found five changes no
    test caught, now pinned: the cached pages dropped only on Meta's
    success, and the lookup's 5-page cap changed
    (`a_refused_creation_or_deletion_drops_the_wabas_cached_pages`,
    `a_template_id_past_the_lookups_five_pages_is_not_found`, both in
    `conformance::run`); the lookup's log line under another target (the
    core's `tests/templates_logs.rs`); Meta's `details` dropped from the
    number services' and the disconnection's errors, and a profile
    change's `email` dropped on its way to the library
    (`conformance::numbers`, now posting all six fields). The service's `tests/templates.rs` and
    `tests/numbers.rs` pass unchanged, and gain
    `metas_failures_on_templates_are_counted` and
    `metas_failures_on_numbers_are_counted` (the handlers'
    `observed_by(state.metrics())` dropped fails each); `openapi/v1.json`
    is byte-identical (the request and answer types, their documentation
    and the handlers' annotations untouched). Left: the tenant's
    listings (`GET /v1/wabas`, `GET /v1/numbers`) stay the handlers' reads
    of the records for the admitted caller (no Graph call, not in this
    item's list); `numbers::ProfileChange`'s `vertical` reads into the
    library's `Vertical`, whose catch-all takes a name it does not know,
    which goes to Meta as before; a list's `limit` is the adapter's to
    bound (the server's `PageQuery`: 1 to 100), as before; the one log
    line that moved keeps its old target by naming it
    (`meta_whatsapp_server::api::templates`); the server's `auth::OwnedWaba`
    keeps `failed`, `failed_on_object`, `still_bound`, `forget` and
    `forget_or_busy` (public, now unused by the handlers).
- [x] **S5d. Media into core** (`meta-whatsapp-server-core`): upload,
  download as a framework-free byte stream, delete.
  - **After:** S4.
  - **Decisive:** a download through the core's stream ends before its
    last chunk on a digest mismatch (M1b's rule, without axum); the
    media routes' tests pass unchanged.
  - **Landed:** `meta_whatsapp_server_core::media`: `Media` on an
    `OwnedNumber` (`Media::new`, which admits it,
    `Authorizer::admit_number`), over the adapter's transfer slots
    (`media::MediaSlots`, the core's `ratelimit::Slots`, each slot of the
    number's tenant's share): `transfer_slot` (an upload's, before its
    body is read), `upload` (a `media::Upload`, made only by
    `Upload::new`: `checked_type`, Meta's sizes, `filename`), `download`
    (within `media::DownloadLimits`, made only by `parse` or `new`
    within the caps, `MAX_WHOLE_DOWNLOAD` whole) and `delete` (looked
    up first, with `phone_number_id`). A download answers
    `media::Downloaded`: the content type, Meta's SHA-256, and the whole
    verified file or a `media::MediaStream`, a `futures::Stream` of the
    library's `Bytes` (no HTTP framework; the core gains `bytes` and
    `base64`, both already in the library's graph) held one chunk
    behind, which yields `StreamStopped::Integrity` instead of its last
    chunk on a mismatch (`TooLarge` past `max_bytes`, `Failed`) and
    holds its slot until dropped. The handlers keep the multipart
    reading (framing budget, `media::upload_cap` as the file streams
    in), the upload's `idempotency::run` (`Idempotent-Replayed`), the
    OpenAPI types and the headers, and render the stream as the body.
    Decisive:
    `conformance::media::a_download_through_the_cores_stream_ends_before_its_last_chunk_on_a_digest_mismatch`
    (in `conformance::run`), with no HTTP layer and a transport whose
    downloads arrive in the chunks the case gives: a tampered file of
    three chunks yields two, then the error; a file still arriving is
    passed on with one chunk held; past `max_bytes` nothing; a whole
    tampered file is `502 integrity` with no byte; the reported size,
    an unusable digest and a URL off `lookaside.fbsbx.com` (`502
    upstream`, the token not sent) are refused before the download;
    another node's or a refused media id is `404`; another
    `Authorizer`'s number is `403`; a slot of the tenant's share comes
    first. It runs on the core's memory backend (`tests/media.rs`), on
    the service's `MemoryBackend` (`tests/store.rs`) and live on
    Postgres (`tests/live_postgres.rs`); a second case,
    `the_media_services_upload_and_delete_as_meta_documents`, pins the
    upload's multipart request and the deletion's lookup, a third,
    `a_failed_download_yields_no_unverified_end_and_frees_its_slot` (from
    the sabotage review), that no failure or odd chunking (empty chunks,
    a connection cut mid-file, one chunk, none, exactly `max_bytes`)
    yields the last chunk unverified, that every failure gives its slot
    back, the refusals' order (the id before the slot, the reported size
    before the digest), that a URL off `https://lookaside.fbsbx.com` is
    `502 upstream` with nothing sent to it, and that a `190` from Meta's
    URL marks the number, and `tests/media_logs.rs` the three moved log
    lines' target (`meta_whatsapp_server::api::media`) and that none
    names Meta's URL.
    By mutation, each fails the core suite: the digest check dropped
    (a mismatch taken for the end, in the stream or the whole read);
    the digest checked only once the last chunk was yielded (nothing
    held back); `Media::new`'s admission dropped; each size cap dropped
    (the stream's, the whole read's, the reported size's, the limits'
    in `parse` and in `new`, an upload's kind limit in `Upload::new` and
    in `upload_cap`), and, since the review, a failure taken for the
    verified end, an empty chunk held as the last one, a slot kept past
    a failure, the download's own `190` not marking, and either order
    swapped. The service's `tests/media.rs` passes with its tests
    unchanged (the review added one: an upload's key covers its type and
    file name) and fails on each mutation but the admission (no forged capability
    reaches a handler) and the limits' caps (`parse` and `new` each
    hold the other's). `openapi/v1.json` is byte-identical. Left: the
    server's `auth::OwnedNumber` keeps `failed` and `failed_on_object`
    (public, now unused by the handlers, as S5c left `OwnedWaba`'s); the
    multipart reader stays the server's (its framing budget is
    multer's, an HTTP body's concern).
- [x] **S5e. The webhook pipeline into core** (`meta-whatsapp-server-core`,
  or `meta-whatsapp-server-http` for what needs a listener): routing and
  recording, framework-free.
  - **After:** S4.
  - **Decisive:** M1.2's routing cases run against the core pipeline
    with no listener; routing an unowned number's event to a tenant
    fails them.
  - **Landed:** `meta_whatsapp_server_core::inbound`: `Inbound` (the app
    secrets, the verify token, the clock; moved from the server, which
    re-exports it) and `Pipeline`, built on a `Backend` alone
    (`Pipeline::new(Inbound, &dyn Backend, Arc<dyn Observer>)`, the
    stores the server's `Events` took before). `Pipeline::deliver(signature,
    body)` takes the body as read and its `X-Hub-Signature-256` value
    (bytes, no HTTP type), checks the signature against every app secret
    first (`Refused::Unauthenticated`: no turn waited for, no store
    touched, nothing logged), takes one of `MAX_DELIVERIES_RECORDING`
    turns within `RECORDING_WAIT` (`Busy`), runs the library's
    `WebhookHandler` (3 MiB, `TooLarge`; a signed body that is not a
    webhook one `unparsed` event; each event under its dedup lease,
    `InFlight` while another request holds one) into a sink private to
    the core (routed by the records, `events::route`; the inbox when
    owned; the outbox row; `Busy` for a busy outbox, `Failed` for a
    failed store, the lease released), and answers `Delivered` only once
    every event is recorded or was already. `Refused::status` and
    `outcome` are the answer and the metric label as data (`DELIVERED`
    for a recorded one); the sink's counts go to the `Observer`'s three
    new required methods (`webhook_event`, `webhook_duplicates`,
    `webhook_failure`, the server's `Metrics` counting them as before),
    and its log lines keep the target `meta_whatsapp_server::events`
    (`inbound::LOG_TARGET`) and their fields. The split: no
    `meta-whatsapp-server-http` crate (S7 makes it); what needs a
    listener stays the server's (`api::webhooks`: the signature header's
    shape before the body, `MAX_DELIVERIES_IN_FLIGHT` places,
    `BODY_READ_TIMEOUT`, the 3 MiB read, the rejection log at most once a
    minute per reason, the outcome's count, the subscription handshake),
    its handler matching the core's `Refused` exhaustively. Decisive:
    `conformance::webhooks::routing_is_an_allow_list_with_no_listener`
    (in `conformance::run`), over a view of the backend that records
    what reaches its key/value store and its outbox, with no listener:
    twelve signed payloads from Meta's examples; an unbound number
    (under the tenant's WABA or not), the tenant's number under another
    WABA than the records say or under an unbound one, an unbound
    number's message whose payload names a tenant, a replay dated before
    the dedup lease's memory and an unbound WABA's template update are
    operator-only rows, the others their holder's, each guarded by the
    binding it was routed by; each tenant polls exactly its own, the
    inbox holds only an owned number's message, and each row is told to
    the `Observer` by audience. It runs on the core's memory backend
    (`tests/webhooks.rs`, its records joined by an outbox that checks the
    routing again under their lock and the library's memory inbox), on
    the service's `MemoryBackend` (`tests/store.rs`) and live on Postgres
    (`tests/live_postgres.rs`). By mutation, each fails the core suite:
    the WABA the event names not compared with the number's (route by
    the payload's id without the ownership check), the event's WABA
    looked up before its number, and an unbound number falling back to
    its WABA's binding (an unowned number's event recorded under a
    tenant) each fail the decisive case (and the service's
    `routing_is_an_allow_list`); the dedup lease dropped fails
    `an_event_is_recorded_once_under_its_dedup_lease`; the answer given
    before the recording (spawned) or whatever it did fails
    `the_answer_comes_only_once_every_event_is_recorded` (and most of
    the suite); the core's signature check moved after the handler fails
    `a_delivery_is_verified_before_anything_is_stored` (a forged delivery
    waits for a turn) and, in the service, only `log_rate.rs` (the
    library logs each forged delivery); an unowned event recorded in the
    inbox, the observer's counts dropped, a busy outbox answered `500`
    and a moved log line's target each fail a case too (the last only
    the core's `tests/webhooks_logs.rs`: no service test names that
    target). The review (sabotage, security lens) closed what survived
    the core suite: dropping the turn to record, or giving one more,
    failed only the service's paused-clock
    `a_delivery_with_no_turn_to_record_is_503_after_the_wait`; now
    `a_delivery_waits_for_a_turn_to_record` (in `conformance::run`: with
    every turn held, a fifth delivery claims no lease and asks for no
    row) and `a_delivery_with_no_turn_is_busy_after_the_wait` (the core's
    `tests/webhooks.rs`, on a paused clock: `503 busy` after
    `RECORDING_WAIT`, nothing touched) fail too. Dropping the replay
    window's check, or the binding's date's, passed the core suite (the
    decisive case's replay is dated before its binding, so each check
    hid the other's absence; the service's tests caught both):
    `an_event_is_routed_only_after_its_binding_and_within_the_replay_window`
    (a pipeline whose clock is past the window) fails for each. The inbox
    written after the outbox failed only the service's tests (now the
    answer case asserts the inbox holds the message while its row is on
    its way), the failure line's target no test (now
    `tests/webhooks_logs.rs`), and
    `a_number_moved_while_its_event_records_reaches_neither_tenant` runs
    the outbox's re-check of the routing through the pipeline: a number's
    event guarded by its WABA's binding alone passed every test of the
    core and the service, and fails its second round (the number moving
    alone, to another tenant's WABA). Every
    webhook test of the service passes unchanged, the event-data
    snapshots are unchanged, and `openapi/v1.json` is byte-identical. The
    server's public but unusable `events::ServiceSink` is gone (the sink
    is private to the core, pinned by the UI case `inbound_sink`), and
    `Inbound::clock` is public.
- [x] **S6. Adapter-neutral pieces** (`meta-whatsapp-server-core`; D27):
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
  - **Landed:** `meta_whatsapp_server_core::operation::Operation` names
    the sixteen tenant operations by id (the HTTP adapter's
    `operationId`, its handler's name) with their scope. A request's
    rate class is its operation's (`RouteClass::of(Operation)`), and
    `RateLimiter::charge(tenant, operation)` is the only public way to
    spend a budget (`check` is crate-private: the UI case
    `ratelimit_check`); the server's tenant guard resolves a request to
    its operation (`auth::Operations`, built from the OpenAPI document:
    the method and the route template, `HEAD` as its `GET`) and charges
    it, `500 internal` for a tenant route that is no operation (a test
    holds every tenant route to one core operation with its group's
    scope, a path of each template, plain, percent-encoded or with an
    empty parameter (which axum routes: `/v1/numbers//profile` is
    `get_profile`'s, charged, then `404`), matching no other). The fingerprint is the operation and its canonical input,
    defined once in the core: `Fingerprint::send_message`, `upload_media`
    and `create_template` hash the operation's id, its path parameter as
    a typed id (the number or the WABA, decoded), and the JSON body
    canonical (object keys sorted, whitespace ignored) or the checked
    upload's type, file name and SHA-256; no method and no path (the
    generic `json` and `parts` are private, so every adapter derives the
    same fingerprint), its known answers pinned again apart from the
    code. `ErrorCode`'s constants, `ErrorCode::ALL` and `CODES` come from
    one `codes!` table in `error.rs`, kept equal to §5.2 by the core's
    own `the_codes_are_the_designs` (beside the service's) and each
    constant's name to its code by `each_constant_is_named_after_its_code`;
    `ServiceError::new` and the server's `ApiError::new` take an
    `ErrorCode`, so a misspelled code fails to compile (the
    `compile_fail` doctests on `ErrorCode`: `E0599` for a misspelled
    constant, `E0308` for a string), and the source scans for string
    codes are gone with the strings (the server's schema markers are
    `ErrorCodeSchema` and `KnownErrorCodeSchema`, their schema names
    unchanged). The `Authorizer`'s admin writes (`store_token`,
    `rotate_vault`, `forget_for_admin`, `graph_failed_for_admin`) are
    crate-private: an adapter reaches them only through `admin::Admin`,
    opened by `Admin::new`'s admission (S5a's pattern), and each still
    checks the admin (the UI cases `authorizer_*`; the conformance case
    `capabilities::an_admins_vault_writes_take_an_admitted_admin`, in
    `conformance::run`: the core's memory backend, the service's and
    live Postgres). No dependency of either crate was unused (rustc's
    `unused-crate-dependencies` on both libraries, the dev-dependencies
    by grep; `cargo udeps` is not in the pinned toolchain), so none was
    dropped; `just features` checks the core's library alone and
    documents it with `conformance`. Decisive:
    `two_spellings_of_one_path_replay_the_same_key` (the service's
    `tests/messages.rs`, with the template and upload ones beside it),
    which fails with `uri.path()` back in the fingerprint (the
    percent-encoded repeat answers `422 idempotency_key_reused`) and
    with the number out of it (another number's send replays);
    `a_read_charged_by_operation_id_lands_in_the_read_budget_whatever_its_method`
    (the core) and `a_read_served_by_post_lands_in_the_read_budget` (the
    service's `tests/rate_limits.rs`: an RPC-shaped router whose reads
    are `POST`s, behind the service's own guard and limiter), the latter
    failing with the class read from the method; in `/v1` every read is
    a `GET`, so no route of the HTTP adapter can tell the two rules
    apart (`v1s_routes_keep_their_class` pins that the budgets did not
    move); and the `compile_fail` doctests. By mutation: an entry added
    to or dropped from `codes!` fails `the_codes_are_the_designs` (core
    and service), a constant named after another code
    `each_constant_is_named_after_its_code`; a vault write's own check
    dropped (`store_token`'s, `rotate_vault`'s) fails the conformance
    case, `Admin::new`'s admission dropped fails it and
    `the_admin_services_refuse_another_authorizers_admin`, and
    `store_token` public again the UI case; a read moved to the send
    class fails `an_operations_class_is_the_tables` and
    `v1s_routes_keep_their_class`. `openapi/v1.json` is byte-identical,
    and no status or body changed but the fingerprint's semantics, a
    pre-release change of `/v1` (D27). As first built, a key claimed
    before the upgrade and repeated after it with the same request
    answered `422 idempotency_key_reused`, never a second send, until its
    record expired (`WA_SERVER_IDEMPOTENCY_TTL` from its claim, 24 hours
    by default); the window decided below replays it instead, and the
    guide and the CHANGELOG say so.
  - **Decided 2026-10-31** (the coordinator, under the owner's
    delegation; each swappable, recorded in design D27): the canonical
    input as proposed (the received JSON, canonicalized); `charge`
    taking a bare `&TenantId`; the server-crate renames
    (`ErrorCodeSchema`, `KnownErrorCodeSchema`) and `auth::guard`'s
    `Operations` argument; `graph_failed_for_admin` among the
    crate-private admin writes; `authz::rotate_vault` public for the
    command line; no dependency dropped. **Changed the same day: the
    422 window.** A deployment exists, so a key claimed before the
    upgrade keeps replaying: for one TTL a repeat also matches a record
    under the fingerprint of before (`Fingerprint::legacy_v1_json`,
    `legacy_v1_upload`, pinned to main's known answers by
    `the_legacy_fingerprint_is_mains`; the HTTP adapter adds it with
    `Fingerprint::or_legacy`; a claim stores the new one). Tests: the
    conformance case
    `messages::a_key_claimed_before_s6_replays_under_its_legacy_fingerprint`
    (in `conformance::run`: a record stored by hand under the legacy
    fingerprint replays with no send, another body or the new
    fingerprint alone is `422`, a fresh claim stores the new one), the
    core's `a_record_under_the_legacy_fingerprint_is_the_same_requests`,
    and per route the service's
    `a_key_claimed_before_s6_replays_after_the_upgrade`,
    `a_creation_key_claimed_before_s6_replays_after_the_upgrade` and
    `an_upload_key_claimed_before_s6_replays_after_the_upgrade`. By
    mutation: the legacy match dropped fails them all; a legacy match
    taken for any body fails the `422` steps (and the two-spellings
    tests); a claim storing the legacy fingerprint fails the conformance
    case and the two-spellings tests; the send handler without
    `or_legacy` fails its HTTP test; `uri.path()` in the primary
    fingerprint still fails `two_spellings_of_one_path_replay_the_same_key`.
- [x] **S7. The crate split**: `meta-whatsapp-server-store-memory`,
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
  - **Landed:** the four crates, and the binary composing them with its
    features `api-axum`, `store-memory` and `store-postgres` (all
    default; without `api-axum`, the only API adapter, it does not
    compile; a build without a backend refuses that storage at start,
    naming the feature), re-exporting the moved modules under their old
    paths (design §2.1, §8.1 "The crate split"). `openapi/v1.json` is
    generated by the adapter and stays committed beside the binary,
    byte-identical (`cmp` against `main`, the `openapi` command, and
    `tests/openapi.rs`). The memory backend runs `conformance::run` in
    its crate's `tests/conformance.rs`
    (`the_memory_backend_passes_the_conformance_suite`, with the cases
    it must refuse), the Postgres one live in its crate's
    `tests/live_postgres.rs`
    (`live_postgres_backend_passes_the_conformance_suite`, with the
    Postgres-only cases: migrations, locks, purges, a deadlocked
    insert), which `just test-live` runs with
    `META_WHATSAPP_RS_REQUIRE_LIVE=1`. `just features` builds the binary
    with `api-axum` and each backend alone, and each new crate alone.
    The migrations moved to the Postgres crate byte for byte (sqlx knows
    one by its version and checksum, both pinned by
    `the_migrations_and_their_checksums_are_pinned`; the table and the
    lock key unchanged): the binary's
    `live_postgres_a_database_mains_binary_migrated_starts_this_one_with_no_migration`
    migrates a database as `main`'s binary did (its lock, the library's
    migrations, sqlx's migrator with `main`'s checksums) and starts the
    split binary's `migrate` and `serve` on it: both exit 0, `/readyz`
    is 200, and both history tables are row for row unchanged
    (`installed_on` included); an edited file, a renamed table or a
    renumbered migration fails it. Run once more against `main`'s own
    binary (dda06c9, built from its tree): `migrate` and `serve` by
    `main`, then by the split binary, then `migrate` by `main` again,
    each exit 0, with both histories and the schema dump identical. An
    API adapter's handlers reach the records, the authorization order
    and the idempotency engine through the state's `Grant`, which only
    `AppState::from_backend`'s caller is handed (UI cases
    `grant_from_state` and `grant_from_conversions`: no constructor, no
    `From` and no `FromRef` makes one from a state; the state's
    accessors stay crate-private, and the state no longer holds its
    backend). A grant proves its holder composed a state, not which
    one: `OwnedNumber::capability` and `OwnedWaba::capability` take any,
    and the core's services admit the capability with the authorizer
    they are given, so another state's grant opens nothing. The grant's
    own `Caller` and `AdminCaller` extractors admit what they extract,
    as the state's do
    (`a_forged_capability_is_403_at_every_extractor_on_a_grant_too`).
    Every moved
    log line keeps its target: the ones a request logs
    (`every_line_keeps_its_target_from_before_the_crate_split`) and,
    read from the sources, every one of the new crates
    (`every_moved_log_line_names_its_target_from_before_the_crate_split`:
    each module's `LOG_TARGET` is its path before the split, each line
    names it).
  - **Decided (coordinator, under the owner's delegation, each
    swappable):** the `Grant`, with `AppState::from_backend` returning
    `(AppState, Grant)` (to swap: another proof type, the same call
    sites); the moved modules re-exported under their old paths
    (`meta_whatsapp_server::{api, auth, state, store, …}`; to swap, drop
    a re-export); `openapi/v1.json` committed beside the binary, which
    prints it, rather than in the adapter's crate (to swap, move the
    file and the paths `tests/openapi.rs`, the skills gate and
    `just skills-ts` read); the features `api-axum`, `store-memory` and
    `store-postgres`, all default, with no API adapter a compile error
    and a missing backend a refusal at start naming its feature;
    `api::common`'s four helpers (`encode_cursor`, `next_cursor`,
    `rfc3339`, `json`) public, as the adapter's handlers use them (pure
    functions, no capability); `From<MintError> for ApiError` in the HTTP
    crate's `error.rs`, where both types are in reach; the legacy
    fingerprint's removal line moved to M2a; and the docs tables'
    resolver prefixes for the new crates (`server_http::`,
    `server_api_axum::`, `server_store_memory::`,
    `server_store_postgres::`, `crates/meta-whatsapp-rs/tests/docs_tables.rs`).
  - **Also, after the first release:** the legacy fingerprint's removal
    moved to M2a, the service's next item, as S7 landed before the
    first release.
- [x] **S8. MongoDB in the dev container and CI** (`.devcontainer/`,
  the `just test-live` Compose file, `.github/workflows/ci.yml`): a
  single-node replica set (transactions need one) beside Postgres and
  Redis, and `META_WHATSAPP_RS_TEST_MONGODB_URL`, for S9, S17 and S18.
  - **After:** nothing.
  - **Decisive:** `just test-live` with
    `META_WHATSAPP_RS_REQUIRE_LIVE=1` and no MongoDB URL fails; with it,
    a live smoke test commits a multi-document transaction.
  - **Landed:** `mongodb` in `compose.test.yaml` (`127.0.0.1:57017`) and
    as a devcontainer sidecar: `mongo:7.0.45-jammy` with
    `--replSet rs0`, whose healthcheck runs `rs.initiate()` when
    `rs.status()` reports the set not yet initiated (code 94), with one
    member, `localhost:27017`, and passes once that member is primary
    (a few seconds after start). The URL carries `directConnection=true`,
    as the member address is the container's own: `just test-live`
    exports `mongodb://127.0.0.1:57017/?directConnection=true` (and
    starts Compose when it is unset), the devcontainer sets
    `mongodb://mongodb:27017/?directConnection=true`, and `just
    test-live-clean` drops the `wa_test_*` databases (through `mongosh`,
    which the devcontainer's image gains). The smoke test is
    `live_mongodb_replica_set_commits_a_multi_document_transaction`
    (meta-whatsapp-adapters' `tests/live_mongodb.rs`, in `test-live`'s
    `live_` filter): a transaction across two collections, not visible
    outside it before the commit and all visible after, then an aborted
    one that leaves nothing; with the URL unset under
    `META_WHATSAPP_RS_REQUIRE_LIVE=1` it panics. Its driver, `mongodb`
    3.9 (Apache-2.0), is a dev-dependency there, without default features
    (`rustls-tls` is rustls on `ring`) and with `rustls-tls-aws-lc`,
    which builds its configuration on an explicit aws-lc-rs provider: no
    library build links it, and it enables no `ring`. CI needs no
    workflow change (`just ci` runs `test-live`, which starts Compose).
    The server is MongoDB's, under the SSPL, and runs as a test service
    only.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable by its tag, line
    or setting): the image `mongo:7.0.45-jammy`, pinned by tag: the 7.0
    series, as every 8.x release refuses to start on Linux 6.19 and
    later (MongoDB's SERVER-121912, an rseq incompatibility in its
    tcmalloc; the CI runner's kernel is one), and 7.0 has transactions on
    a replica set; back to 8.x once MongoDB ships a release that starts
    there; the driver a dev-dependency of the adapters only,
    with `rustls-tls-aws-lc` and `bson-3`, until S17 makes it an optional
    dependency; the healthcheck's `start_interval` of 1 s and `interval`
    of 30 s (a check every 2 s cost half a core); `mongosh` 2.14.1 in the
    devcontainer image; `MongoCleanup` in `live_mongodb.rs`, so the other
    test binaries do not build the driver (S17 may move it). The SSPL
    server is run, never linked or shipped, as the test service for the
    database the owner named (design §10); the owner may still rule on
    it, and the final report lists it.
- [x] **S9. An `Outbox` spike on MongoDB**
  (`meta-whatsapp-server-store-mongodb`, the `Outbox` only, not composed
  into the binary), before the port shapes freeze: per-tenant commit
  order, the routing re-check, the stream purge, against a replica set.
  - **After:** S3, S8.
  - **Decisive:** the spike passes the core conformance suite's outbox
    cases, the cross-port invariants included, live against a replica
    set. If a case cannot pass, the port changes first (memory and
    Postgres with it, through S3's suite), and the spike passes after
    that change.
  - **Landed:** the crate (`publish = false`, a workspace member but not
    a default one, composed into no binary), whose `MongoOutbox`
    implements `Outbox` with the `mongodb` driver 3.9 (no default
    features; `compat-3-3-0`, `bson-3`, `rustls-tls-aws-lc`: `cargo tree`
    shows no `ring` from it). Every write is a multi-document transaction
    (snapshot reads, majority writes), tried again from the start on a
    write conflict. An insert reads the dedup key, then the guarded
    binding's document *and writes it* (`insert_checks` incremented: the
    port's own prescription for a database whose snapshot transactions
    see write-write conflicts only), decides with the core's
    `RouteGuard::admits`, then draws its sequence by incrementing its
    stream's document and stores the event, `created_at` the server's
    `$currentDate`. Two transactions writing one document cannot both
    commit, so a stream's sequence `n + 1` is drawn only once `n`
    committed (per-tenant commit order, gap-free), and an unbinding waits
    for an insert that checked its binding, or makes it start over. A
    page reads in one snapshot transaction, sizes first, then only the
    kept rows' data. Retention (`purge`) cuts each stream's prefix up to
    its newest event older than the cutoff (the server's `localTime`), in
    transactions of at most 1,000 events that delete and move
    `purged_through` (`$max`) together, with no lock of its own: two
    purges write the same stream document, so one starts over and counts
    only what it deleted. A deleted tenant's stream is purged by
    `MongoOutbox::forget_tenant`, in one transaction that writes the
    stream's document (creating it for a tenant with no event yet), so a
    first insert in flight conflicts with it too. Contention past 2
    seconds (`BUSY_AFTER`, `with_busy_after`) is `StorageError::Busy`.
    **How the cross-port invariants get their binding:** the records
    are S18's, so the outbox reads a projection, `wa_server_wabas` (a
    WABA's tenant, `attached_at`, `previous_tenant_unbound_at`, its
    numbers), which the records' owner writes with
    `MongoOutbox::record_binding` and `forget_binding`; in the crate's
    tests the bundle (`tests/support`, `SpikeBackend`) is the memory
    backend's ports with this outbox, its records wrapped
    (`MirroredRecords`) to write every binding they change, and to
    purge a deleted tenant's stream, before they return, under one lock.
    The decisive tests, live against the S8 replica set (MongoDB
    7.0.45, in `tests/live_mongodb.rs`, which `just test-live` runs):
    `live_mongodb_outbox_passes_the_outbox_cases`
    (`conformance::outbox::run`, every case),
    `live_mongodb_outbox_passes_the_cross_port_invariants`
    (`conformance::invariants::run`, every case: the same data, a
    deleted tenant's stream purged, the six route-guard cases of S2 and
    S2b, commit order under eight writers),
    `live_mongodb_an_insert_racing_a_move_never_reaches_the_new_holder`
    (the race, 200 rounds, 167 to 196 of them overlapping in four runs) and
    `live_mongodb_outbox_in_a_bundle_passes_the_conformance_suite`
    (`conformance::run` whole, the webhook pipeline recording into it);
    no case failed, so **no port changed**. MongoDB's own:
    `live_mongodb_an_unbinding_waits_for_the_insert_in_flight` (feature
    `test-hooks`: an insert held after its check; the unbinding returns
    only once it committed),
    `live_mongodb_write_conflicts_past_the_budget_are_busy`,
    `live_mongodb_two_purges_at_once_count_each_event_once` (two clients,
    1,400 events in two streams, one past a purge step) and
    `live_mongodb_migrate_makes_the_named_collections_and_indexes_once`;
    the stored names pinned by `schema::tests::the_stored_names_are_pinned`.
    **Brought up to `main` on 2026-11-02** (PRs #92 to #109): the two
    `Outbox` methods added since, implemented as Postgres does them.
    `received_media` (M2e, required): an insert keeps the media id its
    event records as received (`NewEvent::received_media`) in
    `media_id`, beside `media_order`, an `ObjectId` the inserting process
    draws (instants keep MongoDB's millisecond, so two rows of one
    millisecond need it to come back newest first), and the lookup reads
    the partial index `wa_server_events_media_idx` (`media_id`,
    `created_at` descending, `media_order` descending; only the rows with
    a string `media_id`) up to `RECEIVED_MEDIA_ROWS`, its filter
    repeating the index's `$type` so that the planner may use it; the
    rows go with their stream's purge and their tenant's. `get` (M2b,
    which has a default that pages the stream): one lookup by `_id` in
    the tenant's own stream. The bundle of the tests gained the ports'
    new methods (the memory backend's notifier; the records' OTP
    settings, credit allocation, signup attempts and vault entries,
    delegated, as none changes a binding the outbox reads). Their tests:
    `conformance::outbox::received_media` (in
    `live_mongodb_outbox_passes_the_outbox_cases`), `conformance::live`'s
    lookups by id and M2d's `conformance::number_events` (in the whole
    suite), and
    `live_mongodb_received_media_reads_its_index_in_order` (no field on
    a row without media, the index's use counted by `$indexStats`, thirty
    rows of three tenants read back newest first).
    **Brought up to M2f on 2026-11-02** (PR #115): `erase_contacts`
    (required), as Postgres' `ERASE_SQL` does it. An insert keeps the
    event's `NewEvent::contacts` in `contacts`, an array of strings,
    absent when the event names nobody (Postgres' `NULL`); the erasure
    deletes, whole, the rows whose `tenant_id` is the tenant (an
    operator-only row has none, so it is never one; and `tenant_id`
    rather than `stream`, which leads the stream's index, so the
    planner's only index is the erasure's), whose `phone_number_id` is
    the number and whose `contacts` hold one of the identities (`$in`),
    in one `delete_many` at majority write concern, no stream's document
    written (its sequences and `purged_through` stay), and answers how
    many rows went; an empty list asks nothing. Its index,
    `wa_server_events_contacts_idx`, is partial and multikey (`contacts`,
    over the rows where it `$exists`, which an `$in` of strings implies,
    so the erasure's filter need not repeat it and the `$in` gives the
    bounds). Its tests: `conformance::outbox::erase_contacts` (in
    `live_mongodb_outbox_passes_the_outbox_cases`), M2f's inbox and
    webhook cases (in the whole suite),
    `a_row_naming_customers_stores_its_contacts_and_no_other_row_does`
    (offline; by mutation, the field written always, never, or only in
    part each fail it) and
    `live_mongodb_erase_contacts_reads_its_index_and_keeps_the_rest` (the
    index's use counted by `$indexStats`; the number's operator-only row,
    a previous holder's row in its own stream, another customer's row
    and the customer's row on another number kept). The live cases were
    not run on the branch (no MongoDB there): CI's `just test-live` runs
    them.
    By mutation, live: the re-check dropped (the routed tenant kept), and
    `RouteGuard::admits` dropped (the binding's tenant and numbers still
    matched), each fail the invariants (`only the rows whose guard held`)
    and the race (the new holder polls the old holder's event); the
    sequence drawn outside the transaction fails the commit-order case (a
    poll skipped and reordered events); no transaction at all fails the
    commit-order case and the in-flight case (the race failed it in one
    run of two); the binding's write dropped (a read only) fails the
    in-flight case (the unbinding returned while the insert was held) and
    passes the race, the invariants and the whole suite, so the in-flight
    case is the fence's decisive test; before `forget_tenant` wrote the
    stream's document it failed the race too (a tenant created again
    polled an event whose first insert created the stream after the
    deletion's purge), which is why that write was added: dropped alone it
    fails nothing (the fence covers it), dropped with the fence it fails
    the race; the deletion's mark dropped fails
    `deleting_a_tenant_purges_its_stream`; a purge step's mark dropped
    fails the outbox's purge case and the two purges; `Busy` mapped to a
    backend error fails the budget case. Every one of these failures is
    the case's own assertion.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable, the bundle and
    the conformance suite being the seam; the licence below excepted):
    the bindings the spike checks as a projection collection its records'
    owner writes (`record_binding`, `forget_binding`), until S18's
    `RecordStore` writes the same documents in its own transactions; the
    fence as an increment of the WABA document's `insert_checks` (rather
    than, say, a version the binding moves); the write-conflict budget of
    2 seconds (Postgres' `lock_timeout`) with a random pause of 0.1 to 64
    ms between attempts; purge steps of 1,000 events; the database's
    clock (`$currentDate`, `hello`'s `localTime`); instants at MongoDB's
    millisecond, a binding's start rounded down (its second kept) and a
    dedup window's end rounded up (a key held longer, never shorter); an
    event's `_id` its public id and a stream's its tenant's id (`""` for
    the operator-only rows), the event's JSON a BSON string beside its
    size; the hook behind a `test-hooks` feature, test code only; the
    driver a direct dependency (the facade re-exports none until S17).
    **Pending the owner** (stored names, before the first release): the
    collections `wa_server_events`, `wa_server_event_streams` and
    `wa_server_wabas`, their fields (since the merge of 2026-11-02,
    `media_id` and `media_order` among them, and since M2f's, PR #115,
    `contacts`) and the index names (`wa_server_events_media_idx` and,
    since M2f's, `wa_server_events_contacts_idx` among them;
    docs/architecture.md § Stable identifiers). **The licence, decided by the owner on
    2026-11-02** ("Yes, upgrade deny"): CC0-1.0 is allowed, in
    `deny.toml`'s `allow` list with the decision's date beside it, for
    `tiny-keccak` through the driver (S9, S17), and `just deny` passes;
    removing the entry fails it again. The same day the owner said yes
    to D20 (a), BlueOak-1.0.0 for `minicbor` and `minicbor-serde` only,
    which S10 adds as a scoped exception, not this item. What the
    licence check met before: `cargo deny check` failed on this branch
    (licenses only; advisories, bans and sources passed). The driver depends, through
    `mongodb-internal-macros`, `macro_magic`, `const-random` and the
    proc-macro `const-random-macro`, on `tiny-keccak` 2.0.2, under
    CC0-1.0, which `deny.toml` does not allow. Since S8 that chain was a
    dev-dependency's (meta-whatsapp-adapters' tests), whose licences
    cargo-deny does not check; this crate makes the driver a normal
    dependency, so it is checked now (S17's `mongodb` feature will meet the
    same). Allowing CC0-1.0 was a licence choice: the owner's (§ Owner
    touchpoints), made on 2026-11-02 as above.
  - **Left, and why:** the bindings are the fixture's mirror, consistent
    with the records only between two binding calls, and a deleted
    tenant's stream is purged after the records' deletion, not in its
    transaction (S18 does both in one); a stream's inserts serialize by
    aborting and trying again, not by queueing on a lock as on Postgres,
    so a hot stream (the operator-only one is shared by every WABA)
    answers `Busy` sooner under load, and every admitted insert writes
    its WABA's document too (measured: eight writers of one stream pass
    the commit-order case; no load test); `forget_tenant` deletes a
    stream in one transaction, unbounded; an instant written from a
    clock finer than a millisecond (the fixture's memory records) is
    floored, so the window after a move may admit an undated event less
    than a millisecond early there (S18's records write the database's
    milliseconds, which the routing reads too); `migrate` is not a
    `SchemaMigrator` under a lock (idempotent, and safe on two replicas
    at once); a commit whose result stays unknown past the budget is a
    backend error, not `Busy` (it may have committed; Meta's redelivery
    then meets the dedup key); one MongoDB version, one node, no sharded
    cluster.
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
- [x] **S10. CrateStack enters the workspace** (D20; the workspace
  manifest, `deny.toml`, `meta-whatsapp-server`): the `deny.toml`
  licence exception for BlueOak-1.0.0 scoped to `minicbor` and
  `minicbor-serde`, aws-lc-rs installed as rustls's process default at
  start, a minimal `meta-whatsapp-server-api-cratestack` with one
  procedure, not the default. D20 (a) was decided by the owner on
  2026-11-02: yes, so S10–S16 go ahead.
  - **After:** S7. The owner's yes to D20 (a) came on 2026-11-02
    (BlueOak-1.0.0 allowed for `minicbor` and `minicbor-serde` only):
    this item adds that scoped exception to `deny.toml`, and nothing
    before it does.
  - **Decisive:** `just deny` passes and fails again when the exception
    is removed; a live Postgres test with `sslmode=require` passes in a
    binary that links both providers (sqlx picks `ring` itself until
    U2); a test that builds a rustls client configuration from the
    process default after start-up passes in that binary, and panics
    when the install is removed.
  - **Landed:** `deny.toml` gains two `[[licenses.exceptions]]` entries,
    BlueOak-1.0.0 for `minicbor` and for `minicbor-serde`, and no `allow`
    entry: `just deny` exits 0, and 4 with the entries removed (both
    crates rejected, nothing else). CrateStack comes from crates.io as
    `cratestack-api` 0.17.2, its `db = None` facade, pinned in the
    workspace manifest to `~0.17.2` under the key `cratestack` (its
    macros' generated code names `::cratestack::` paths), without
    default features but `codec-json`; it brings no sqlx. The new crate,
    `meta-whatsapp-server-api-cratestack` (`publish = false`, a member,
    not a default one), compiles `schema/api.cstack`
    (`datasource { provider = "none" }`) with one procedure, `version`:
    `POST /$procs/version`, `@allow(true)` and no key, as
    `GET /v1/version`, answering its four fields (`server`,
    `meta_whatsapp_rs_revision`, `graph_api_version`, `api_version`),
    JSON only (D19: a CBOR caller gets `406`, a CBOR body `415`), a
    1 KiB body bound of its own (`413` past it). Its `router(ApiVersion)`
    returns an axum 0.8 router (the facade's axum, one crate), and its
    tests pin the exact request and answer. The binary compiles it in
    with a new feature, `api-cratestack`, not a default, re-exports it
    as `meta_whatsapp_server::api_cratestack`, and does not serve it:
    `api-axum` stays the default and the only adapter the internal
    listener serves, and `openapi/v1.json` is unchanged; its
    `tests/api_cratestack.rs` checks the procedure's answer equals
    `GET /v1/version`'s on one state. CrateStack's HTTP client
    (`cratestack-client-rust`) brings rustls with `ring`, so a build with
    the feature compiles both providers into rustls (sqlx keeps
    aws-lc-rs: `ring` reaches sqlx only with `cratestack-pg`, S14, until
    U2). `meta_whatsapp_server::tls::install_default_provider` installs
    aws-lc-rs as the process default unless one is installed already,
    and `cli::run_with` calls it before every command. The service's
    `tests/tls.rs` (built with `api-cratestack`):
    `a_client_configuration_builds_from_the_process_default_after_start_up`
    shows `ClientConfig::builder()` panicking before start-up (both
    providers linked), runs the command line (`openapi`), then builds
    one on the installed aws-lc-rs; with the install removed from
    `run_with` it panics there with rustls' "Could not automatically
    determine the process-level CryptoProvider".
    `live_postgres_sslmode_require_with_both_providers_linked` connects
    through `config::database` and `serve::connect` with
    `sslmode=require` and reads `pg_stat_ssl` (a session without TLS
    fails it). For it, the test Postgres of `compose.test.yaml` and the
    devcontainer speaks TLS, on the test-only server certificate of
    `tools/test-tls/generate.sh` (copied to a 0600 key in the container
    first), and still accepts plain connections. `just features` checks
    the new crate alone and the binary with `api-cratestack`. *From the
    sabotage review (2026-11-02)*: sqlx does read the process default in
    one place, `sslmode=verify-ca`, whose certificate verifier
    (`WebPkiServerVerifier::builder`) takes it, so in a build with both
    providers and none installed every database connection panics, the
    migrations' included; `tests/tls_verify_ca.rs`
    (`live_postgres_sslmode_verify_ca_needs_the_process_default`, its own
    binary) shows that panic, then connects, encrypted and verified
    against the test CA, after `install_default_provider` (removing the
    install fails it; `sslmode=require` in its place fails it too). The
    `serve::serve*` functions and `serve::connect` say in their rustdoc
    to call the install first. `tests/licence_scope.rs` in the new crate
    reads `deny.toml` and fails when BlueOak-1.0.0 is widened: moved into
    `allow`, excepted for a third crate, or excepted with another
    licence (`just deny` passes all three). The adapter's tests pin the
    body bound to the byte (1024 read, 1025 refused) and on a streamed
    body with no `content-length`, and the `CODEC_ERROR` body exactly
    (no parser text). The deploy image's build context leaves out
    `.test-tls/` (`deploy/compose/Dockerfile.dockerignore`).
  - **Decided** (AGENTS.md § Decisions; each swappable):
    - The procedure. **Coordinator's decision 2026-11-02 under the
      owner's delegation, swappable**: `version`, mirroring
      `GET /v1/version`'s fields and its no-key access (`@allow(true)`),
      so it exposes nothing that route does not. To swap, edit
      `schema/api.cstack`; the wire name and fields stay pending the
      owner (below).
    - The router. **Coordinator's decision 2026-11-02 under the owner's
      delegation, swappable**: `router(ApiVersion)`. To swap, S13 moves
      it to the state and its grant.
    - The provider install. **Coordinator's decision 2026-11-02 under
      the owner's delegation, swappable**: at the top of
      `cli::run_with` only, keeping a provider already installed, which
      is the swap (install another before the command line runs). A
      direct `serve::serve*` caller calls
      `tls::install_default_provider()` first, as the guide and the
      `serve` functions' rustdoc say.
    - The body bound. **Coordinator's decision 2026-11-02 under the
      owner's delegation, swappable**: 1 KiB. To swap, change
      `BODY_LIMIT_BYTES` (one constant, the router's only bound).
    - Serving. **Coordinator's decision 2026-11-02 under the owner's
      delegation, swappable**: re-exported as
      `meta_whatsapp_server::api_cratestack`, not served; serving it,
      and the setting that picks an adapter, come in S13.
    - The pin. **Coordinator's decision 2026-11-02 under the owner's
      delegation, swappable**: `~0.17.2`, one minor (S12 adds the test
      that keeps every `cratestack*` requirement to one minor). To swap,
      an upgrade PR moves the requirement.
    - The test Postgres. **Coordinator's decision 2026-11-02 under the
      owner's delegation, swappable**: `compose.test.yaml`'s and the
      devcontainer's always speak TLS (plain connections still
      accepted). To swap, drop the `entrypoint` and the TLS volume from
      both; the `sslmode` tests then fail, as they should.
    - The two-adapter rule. **Coordinator's decision 2026-11-02 under
      the owner's delegation, swappable**: every new route lands in both
      adapters from S13 on, not from S10 (the adapter serves nothing
      before S13); a route landing before S13 goes into `api-axum`, and
      S13 carries it over (§ Routes above).
  - **Left, and why:** CrateStack's HTTP client,
    `cratestack-client-rust`, is a non-optional dependency of
    `cratestack-api` 0.17.2 (no feature of the facade drops it), so a
    server-side build links its reqwest, rustls with `ring`, and
    `minicbor-serde`, though the service never constructs a client;
    dropping it needs an upstream change (a feature on `cratestack-api`
    making the client optional), not yet filed. `cratestack-api` also
    links its schema parser (`cratestack-parser`, on `chumsky`), which
    brings the only duplicate versions S10 adds (`hashbrown` 0.15 and
    `foldhash` 0.1, warnings under `just deny`'s bans). The supply chain
    as reviewed: every `cratestack*` crate is published by its one
    crates.io owner, the repository owner of
    [cratestack/cratestack](https://github.com/cratestack/cratestack),
    the one design §8 names, under MIT, checksums as `Cargo.lock`
    records; none has a build script; the proc-macro
    (`cratestack-macros`) reads the schema file (and a contract lock,
    unused here) under the crate's manifest directory and nothing else,
    no network and no process. The new build scripts elsewhere probe
    `rustc --version` (`erased-serde`, `typeid`) or build for Haiku
    only (`iana-time-zone-haiku`, not compiled on Linux). The refusals
    the generated router answers come in three shapes, none of ours:
    CrateStack's `{code, message, details}` for `400` and `415` (whose
    message lists `application/cbor` as accepted, though a CBOR body is
    refused), and axum's plain text for `406` and `413`; an argument
    the procedure does not declare is ignored, not refused. All of this
    changes with S13 and U1.
  - **Pending the owner** (names before the first release): the
    feature `api-cratestack`, the crate name
    `meta-whatsapp-server-api-cratestack`, and the procedure's wire name
    and fields (`/$procs/version`, the four above).
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
  (`~0.17`: S10 pinned `cratestack-api` to `~0.17.2`) and to one
  version between them; a weekly job that opens an
  issue when CrateStack releases a new minor; one upgrade PR per
  release, through the review pipeline.
  - **After:** S10.
  - **Decisive:** a workspace test reading `Cargo.toml` fails when a
    `cratestack*` requirement is not pinned to one minor, or when two
    CrateStack crates are pinned to different versions.
- [ ] **S13. `meta-whatsapp-server-api-cratestack` parity** (one PR per
  resource group: numbers, messages and media, templates, events,
  admin, and each group added before S13): `api.cstack` with
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
  - **After:** S9, S17, M2a–M2h.
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
    (subcommands and flags: B1b, since done) and row 17 partial (rich
    replies beyond text: B1c, since done). Folder loading and hot reload
    stay out (D28).
- [x] **B1b. Subcommands and flags** (`meta-whatsapp-bot`; row 85): a
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
  - **Landed:** row 85 done in the library. `Command::subcommand` (one
    level), `Command::flag`, `Command::pass_unknown_flags`,
    `Invocation::subcommand`, `Args::flags`, `Args::has_flag`,
    `Args::flag`, `Args::parse_with_flags`, `Flag`,
    `Refusal::UnknownFlag`, and the subcommands in `CommandInfo` and
    `CategoryHelp`. The decisive tests, in
    `crates/meta-whatsapp-bot/tests/subcommands.rs`:
    `order_status_runs_the_subcommand_and_order_alone_the_parent`
    (fails when the match runs the parent instead) and
    `a_subcommands_own_cooldown_refuses_a_second_run` (fails when the
    cooldown is read from the parent); in `src/command.rs`,
    `a_quoted_flag_stays_a_positional_argument` (fails without the
    quote check). The parent's scope and owner-only guard its
    subcommands; a subcommand's cooldown key is the same digest over
    its path (`order status`), so no stored key changes. Flags: the
    long form only, `=` for a value, `--` ends them, an em dash reads
    as `--`. Decided by the coordinator (AGENTS.md § Decisions; the
    draft refused undeclared flags on every command): only a command
    that declares a flag, or takes any (`Command::pass_unknown_flags`),
    reads flags, each command and subcommand by its own set, and one
    such command typed with a flag it does not declare is refused with
    its usage. The reason: a mistyped flag on a command that has flags
    (`--dryrun` for `--dry-run`) must not run the real thing, while
    free text on a flag-less command (`/say --hi there`) must reach it
    untouched, as before B1b, so no existing command changes. Swappable
    per command (`Command::pass_unknown_flags`), and for the whole bot
    `Refusals` decides what the user is told; the tests are
    `a_command_without_flags_keeps_dashes_as_arguments` and
    `an_unknown_flag_is_refused_with_the_usage`. Not in it: nested
    subcommands, short or space-separated flags, and typed flag values
    (a handler parses `Args::flag` itself).
- [x] **B1c. Rich replies beyond text** (`meta-whatsapp-bot`; row 17):
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
  - **Landed:** row 17 done in the library. `Ctx::reply_rich` sends a
    `rich::RichReply` (`markdown`, `image`, `image_with_caption`,
    `suggestions`, `products`, `list_button`; `to_messages` builds
    without sending) of `rich::Suggestion`s (`new`, `command`,
    `description`), each part as the client's own builder
    (`ReplyButtons`, `ListMessage`, `ProductCarousel`, `Image`), all
    checked with `OutboundMessage::validate` before the first send. The
    decisive tests, in `crates/meta-whatsapp-bot/tests/rich_replies.rs`:
    `a_paragraph_and_an_image_send_a_text_then_an_image` (fails with the
    parts or the sends reversed),
    `three_suggestions_become_buttons_and_four_a_list` (fails with the
    count check forced either way or off by one) and
    `a_javascript_image_url_is_never_sent` (fails without the URL
    check); `a_failure_midway_stops_the_reply_and_names_the_step` and
    `a_timeout_midway_is_not_resent_or_followed` fail when the reply
    goes on after a failed send. Decided by the coordinator on
    2026-10-30 (AGENTS.md § Decisions), as the draft proposed: the
    builder API above; every message built and validated first, then
    sent in order, a failed send stopping the reply as an `Error::Step`
    (`rich::STEP_TEXT`, …) with the parts before it never resent;
    buttons, lists and carousels take as their body the last message of
    the Markdown part right before them (none: refused), so an answer
    and its buttons are one bubble and a separate question is one more
    `markdown` part; `Suggestion::command` sends the command's first
    payload and is refused when it has none; an image link must pass the
    renderer's URL rule and be an absolute `http` or `https` URL with a
    host (Meta fetches it; its page refuses no `http`, so `http:` is
    allowed), without a user name or password (added in review: it
    would be sent to Meta); a list's single section is titled with its
    button's label, `rich::LIST_BUTTON` (`Options`) unless
    `RichReply::list_button`; a suggestion's description shows in lists
    only (buttons have none). A suggestion may name a hidden or
    owner-only command: it shows only the title the bot gives it, and
    the tap passes the command's guards
    (`an_owner_only_suggestion_is_still_guarded_when_tapped`). Swappable:
    `RichReply::to_messages` gives the messages to send another way,
    and the Markdown goes through the bot's `MarkdownRenderer`. Not in
    it: an image by uploaded media id, Markdown's own `![alt](url)` as
    an image (it stays text), headers and footers on buttons and lists,
    several list sections, and a partial result on failure (the error
    carries the step, not the responses of the parts already sent).
- [x] **B2. Paced broadcast** (`meta-whatsapp-bot`; rows 89, 92; D29): a
  per-number rate under Meta's throughput (80 messages a second by
  default), progress, retries only when `Error::may_have_been_sent` is
  false, the pair and per-user marketing limits read from `ErrorKind`;
  typing indicators and group operations throttled through the same
  pacer.
  - **After:** B1.
  - **Decisive:** under a fake clock, 200 sends at a configured 20 a
    second never exceed it; a scripted timeout is never retried
    (removing the `may_have_been_sent` check fails the test).
  - **Landed:** rows 89 and 92 done in the library (`Broadcast`,
    `Pacer`, `TokenBucket`, `BroadcastPolicy`, `BotBuilder::pacer`,
    `PacedGroups` and `PacedGroup` for group operations). Each person
    once by default (`BroadcastBuilder::dedupe`); lines streamed to a
    `ReportSink` for long lists; every retry paced
    (`Client::with_retry`), and resent only when `Error::may_resend`
    holds, the client's rule too. Swappable: the rate limiter
    (`RateLimiter`), which errors slow a number down (`SlowDownRule`),
    the clock (`Timer`), the failure policy (`BroadcastPolicy`) and the
    report sink (`ReportSink`) are traits with defaults; a number's rate
    changes while running (`TokenBucket::set_rate`). Not in it: acting
    on Meta's throughput upgrade by itself (B2a), a limiter shared by
    replicas (B2b; the trait allows one), Meta's daily messaging limit
    (Meta's to enforce), and runs that survive a restart (B3).
- [x] **B2a. Throughput upgrades, followed** (`meta-whatsapp-bot`): an
  opt-in listener that sets a number's rate to `Rate::HIGHER_THROUGHPUT`
  (`TokenBucket::set_rate`) when its `phone_number_quality_update`
  webhook says `THROUGHPUT_UPGRADE`
  (`PhoneNumberQualityEvent::ThroughputUpgrade`, `throughput`: Meta
  documents no other mapping from a level to a rate), off by default.
  Companions: the bots guide, the bot skill's broadcast reference.
  - **After:** B2.
  - **Decisive:** delivering the `THROUGHPUT_UPGRADE` fixture moves that
    number's sends from 12.5 ms to 1 ms apart on a `ManualClock`;
    removing the `set_rate` call fails the test.
  - **Landed:** `ThroughputUpgrades` (module `throughput`), wired in with
    `BotBuilder::follow_throughput_upgrades` (a listener of
    `phone_number_quality_updated`) or delivered to as an `EventSink`
    without a bot; nothing follows an upgrade unless one is set up. The
    webhook names the number by its WABA and display phone number only,
    so a `NumberDirectory` says which phone number id that is (default
    `KnownNumbers`: registered by WABA and display number, compared by
    their digits); an unknown number, an `ONBOARDING`, a `FLAGGED` or a
    value Meta adds later changes no rate, and an upgrade never lowers
    one (heard twice, it changes nothing). In memory: a restart or
    another replica is back at the configured rate, and Meta sends the
    webhook once (`ThroughputUpgrades::observe` returns the number, to
    record it yourself; no store namespace added). The rate an upgrade
    sets is a setting (`ThroughputUpgrades::rate`). The decisive test,
    in `crates/meta-whatsapp-bot/tests/throughput.rs`:
    `the_upgrade_fixture_moves_the_numbers_sends_from_12_5_ms_to_1_ms_apart`
    (fails without the rate raised: the gaps stay 12.5 ms);
    `other_numbers_keep_their_rate` fails when the rate goes to every
    number, `an_upgrade_of_an_unknown_number_changes_nothing` when the
    WABA is not part of the match,
    `only_a_throughput_upgrade_changes_a_rate` when an unknown value
    counts as an upgrade,
    `an_upgrade_is_idempotent_and_never_lowers_a_rate` without the
    never-lower check (made with the set under one lock),
    `a_middleware_that_stops_the_event_stops_the_upgrade` pins the
    listener's place, `the_logs_and_debug_name_no_number` the logs
    (alone in `tests/throughput_logs.rs`: its capture is a thread's
    subscriber, and `tracing` caches call sites across threads), and
    `without_following_upgrades_the_fixture_changes_no_rate` off by
    default. Decided by the coordinator (AGENTS.md § Decisions, each
    swappable): the `ThroughputUpgrades` shape above (the directory a
    trait, `NumberDirectory`; the rate a setting); off by default, by
    construction (nothing follows an upgrade unless one is set up);
    matching on the WABA and the display number's digits (`KnownNumbers`,
    replaceable by any `NumberDirectory`); in memory only, a restart or
    another replica back at the configured rate (documented; recording
    upgrades is the integrator's, no `KvStore` namespace, which would be
    a stable identifier and is the owner's to name); every other `event`
    value ignored and logged at debug level; never lowering a rate; and,
    as the bot's listener, running after the middleware, so a middleware
    that stops the event stops the upgrade (documented; the sink is the
    way around it).
- [x] **B2b. A pacer shared by replicas** (`meta-whatsapp-bot`; for the
  service's broadcasts, M5k): a `RateLimiter` keeping each number's rate
  schedule (the generic cell rate algorithm's state) as a typed store on
  `KvStore`, read and written with `compare_and_swap` (no new port), so
  every replica books from one budget. A new store namespace,
  `wa.bot.pacer` (the `wa.<module>[.<purpose>]` rule; ephemeral, like
  `wa.bot.cooldown`), added to architecture.md § Stable identifiers with
  its pin.
  - **After:** B2.
  - **Decisive:** two `Pacer`s on one `MemoryKvStore` at 20 a second
    send 200 interleaved sends and never exceed 20 in any one-second
    window; replacing the compare-and-swap by a plain put fails it. The
    live runs on Redis and Postgres go through the store conformance
    suite.
  - **Landed:** `KvRateLimiter` (module `pacer`, opt-in: `TokenBucket`
    stays the default). Each number's record, under `PACER_NAMESPACE`
    (`wa.bot.pacer`, keyed by the SHA-256 of the length-prefixed phone
    number id, as the cooldowns' keys), is a 49-byte version 1 layout
    (a version byte, then the due time, the raised rate, the slow-down's
    divisor and times, the last slot's id and base), read and swapped on
    its version, expiring `KvRateLimiter::idle_expiry` (24 hours by
    default) after its last slot. Under contention it reads again after a
    jittered backoff (1 ms doubling to 64 ms), at most 8 times
    (`KvRateLimiter::contention`), then `StorageError::Busy`; a store
    error, contention past the bound or a record of another version
    fails closed (the booking fails, nothing is sent: a broadcast ends
    `BroadcastEnd::LimiterFailed`). A throughput upgrade is shared:
    `RateLimiter::raise_rate` (a new method, defaulting to raising
    nothing) writes the raised rate into the record, checked against the
    record read under the same compare-and-swap so it is never lowered,
    and `ThroughputUpgrades` takes any limiter (`ThroughputUpgrades::new`
    generic, `ThroughputUpgrades::shared` over `Arc<dyn RateLimiter>`); a
    slow-down is shared too. The replicas' clocks are assumed
    synchronized (NTP): a skew lets up to `⌈skew × rate⌉` more into a
    one-second window (documented on `KvRateLimiter`), and the schedule
    never moves back. The decisive test, in
    `crates/meta-whatsapp-bot/tests/shared_pacer.rs`:
    `two_pacers_on_one_store_never_exceed_20_a_second` (the two replicas
    of each pair both read the record before either writes, driven by a
    store that yields after each read; with a plain put, 40 sends fall in
    one second). The others fail on their own mutation:
    `two_bookings_racing_for_one_slot_both_succeed_in_different_slots`
    (the plain put), `contention_past_the_bound_is_busy_after_backing_off`
    (the bound removed, or the backoff), `a_raised_rate_is_shared_and_never_lowered`
    (the never-lower check), `a_store_that_fails_lets_nothing_through`
    and `an_unreadable_record_fails_closed_and_is_kept` (failing open),
    `a_slot_given_back_is_reused_only_when_none_was_booked_after_it`;
    `an_idle_record_expires` the TTL, and the unit tests
    `the_pacer_key_is_pinned_and_hashed` and `the_pacer_record_is_pinned`
    the namespace, key and bytes. The pacer needs no new `KvStore`
    behaviour (`get`, `put_if_absent`, `compare_and_swap` with
    `Expiry::After`), so the store conformance suite proves what it
    relies on, on Redis and Postgres; no live test runs the limiter
    itself there. A slot given back is reused only while it is the last
    booked. The namespace and the record layout wait for the owner's
    confirmation before the first release (§ Owner touchpoints).
  - **Review (concurrency):** a new record's slot ids start at a random
    value, so a slot of a record deleted or expired since gives back
    nothing of the record created after it
    (`a_slot_of_a_forgotten_record_gives_back_nothing`). Tests added for
    what no test caught: a booking that lost the swap books as of its
    new read and never before its request
    (`a_booking_that_lost_books_again_as_of_its_new_read`,
    `a_booking_that_lost_never_books_before_its_request`), the burst,
    a multi-send cost, the divisor bound, a slow-down after recovery, an
    upgrade whose store fails, a seeded interleaving property of three
    replicas booking, giving back and racing
    (`any_interleaving_keeps_every_kept_slot_an_interval_apart`), and
    record round trips with far instants saturating. Documented:
    deleting the record (`KvRateLimiter::store_key`) is the remedy for
    an unreadable record, slots a clock stepped forward booked, and a
    raised rate to lower, and drops the slots booked; a raised rate is
    forgotten silently when the record expires. Not changed: with a
    burst above one, a slow-down (or a lower rate) while slots are booked
    re-reads the burst's tolerance at the slower interval, so the next
    bookings can start before ones booked earlier; `TokenBucket` does
    the same, and the default burst of one is not affected.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each a setting or behind
    `RateLimiter`): it fails closed, a broadcast stopping on
    `StorageError::Busy` too, the attempts and backoff a setting
    (`KvRateLimiter::contention`); the defaults of 8 attempts, a 1 ms
    base delay doubling to a 64 ms cap with jitter, and a 24-hour idle
    expiry (`KvRateLimiter::idle_expiry`); a number's rate is the higher
    of the configured one and the raised one; `RateLimiter::release`
    gives back the last slot only, one level deep; the shared schedule
    never moves back; the name `KvRateLimiter`. **Pending the owner**
    (a stable identifier): the namespace `wa.bot.pacer` and the version
    1 record layout.
- [x] **B3. Durable scheduling** (`meta-whatsapp-bot`; row 90; D29): jobs
  as a typed store on `KvStore` (a bucketed due-time index, claims by
  compare-and-swap with a lease; no new port), send at a time, cancel,
  retry, survive a restart. Companion: architecture.md § Stable
  identifiers for the jobs' namespace.
  - **After:** B1.
  - **Decisive:** two runners on one store send a due job once
    (replacing the compare-and-swap by a plain put fails it); a job
    scheduled before a restart is sent by a new runner on the same
    store; a resumed broadcast job never resends a recipient recorded as
    sent (removing the per-recipient marker fails it); a `131049`
    recorded from a status webhook (the job keeps each message's id)
    makes a re-run within 24 hours skip that recipient and send everyone
    else.
  - **Landed:** row 90 done in the library (with Meta's own campaign
    schedules, L10b). The module `jobs`: `KvJobs` (`KvJobs::schedule`,
    `KvJobs::cancel`, `KvJobs::rerun`, `KvJobs::get`, `KvJobs::observe`,
    `KvJobs::feedback`), `Job`, `JobId`, `ScheduledJob`, `JobState`,
    `JobSummary`, `JobRunner` (`JobRunner::tick`, `JobRunner::run_until`)
    and its builder, `JobFeedback` (the `131049` path as a bot listener or
    an event sink), `TickReport`, `JobRun`, `JobEnd`; five store
    namespaces under `wa.bot.jobs`, pending the owner's confirmation
    (architecture.md § Stable identifiers, § Owner touchpoints below). A
    job's message is not stored: a composer of the runner makes it from
    the job's JSON payload at each send, so every replica and every
    release after a restart registers the same one. The decisive tests,
    in `crates/meta-whatsapp-bot/tests/jobs.rs`:
    `two_runners_on_one_store_send_a_due_job_once` (both runners read the
    job at one version through a gate in the store; with the claim's
    compare-and-swap replaced by a plain put both claim and it fails; the
    send itself is guarded a second time by the recipient's marker),
    `a_job_scheduled_before_a_restart_is_sent_by_a_new_runner`,
    `a_resumed_broadcast_never_resends_a_recipient_recorded_as_sent`
    (fails without the per-recipient marker, written or read) and
    `a_131049_status_makes_a_rerun_within_24_hours_skip_that_recipient`
    (Meta's failed-status fixture; fails without the limited skip). Each
    of these fails a test too:
    `a_crashed_runners_lease_holds_the_job_until_it_expires` (the lease
    ignored), `a_timed_out_send_is_marked_unknown_and_never_resent` (a
    timed-out send resent), `a_job_cancelled_while_it_runs_stops_before_its_next_send`
    (no cancel check), `a_job_indexed_while_the_cursor_moves_is_not_lost`
    and `a_rerun_into_the_minute_it_ran_in_is_not_lost` (the cursor's
    second read, the page written again). The runner paces through any
    `Pacer`, B2b's `KvRateLimiter` included
    (`jobs_pace_through_a_pacer_shared_by_replicas`). Not in it: a
    job-store trait (B3b; here the swap is the `KvStore` adapter, and the
    runner's outbound, pacer, policy and composer), recurring schedules,
    a job's lines per recipient beyond its summary, and live runs on
    Postgres and Redis (the jobs use only the port the store conformance
    suite checks).
  - **Sabotage review** (concurrency, at-most-once, crash safety), each
    fixed with a test that fails without the fix:
    `a_runner_failing_right_after_moving_the_cursor_loses_no_job` (a
    runner failing, or crashing, between moving the cursor and reading
    the minute again lost a job indexed meanwhile, for good: the cursor
    now keeps the minute's page versions and every tick reads it again
    first); `a_job_due_long_ago_is_run_without_reading_every_minute_since`
    (a due time a month past moved the cursor back a month, 86 410 reads
    for one job, the Unix epoch some 56 million: a due time already past
    is indexed under the current minute, its due time kept);
    `an_unreadable_marker_is_an_outcome_unknown_not_a_stuck_runner` (one
    unreadable marker failed every tick at that job, every later job
    waiting behind it: it is now an outcome unknown, never resent);
    `a_long_marker_scan_keeps_the_lease` (reading many markers outlasted
    the lease, and another replica took the job over, again and again
    with more replicas: the scan renews it);
    `a_send_checked_before_a_cancel_and_a_rerun_is_not_made` (a cancel
    and a re-run between a runner's check and its marker let the old
    run's runner send to someone the re-run had sent to, its marker
    being the old run's: the job is read again with the marker in place).
    Tests added for what held: a lease that expires mid-send while
    another runner claims the job, the marker stopping the second send
    (`a_runner_whose_lease_expired_mid_send_never_sends_after_the_new_claim`),
    the lease renewed between long sends
    (`a_runner_renews_its_lease_between_sends`), the cursor's second read
    before a tick moves on through more minutes, against a writer whose
    clock lags
    (`a_job_indexed_behind_a_cursor_moving_on_through_minutes_is_not_lost`;
    the first tick's own re-read covers only the minute just behind the
    cursor), a cancel racing a claim
    in both orders (`a_cancel_racing_a_claim_sends_nothing`), the 131049
    skip still on two minutes short of 24 hours, and scoped to its job
    (`a_131049_skip_is_scoped_to_its_job`); the key pins are known
    answers computed outside the crate. Documented: at most once, not
    exactly once (a crash after the marker and before the request leaves
    that recipient unsent, reported unknown); leases read each runner's
    clock (skew, or a pause past the lease, costs a takeover, never a
    duplicate). Not changed: a tick runs its jobs one after the other
    (a long broadcast delays the jobs due after it) and `run_until`
    stops between ticks only; a job left unrun past its index pages'
    retention stays pending, never found (decided below).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each a setting, or swappable
    through the `KvStore` adapter until B3b): the namespaces
    `wa.bot.jobs`, `wa.bot.jobs.due`, `wa.bot.jobs.sent`,
    `wa.bot.jobs.message` and `wa.bot.jobs.limited`; minute buckets of
    256 ids a page (`BUCKET_SECONDS`, `BUCKET_PAGE_CAPACITY`); a
    2-minute lease renewed at half (`JobRunnerBuilder::lease`); the
    `131049` skip scoped to the job, its 24 hours counted from the
    webhook's arrival (`MARKETING_LIMIT_WAIT`); retention of 7 days for
    message ids (`KvJobs::message_ttl`) and 30 days for settled jobs,
    markers and index pages (`KvJobs::retention`), **a job left unrun
    past 30 days no longer found**; job retries 5 × (30 s doubling to
    30 min) (`JobRunnerBuilder::retries`, `JobRunnerBuilder::retry_delay`);
    8 sends in flight per job (`JobRunnerBuilder::concurrency`); the
    default runner id of the process id, the time and a counter, each
    replica setting its own (`JobRunnerBuilder::runner_id`); D29's
    "the job store is a trait" met for now by the concrete `KvJobs`,
    swappable through its `KvStore` adapter, the trait itself B3b
    (design D29 amended); row 90 done in the library. **Pending the
    owner** (stable identifiers, before the first release): the five
    namespaces, their keys' derivations and their records' layouts
    (architecture.md § Stable identifiers, § Owner touchpoints below).
- [x] **B3b. A `JobStore` trait** (`meta-whatsapp-bot`; D29): the job
  store's operations (schedule, cancel, re-run, get, the 131049
  feedback, and the runner's claim, markers and due index) behind a
  trait the runner takes, `KvJobs` its default, so a job queue of the
  integrator's own can stand in.
  - **After:** B3.
  - **Decisive:** `KvJobs` implements it, and a second, in-memory
    implementation passes the same job suite.
  - **Landed:** `JobStore` (module `jobs`), object-safe, the runner
    holding an `Arc<dyn JobStore>` (`JobRunner::builder` takes any
    `impl JobStore`, `JobRunner::builder_shared` an `Arc`; `Arc<T>`
    forwards): `schedule`, `cancel`, `rerun`, `get`, the `131049`
    feedback (`limit_message`, `observe` provided on top of it), and the
    runner's steps: `claim_due` (the store claims each due job and hands
    it, a `JobClaim`, to a `ClaimHandler`, the runner's pass, whose
    `JobEnd` tells `KvJobs`'s due index whether to keep the minute),
    `hold` (the claim still holds, renewed at half the lease),
    `check_run` (the run unchanged, not cancelled), `recipient` (a
    `RecipientMark`), `mark_sending`, `mark_outcome` (a `SendRecord`),
    `unmark`, `limit_recipient`, `settle`, `retry` (each
    `Option<JobHalt>`); `clock`, the time jobs are due and leases
    expire by. Its rustdoc is the contract under concurrency (claims
    atomic under a lease, a claim checked on every write, markers
    created atomically and durably before the send and only their
    runner's to complete or remove, a "sending" nobody completed an
    outcome unknown, cancels winning or losing whole, the skip per job,
    each job handed over once a call and no lock held while it sends).
    `KvJobs` implements it with B3's code moved behind the trait (the
    runner's cursor, claim, marker and settle steps now `KvJobs`'s; the
    records, keys and namespaces unchanged). `MemoryJobs`, in memory
    behind one mutex, independent of `KvStore`, is the second
    implementation. `jobs::conformance::run` is the job suite, 24
    checks: B3's tests that do not depend on `KvStore` (moved out of
    `crates/meta-whatsapp-bot/tests/jobs.rs`, names kept) plus two
    races (two to five runners on one job, four on twenty, interleaved
    at every call they make to the store), a runner ticking during
    another's send, a retry within a tick, and each method on its own
    (`markers_are_exclusive_and_their_runners`,
    `a_claim_is_checked_on_every_write`). In `tests/jobs.rs`:
    `kv_jobs_pass_the_job_suite`, `memory_jobs_pass_the_job_suite` (the
    decisive pair), `kv_jobs_pass_the_job_suite_raced_inside_the_store`
    (on a `KvStore` that yields at every call: replacing the claim's
    compare-and-swap by a plain put fails it, as it fails
    `two_runners_on_one_store_send_a_due_job_once`), and
    `runners_on_threads_send_each_recipient_once` (six runners on four
    threads, twenty rounds, both stores); `KvJobs`'s own tests (the due
    index, unreadable records, a failing store, the restart on one
    `KvStore`) stay. Mutations of `MemoryJobs`, each failing the suite:
    the claim checked, then an `.await`, then written
    (`two_runners_racing_one_job_send_it_once`); the same without the
    `.await` (only the threaded test, in three runs of four: the suite
    runs on one task); no "sending" marker before the request
    (`a_resumed_broadcast_never_resends_a_recipient_recorded_as_sent`),
    a marker created over an existing one
    (`a_runner_whose_lease_expired_mid_send_never_sends_after_the_new_claim`),
    the `131049` skip ignored, or extended by a redelivery
    (`a_131049_status_makes_a_rerun_within_24_hours_skip_that_recipient`),
    the lease ignored, `check_run` blind to the run
    (`a_send_checked_before_a_cancel_and_a_rerun_is_not_made`) or to a
    cancel (`a_cancel_racing_a_claim_sends_nothing`), `hold` never
    renewing (`a_long_marker_scan_keeps_the_lease`), `settle` not
    checking the claim; and of the runner, the request made before the
    marker. Not in it: a suite for a store on a clock of its own (a
    database's `now()`: the suite moves a `ManualClock`), and
    durability across processes (a restart in the suite is a new
    runner on the same store value).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; design D29's amendment closed; each
    swappable by implementing the trait): the trait object-safe
    and held as `Arc<dyn JobStore>` (not a generic `JobRunner<S>`);
    claims through a callback (`claim_due` with a `ClaimHandler`)
    rather than a list of due ids and a `claim(id)`, which keeps
    `KvJobs`'s cursor logic whole and fits a `FOR UPDATE SKIP LOCKED`
    loop; recipients addressed by their index into the claimed job;
    the store owning time (`JobStore::clock`) and the lease's
    half-renewal rule written into `hold`'s contract; `KvJobs`
    re-reading a marker before completing or removing it (one more
    `KvStore` read per send) rather than carrying the marker's version
    through the trait; the operations moved off `KvJobs` onto the trait
    (callers import `JobStore`) rather than kept as inherent twins;
    `MemoryJobs` public, nothing expiring in it but the `131049` skips
    and the message ids; the suite in the library, always compiled, as
    the adapters' `KvStore` suite is; design D29's amendment ("until
    B3b") to be closed.
- [x] **B4. Retention and auto-delete** (`meta-whatsapp-bot`; row 91): by
  age and a cap per chat, through `ConversationStore`'s erasure.
  - **After:** B1, L5.
  - **Decisive:** removing the age filter fails the test; the erased
    messages are gone from `messages` and `conversations` on memory and,
    live, on Postgres.
  - **Landed:** L5 gave the age (`purge_before`, `apply_retention`);
    the cap per chat is a new port method,
    `ConversationStore::trim_conversations(phone_number_id, keep_last)`
    (no composition of the existing ones deletes some rows of one
    conversation): each conversation keeps its newest rows by
    `(timestamp, id)`, every row counting (tombstones and redacted group
    messages too), and the summary goes when its latest message does
    (the newest rows all tombstones, or a cap of 0), as under a purge;
    window events, ownership records, contact removals, synced contacts
    and identity links stay (D35). Memory under its lock; Postgres in
    one statement under the existing purge lock, ranking on the
    existing `messages_conversation_idx`: no migration and no new stable
    identifier. In the bot, module `retention`: `RetentionPolicy`
    (`max_age`, `max_messages_per_chat`), applied once
    (`RetentionPolicy::apply`) or on the bot's `Timer`
    (`RetentionSweeper`, every hour by default, restricted to the bot's
    numbers with `number`). The decisive tests:
    `the_age_filter_deletes_exactly_what_is_older_in_memory` and its
    `live_postgres_` twin (`crates/meta-whatsapp-bot/tests/retention.rs`:
    disabling the policy's age, setting its cutoff to now, or removing
    the memory store's age filter fails them), and
    `a_sweep_deletes_by_age_and_cap_from_messages_and_conversations_*`
    (the chats gone from both, memory and live). The cap's off-by-ones,
    a trim that leaves the summary, and a reversed tie order fail the
    conformance case `trim_keeps_the_newest_rows_of_each_conversation`
    (memory, and live under both erasure modes); a trim deleting the
    summary from its own snapshot fails
    `live_postgres_a_trim_keeps_the_summary_an_append_moved`; a trim in
    two statements, which loses the message appended between them,
    fails `live_postgres_trims_racing_appends_keep_every_new_message`
    (three runs of three); a trim without the purge lock fails
    `live_postgres_the_locks_are_the_documented_ones`. Not caught: the
    key order of the summaries' locks (the ranking already yields them
    in key order); in memory a trim cannot race an append (one lock).
    SQLite and Redis implement it since L8's merge of B4: SQLite in one
    `BEGIN IMMEDIATE` transaction (summaries, then rows, on one
    ranking), Redis in one script per conversation of at most 1024 rows
    (several for a conversation further over its cap), over a new set of
    each number's conversations with a history (`B:c:<pn>`); both pass
    the trim cases, and `live_redis_a_trim_beyond_one_script_takes_several`
    fails a trim stopping after one script.
    The server compiles and passes unchanged (it implements no
    `ConversationStore`); retention over HTTP is M5k. The sabotage
    review added what three surviving mutations showed missing: a cap
    shared by the same contact on two numbers fails the conformance
    case (its every-number part), a sweeper sweeping only its first
    number fails `the_sweeper_sweeps_every_number_given_and_no_other`,
    and a sweeper stopping at its first failed sweep (or logging more
    than counts) fails `retention_logs.rs`. A trim of one number deletes
    by that number's range of the index, no longer by a scan of every
    number's rows; the cost of a trim, and that a deleted summary
    takes its open window with it, are documented on the port and the
    Postgres store.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable: the policy is the
    bot's, over the port, and a deployment calls the port itself for
    another): the name `RetentionPolicy`; a store-wide method shape
    mirroring `purge_before` (one number or every number); a required
    port method, no default; every row of a history counting toward the
    cap, tombstones and redacted group messages too; the summary rule of
    a purge (deleted with its latest message), a cap of 0 allowed; both
    `RetentionPolicy::apply` and `RetentionSweeper`; the age applied
    first, then the cap, a sweep stopping at its first error. Not
    changed, the owner's (§ Owner touchpoints): D35 and L5's erasure
    semantics; a trim applies L5's rules (identity links and synced
    contacts kept, a deleted message's id free again, the summary going
    with its latest message), and adds no rule of its own.

## 3. Library gap batches

By crate and topic; each is one PR through the review pipeline, with
`ScriptedTransport` tests asserting method, path, query, token and exact
JSON from Meta's pages. A batch whose pages are not in the mirror
(parity.md's (nm)) starts by reading them.

- [x] **L4. A code-less `OnboardingRequest` for `resume`**
  (`meta-whatsapp-client`, `client::embedded_signup`; OPEN_QUESTIONS
  #10).
  - **Kind:** additive, plus one breaking type change (the public field
    `OnboardingRequest::code` became an `Option<SignupCode>`; before the
    first release, in the CHANGELOG).
  - **After:** nothing.
  - **Decisive:** a new process resumes from stored session info alone;
    sending a placeholder code to Meta fails the test's request
    assertion.
  - **Landed:** `OnboardingRequest::for_resume(session)`, with no code;
    `onboard` and `onboard_with_approval` refuse a code-less request
    with a `ValidationError` on `code` before any request. The
    decisive test is
    `a_new_process_resumes_from_the_stored_session_info_alone`
    (`embedded_signup::onboard`): a fresh client and vault on the first
    process's store resume from the serialized `SessionInfo`, and its
    two requests (subscribe, register) are asserted exactly, before the
    error the same request gets from `onboard`; a `for_resume` that put
    a placeholder code in would make that `onboard` send a third
    request, the code exchange, and fail the request assertion.
    `a_code_less_coexistence_resume_never_registers` pins the
    coexistence refusal on that path (`finish_kind` set from what was
    saved). The constructors are unchanged. OPEN_QUESTIONS #10 is
    closed and deleted (CHANGELOG). No parity row changes: parity row
    120 and coverage row 1 already counted `resume`.
- [x] **L5. The `ConversationStore` port change** (`meta-whatsapp-core`,
  `meta-whatsapp-adapters`; D10; OPEN_QUESTIONS #32, #44; rows 69, 111):
  one change for everything the port gains before M2, so adapters change
  once. Erasure of a person on one number (their keys through
  `identities`, identity links, their group messages redacted or
  deleted: D30, D31), purge by age, retention set per store (D10);
  window events (the calls that reopen the window,
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
    another number; an erasure through `identities` of a BSUID leaves
    nothing of the person's thread under their phone number, and the
    live sweep of every table fails when the erasure's sender clause
    (their group message) is removed.
  - **Landed:** row 91 partial in the library (purge by age, retention
    set per store, erasure of a person on one number; no cap per chat
    and no bot that deletes on its own: B4). Rows 69, 111 and 139 stay
    partial: the inbox records what the port now keeps (synced
    contacts, window events, thread ownership, identity links) in L7
    and L8, and there is no Redis or SQLite store (L8) nor thread
    control API (L15). OPEN_QUESTIONS #33 stays open. The erasure's
    defaults and stored names wait for the owner before the first
    release ([§ Owner touchpoints](#owner-touchpoints)).
- [x] **L7. Window events and thread ownership in the inbox**
  (`meta_whatsapp_rs::inbox`; OPEN_QUESTIONS #32, #44; rows 119, 139):
  `InboxSink` records the calls that reopen the 24-hour window and
  standby messages as window events (never unread), and ownership from
  the handovers; `Inbox::reply` refuses locally when another app owns
  the thread; a caller's explicit override of the local check. It also
  records L5's identity links (`ConversationStore::link_identity`), so
  that an erasure finds a person's other keys: from `user_id_update`
  (`WebhookEvent::UserIdChanged`), from a number-change `system`
  message, and a phone number to BSUID link when an inbound message
  carries both `from` and `from_user_id` (a thread keyed by the `wa_id`
  before BSUIDs is found only through one).
  - **Kind:** additive (on L5's port).
  - **After:** L5.
  - **Decisive:** after a scripted call, `Inbox::reply` sends a
    free-form reply it refused before; after `control_taken`, a reply is
    refused locally with zero requests; after a `user_id_update`,
    `Inbox::identities` of the new BSUID holds the previous one; each
    fails when its recording is removed.
  - **Landed:** the decisive tests and the rules they rest on run on the
    memory store and live on Postgres
    (`crates/meta-whatsapp-rs/tests/inbox_events.rs`; `just test-live`
    now runs the facade's `live_postgres_` tests). The override is per
    inbox, `ReplyChecks` (the window check, its trust in a handover to
    this app, and the ownership check, each on or off; per call through
    a clone of the inbox). A handover is recorded under the conversation
    its phone number leads to (the identity links, else a synced
    contact's BSUID, else the phone number). The coordinator's rulings
    on the review (2026-09-27): the ownership refusal has an `ErrorKind`
    of its own, `ThreadOwnedElsewhere` (no Graph code yet), which the
    service answers `409 thread_owned_elsewhere`; each recording is a
    switch (`InboxSink::with_recording`), the rules are public
    (`call_window`, `call_key`, `call_status_key`, `handover_key`) and
    the idle timeout a setting (`Inbox::with_thread_idle_after`), rather
    than a trait; after a handover to this app newer than the customer's
    last recorded message, the window check lets Meta decide
    (`ReplyChecks::trust_handover`, OPEN_QUESTIONS #44); a
    `user_id_update`'s `wa_id` is linked too. Row 139 stays partial (the
    thread control API, L15); `conversation_context` is not stored.
- [x] **L8. Message and contact stores** (`meta-whatsapp-adapters`,
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
  - **Landed:** `store::{SqliteKvStore, SqliteConversationStore}`
    (feature `sqlite`, `store::sqlite::{open, memory, migrate}`) pass
    both suites in process, in `just test` (an in-memory database, and
    a file shared by a pool of connections under another prefix);
    `store::RedisConversationStore` passes the conversation suite live
    (`live_redis_conversation_*`, both erasure modes and a retention).
    The suite gains `conversation_conformance::ids_are_stored_once_per_store`,
    outside `run` (the port allows either answer to OPEN_QUESTIONS #33,
    which stays open): the memory store and both new stores pass it in
    `just test`, Redis and Postgres live; it fails a SQLite store whose
    id index is not unique, and a Redis store checking ids per number.
    `InboxSink` records `smb_app_state_sync`, and
    `crates/meta-whatsapp-rs/tests/synced_contacts.rs` reads Meta's
    examples back through `Inbox::contacts` and `Inbox::contact` on
    memory and SQLite (it fails when a removal is applied under one key
    only). Rows 69 and 111 are done in the library (the service's sides
    stay with M3b and M2a); categories 30 and 39 too. The sabotage
    review added `conversation_conformance::erase_one_person_of_every_record`
    and `purge_one_person_of_every_record` (outside `run`), after which
    each adapter scans its own storage for what must be gone (Redis
    kept a redacted group message's sender and index members naming the
    person, with every port read passing), and `run`'s
    `an_unbounded_limit_reads_everything` (Redis refused `usize::MAX`).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable behind the
    `KvStore` and `ConversationStore` ports, or a switch):
    1. **SQLite through sqlx's driver**, the library compiled in
       (`sqlite-bundled`: MIT, no TLS); sqlx's Postgres driver, its TLS
       and its macros move from the dependency to the `postgres`
       feature, so `sqlite` alone links neither (`just features` checks
       both alone). Its migrations are embedded with `include_str!` from
       `migrations-sqlite/`, templates under the same `TablePrefix`, now
       `store::TablePrefix` (still `store::postgres::TablePrefix`), and
       each prefix keeps its own migration history
       (`<prefix>sqlx_migrations`). Swappable: another store behind the
       same ports.
    2. **SQLite concurrency is SQLite's write lock**: every write one
       `BEGIN IMMEDIATE` transaction (read, decide in Rust, write), the
       busy timeout `store::sqlite::open` sets (30 s) the wait; times
       `INTEGER` microseconds, identifiers `TEXT` refusing U+0000 (as on
       Postgres), content `BLOB`; the key/value store's expiry on the
       application's injectable clock (`SqliteKvStore::with_clock`), its
       versions from one counter row, so a deleted record's row goes at
       once. Swappable by building the pool yourself
       (`store::sqlite::sqlx`).
    3. **One Redis hash tag per conversation store**
       (`<prefix>{conversations:<prefix>}`, so stores of other prefixes
       hash to other slots: inboxes spread with one store per prefix,
       `with_prefix`): every write one Lua script over keys of one slot.
       Status updates are a compare-and-swap decided in Rust
       (`DeliveryStatus::supersedes`); a purge of every number is one
       script per number over the store's set of numbers, which never
       shrinks (it names businesses only). Times are 16 hex digits that
       sort as they compare.
    4. **The inbox's contact rules, as built**: a switch,
       `RecordingSwitches::contacts`, on in the library's
       `RecordingSwitches::ALL`; an `add` stored under
       `inbox::synced_contact_key` (the BSUID, else the phone number
       without `+`, which is also the phone number stored, so that
       `identities` meets the conversations' keys); a `remove` under
       each key it names (the BSUID and the phone number), so that an
       entry keyed by the phone number before Meta sent the BSUID does
       not keep the person's names; a change without
       `metadata.timestamp`, without a usable id or with U+0000 in one
       is skipped with a warning, without content (Meta's order is the
       timestamp; the sink's clock would let a late redelivery win). A
       `remove` naming only a phone number leaves an entry keyed by the
       BSUID (Meta's reference shows a removal with both).
    5. **Category 39's library side is done**; MongoDB adapters stay
       with S17.
    6. **The service records no synced contacts until M3b**:
       `meta-whatsapp-server-core`'s `inbound::INBOX_RECORDING` is the
       library's switches with `contacts(false)`, the events still
       reaching their tenant (decisive:
       `conformance::webhooks::an_address_book_change_reaches_its_tenant_and_no_inbox`);
       M3b turns it on with the routes that read them.
  - **Pending the owner's confirmation before the first release**
    (architecture § Stable identifiers): the new stored names, SQLite
    migration 1 (`wa_kv_version`, the SQLite tables and their columns,
    its checksum pinned) and the Redis conversation layout (keys,
    fields, the hash tag, the time encoding).
- [x] **L9. Phone numbers** (`meta-whatsapp-client`,
  `client::phone_numbers`; rows 66, 129–133, 151): the business
  username calls, deleting a contact book entry, search visibility,
  security notifications and number-change notices, the Official
  Business Account request and status, business compliance
  information, health status on numbers, WABAs and businesses, bot
  details.
  - **Kind:** additive, plus one breaking type change
    (`templates::HealthStatus` became the shared
    `client::common::HealthStatus`, its `can_send_message` typed; before
    the first release, in the CHANGELOG).
  - **After:** nothing.
  - **Decisive:** exact JSON per field from
    `reference/whatsapp-business-phone-number/*`; dropping any one field
    from its body fails its test.
  - **Landed:** rows 66, 129–133 and 151 done in the library; the
    service's side stays with M5c3. One `client::common::HealthStatus`
    serves numbers, WABAs and templates (`templates::HealthStatus` is
    that type now, its `can_send_message` typed); Meta documents no
    `health_status` on the business node, so a business's status is the
    `BUSINESS` entity of the others. The contact book deletion is an
    explicit call nothing else in the library makes. Bot details take a
    new `core::ids::WabaBotId`. Not offered, for want of a documented
    field: withdrawing an Official Business Account application. The
    username errors `147001`–`147005` are not classified in `ErrorKind`
    yet. The number's settings it leaves (payload encryption,
    `connection_status`, `webhook_url`) are L26.
- [x] **L10a. The parent BSUID accounts API, and the credential host
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
  - **Landed:** row 152 done in the library; the service's side stays
    with M5c4. `Business::parent_bsuid_account` returns a
    `ParentBsuidAccount` (the account id a string: core has no id type
    for it). The allow list is data now, one rule per line next to
    `GraphRequest`: the media host's rule as it was, and
    `GET https://api.facebook.com/{digits}/parent-bsuid-accounts` with no
    version, query, fragment or user info, nothing else on that host.
    Neither rule follows a configured Graph proxy (the token goes to the
    proxy, the media host and this URL); routing every request through a
    proxy is the transport's. What Meta answers for a portfolio that is
    not enrolled is not documented (a decode error without
    `parent_bsuid_account_id`). A redirect cannot carry the token past
    the rule: the stock `ReqwestTransport` follows no redirect that
    would keep a request's credentials (same scheme, host and port),
    and the `HttpTransport` port states that rule; `GraphRequest::header`
    refuses credential headers, so the rules cannot be stepped around
    (both found in the L10a reviews, CHANGELOG § Security).
- [x] **L10b. WABAs, accounts, billing, history** (`meta-whatsapp-client`,
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
  - **Landed:** rows 127 and 146–148 done in the library, row 90 partial
    there (Meta's schedules wrapped, the bot's durable ones are B3); the
    service's sides stay with M5c4, M5j and M5k. The decisive tests, in
    `client::waba`'s modules: the exact bodies and queries
    (`accounts::create_sends_the_documented_body_and_parses_the_answer`,
    `system_users::a_client_token_is_fetched_with_the_proof_of_the_system_token`,
    `billing::the_intent_sends_the_pages_example`,
    `schedules::create_sends_every_documented_field` and a test per
    read); no replay after a timeout
    (`accounts::a_creation_is_never_replayed_after_a_timeout`, and the
    same for a schedule, a migration intent, a resume and a token
    generation; a `fetch_only` token read, read-only by Meta's page, is
    replayed as a read); the sentinel
    (`system_users::a_system_user_token_never_reaches_debug_or_an_error`:
    the token, the system token, the app secret and the proof reach no
    `Debug` and no error). The client business's token takes the
    `appsecret_proof` of the partner's, computed from the app secret
    and sent in a form body (`embedded-signup/hosted-es`), its
    parameters from `facebook-login/facebook-login-for-business`
    (§ Get tokens); L11a builds on it. The billing migration's intent and
    resume are explicit calls no other call makes. Not found or not
    offered: an API call for partner-initiated creation (the page
    describes a Business Suite flow only); the creation's
    `on_behalf_of_business_id` (the On-Behalf-Of model is deprecated);
    `GET /{BUSINESS_ID}/whatsapp_business_accounts` (not in the item;
    its `business_type` array has no shown encoding); where a message
    history id or a schedule's `audience_id` comes from (no page says).
    Message history, migration, schedule and solution ids are strings:
    core has no type for them yet. Decided by the coordinator
    (2026-11-01): the billing migration is not a legal act in AGENTS.md's
    sense (it accepts no terms), so it stays two explicit calls and no
    operator gate; a `fetch_only` token read is replayed
    (`facebook-login-for-business`: "The operation is read-only"); the
    token request goes in a form body, never the query; the new ids stay
    `String`; business users are keyed by `SystemUserId`; the Line of
    Credit id goes with the intent (`extended_credit_id`), as the
    requests show; row 90 is partial; the overlap with L12, which landed
    first (PR #67), is reconciled here: `WabaSolution` keeps an optional
    id (a `SolutionId`, returned only when `fields` asks) but takes
    `partners::solutions`' `SolutionStatus`, `PendingRequestStatus` and
    `SolutionApp`, and the payment-method migration's status is
    `PaymentMigrationStatus`, apart from the WABA migration intent's
    `partners::migration::MigrationStatus`. Found in review: system users are read from the
    business portfolio (`Business::system_users`,
    `GET /{BUSINESS_ID}/system_users`), the edge the Marketing API's
    Business node documents, not from the WABA the guide's request prints
    (its prose asks for a business id); a non-2xx answer to the token
    request never quotes its body.
- [x] **L11a. Hosted Embedded Signup** (`meta-whatsapp-client`,
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
  - **Landed:** row 121 done in the library (the service's side stays
    with M3f). `HostedSignup` (the webhook's `waba_info.waba_id` and
    `owner_business_id`, and the token request, `fetch_only` by
    default) and `EmbeddedSignup::onboard_hosted` /
    `onboard_hosted_with_approval`: step `fetch_token`
    (`Business::system_user_access_token` on the customer's portfolio,
    with the system token passed in and the app secret of
    `AppCredentials`, borrowed) in place of `exchange_code`, then the
    code flow's steps, shared with it (`onboard_granted`); `verify_assets`
    also refuses a WABA whose owner, as Meta reports it, is another
    business than the webhook's. Resumed by `resume` with
    `OnboardingRequest::for_hosted`. The decisive tests, in
    `embedded_signup::hosted`:
    `a_partner_added_fetches_the_token_with_the_proof_and_onboards`
    (the form body exactly, the proof both a fixed vector computed with
    `openssl dgst -sha256 -hmac` and an HMAC the test computes),
    `a_wrong_app_secret_sends_another_proof`,
    `the_stored_record_is_the_code_flows` (the same keys, clear fields
    and sealed record but the random nonce), a failure at each step
    before `store_token` re-run and after it resumed,
    `a_fetch_is_replayed_and_a_generation_is_not`; and through the
    webhook parser, the skill's `hosted_signup.rs` (Meta's
    `PARTNER_ADDED` example, verbatim). Found on the way: the webhook
    names no tenant and also comes for code-flow onboardings, and the
    hosted page asks for no PIN; both are the integrator's, documented
    in the guide. Found in review: `PARTNER_ADDED` comes for every WABA
    shared with the app (the code flow's and coexistence's onboardings
    too) and may be delivered again, and `store_token` replaces a
    record (as the code flow's re-onboarding must), so a hosted run
    after a code-flow onboarding would have replaced its token and
    `created_at` (and with it a Solution Partner's recorded approval)
    unasked; `onboard_hosted` now refuses a WABA the vault holds,
    before any request (a presence read, an unreadable record counting
    as held; `a_waba_the_vault_holds_is_refused_before_any_request`),
    and re-onboarding one is deleting it first. A read, not a lock: a
    code-flow onboarding still running when the webhook arrives can
    store after it; the approval's atomic reservation closes that
    window, and the rustdoc says so.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable): the token
    request is `fetch_only=true` by default, swapped per signup by
    `HostedSignup::token_request` (a fetch is replayed as a read, a
    generation never is); the system user token is an explicit argument
    of `onboard_hosted`, not held by `EmbeddedSignup`; a WABA whose
    owner, as Meta reports it, is another business than the webhook's
    is refused, and when Meta reports no owner none is stored, as in
    the code flow; `debug_token` runs on the fetched token, on the
    assumption that hosted signup's business tokens carry
    `granular_scopes` naming the WABA, as the code exchange's do (one
    that does not fails `verify_assets` on `granular_scopes`, closed);
    the integrator's, documented: re-onboarding a WABA already held
    (delete it first; the library refuses it otherwise), the tenant
    binding (in the approval) and the PIN; no facade helper (the glue
    stays in the skill's `hosted_signup.rs`); the skill not re-stamped.
    The library's side of OPEN_QUESTIONS #11 is decided; the service's
    is M3f.
- [x] **L11b. Several WABAs in one signup** (`client::embedded_signup`;
  row 122; OPEN_QUESTIONS #5): opt-in onboarding of every granted WABA,
  each bound and resumable on its own.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** with the option off, a signup granting two WABAs
    onboards only the claimed one; with it on, both, and a failure on
    the second leaves the first stored and resumable.
  - **Landed:** row 122 done in the library (the service's side stays
    with M3f). `OnboardingRequest::every_granted_waba` (field and
    setter, off by default) and `EmbeddedSignup::onboard_wabas` /
    `onboard_wabas_with_approval` (module `embedded_signup::multi`):
    `exchange_code` and `debug_token` once; with the option on, a
    session info naming a WABA (`waba_id`, `waba_ids`) the token was not
    granted fails the call at `verify_assets` before anything is read or
    stored; then the claimed WABA (else the newest grant: what `onboard`
    picks) with the request as it is, and every other grant of
    `granular_scopes`' `whatsapp_business_management`, in Meta's order
    (newest first), each through its own `verify_assets`, `approve`,
    `store_token` and tail (its own vault record, phone index,
    subscription, system user and credit line; the shared steps refactored
    out of `onboard_granted` into `onboard_waba`, so `onboard` runs the
    same code). The result is `WabasOnboarded` (`#[must_use]`), one
    `WabaOnboarding` per WABA with a `WabaOutcome`: `Onboarded`, `Held`
    or `Failed { error, steps_completed }` (an `Error::Step`;
    `is_resumable` once `store_token` ran). Meta documents the
    session info's `waba_ids` (`embedded-signup/implementation`) and the
    grants, newest first (`solution-providers/manage-accounts`), and no
    more: nothing on which grants a multi-WABA flow's token carries
    beyond those, and no multi-WABA hosted flow. Nothing new is stored.
    The decisive tests, in `embedded_signup::multi`:
    `with_the_option_off_only_the_claimed_waba_is_onboarded` (through
    `onboard` and `onboard_wabas`; the other grant never read),
    `with_the_option_on_every_granted_waba_is_onboarded` (each request
    exact: one exchange, one `debug_token`, the claimed WABA's owner,
    numbers, subscription and registration, then the other's owner,
    numbers and subscription, each record and phone index under its own
    WABA), `a_failure_on_the_second_waba_leaves_the_first_and_both_resume`;
    with `a_failure_on_the_claimed_waba_does_not_stop_the_others`,
    `another_granted_waba_the_vault_holds_is_left_as_it_is`,
    `a_claimed_waba_the_token_was_not_granted_refuses_the_signup`,
    `one_waba_calls_refuse_the_option_before_any_request` (`onboard`,
    `onboard_with_approval`, `onboard_hosted`),
    `only_the_claimed_number_of_a_coexistence_signup_syncs`,
    `a_solution_partner_approves_records_and_funds_each_waba` (one
    approval per WABA, each recorded for its own token record, a share
    for each though the business-wide lookup lists the first's
    allocation, and a plain `resume` of the second sharing nothing
    again) and `a_refused_approval_skips_that_waba_only`. Mutations that
    each fail them: the option ignored, a WABA's failure returned as the
    call's, a failure stopping the next WABAs, the other WABAs onboarded
    without the approval, a held WABA replaced, the claimed session's
    phone given to the others, an unbacked claim accepted, the others
    registering or keeping the finish, `onboard` or `onboard_hosted`
    accepting the option, `is_resumable` ignoring `store_token`.
  - **Found in review** (sabotage review, security lens): a token's
    `granular_scopes` list every WABA that granted the app, across
    signups, and the token in Meta's own `debug_token` example is a
    user's, whose grants may span businesses; the draft onboarded every
    grant whoever owned it, under the request's credit settings (a
    `reshare_after_revocation` meant for one business re-funding
    another's revoked line). The other grants must now be the primary
    WABA's business's: Meta must report, for each, the owner it reported
    for the primary WABA, or the grant fails at `verify_assets` before
    its numbers are read, approved or stored; when the primary's owner is
    unknown (it failed before it was verified, or Meta reported none),
    every other grant fails there with no request
    (`a_grant_of_another_business_is_not_onboarded`,
    `the_others_fail_closed_when_the_signup_business_is_unknown`). And
    `Held` was a read followed, two Meta round trips and the approval
    later, by a replacing `store`: another signup or a hosted
    `PARTNER_ADDED` storing the WABA in that window was overwritten. The
    other grants are now stored with `put_if_absent`
    (`TokenVault::store_new`, crate-private, the phone index untouched
    when it loses) and fail at `store_token` when the vault came to hold
    them (`a_grant_stored_meanwhile_is_not_replaced`); `onboard_hosted`,
    whose `holds` read had the same window the other way round (a
    code-flow or multi-WABA onboarding storing after it was replaced),
    now stores the same way and fails at `store_token`
    (`a_hosted_signup_never_replaces_a_record_stored_meanwhile`). A read of the
    vault that fails fails the grant closed
    (`a_grant_whose_record_cannot_be_read_fails_closed`); the others keep
    the request's subscription override
    (`the_others_keep_the_subscription_override`) and a revoked
    business is funded again for none of them
    (`a_revoked_business_is_funded_again_for_no_waba`); with nothing
    claimed the newest grant is the primary one, and every record carries
    the code exchange's expiry
    (`with_nothing_claimed_the_newest_grant_is_primary`). Mutations the
    review added that each fail a test: the others' owner checked only
    when reported, or not at all; the signup's owner taken from the
    browser's `business_id`, or not recorded; the others stored with a
    replacing `store` (or `store_new` with `put`); a failed vault read
    taken as absent; the others' subscription override, or the token's
    expiry, dropped. Three survive as equivalent: the `waba_id` claim
    left out of the unbacked check (`verify_grant` already refuses it),
    and the others' `currency` or `reshare_after_revocation` changed
    (the credit plan is computed once, from the request, for every
    WABA).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable):
    1. the option is the request's (`OnboardingRequest::every_granted_waba`,
       OPEN_QUESTIONS #5), and only the new calls, `onboard_wabas` and
       `onboard_wabas_with_approval` (its approval an `FnMut`, called and,
       for a Solution Partner, recorded once per WABA), act on it;
    2. `onboard` and `onboard_hosted` refuse the option rather than drop
       the other WABAs; `resume` ignores it (it resumes the WABA it is
       given);
    3. the token's grants decide what is onboarded, not the browser's
       `waba_ids`: every WABA the session info names must be among them,
       or the signup is refused before anything is stored; the claimed
       WABA first, then the grants in Meta's order; tightened in review,
       a grant other than the claimed WABA is onboarded only when Meta
       reports the claimed WABA's owner as its owner too, and none is
       when that owner is unknown;
    4. the other WABAs register no number and request no coexistence
       sync (the PIN, region and finish are the claimed number's;
       registering counts against 10 per 72 hours, the sync is once per
       number), and keep the request's subscription override and credit
       settings;
    5. another grant the vault holds is `Held` and left alone, and,
       tightened in review, stored only where the vault still holds none
       (`put_if_absent`): the read alone let a signup or a hosted
       `PARTNER_ADDED` racing this one be overwritten; the claimed WABA is
       replaced, as `onboard` replaces it;
    6. per-WABA failures are outcomes in `WabasOnboarded` (`#[must_use]`),
       not the call's error; only the shared steps fail the call;
    7. hosted signup stays one WABA per `PARTNER_ADDED`;
    8. `OnboardingRequest` carries an
       `#[expect(clippy::struct_excessive_bools)]`: each bool is an
       independent option of its own setter, not a state machine.

    Swappable by the option (off: `onboard`'s behaviour), and by calling
    `onboard` per signup instead. Not offered, a follow-up if wanted:
    onboarding a WABA that failed before `store_token` from the token
    another WABA of the same signup stored.
- [x] **L11c. Pre-verified numbers** (`client::embedded_signup` and
  `client::waba`; row 123): pools, adding, codes, sharing, partners
  (`reference/business/*pre-verified*`,
  `reference/whatsapp-business-pre-verified-phone-number/*`).
  - **Kind:** additive, plus two breaking type changes before the first
    release (CHANGELOG): `embedded_signup::PreVerifiedPhone::ids` and
    `waba::NewPhoneNumber::preverified_id` take the new id type.
  - **After:** nothing.
  - **Decisive:** exact JSON per endpoint; a pre-verified phone id is
    checked as digits before any request.
  - **Landed:** row 123 done in the library; the service's side stays
    with M3f. Seven endpoints from Meta's seven pages (the guide, three
    `reference/business/` pages and the four of
    `reference/whatsapp-business-pre-verified-phone-number/`): the pool
    on the partner's portfolio, `Business::add_pre_verified_number`
    (`POST /{Business-ID}/add_phone_numbers`, a `CreatedPreVerifiedNumber`),
    `Business::pre_verified_numbers` and `_stream`
    (`GET …/preverified_numbers`, `ListPreVerifiedNumbers`: `fields`,
    `phone_number`, `code_verification_status`, `limit`, cursors) and
    `Business::share_pre_verified_number`
    (`POST …/share_preverified_numbers`); each number through
    `Client::pre_verified_phone_number` (`waba::PreVerifiedPhoneNumber`:
    `get`, `request_code`, `verify_code`, `partners` and `_stream`, with
    `ListPreVerifiedNumberPartners`, and `delete`), read as a
    `PreVerifiedNumber` (the six fields of Meta's schema, the status the
    phone numbers' `CodeVerificationStatus`). Its id is a new
    `core::ids::PreVerifiedPhoneNumberId`, numeric (the node and
    partners pages' own error examples: "must be a valid numeric
    string"), checked when built and again as digits by every call
    before its path or body is built, so an id built unchecked holding
    `/` reaches nothing; the launch option and a WABA's new number
    (`preverified_id`) take it and check it too. The code is the phone
    numbers' secret `VerificationCode`, in the JSON body. The decisive
    tests, in `waba::pre_verified`:
    `a_pre_verified_id_that_is_not_digits_is_refused_before_any_request`
    (five hostile ids, built unchecked, through each of the eight calls
    that take one, every answer scripted: nothing is sent), one test per
    endpoint asserting method, path, bearer, query and the exact body
    (`adding_sends_the_number_in_the_body_and_parses_the_guides_answer`,
    `the_pool_is_listed_with_every_documented_filter`,
    `sharing_sends_both_ids_in_the_body`,
    `a_code_request_sends_method_and_language`,
    `verify_code_sends_the_code_in_the_body_only`,
    `the_partners_are_listed_with_the_documented_parameters`,
    `a_number_is_read_with_its_fields`,
    `delete_addresses_the_number_and_maps_meta_errors`), with
    `a_verification_code_reaches_no_debug_url_or_error` (a sentinel code
    against a 403) and `no_post_is_replayed_after_a_timeout` (the four
    POSTs, retries on); in core,
    `ids::tests::a_numeric_id_is_one_to_sixty_four_ascii_digits` lists
    the new type; the launch's `pre_verified_numbers_page_example`. Not
    offered: unsharing (Meta documents its `DELETE` as removed) and the
    reference's `IVR` code method (`CodeMethod` has `SMS` and `VOICE`,
    the values both pages show). Meta's pages print no response example
    for the pool list, a number or its partners: those tests parse
    Meta's schemas with our values. No legal act: no endpoint accepts
    terms. The time limits (30 days to verify, 90 to be claimed or
    registered, re-verification from 45 days before expiry) are
    documented, not enforced; tracking who claimed which number stays
    the integrator's, as Meta says.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable): the bodies are JSON, as the references define, not the
    guide's query strings (the code and the phone number stay out of
    URLs, as `PhoneNumber::verify_code` does; swapped per call by
    moving a field to `.query`); a pre-verified id is numeric (one
    `numeric_id_type!` line) and re-checked at request time, as
    `commerce::catalog` does for its ids; the API lives in `client::waba`
    (`Business` for the pool, a new accessor for the number) rather
    than a new module, the launch option staying in
    `client::embedded_signup`; the two breaking type changes above; the
    status filter refuses `UNVERIFIED` and `Unknown`, which the
    reference does not list; `delete` and `share_pre_verified_number`
    are explicit calls no other call makes; `IVR` is left out until a
    page shows it; the phone number to add is checked locally (digits,
    with `+`, spaces, hyphens, parentheses: the reference's formats).
- [x] **L11d. The coexistence sync inside onboarding**
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
  - **Landed:** two steps, not one, because Meta's page has two
    once-only calls: `steps::SYNC_CONTACTS` (`sync_contacts`,
    `sync_type` `smb_app_state_sync`) then `steps::SYNC_HISTORY`
    (`sync_history`, `history`), the last of the tail shared by
    `onboard`, `onboard_hosted` (whose `for_hosted` request names no
    finish, so it never reaches them) and `resume`, after `subscribe_app` and
    a Solution Partner's credit line (Meta's order:
    `onboarding-business-app-users` starts the sync once the
    onboarding steps are done). They run for a coexistence
    `finish_kind` unless `OnboardingRequest::coexistence_sync(false)`;
    Meta's answers are `Onboarded::coexistence_sync`
    (`CoexistenceSync`, `SyncOutcome`). Meta refuses a second request
    (`2593107`) and a late one (`2593108`), and documents no way to
    read whether one was made, so each request is claimed in a record
    before it is posted (`COEXISTENCE_NAMESPACE`, `wa.es.coexistence`,
    keyed by phone number, scoped to the stored token record so that a
    new signup syncs again; a new stored identifier, pending the
    owner's confirmation) and `resume` never posts one that was
    accepted, refused, or whose answer was lost; a Meta refusal is
    reported as `SyncOutcome::Refused`, not as a failure. The decisive
    tests, in `embedded_signup::onboard`:
    `coexistence_resolves_the_number_and_never_registers` (each request
    once, exact path, bearer and body, after the subscription),
    `with_the_option_off_no_sync_is_requested`,
    `an_onboarding_without_coexistence_sends_no_sync` (the Cloud API and
    WABA-only flows, `for_resume` and `for_hosted`); with
    `a_resume_never_requests_an_accepted_sync_again` (on a clock off the
    whole second: the record keeps the vault's seconds),
    `resume_after_a_refused_request_asks_only_for_what_is_missing`,
    `a_timed_out_sync_is_never_replayed` (retries on, then `resume`),
    `a_sync_meta_refuses_is_reported_and_not_asked_again`,
    `a_new_onboarding_of_the_number_syncs_again` (one second later: the
    scope's precision), `the_sync_record_is_pinned`; and, from the
    review, `a_claim_lost_to_a_concurrent_attempt_posts_nothing` (both
    the `put_if_absent` and the `compare_and_swap` claim),
    `an_answer_is_never_recorded_over_a_later_onboarding`,
    `a_resume_never_takes_over_a_later_onboardings_record` and
    `a_4xx_releases_the_claim_and_a_5xx_keeps_it` (a 409 releases, a
    500 with a Graph body keeps). Hosted signup syncs nothing: Meta's
    hosted page onboards to Cloud API only. M3b builds on the steps
    rather than calling `smb_app_data` itself (OPEN_QUESTIONS #7).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable): the record is
    `wa.es.coexistence`, keyed by phone number id, plain JSON of layout
    `v` 1 (the WABA, the token record's `created_at` in seconds, a
    `pending`, `requested` or `refused` slot per request), its namespace
    and layout still **pending the owner's confirmation before the first
    release** (`docs/architecture.md` § Stable identifiers, § Owner
    touchpoints); two steps, two names (`sync_contacts`,
    `sync_history`), as Meta has two once-only calls; placed after the
    credit-line steps, in Meta's order (the sync begins once onboarding
    is done), at the cost that a failure before them holds them back
    within Meta's 24 hours (the guide says so); Meta's refusals
    (`SyncNotAllowed`: `2593107`, `2593108`) reported as
    `SyncOutcome::Refused`, not as a failure; a failure releases its
    claim only when `!may_have_been_sent()` (a 4xx: Meta's errors page
    gives codes, not statuses, for this endpoint); a lost answer, or a
    process stopped between claim and answer, keeps the claim, and the
    way out is manual and documented (check the webhooks, resume with
    the sync off, ask again yourself: Meta refuses a repeat), a call
    that clears a pending claim being **a follow-up, not scheduled**;
    the option is `OnboardingRequest::coexistence_sync(bool)`, on by
    default, and the record never expires; M3b builds on these steps
    (OPEN_QUESTIONS #7, confirmed). Swappable by the option (and by
    `PhoneNumber::sync_smb_app_data` called yourself).
- [x] **L11e. A token-expiry signal** (`client::embedded_signup`; row 6;
  OPEN_QUESTIONS #8): a configurable lead time before a stored business
  token's `expires_at`.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** under a fake clock, a token expiring inside the lead
    time is reported and one expiring after it is not; removing the
    comparison fails the test.
  - **Landed:** row 6 done for the library. `TokenVault::token_expiry(waba_id,
    lead)` answers, on the vault's clock, with a `TokenExpiry`: `Never`
    (no `expires_at`: Meta reported none, or `0`), `Fresh`,
    `ExpiresSoon { expires_at, remaining }` (`now + lead` included) or
    `Expired` (`expires_at <= now`, as `StoredBusinessToken::is_expired`);
    `is_due()` is the last two. `DEFAULT_EXPIRY_LEAD` is 7 days (business
    tokens default to never expiring; one generated to expire lasts 60
    days). It reads the record's clear `expires_at` only: nothing is
    decrypted, no key is needed, nothing is written (no re-encryption on
    read), and the stored format (`wa-rs/token-vault/v1`) is unchanged;
    the clear copy is not authenticated, so `StoredBusinessToken::expiry`
    gives the same answer on a token `get` decrypted. A record naming
    another WABA or of another version is refused. The vault cannot list
    its records: the caller walks the WABAs it holds, and M3e walks the
    service's bindings as its vault rotation does. The decisive tests, in
    `embedded_signup::vault`:
    `a_token_inside_the_lead_is_reported_and_one_after_it_is_not` (a
    `ManualClock`; inside, outside, exactly at the lead, expired, at
    `now`, absent; the clock moved across the boundary) and
    `the_boundaries_of_the_lead_time`; with
    `the_expiry_check_decrypts_nothing_and_writes_nothing` (a record
    under a key the vault lacks still answers; a record under a previous
    key is not rewritten), `an_absent_expiry_never_expires` and
    `the_expiry_check_refuses_what_is_not_this_wabas_record`. Removing
    the lead comparison, `<` for `<=` at either boundary, and an absent
    expiry read as expired each fail them.
- [x] **L12. Partners** (`meta-whatsapp-client`, new client modules; rows
  144, 145): Multi-Partner Solutions (create, accept, reject,
  deactivation, the solution token, an app's solutions and connected
  client businesses) and migration intents.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON per endpoint; the solution token never
    reaches `Debug` (a sentinel test).
  - **Landed:** rows 144 and 145 done for the library, in a new module
    `client::partners`: `Client::partner_solutions`
    (`partners::solutions::PartnerSolutions`: `create`, `accept`,
    `reject`, `send_deactivation_request`, `accept_deactivation_request`,
    `reject_deactivation_request`, `get`, `partners`, `app_solutions`,
    `connected_client_businesses`, each list with a `…_stream`, and
    `access_token`) and `Client::migration_intents`
    (`partners::migration::MigrationIntents`: `create`, which is
    `set_solution_migration_intent`, and `get`); core gains
    `ids::SolutionId` (numeric) and `ids::MigrationIntentId` (opaque).
    The solution token is the client's business integration system user
    token, a bearer credential for the Graph host already allowed: an
    `AccessToken`, decoded with `send_withheld`. No page ties a call to
    terms or an agreement; each state change is its own call, and
    accepting or rejecting a solution cannot be undone. Decisive tests
    (`partners::tests`): `create_sends_the_embedded_creation_body`,
    `create_migration_intent_matches_the_guides`, the guide's answers
    (`get_before_accepting_matches_the_guide`,
    `app_solutions_match_the_guide`, `partners_match_the_guide`,
    `get_migration_intent_matches_the_guide`,
    `deactivation_answers_decode_in_both_shapes`), the sentinel tests
    `access_token_reads_both_shapes_and_never_shows_the_token` and
    `an_access_token_answer_that_does_not_decode_is_withheld`, and
    `state_changes_are_never_replayed`; the lists join
    `request::paging_withheld_tests`. Where the pages disagree (the
    token's answer wrapped in `data` or bare, a deactivation answered
    `success` or with the solution) both shapes decode. A WABA's
    solutions (`GET /{WABA_ID}/solutions`) are left to L10b. Not
    wrapped: `POST /{WABA_ID}/migration_intent`, from an unindexed
    reference page with no example that the guides do not use; the
    `fields` parameter of the two deactivation answers.
  - **Decided** (by the coordinating agent, each swappable): the core ids
    (`SolutionId` numeric, from the Solution Details API's "must be a
    valid numeric string"; `MigrationIntentId` opaque, no page gives its
    shape); the module and accessor names (`client::partners::{solutions,
    migration}`, `Client::partner_solutions`, `Client::migration_intents`);
    both answer shapes of the token (`{"data": [...]}` and the bare
    object); `CreateSolution::validate` checking only what the pages agree
    on (a name, and `MESSAGING` on at least one side: the creation
    reference allows one side, the embedded creation guide's example
    gives it to both); page sizes 1 to 100 (the list references); roles
    not checked locally (the pages disagree on who rejects and who
    answers a deactivation request; Meta decides from the token).
  - **Hardened in review:** a non-2xx answer of the token's endpoint that
    is not Meta's error envelope keeps its body out of `Error::Http`
    (`a_failed_token_answer_never_quotes_its_body`), and a blank
    `access_token` is refused like an empty one
    (`malformed_token_answers_are_withheld`).
  - **Follow-up:** `LaunchOptions::solution_id` stays a `String`; typing
    it as `SolutionId` is a breaking change to the Embedded Signup
    builder, left for its own item.
- [x] **L13. Marketing** (`meta-whatsapp-client`, `client::marketing`;
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
  - **Decisive:** exact JSON per endpoint; a max-price send makes no
    agreement request (a transport scripted for the send alone fails on
    any other call). As first written it also checked the 15-entry
    allow-list cap locally; that test cannot hold: Meta's
    `marketing-messages/changelog` of 2026-10-01 removed partner
    allowlisting with the open beta, so there is no allow list and no
    cap (the local cap checked instead is the 240 country multipliers,
    below).
  - **Landed:** rows 137 and 138 done for the library, against Meta's
    pages of 2026-11-01 (`marketing-messages/pricing`,
    `marketing-messages/pricing/enroll-max-price`,
    `marketing-messages/pricing/duplicate-templates`,
    `marketing-messages/changelog`, `ctwa/welcome-message-sequences`). In
    `client::marketing`: the agreement on `MarketingBusiness`
    (`sign_max_price_agreement`, `max_price_agreement`), duplicating a
    template (`MarketingAccount::duplicate_template`, a
    `DuplicateTemplate` with a `MaxPrice`, answering a
    `DuplicatedTemplate` with the `source_template_id` the guide says
    the response carries), reach estimates
    (`MarketingAccount::reach_estimate`), a template's max price
    (`Client::marketing_template`: `MarketingTemplate::max_price`,
    `set_max_price`, `clear_max_price`) and the welcome message sequences
    (`Client::welcome_sequences`: `WelcomeSequences::create`, `update`,
    `list`, `get`, `delete`); core gains `ids::WelcomeSequenceId`
    (opaque). **The allow list is gone:** Meta's changelog raised the
    partner cap from 15 to 100 (2026-07-31) and 500 (2026-09-07), then
    removed allowlisting with the open beta (2026-10-01): a partner's
    agreement covers the client WABAs shared with or managed by it, and
    no current page shows the allow list's request, so it is not
    wrapped and there is no cap to check; the cap checked locally is
    `optimization_spec`'s 240 country multipliers
    (`marketing::MAX_COUNTRY_MULTIPLIERS`). The agreement is isolated:
    `sign_max_price_agreement` is the one call that sends its `POST`,
    never replayed (whatever the client's `RetryPolicy`: not after a
    timeout, nor on Meta's HTTP 429 within 5 minutes or a throttling
    error, which the policy resends for other calls), and an error naming
    the missing agreement (`100`, a duplicate's 403) is returned, not
    answered by signing. Its errors keep the signer out: a decode error
    carries no body, and an `Error::Http` (an answer that is not Meta's
    envelope) a placeholder (a 429 is `ErrorKind::RateLimited`; Meta's
    wait is 5 minutes from the last email).
    Decisive tests (`marketing::tests_max_price`):
    `a_max_price_send_makes_no_agreement_request` (one scripted
    response, the send's), `no_other_max_price_call_signs_the_agreement`,
    `only_the_agreement_module_names_the_agreement` (no code line of the
    client's, the facade's or the bot's sources but this module's names
    the call or its path, called or not),
    `signing_the_agreement_posts_the_documented_body`,
    `signing_is_not_replayed_after_a_timeout`,
    `signing_is_not_replayed_on_a_429`,
    `a_failed_agreement_answer_never_quotes_the_signer`,
    `duplicate_matches_the_documented_example`,
    `set_max_price_matches_the_documented_update`,
    `clear_max_price_sends_exactly_the_two_nulls`,
    `template_max_price_reads_the_documented_list_shape`,
    `reach_estimate_matches_the_documented_example`,
    `a_max_price_is_checked_against_the_documented_limits` (240 entries
    pass, 241 do not; multipliers in (0, 10]; alpha-2 keys; `bid_amount`
    1 to `i32::MAX`); in `marketing::tests_welcome`, the guide's create,
    update, list, get and delete examples and
    `create_is_not_replayed_after_a_timeout`. Row 154 stays partial: no
    page of 2026-11-01 (`templates/components`, `templates/template-media`,
    `marketing-messages/features`, `marketing-messages/send-marketing-messages`,
    the MM API and Messages API references) shows a GIF header's
    send-time parameter, so none is typed (`templates::Parameter::Other`
    carries one verbatim).
  - **Not wrapped:** the allow list (withdrawn, above); the WhatsApp
    Manager visibility switch
    (`whatsapp_manager_marketing_messages_max_price_enroll_status`,
    `marketing-messages/pricing/whatsapp-manager-visibility`, read in
    review on 2026-11-01: a field of the WABA, read with `fields=` and
    set by `POST /{WABA_ID}`, `OPT_IN` or `OPT_OUT`, which shows or
    hides the max price screens in WhatsApp Manager and changes neither
    eligibility nor API settings; no legal act, outside row 137's
    definition, left for a follow-up); `optimization_spec` on `templates::TemplateDefinition` at
    creation (a template takes its max price after creation with
    `MarketingTemplate::set_max_price`, or as a duplicate); an
    `ErrorKind` for `131061`, `131065` and the welcome sequences'
    `4027xxx` codes (core's error tree, not this item).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation; each swappable, the agreement's isolation aside, which
    is AGENTS.md's rule for legal acts and OPEN_QUESTIONS #45's
    decision): a `bid_amount` of `0` is refused; country keys are
    checked for shape only (two capital letters), whether the code
    exists is Meta's check; the welcome sequences' form sends plain
    values (curl strips `-F 'name="…"'`'s quotes); the welcome list
    reads a bare array or `{"data": […]}`; `WelcomeSequences::update` is
    idempotent (it sets the values it names), while create, a duplicate,
    `set_max_price`, `clear_max_price` and the agreement are not; blank
    sequence ids are refused; the signer's email is not checked for
    shape and `MaxPriceAgreementSigner`'s `Debug` is not redacted
    (personal data, not a secret), but neither the name nor the email
    reaches an error (above); a duplicate may carry country
    multipliers; the names as landed; `Client::marketing_template`'s
    max price API stays.
- [x] **L14. Call recording and transcription** (`meta-whatsapp-client`,
  `client::calling`; row 135): the per-call `recording` and
  `transcription` objects on connect and accept (`calling/call-recording`,
  `calling/call-transcription`).
  - **Kind:** breaking (a new field on `ConnectCall`, whose fields are
    public; accept's options can come as an additive method).
  - **After:** nothing.
  - **Decisive:** exact JSON from those pages; a connect without the
    options sends neither key.
  - **Landed:** row 135 done for the library. `ConnectCall` gains
    `recording: Option<CallRecording>` and
    `transcription: Option<CallTranscription>` (with setters of the same
    names), and `Calling::accept_with` takes them in `AcceptOptions`
    (`Calling::accept` unchanged: it sends neither). Both objects are
    `status` (`ENABLED`/`DISABLED`, reusing
    `calling::settings::FeatureStatus`), `purpose` and
    `announcement_language`; off unless the caller sets them, on
    `connect` and `accept` only (the pages name no other action).
    Decisive tests (`calling::tests`): the pages' four request examples
    as exact JSON (`connect_with_recording_matches_docs_example`,
    `connect_with_transcription_matches_docs_example`,
    `accept_with_recording_matches_docs_example`,
    `accept_with_transcription_matches_docs_example`) and
    `connect_and_accept_without_options_send_neither_key`. Checked
    locally (`recording_limits_are_enforced`,
    `transcription_limits_are_enforced`): with `ENABLED`, `purpose` and
    `announcement_language` present and not blank; `purpose` at most 250
    characters (`calling::recording::PURPOSE_MAX_CHARS`); a given
    language not blank. Not checked locally: the list of announcement
    languages (Meta's to judge, and it changes). Neither action is
    replayed after a timeout (`accept_with_options_is_never_replayed`).
    The results already arrive as typed webhooks
    (`call_recording_available`, `call_transcription_available`); the
    downloaded transcript document is not typed. Decided with it
    (coordinator, owner's delegation; each swappable as noted):
    `ConnectCall`, `AcceptOptions`, `CallRecording` and
    `CallTranscription` carry no `#[non_exhaustive]`, as request types
    (OPEN_QUESTIONS #28); the language list is not enforced locally
    (Meta's, and it changes; a local check is one more rule in
    `calling::recording`); each object is validated on its own, even
    though Meta ignores the transcription's `purpose` and language when
    both are enabled (both pages require them; dropping the check is a
    one-line change); a whitespace-only `purpose` or language counts as
    empty; `status` reuses `calling::settings::FeatureStatus` (its own
    enum later is additive if Meta's values diverge); the accept side is
    a separate `AcceptOptions` struct for an additive `accept_with`,
    leaving `accept`'s signature as it was. The 250-character limit
    applies whatever the `status` (the pages' error table states it
    without one), and only `ENABLED` requires a `purpose`
    (`only_enabled_needs_a_purpose_and_length_counts_characters`, which
    also pins the count in Unicode scalar values).
- [x] **L15. The thread control API** (`meta-whatsapp-client`, a new
  client module; row 139): pass, take, release.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** exact JSON from `conversation-routing/thread-control`.
  - **Landed:** `meta_whatsapp_client::thread_control`, reached with
    `client.thread_control(phone_number_id)`: `ThreadControl::pass(&user,
    &Pass)`, `take(&user, metadata)` and `release(&user)`, each a `POST
    /{phone-number-id}/thread_control` with its `action`, answering
    `ThreadControlResponse` (`messaging_product`, `request_id`). `Pass`
    is untargeted (`Pass::to_escalation_partner`, the default: no
    `control_pass`) or names a `TargetRole` (`Pass::to_role`: the page's
    six values, `Other(String)` for a later one), with `metadata`. Row
    139 stays partial for the library: the Routing Roles API (L15a).
    Decisive tests (`thread_control::tests`): the page's four request
    examples and its response as exact JSON
    (`untargeted_pass_matches_docs_example`,
    `targeted_pass_matches_docs_example`,
    `release_matches_docs_example`, `take_matches_docs_example`), the two
    handover examples' `metadata` as sent by the pass and the take, every
    target role's wire name (`every_target_role_has_its_wire_name`) and a
    BSUID in `recipient` on all three
    (`a_bsuid_is_sent_as_recipient`). Checked locally, before any
    request: `metadata` at most 2,000 characters
    (`thread_control::METADATA_MAX_CHARS`, Unicode scalar values);
    exactly one non-empty identifier, so `Recipient::PhoneAndUser` and a
    group are refused (`exactly_one_user_identifier_is_required`); a
    non-empty target role; `control_pass` on `pass` only, by
    construction. None of the three is replayed after a timeout
    (`no_action_is_replayed_after_a_timeout`). Not checked locally: who
    owns the thread, the escalation role, whether a target is enabled
    (Meta's errors say which). The `control_passed` and `control_taken`
    events were already typed (`ThreadControlChanged`, with fixtures of
    this page's examples); the inbox does not call the API itself (its
    callers record their own calls: the `meta-whatsapp-rs-cms-inbox`
    example now makes the pass). With it, the page's error `131070` (a
    service message without owning the thread, idle threads included)
    maps to `ErrorKind::ThreadOwnedElsewhere`, the inbox's local
    refusal's kind, so the service answers it `409
    thread_owned_elsewhere` instead of `502 unknown`
    (`metas_ownership_refusal_has_the_local_refusals_kind`,
    `classification_table_spot_checks`, and the service's
    `each_kind_answers_its_code_and_no_meta_text`); `2494191` (a `take`
    by a caller that is not the escalation partner) stays `Unknown`, not
    `Permission`, which names the token's permissions and asset access
    while this is the business's routing role
    (`take_by_a_non_escalation_partner_keeps_its_code`).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation; each swappable as noted): the module and accessor names,
    `meta_whatsapp_client::thread_control` and `client.thread_control`;
    `release` not idempotent although Meta documents a release of an
    idle thread as a success (a replay can release a thread a new
    message routed back in between; a caller that wants a replay calls
    it again); `release` sends no `metadata` (Meta forwards it only on a
    handover event, and a release has none; a parameter is an additive
    change); `Recipient::PhoneAndUser` and groups refused locally,
    rather than reduced to the BSUID or the phone number (the caller
    picks its identifier; Meta refuses both); `#[non_exhaustive]` on
    `TargetRole` and `ThreadControlResponse` only, not on `Pass`, a
    request type (OPEN_QUESTIONS #28); and L15a kept as its own item,
    with row 139 partial until it lands.
- [x] **L15a. The Routing Roles API** (`meta-whatsapp-client`, next to
  `client::thread_control`; row 139): `GET
  /{phone-number-id}/routing_roles`, the roles the caller's business
  holds on a number (`roles`, `is_standby`), which say which thread
  control actions it may take (`conversation-routing/routing-roles`).
  Proposed by L15's implementation; decided with it (2026-11-01).
  - **Kind:** additive.
  - **After:** L15.
  - **Decisive:** exact response JSON from
    `conversation-routing/routing-roles` (both examples, the empty
    `roles` among them).
  - **Landed:** `ThreadControl::routing_roles()` on
    `client.thread_control(phone_number_id)`, a `GET
    /{phone-number-id}/routing_roles` with no query and no body,
    answering `RoutingRoles` (`roles: Vec<RoutingRole>`, `is_standby:
    bool`, and `RoutingRoles::holds`, which compares wire names).
    `RoutingRole` is the page's six values (`calls`, `ctwa`,
    `customer_service`, `escalation`, `marketing`, `utility`) with
    `Other(String)` keeping any other verbatim, `ai_agent` among them
    (the page: never returned to a partner). Row 139 is done for the
    library; the service's side stays M2d and M5l. Decisive tests
    (`thread_control::tests`): both of the page's responses as exact
    JSON, with the request's method, path, absent query and body, and
    token (`routing_roles_match_docs_example`,
    `empty_routing_roles_match_docs_example`: an empty `roles` is a
    success), every role's wire name (`every_routing_role_has_its_wire_name`),
    a response missing either field refused
    (`routing_roles_tolerate_new_fields_and_require_both`), the page's
    two errors, both code `100`, as `ErrorKind::InvalidParameter`
    (`routing_not_enabled_is_an_invalid_parameter`,
    `an_unreadable_number_is_an_invalid_parameter`: no kind of their
    own, which would be a core change and could not split one code), a
    retry after a timeout (`routing_roles_are_retried_after_a_timeout`)
    and the id kept one segment. Mutated by hand: the path segment, GET
    to POST, either field's name, a default on either field, `calls`'s
    wire name, refusing an empty `roles`, `holds` by equality, and the
    catch-all removed; each fails a test.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable as noted): a
    method on `ThreadControl` rather than a module and accessor of its
    own (`client.routing_roles(id)` would be an additive accessor);
    `RoutingRole` its own enum, not `TargetRole` (`calls` is no pass
    target, `ai_agent` never a held role; a conversion between the two
    is additive), with `#[non_exhaustive]` and the `Other(String)`
    catch-all like `TargetRole`; both fields required, with no serde
    default (the page says both are always present, and a default would
    read as "no role" or "not standby"; relaxing it is a one-line
    change); `roles` a `Vec` in Meta's order rather than a set (the
    page calls it unordered; `holds` is the set test); the read retried
    as every GET is; no local cache or rate limiting (Meta rate limits
    the endpoint per number and per app and asks for periodic reads:
    the caller schedules them); M5l, not this item, decides whether
    the service exposes the roles.
- [x] **L16a. The Messaging Account a send bills**
  (`meta-whatsapp-client`, `client::messages`; row 140). Meta's account
  model evolution (`account-model-evolution`) is generally available
  from H2 2026, not a beta: a WABA's id now names its Messaging Account
  (templates, billing, webhook subscriptions), and its numbers move
  under a new WhatsApp account (WAAC) whose id Meta exposes
  only in phase 2 (H1 2027). Existing ids and calls keep working. A
  number can belong to several Messaging Accounts (one per partner, or
  per billing currency); a send whose access token can send from more
  than one of them names the one billed with
  `messaging_account_id` (`account-model-evolution/messaging`): optional
  now, required in that case on phase 2's Graph API version and on
  every version from phase 3 (H1 2028). Meta's alias
  `paid_messaging_account_id` is deprecated (migrate by 2026-12-31).
  - **Kind:** breaking (a new field on `OutboundMessage`, whose fields
    are public; built with its constructors, nothing changes).
  - **After:** nothing.
  - **Decisive:** the page's request example, with the docs' sample
    ids, as exact JSON on `/{phone_number_id}/messages`; a message
    without the option sends neither key; an id that is not digits
    never reaches the transport.
  - **Landed:** `OutboundMessage::messaging_account_id`, a `WabaId`
    (the id the Messaging Account kept), checked as digits by
    `OutboundMessage::validate`; the alias is never sent, and a
    `MessageContent::Raw` type named like either key is refused. The
    `send_message` example sends it from `WA_MESSAGING_ACCOUNT_ID`.
    Listing the Messaging Accounts a business shares with a partner is
    the client WABAs call,
    `client::waba::Business::client_whatsapp_business_accounts`,
    unchanged.
- [ ] **L16b. WhatsApp accounts (WAAC) and Messaging Accounts
  as their own objects** (`meta-whatsapp-client`, `meta-whatsapp-webhooks`;
  row 140): phase 2's endpoints for managing WAACs and Messaging
  Accounts, the WAAC id returned next to a phone number id (Embedded
  Signup's result among them), its `whatsapp_account` webhook topic, and
  the WAAC ids that replace phone number ids in paths from phase 3;
  and the Messaging Account's own reads published today
  (`account-model-evolution/messaging`: its `primary_funding_id`, which
  says whether it can pay), with Meta's Template Migration API.
  Meta has not published phase 2's reference yet; the mirror's
  `reference/whatsapp-account-number/*` is read again when it is.
  - **Kind:** additive (a new id type, new calls, a new webhook field
    parsed into its own event, `Unknown` until then).
  - **After:** nothing; the reference must exist first (phase 2, H1
    2027).
  - **Decisive:** exact JSON from the published reference; a
    `whatsapp_account` webhook parses into its event, and one with an
    unknown shape into `Unknown { raw }`.
- [x] **L17. Catalog and product reads** (`meta-whatsapp-client`,
  `client::commerce`; row 103), against Meta's Catalog API, whose docs
  sit outside the WhatsApp docs (not mirrored: read them first).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** a paged product list follows its cursors to the end
    with no repeat.
  - **Landed:** row 103 done in the library; the service's side stays a
    gap (M5g). `Waba::product_catalogs` reads the catalog connected to a
    WABA (`GET /{WABA_ID}/product_catalogs`); `Client::catalog` gives a
    `Catalog` (`get`, `products`/`products_stream`,
    `product_sets`/`product_sets_stream`), `Client::product_set` a
    `ProductSet` (`get`, `products`/`products_stream`) and
    `Client::product` a `Product` (`get`). The lists take their cursors
    in `ListProducts`, `ListSetProducts` and `ListProductSets`, next to
    the edges' documented filters (a JSON `filter` rule,
    `return_only_approved_products`, `error_priority`, `error_type`,
    `bulk_pagination`, `summary`; for sets `ancestor_id`,
    `has_children`, `parent_id`, `retailer_id`); the streams follow
    `paging.cursors.after` until a page comes without `next`. Every id
    in a path is checked as digits before any request. The decisive
    test (`products_stream_follows_three_pages_to_the_end_with_no_repeat`)
    walks three pages, asserting each request's query and `after`, five
    distinct products and no fourth request after the page without
    `next`; reusing the first cursor, dropping `after` or following
    `cursors.after` past the last page each fail it. Read from Meta's
    Marketing API reference (`product-catalog`, its `products` and
    `product_sets` edges, `product-set` and its `products` edge) and
    the Graph API reference (`product-item`,
    `whatsapp-business-account/product_catalogs`, `results`) on
    2026-10-27. Not wrapped: every catalog write (creating, updating or
    deleting catalogs, products and sets, and connecting a catalog to a
    WABA); a product's `catalog_id`, `image_height`, `image_width`,
    `override_country` and `override_language` parameters, which the
    reference names without saying what they do. `ProductItemId` and
    `ProductSetId` are `meta_whatsapp_core::ids` newtypes, re-exported
    by `client::commerce` (numeric since L20c).
- [x] **L18. A typed Flow JSON builder** (`meta-whatsapp-client`,
  `client::flows`; row 63).
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** every Flow JSON example in `flows/guides/flowjson`
    round-trips through the builder byte for byte (as JSON values).
  - **Landed:** row 63 done in the library (the service has no side:
    callers send Flow JSON as JSON). `client::flows::flow_json` models
    the document (`FlowJson`, `Screen`, `Layout`, `DataField`), its
    components (`Component`, tagged by `type`, one struct per component
    of `flows/guides/components` and `flows/guides/media_upload`, plus
    `Form`) and actions (`Action`, tagged by `name`); `Dynamic<T>` holds
    a value or a reference. Unknown components and actions are kept
    whole (`Unknown { raw }`) and unknown properties in flattened
    `extra` maps. Decided (AGENTS.md § Decisions): a component or action
    whose known tag does not fit its struct (a property of another type,
    a required one missing, a `null`) is `Unknown { raw }` too, verbatim,
    instead of failing the document, because a new or changed Meta shape
    must never break parsing and the docs disagree with themselves
    (`RadioButtonsGroup`'s `init-value` is a string in one table and a
    list in the other; it is now `OneOrMany`, either); the document
    skeleton stays strict (`version`, `screens`, the screens' typed
    properties and `layout`, a layout's `type` string and `children`
    list), each with one documented type that Meta refuses others of
    (`INVALID_PROPERTY_TYPE`). Pinned by
    `a_known_component_or_action_that_does_not_fit_its_struct_is_kept_whole`
    and `the_document_skeleton_stays_strict`; the decisive test still
    refuses any `Unknown` in the documented examples, so the fallback
    cannot hide a modelling mistake there. Also decided as proposed:
    the local checks are strict, taking the reading that refuses less
    where the docs can be read two ways; screen ids are ASCII letters
    and underscores (the error codes' `PATTERN_MISMATCH`, and no
    documented example has a digit); `SUCCESS` is refused in any case
    (the guide spells it `SUCCESS`, the error codes `success`);
    `Component` and `Action` are `#[non_exhaustive]`; `label-variant`
    and `ChipsSelector`'s root actions are taken from the changelog;
    category 11 is done for the library. `FlowJson::builder` and `FlowJson::validate` check the
    hard limits of the component pages and of the static validation
    errors in `flows/reference/error-codes`, the first failure a
    `ValidationError` with the path from the root;
    `CreateFlow::flow_json_typed` and `Flow::upload_flow_json_typed`
    validate and send it, the raw-JSON calls unchanged. The decisive test
    is `every_flow_json_example_round_trips_through_the_typed_model`
    (`crates/meta-whatsapp-client/tests/flow_json.rs`): the 27 JSON
    examples of `flows/guides/flowjson` (3 whole Flows, 21 components, 2
    actions, a data model), and the 28 of `components`, `media_upload`,
    `flowsapi` and `flows-templates`, each parsed, serialized and
    compared as a JSON value, with no unknown component, action or
    property left (so a model that passed everything through `Unknown`
    fails it); four of them come out of the builder identical
    (`the_*_example_comes_out_of_the_builder`), and the error-code
    samples round-trip or are refused, never altered, and those that
    parse as a document fail local validation, all but one (a global
    reference into an array schema, Meta's to check), each outcome
    pinned per sample. The components the Markdown pages give no
    example of are pinned by
    `every_documented_property_of_every_component_is_modelled`: each
    property of each component's table, under its documented name,
    lands in a typed field (a misspelt wire name fails it). Dropping the
    catch-all, keeping only its tag, or
    turning any `extra` into `#[serde(skip)]` fails
    `unknown_components_actions_and_properties_are_kept_as_sent`; lifting
    a limit fails its unit test in `flows::flow_json::tests`. Read on
    2026-10-28. Not covered: the examples Meta renders in an interactive
    preview, which the Markdown pages do not carry (most component
    examples: the static and dynamic `data-source` ones, `If`, `Switch`,
    `NavigationList`, `CalendarPicker`, `Image`, the media pickers, the
    routing-model and form examples); checks that need the data model
    resolved (references, payload schemas, `__example__` types, form
    names), component types per Flow JSON version, and the limits the
    docs call soft (`NavigationList` content truncated or not rendered).
- [x] **L19. Media conversion** (`meta-whatsapp-client`: a
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
  - **Landed:** the port `meta_whatsapp_core::media::MediaConverter`,
    an object-safe async trait, `convert(MediaInput, TargetFormat) ->
    Result<ConvertedMedia>`, with `TargetFormat::OggOpus` (mono Ogg
    Opus, uploaded as `audio/ogg`: Meta's media page reads "OPUS codecs
    only; base audio/ogg not supported; mono input only", read on
    2026-11-01) and its limit, core's `media::AUDIO_MAX_BYTES` (16 MiB);
    `meta_whatsapp_client::media` re-exports them and uploads a result
    with `Media::upload_converted` (its format's type and extension,
    through `validate_upload`); and `meta_whatsapp_adapters::media::FfmpegConverter`
    behind the adapters' `ffmpeg` feature (forwarded by the facade's;
    off by default; in `full`). The converter starts the `ffmpeg` binary
    with `tokio::process`, never a shell, with fixed literal arguments:
    the input on stdin and the result on stdout, `-protocol_whitelist
    pipe` and a `-format_whitelist` of recording formats (WAV, MP3, Ogg,
    FLAC, AAC, MP4/M4A/3GP, AMR, Matroska/WebM) before `-i`, the first
    audio stream only, tags and chapters dropped, `-ac 1`, libopus at
    32 kb/s. The input (64 MiB) and the run (60 s, killed past it) are
    bounded, and the result to the 16 MiB upload limit (killed past it);
    stderr is drained and withheld; the output is checked to be mono Ogg
    Opus. Failures are a new node, `Error::Conversion(ConversionError)`
    (`BinaryMissing`, `EncoderMissing`, `Timeout`, `InputRejected`,
    `OutputInvalid`, `Killed`; architecture.md § Error tree), never
    `may_have_been_sent`, retryable only on `Timeout`; an empty or
    oversized input (or result) is a `ValidationError` on `input`.
    `FfmpegConverter::check` finds a missing binary or libopus at
    startup. Decisive: `ffmpeg_turns_wav_into_ogg_opus_that_upload_accepts`
    (`crates/meta-whatsapp-rs/tests/ffmpeg.rs`, in the facade, where the
    adapters and the client meet: a stereo 44.1 kHz WAV comes out as Ogg
    whose first packet is an `OpusHead` for one channel, ffprobe
    agreeing, and goes through `Media::upload_converted` as `audio/ogg`,
    its exact request asserted) and `a_missing_ffmpeg_is_a_typed_error`
    (`ConversionError::BinaryMissing`). The real-`ffmpeg` tests use
    `META_WHATSAPP_RS_TEST_FFMPEG`, else `ffmpeg` on `PATH`, and skip
    without one unless `META_WHATSAPP_RS_REQUIRE_FFMPEG=1`; CI installs
    `ffmpeg` on its runner and sets that variable
    (`.github/workflows/ci.yml`, docs/dev-environment.md), so they count
    there; no image or dev container carries one. They ran against
    ffmpeg 6.1.1 when this landed. Removing the protocol or format
    whitelist, mono, the timeout, the kill on timeout, an input or
    output bound or the output check, or withholding stderr, changing
    the codec, or a timeout that says `may_have_been_sent`, fails a test
    (the protocol whitelist only the fixed-argument one: with these
    formats, ffmpeg 6.1.1 opens nothing else from a pipe even without
    it, so it is a second wall, not the first). The security review
    added: the child's environment reduced to `PATH` (no `FFREPORT` log
    file), `-max_alloc 16777216` (a 16 KiB WAV declaring 1 Hz otherwise
    took 1.7 GB and a minute), `-abort_on empty_output` (an input
    without audio came back as an empty voice note), an MP4 whose index
    follows its media data moved index first in memory (from a pipe,
    ffmpeg read next to nothing of one longer than a few seconds, and
    the converter returned it as success), and real-`ffmpeg` tests for
    each, one of them an MP4 whose data reference names a host file,
    never read.
    Not done: images into stickers and resizing (L19b), a conversion
    sidecar over HTTP, and a limit on conversions at once (the caller's).
  - **Decided** (coordinator, under the owner's delegation, 2026-11-01;
    each swappable):
    - The trait lives in `meta-whatsapp-core`, beside the other ports,
      not in the client as planned: the adapters then depend on core
      alone, with no dependency cycle through the client's tests. Core
      carries the MIME type, the extension and the size limit of each
      format; the client re-exports the port from `client::media`, and a
      client test keeps core's limit equal to its upload table.
    - The upload MIME type is `audio/ogg`, the literal of Meta's table,
      not `audio/ogg; codecs=opus` (the upload check takes both).
    - Failures get a typed node, `Error::Conversion(ConversionError)`,
      with existing kinds (no new `ErrorKind`: the service's error codes
      and OpenAPI document are unchanged); a missing binary is
      `ConversionError::BinaryMissing`, not a `ConfigError`, so every
      converter failure is handled in one place; an empty or oversized
      input stays a `ValidationError` on `input`.
    - Defaults: 64 MiB of input, 60 s, 32 kb/s at 48 kHz in `voip` mode,
      the recording formats above (each a setting or a literal of the
      converter, swapped by another `MediaConverter`).
    - The facade's `ffmpeg` feature is in `full`; the gate variables are
      `META_WHATSAPP_RS_REQUIRE_FFMPEG`, `META_WHATSAPP_RS_TEST_FFMPEG`
      and `META_WHATSAPP_RS_TEST_FFPROBE`, apart from
      `META_WHATSAPP_RS_REQUIRE_LIVE` (no `live_` test name).
    - Row 48 stays partial, its rest planned in L19b.
    - CI proves the conversion: its runner installs `ffmpeg`, which no
      image or dev container does.
- [x] **L19b. Stickers and image resizing** (`meta-whatsapp-core`: new
  `TargetFormat`s; `meta-whatsapp-adapters`: their conversion behind the
  `ffmpeg` feature, or another converter; row 48): an image into a WebP
  sticker within Meta's limits (`image/webp`, 100 KB static, 500 KB
  animated: `messages/sticker-messages`, which gives no dimensions, so
  none is assumed), and a JPEG or PNG resized or
  re-encoded under the 5 MB image limit, through the same port and the
  same safety rules as L19 (fixed arguments, input formats restricted,
  bounded, diagnostics withheld).
  - **Kind:** additive.
  - **After:** L19.
  - **Decisive:** a PNG comes out as a static WebP (its `RIFF`/`WEBP`
    header read apart from the converter) within 100 KB that
    `Media::upload`'s type check accepts as a sticker, and a JPEG over
    5 MB comes out under it, both against a real
    converter in CI; a GIF or a crafted file off the format list is a
    `ConversionError::InputRejected`.
  - **Landed:** three `TargetFormat`s in core, `StaticSticker` (a
    static WebP, `image/webp`, `.webp`, at most
    `media::STATIC_STICKER_MAX_BYTES`, 100 000 bytes), `Jpeg` and `Png`
    (8-bit, `image/jpeg` and `image/png`, `.jpeg` and `.png`, at most
    `media::IMAGE_MAX_BYTES`, 5 000 000 bytes), read from
    `messages/sticker-messages` and `business-phone-numbers/media` on
    2026-11-01; the client's test keeps each within its upload table.
    `FfmpegConverter` makes them from a PNG or a JPEG only: the input's
    first bytes (a PNG's signature, a JPEG's start of image) are checked
    before ffmpeg starts, and ffmpeg's own detection may land on nothing
    but `png_pipe` or `jpeg_pipe` (`-format_whitelist`). Each attempt
    runs fixed literal arguments: the L19 prefix (`-protocol_whitelist
    pipe`, `-abort_on empty_output`, environment reduced to `PATH`,
    stderr drained and withheld), `-max_alloc 268435456`, `-max_pixels
    33554432` (32 Mi pixels), the first video frame, no metadata, a
    filter and an encoder from a fixed table, `-f image2pipe` on stdout.
    Each format is brought under its limit by a ladder of sizes and
    qualities (sticker: its own size at quality 80, then 1024 at 80 and
    50, 512 at 75 and 50, 256 at 50; JPEG: its own size at `-q:v` 2, 5,
    10, then 2048 at 5, 1024 at 10; PNG: its own size, 2048, 1024), the
    aspect ratio kept and never enlarged, each attempt killed once its
    output passes the limit, the whole ladder within `with_timeout`; an
    image no attempt brings under is a `ValidationError` on `input`. The
    result's header is checked (a WebP whose RIFF size is its length and
    whose first chunk is `VP8 `, `VP8L`, or `VP8X` without the animation
    flag; a JPEG of 8 bits and 3 components; a PNG of 8-bit RGB or RGBA
    ending in `IEND`). `FfmpegConverter::check_for(&[TargetFormat])`
    checks each format's encoder; `check()` is it for `OggOpus`.
    Decisive (`crates/meta-whatsapp-rs/tests/ffmpeg.rs`, against ffmpeg
    6.1.1, Ubuntu 24.04's package, the one CI's runner installs, built
    with libwebp, a hard dependency of its `libavcodec60`):
    `ffmpeg_turns_a_png_into_a_static_webp_sticker_that_upload_accepts`
    (a 1600 × 1200 picture with noise, a 4 MB PNG whose own-size WebP is
    nearly 700 KB, comes out as a 1024 × 768 static WebP within 100 000
    bytes, its RIFF header, chunks and size read in the test, ffprobe
    agreeing, `validate_upload` taking it as a sticker, and its upload's
    exact request), `ffmpeg_brings_a_jpeg_over_five_megabytes_under_the_image_limit`
    (a 10 MB, 4000 × 3000 JPEG, which `validate_upload` refuses, comes out
    as an 8-bit colour JPEG under 5 000 000 bytes, 4:3 kept), and
    `ffmpeg_refuses_a_gif_a_jpeg_ls_an_animated_png_and_a_crafted_png`
    (each `InputRejected` for every image format; the JPEG-LS image
    passes the first-bytes check, and without the whitelist ffmpeg reads
    it from a pipe and converts it: the control). Also: a small
    transparent PNG keeps its size and alpha; a 16-bit PNG over 5 MB
    comes out 8-bit RGBA under it; a 6000 × 6000 PNG is refused by the
    pixel bound (ffmpeg without it converts it); a JPEG whose EXIF says
    Orientation 6 comes out rotated (640 × 480 to 480 × 640), its EXIF
    gone. The inputs are made by the tests (`lavfi` sources, an EXIF
    segment written in Rust); nothing is committed. Removing the
    first-bytes check, the whitelist (or widening it to `jpegls_pipe`),
    `-max_pixels`, the ladder (an overflow ending it), the aspect ratio,
    the 8-bit PNG format, the WebP check or its RIFF-size or animation
    test, the ladder-wide timeout, or setting the sticker limit to the
    animated one fails a test (the first-bytes check only in the
    adapters' tests: for a GIF, the whitelist is a second wall). Not
    done: animated stickers (L19c; a GIF or an animated PNG is refused;
    ffmpeg 6.1.1 cannot read an APNG from a pipe at all), WebP or HEIC
    as an input, and a photo of more than 32 Mi pixels (a 48-megapixel
    one), which has to be scaled down before it comes here.
  - **Decided 2026-11-01** (coordinator, under the owner's delegation,
    AGENTS.md § Decisions; each swappable by another `MediaConverter`,
    or a literal of this one):
    1. Meta's "100 KB" and "5 MB" read as decimal (100 000 and
       5 000 000) for what a converter makes, under the upload check's
       binary reading: a converted file then fits whichever Meta means.
       `AUDIO_MAX_BYTES` keeps its binary reading (16 MiB), as L19
       decided: a voice note is far under either.
    2. Inputs: PNG and JPEG only, the formats of Meta's image table; no
       GIF, WebP, HEIC or animation.
    3. Static stickers only: no `AnimatedSticker` target here (L19c).
    4. No sticker dimensions imposed (Meta gives none): a sticker that
       fits keeps its size; the ladder's sizes (1024, 512, 256) are where
       it steps down, not a requirement.
    5. Bounds: 32 Mi pixels, 256 MiB a single allocation, the existing
       64 MiB of input and 60 s, the timeout covering the whole ladder.
    6. The ladders as built; a result over the limit after every attempt
       is a `ValidationError` on `input`, as L19's result over the limit
       is.
    7. Row 48 stays partial on the library's side: L19b's text names
       Meta's 500 KB animated limit and the row's capability is "stickers
       from images", which Zaileys makes animated too; animated stickers
       are L19c. The service's side stays M5c1's.
    8. An input's ICC profile passes into a JPEG or PNG result, so its
       colours stay right; its size is bounded by the output limit.
       Stripping it is one filter in the fixed table. **Decided
       2026-11-01** (coordinator): iCCP kept only up to 16 KiB
       compressed, after a 660 MB measurement (`image::PNG_ICCP_MAX`,
       one literal; the first `iCCP` chunk only). **Decided
       2026-11-01** (coordinator): a JPEG's profile is kept likewise,
       for parity with the PNG side: its first `APP2` `ICC_PROFILE`
       segment, when it is a whole profile in one chunk (1 of 1) of at
       most 16 KiB (`image::JPEG_ICC_MAX`, the same value); every other
       `APP2` is dropped, so a profile split over several chunks is
       dropped whole and that image loses its colour profile. JPEG and
       PNG each keep one profile of at most 16 KiB. One chunk costs
       ffmpeg nothing measurable (51.6 MB at 16 KiB, 51.9 MB at 64 KiB,
       against 51.5 MB without); the largest profile in chunks (255 of
       64 KiB, a 16.7 MB file) took 198 MB, bounded but some nine times
       its size, and is dropped.
    9. The allocation bound stays 256 MiB, its edge documented (a 16-bit
       RGBA frame of 8192 × 4096 is refused by ffmpeg's padding, 8192 ×
       4094 converts); headroom is one literal.
  - **Review** (adversarial, 2026-11-01, against ffmpeg 6.1.1): a PNG's
    `zTXt` chunks were a memory bomb inside every bound (ffmpeg inflates
    each and keeps the text until the frame is decoded: four 100 KB
    chunks inflating to 100 MiB each took 1.6 GB for a 16 × 16 image).
    The converter now walks a PNG's chunks before ffmpeg starts, refuses
    one cut short of `IEND`, and passes it on without `tEXt`, `zTXt`,
    `iTXt` and `eXIf` and without anything after `IEND`
    (`a_png_reaches_ffmpeg_without_its_text_and_nothing_after_its_end`,
    `ffmpeg_gets_the_png_without_its_text_and_the_limit_is_inclusive`);
    the PNG output check wants one image ending the file. Also found and
    tested: the allocation bound refuses a 16-bit RGBA PNG of exactly
    32 Mi pixels (ffmpeg's padding; the docs said it fits;
    `ffmpeg_refuses_a_frame_over_the_allocation_bound`, its first
    behavioural test on images), a result of exactly the limit is taken,
    and only the first of two images in a row comes out. Three guards
    survived their mutation and now have a test: the kill of an attempt
    over the limit (`kill_on_drop` and SIGPIPE hid its removal;
    `an_attempt_over_the_limit_is_killed_and_reaped`, a stand-in deaf to
    SIGPIPE, each attempt reaped before the conversion returns), a failed
    attempt ending the ladder (`a_failed_attempt_ends_the_ladder`) and the
    encoder's kind in `check_for`
    (`check_for_wants_the_encoder_s_kind_too`). `-map_metadata -1`
    survives in behaviour (the `image2pipe` muxer and these encoders
    write no tags) and is pinned by the arguments' literal test. Probed and
    holding: a JPEG-LS image with an `APP0` segment first, or a `SOF0`
    copy before its own frame header, still never reaches `jpeg_pipe`
    (ffmpeg's probe lands on `mjpeg` or nothing, both off the list); an
    animated PNG reaches only its default image at worst; the ladder's
    last attempts stay far under their limits on noise with noisy
    transparency (42 774 bytes for a sticker, 3.8 MB for a PNG); killed
    attempts are reaped (`Child::kill` waits). Left as they are,
    documented: the colour profile (ICC) passes into a JPEG or PNG
    result; a truncated JPEG decodes partly, grey below the cut; the
    first sticker attempt of a 24-megapixel photo takes about 18 s on a
    4-core machine, encoding a WebP far over the limit, so a timeout
    under the default can fail a large photo; an image over 256:1 can
    fail at a smaller attempt (a side scaled to nothing).
  - **Fix: EXIF and colour profile bombs** (2026-11-01, found by L19c's
    implementer, against ffmpeg 6.1.1). The JPEG path passed EXIF
    through for its orientation, and ffmpeg copies an EXIF value once
    for every IFD entry pointing at it, over every `APP1`: a 67 KB JPEG
    of one crafted segment took 916 MB, 264 KB of four 3.6 GB, 658 KB of
    ten 8.4 GB; a PNG's 200 KB `iCCP` inflating to 200 MiB took 660 MB.
    The converter now walks a JPEG's segments before ffmpeg starts
    (`image::jpeg_prepared`) and passes on only what ffmpeg reads by the
    segment's length, since ffmpeg moves on by the bytes it read, not by
    the length (a scan header it skips, a JFIF thumbnail, which it does
    not skip, would otherwise hide a segment from the walk): `DQT`,
    `DHT`, `SOF0`–`SOF3`, `SOF55`, `DRI` and `SOS`, each refused unless
    its length is exactly its content (a scan header with no `FF`), and
    the scans' entropy-coded data; the first JFIF and Adobe segments at
    their fixed fields; the first colour profile segment when it is one
    chunk of at most 16 KiB (decision 8 above); in place of the first
    EXIF segment a minimal
    EXIF of 36 bytes (`image::minimal_exif`: big-endian TIFF, one IFD,
    the Orientation alone, 1 to 8; no EXIF without a valid one); the end
    of image. Everything else is dropped (every other `APP1` and `APP2`,
    `APP13`, comments, other `APPn`, markers ffmpeg does not decode,
    bytes between segments, anything after the end of image), and a
    JPEG cut short or with a segment past its end is `InputRejected`,
    which replaces the review's "a truncated JPEG decodes partly". A
    PNG keeps its first `iCCP` only up to 16 KiB (decision 8 above).
    Decisive: `ffmpeg_applies_the_orientation_of_a_jpeg_whose_exif_is_a_bomb`
    (the 67 KB file as it is peaks at about 980 MB, the control; with a
    second segment, through the converter, about 50 MB, under a 400 MB
    bound, and Orientation 6 still applied) and
    `ffmpeg_keeps_a_small_colour_profile_and_drops_a_large_one` (650 MB
    against 48 MB; a 3 KB profile still reaches a PNG result), both
    reading `VmHWM` from `/proc`; the bytes ffmpeg is given, exactly, in
    `a_jpeg_reaches_ffmpeg_with_only_what_ffmpeg_reads_whole`,
    `a_jpeg_s_exif_is_its_orientation_alone` (each orientation, both
    byte orders), `a_jpeg_cut_short_or_with_a_wrong_length_is_refused`,
    `a_png_keeps_one_colour_profile_of_at_most_16_kib`,
    `a_jpeg_keeps_one_colour_profile_of_at_most_16_kib` and the stand-in
    `ffmpeg_gets_the_jpeg_without_its_metadata`; a JPEG's small profile
    reaching a JPEG and a PNG result, one in two chunks reaching neither,
    in `ffmpeg_keeps_a_jpeg_s_small_colour_profile_in_one_chunk`. Removing the EXIF
    rewrite, keeping a second `APP1`, dropping the length check, raising
    the `iCCP` cap or keeping a second profile, raising or removing the
    JPEG profile's cap, keeping a profile in chunks or a second one, or
    never keeping one, dropping the scan
    header's `FF` check or a table's exact length, keeping the JFIF
    thumbnail, the tail after the end of image or an unknown marker, or
    the orientation's range check, fails a test.
- [x] **L19c. Animated stickers** (`meta-whatsapp-core`: an
  `AnimatedSticker` `TargetFormat`; `meta-whatsapp-adapters`: its
  conversion behind the `ffmpeg` feature, or another converter; row 48):
  an animation (an animated GIF, PNG or WebP) into an animated WebP
  sticker within Meta's 500 KB (`messages/sticker-messages`), through
  the same port and the same safety rules as L19 and L19b (fixed
  arguments, input formats restricted, bounded in pixels, frames and
  time, diagnostics withheld). The input formats it adds are a decision
  of its own (AGENTS.md § Decisions).
  - **Kind:** additive.
  - **After:** L19b.
  - **Decisive:** an animated GIF comes out as a WebP whose `VP8X`
    header has the animation flag and whose `ANIM` and `ANMF` chunks are
    read apart from the converter, within 500 KB, which `Media::upload`'s
    type check accepts as a sticker, against a real converter in CI; an
    animation over the frame bound is refused, and removing that bound
    fails a test.
  - **Landed:** a sixth `TargetFormat` in core, `AnimatedSticker` (an
    animated WebP, `image/webp`, `.webp`, at most
    `media::ANIMATED_STICKER_MAX_BYTES`, 500 000 bytes), read from
    `messages/sticker-messages` and `business-phone-numbers/media` on
    2026-11-01, which give "500 KB" and no dimensions, frame rate or
    length; the client's test keeps it within its upload table.
    `FfmpegConverter` makes it from a GIF only: the input's first bytes
    (`GIF87a`, `GIF89a`) are checked before ffmpeg starts, and ffmpeg's
    own detection may land on nothing but `gif` (`-format_whitelist
    gif`; from a pipe, ffmpeg 6.1.1 detects a GIF as `gif`, never
    `gif_pipe`). The GIF is walked block by block first, and refused
    (`InputRejected`, before ffmpeg starts) when cut short of its
    trailer, with a block GIF does not define or a graphic control
    extension not of four bytes, or out of bounds: a canvas of no pixels
    or over 2 Mi (2 097 152), fewer than 2 frames or more than 500, more
    than 100 frames of 512 × 512 or larger (each frame counted for its
    canvas's pixels up to 512 × 512, 25 Mi in all), or longer than 10 s
    (each frame counted for its delay, or 10 hundredths without one or
    under 2); ffmpeg gets it with its header, colour tables, images and
    the last graphic control extension before each image only, without
    comments, plain text and application extensions, and without
    anything after its trailer. Each attempt runs fixed literal
    arguments: the L19 prefix (`-protocol_whitelist pipe`, `-abort_on
    empty_output`, environment reduced to `PATH`, stderr drained and
    withheld), `-max_alloc 268435456`, `-max_pixels 2097152`, the first
    video stream, no metadata, `-fps_mode vfr`, `-frames:v 500` (a
    second wall behind the walk's frame bound), a filter and
    `libwebp_anim` from a fixed ladder (512 at the GIF's own frame rate
    and quality 75; then 512 at 15 frames a second and 50, 384 at 12 and
    40, 256 at 10 and 40, 192 at 8 and 30, 128 at 5 and 25; compression
    level 4), `-f image2pipe` on stdout; the aspect ratio and the
    transparency kept, never enlarged, the whole ladder within
    `with_timeout`. A still result (libwebp's answer to frames all
    alike) is `InputRejected`; the result is checked (a RIFF size that
    is its length, chunks ending with the file, `VP8X` first with the
    animation flag, `ANIM` before the `ANMF` frames, at least two
    `ANMF`, no `VP8 `, `VP8L` or `ALPH` outside them).
    `check_for(&[TargetFormat::AnimatedSticker])` wants `libwebp_anim`.
    Decisive (`crates/meta-whatsapp-rs/tests/ffmpeg.rs`, against ffmpeg
    6.1.1, Ubuntu 24.04's package, CI's):
    `ffmpeg_turns_a_gif_into_an_animated_webp_sticker_that_upload_accepts`
    (a 480 × 360 GIF of 100 frames at 20 a second, too large at the
    first attempt, comes out as an animated WebP of 494 836 bytes in
    about 3 s, its RIFF size, `VP8X` animation flag, `ANIM` before its
    `ANMF` frames, loop count (0, forever), 5 s of play and 4:3 read in
    the test apart from the converter, `validate_upload` taking it as a
    sticker, and its upload's exact request) and
    `ffmpeg_refuses_an_animation_over_the_frame_bound` (501 frames are
    `InputRejected`, 500 convert; ffmpeg alone converts the 501: the
    control). Also: the time bound (11 one-second frames refused, 10
    converted, 10 s of play), the frame-pixel bound (101 frames of
    512 × 512 refused, ffmpeg alone converting them), the pixel bound (2048 × 1026 refused,
    ffmpeg alone converting it; 2048 × 1024 comes out 512 × 256), a GIF
    with 2 MB of comments, XMP, an ICC profile and plain text converted
    with none of them in the sticker, and an animated PNG, an animated
    WebP, a PNG, a JPEG, a one-frame GIF and a GIF of identical frames
    refused. The inputs are made by the tests (`lavfi` sources, GIF
    blocks written in Rust); nothing is committed. Removing the walk's
    frame bound, its pixel bound, its time bound (or counting a delay
    under 2 hundredths as given), the two-frame minimum, the extension
    stripping, the cut after the trailer, the first-bytes check, the
    still-result refusal, the output check or any of its parts (the
    animation flag, `VP8X` first, `ANIM`, `ANMF`, `ANIM` before `ANMF`,
    the RIFF size, no image outside the frames), `-frames:v` or
    `-max_pixels`, widening the format whitelist, checking for
    `libwebp` instead of `libwebp_anim`, a 16 MiB allocation bound, or
    setting the limit to 512 000 or the static one fails a test, 24
    mutations in all. The extension stripping, the trailer cut, the
    delay rule and the output check's parts fail only the adapters'
    tests (libwebp writes none of a GIF's metadata, and a real ffmpeg
    makes no malformed WebP); `-frames:v`, `-max_pixels`, the whitelist
    and the allocation bound only the arguments' literal test: the
    walk and the first-bytes check stand in front of them, so they are
    second walls.
    Timings (4 cores, ffmpeg 6.1.1, the whole ladder): the decisive GIF
    3.4 s, two attempts; a 10 s, 512 × 512 GIF of 250 busy frames 15 s,
    four; 500 frames of a fine moving pattern 17 s, three; 30 s of noise
    at 15 frames a second (a 51 MB GIF) 34 s, five, fitting at 192
    pixels in 455 180 bytes (the last attempt made 121 844). These were
    made under the first bounds (500 frames, 30 s); the review below
    found inputs within them that ran past the timeout.
    Measured against ffmpeg 6.1.1 (peak resident memory, 4 cores): a
    GIF's extensions are not compressed, so none inflates, but ffmpeg
    holds them with the first frame: 60 MB of comments, XMP, ICC, plain
    text or loop-count data around a 16 × 16 GIF took 162 MB against
    51 MB without them, the same in one extension or 600 (the walk takes
    them out); 60 MB of padding inside one image's data took 162 MB with
    a 64 MiB allocation bound. Frames cost time, not memory: 5 001
    frames of one changed pixel on a 1920 × 1080 canvas (a 2.5 MB GIF)
    took 41 s for one attempt at 66 MB, 200 001 (7 MB) ran past 60 s.
    The worst within the bounds: 500 frames of a fine moving pattern (a
    0.9 MB GIF) made a 25 MB WebP at the first attempt, in 12.5 s and
    130 MB (a 75 MB GIF of a pattern that compresses badly, over the
    input bound, took 26 s and 257 MB). A WebP input never reaches
    ffmpeg (its first bytes): an `EXIF` chunk of 300 entries naming the
    same 1 MB took ffmpeg 2.7 GB. Not done: animated PNG or WebP input
    (ffmpeg 6.1.1 reads neither from a pipe: the APNG reader seeks, the
    WebP decoder skips `ANIM` and `ANMF`), a GIF's own loop count (the
    sticker loops forever), and the service's side (M5c1).
  - **Decided 2026-11-01** (coordinator, under the owner's delegation,
    AGENTS.md § Decisions; each swappable by another `MediaConverter`,
    or a literal of this one):
    1. Meta's "500 KB" means 500 000 bytes (decimal, as L19b reads its
       figures): under the upload check's 500 KiB, a converted file fits
       whichever Meta means.
    2. Inputs: GIF only. An animated PNG and an animated WebP are
       refused by their first bytes, since ffmpeg 6.1 reads neither from
       a pipe; an ffmpeg that does would make each one list entry and
       one first-bytes check, with a walk of its metadata first
       (ffmpeg's EXIF reader copies a value once per entry naming it).
    3. Bounds: a canvas of 2 Mi pixels (1920 × 1080 fits), 500 frames,
       256 MiB a single allocation, the existing 64 MiB of input and
       60 s for the whole ladder, as decided; tightened by the review on
       its measurements (below), as the decision allowed: 10 s rather
       than 30 (WhatsApp's own rule for animated stickers, its sticker
       app guide: "less than or equal to 10 seconds total"), and a bound
       on frames times pixels, 25 Mi (100 frames of 512 × 512 or
       larger, each frame counted for its canvas's pixels up to
       512 × 512). Meta's Cloud API pages give no frame or length limit;
       these are cost bounds.
    4. The ladder as built, starting at 512 pixels rather than the
       GIF's own size (Meta gives no dimensions; an attempt costs its
       frames times its pixels), the frame rate kept on the first
       attempt only; a GIF no attempt brings under is a
       `ValidationError` on `input`, as L19b's.
    5. Frames all alike (a still result) are `InputRejected`, not a
       static sticker passed off as an animated one.
    6. The sticker always loops forever (libwebp's default); the GIF's
       loop count is dropped with its other application extensions.
    7. Row 48 is done on the library's side: voice, stickers static and
       animated, and resizing are all made; the service's side stays
       M5c1's.
  - **Review** (adversarial, 2026-11-01, against ffmpeg 6.1.1 on 4
    cores; the inputs made by a script, nothing committed): the bounds
    as first built let a legal GIF run past the 60 s timeout, a
    `ConversionError::Timeout`, the one retryable error, for an input
    that fails the same way every time. Measured with the ladder's own
    arguments: 500 frames of 512 × 512 four-colour noise over 30 s (a
    40 MB GIF) took 66 s, six attempts, the first 26 s and 241 MB; the
    same with noisy transparency over 10 s, 59 s and 258 MB at the
    first; 250 frames of 1024 × 1024 noise with transparency over
    10 s, 77 s; 450 frames of 2048 × 1024 noise, 52 s. The cost is
    frames times encoded pixels (libwebp at level 4: up to some 160 ms
    a frame of 512 × 512), and every attempt decodes every frame
    again. Hence the time bound of 10 s and the frame-pixel bound of
    25 Mi (decision 3): within them the worst measured took 44 s (100
    frames of 1024 × 1024 noise with transparency at 10 a second: six
    attempts, none within the limit, a `ValidationError`, 136 MB at
    the first), 39 s at 15 a second, 36 s for 2048 × 1024, 22 s for
    512 × 512 noise of 256 colours, 5 s for 500 frames of 128 × 128; at
    64 frames of 512 × 512 the worst would be 24 s, but the decisive
    GIF (100 frames of 480 × 360) would be refused. Also found: a GIF's
    graphic control extensions were all passed on, so 64 MB of them in
    a row made ffmpeg hold a 169 MB packet (the same as the comments
    the walk takes out); the walk now keeps only the last one before
    each image, the one ffmpeg and browsers use
    (`a_gif_reaches_ffmpeg_without_its_metadata_and_nothing_after_its_trailer`).
    The output check took one `ANMF` frame as an animation, a still
    image labelled animated; it wants two (libwebp itself makes a still
    image of a single frame). Four guards survived their mutation and
    now have a test: either half of the graphic control extension's
    size check (four bytes in its first sub-block, eight in all), and a
    stray `VP8L` or `ALPH` chunk outside the frames. Each new guard
    fails a test when mutated: the frame-pixel bound removed, off by
    one, without its 512 × 512 cap, at the cap alone or doubled; the
    time bound back at 30 s; the last control extension not kept,
    kept for the next image too, or every one kept; one `ANMF` taken
    (`a_gif_over_the_frame_pixel_bound_is_refused`, the facade's
    `ffmpeg_refuses_an_animation_over_the_frame_pixel_bound` and
    `ffmpeg_refuses_an_animation_over_the_time_bound` against ffmpeg). Equivalent, and
    left: the colour table's own end check and the sub-blocks' bound
    (the next read past the end refuses the GIF anyway), and
    `gathered`'s uncopied whole (a slice of all of it is the same
    bytes). Probed and holding: a frame declared larger than the
    canvas (65 535 × 65 535 on 16 × 16) or of no size is clipped to the
    canvas by ffmpeg's decoder, so `-max_pixels` on the canvas bounds
    each frame (500 such frames: 0.2 s, 51 MB); disposal 3 (restore
    previous) keeps one copy of the canvas, not one a frame (100 frames
    of 2048 × 1024: 80 MB); a frame without its own graphic control
    extension is merged by ffmpeg's GIF parser into the packet before
    it, so the walk counts at least the frames and the time ffmpeg
    does; an LZW code size outside 2 to 8 is passed on (ffmpeg refuses
    one under 1 or over 11, and its index line is a byte, so no colour
    table is read past its end); a NETSCAPE loop count never reaches
    ffmpeg; the delay sum (at most 500 delays of 65 535) cannot
    overflow, nor the canvas product (65 535² fits a `u32`); and only a
    `gif` demuxer may open the input, so nothing after the GIF's first
    bytes can be sniffed as another format. The PNG walk's shared
    helpers (`keep`, `gathered`) leave its behaviour as it was: its
    tests pass and merging ranges is caught by them. Left as they are:
    an image's LZW data may carry padding after its end code, which
    ffmpeg holds with the frame (64 MB took 162 MB; the input bound
    caps it); the frame rate rungs repeat the frames of a slower GIF,
    which libwebp merges but still compares (50 repeats of 1024 × 1024
    noise cost 2 s); and the worst measured ladder (44 s) is within the
    default timeout on these 4 cores, so a slower host needs a longer
    one.
- [x] **L20a. One catch-all shape for open enums** (`meta-whatsapp-core`
  and every crate with an open enum; OPEN_QUESTIONS #27): `Other(String)`,
  generated by one macro in core.
  - **Kind:** breaking, before the first release.
  - **After:** nothing.
  - **Decisive:** an unknown value round-trips unchanged through
    deserialize and serialize for every open enum; a unit `Unknown` left
    anywhere fails it.
  - **Landed:** `meta_whatsapp_core::open_enum!` generates all 197 open
    enums (133 in `meta-whatsapp-client`, 63 in `meta-whatsapp-webhooks`,
    core's `WindowEventKind`): the webhooks' `open_enum!`, the client's
    `string_enum!` and both `wire_enum!` copies are gone, and so are the
    13 unit `#[serde(other)] Unknown` enums and the hand-written
    `Other(String)` / `Unknown(String)` ones. Options cover what the
    pages need: `#![case_insensitive]` (the former `string_enum!`s),
    `#![normalize = fn]` (`RejectionReason`,
    `PaymentConfigurationStatus`) and `#![string_only]` (Flow JSON's
    enums, so a wrongly typed value still leaves its component raw).
    Every other enum keeps a non-string JSON value as its text in
    `Other`, as the webhooks' did. A documented `OTHER` is
    `FlowCategory::OtherCategory` and `Vertical::OtherVertical`. The
    decisive tests are in `crates/meta-whatsapp-rs/tests/open_enums.rs`:
    `every_open_enum_keeps_an_unlisted_value_verbatim` (three unlisted
    values through `Deserialize`, `Serialize`, `From`, `FromStr` and
    `Display` for every enum, the list compared with the macro's enums
    in the source, and `#![string_only]` exactly on Flow JSON's) and
    `no_enum_has_a_catch_all_of_another_shape` (no `#[serde(other)]`,
    unit `Unknown` or hand-written string catch-all, `String`,
    `Box<str>`, `Cow<str>` or a one-field struct variant, in
    `crates/*/src` or `tools/*/src`, outside `NOT_OPEN`, each entry
    with its reason: `ErrorKind`, the bot's job states, the request-only
    `WabaCurrency` and `GatewayProvider`). Left as they are, not string enums: the
    structured catch-alls (`Unknown { raw }` of webhook messages and
    fields and of Flow JSON, `WebhookEvent::Unparsed`, the templates'
    `Other(Value)`, `EmbeddedSignupEvent::Unknown`) and
    `MessageProductType::Other(u16)`.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable):
    1. a JSON value that is not a string is kept as its JSON text in
       `Other` by default, so a number where the prose says string never
       fails a response or a webhook; Flow JSON's enums are
       `#![string_only]`, as a document this crate writes and reads back
       must keep a wrongly typed component raw. Swappable per enum: the
       option is one line of its `open_enum!`;
    2. the documented `OTHER` values are `FlowCategory::OtherCategory`
       and `Vertical::OtherVertical`, since `Other` is the catch-all; the
       wire value stays `"OTHER"`. Swappable: a variant's name is its
       line of the macro;
    3. `#![normalize = fn]`, a rewrite for the comparison only (spaces
       and underscores, in `RejectionReason` and
       `PaymentConfigurationStatus`), never applied to the value kept in
       `Other`. Swappable per enum, by dropping or replacing the
       function;
    4. `WabaCurrency` and `GatewayProvider` stay hand-written, in the
       test's `NOT_OPEN` with the reason: a request value whose parse
       refuses an unlisted code, and a request union whose variants carry
       each gateway's fields. Swappable: converting one is moving it into
       the macro and out of `NOT_OPEN`;
    5. the macro puts `#[non_exhaustive]` on every enum it generates, so
       naming a value Meta adds is not a breaking change; this settles
       L20b's rule for open enums ahead of it (below). Swappable: one
       attribute in the macro.
- [ ] **L20b. The `#[non_exhaustive]` rule** (`meta-whatsapp-client`,
  `meta-whatsapp-webhooks`, docs/architecture.md; OPEN_QUESTIONS #28): on
  every response struct, webhook payload struct and open enum, never on
  request or builder types; written into architecture.md.
  - **Kind:** breaking, before the first release.
  - **After:** L20a.
  - **Decisive:** a source scan over the crates fails on a response or
    webhook struct, or an open enum, without the attribute; removing it
    from one type fails the scan.
  - **Overlap with L20a:** the open enums already carry it, every one
    from `open_enum!`, which adds it (decided 2026-11-01, L20a's
    Landed note); L20b's scan only has to keep it there, and its work is
    the structs.
- [x] **L20c. Ids checked against their documented shape**
  (`meta-whatsapp-core` ids and `client::GraphRequest`; OPEN_QUESTIONS
  #20): digits for numeric ids; whole-segment percent-encoding kept for
  opaque ones (BSUIDs, group ids).
  - **Kind:** breaking (an id of the wrong shape is refused).
  - **After:** nothing.
  - **Decisive:** a numeric id holding `/` is refused before any
    request; a group id holding `/` stays one percent-encoded segment.
  - **Landed:** `AppId`, `BusinessId`, `WabaId`, `PhoneNumberId`,
    `MediaId`, `TemplateId`, and L17's `ProductItemId` and
    `ProductSetId` (the ids Meta documents as numeric strings, the last
    two in the Catalog API's field tables) refuse anything but 1 to 64 ASCII digits when built
    (`new` returns a `Result`, `FromStr` and `TryFrom` replace the
    infallible conversions; `ids::is_numeric_id`, `MAX_NUMERIC_ID_LEN`,
    64 being the service's existing cap), a `ValidationError` on the
    type's `FIELD`; every other id stays opaque. `Deserialize` does not
    check, so parsing never fails on an id's shape, and `new_unchecked`
    wraps Meta's plain-string values (webhook entry ids, `debug_token`
    targets) and stored rows the same way; such an id is still one
    encoded segment. The decisive tests, in `client/src/request.rs`
    (`id_shape_tests`), are
    `a_numeric_id_holding_a_slash_is_refused_before_any_request` (each
    of the six types built from `123/subscribed_apps` and, only if that
    succeeds, used in a call its path names, every response scripted:
    nothing is sent) and
    `a_group_id_holding_a_slash_stays_one_encoded_segment` (both group
    accessors: `DELETE /v25.0/Y2FwaV9ncm91cDox%2Fparticipants`), with
    `a_numeric_id_meta_sent_is_kept_and_stays_one_segment`, and in core
    `ids::tests::a_numeric_id_is_one_to_sixty_four_ascii_digits` and
    `a_numeric_id_from_meta_is_deserialized_as_sent`; the first checks
    all eight types, and is the decisive one for the two product ids,
    whose paths `commerce::catalog` also checks as digits at request
    time (`ids_that_are_not_digits_are_refused_before_any_request`,
    with ids built unchecked). Making the check always pass sends all
    six requests (the test lists their paths);
    splitting a segment on `/` in `GraphEndpoint::url_segments` fails
    the group test and the Meta-sent one. The endpoint modules keep
    their request-time checks (empty, `.`, `..`; digits where a page
    asks), now tested with ids built unchecked. Stored data is
    untouched: the vault's and the ledger's pinned records, written
    under the WABA id `W1`, still open. The service's `graph_id` runs
    `is_numeric_id` plus its own no-leading-`0` rule (D4); a
    non-numeric `{pn}` or `{waba_id}` is `404 not_found` as before
    (`tests/authz.rs`, `a_path_id_that_is_not_numeric_is_not_found`, on
    every such route), a body's media `id` reaches Meta as given
    (`tests/messages.rs`, `a_media_id_in_a_body_reaches_meta_as_given`),
    and `openapi/v1.json` is unchanged. Each place an id Meta sent is
    wrapped unchecked has a test that an id of another shape is kept as
    sent (webhooks' `ids_meta_sends_are_kept_as_sent_whatever_their_shape`,
    and the client's `debug_token` targets, template insights, archive
    answers and voicemail media id; the service core's untyped-change
    number): checking any of them fails its test. OPEN_QUESTIONS #20 is closed and
    deleted (CHANGELOG); what stays unverified for opaque ids (whether
    Graph decodes `%2F`) is in architecture.md's endpoint rule 1. No
    parity row changes: none cited L20c.
  - **Follow-up:** the ids left opaque although Meta's examples show
    them as digits (`FlowId`, `CatalogId`, `TemplateGroupId`,
    `CreditLineId`, `AllocationConfigId`, `VerificationSubmissionId`,
    `WabaBotId`, among others): no page states their shape, so the rule
    OPEN_QUESTIONS #20 decided (the shape Meta documents) keeps them
    opaque (a template group id is still checked as digits when its
    request is built, by `client::templates`). Each moves to
    `numeric_id_type!` once a reference page states digits for it (a
    breaking change to its constructor, as here), with that page's
    example as its test.
- [x] **L20d. A check for a blank verify token** (`meta-whatsapp-webhooks`,
  `WebhookHandlerBuilder`; OPEN_QUESTIONS #16).
  - **Kind:** additive (the builder stays infallible).
  - **After:** nothing.
  - **Decisive:** the check reports a handler built with a blank verify
    token and passes one built with a real token.
  - **Landed:** `WebhookHandler::check(&self) -> Result<(), ConfigError>`,
    run by the caller right after `build` (nothing else runs it). Blank
    is what the handshake already refused: empty, or only whitespace
    (`str::trim`'s Unicode set), one predicate shared with
    `verify_subscription`, whose constant-time comparison is unchanged.
    The error's text is fixed and names no part of the token. The
    decisive tests are `check_reports_a_blank_verify_token` and
    `check_passes_a_real_verify_token` (`tests/handler.rs`), with
    `the_check_error_carries_no_token_text`; making `check` always pass
    fails the first and the last, and the doctest on `check`. The
    `cms_inbox` example's `app` runs it
    (`the_app_refuses_a_blank_verify_token_at_boot`), as do the
    webhook-endpoint and bot skills' examples
    (`blank_secrets_fail_at_startup`,
    `a_blank_verify_token_stops_the_boot`); the service does not, as its
    configuration already refuses a blank `WA_VERIFY_TOKEN` before a
    handler exists, with its own test of the same shape (`str::trim`,
    then empty).
    OPEN_QUESTIONS #16 is closed and deleted (CHANGELOG). No parity row
    changes: coverage row 2 already counted the verify token.
- [x] **L20e. Product-card carousel templates: two cards at creation**
  (`meta-whatsapp-client`, `client::templates`; OPEN_QUESTIONS #22).
  - **Kind:** breaking (creating one with 3–10 cards is refused).
  - **After:** nothing.
  - **Decisive:** a product-card carousel with three cards is refused
    before any request; a media-card carousel with three is created; a
    send with 10 cards is accepted.
  - **Landed:** `TemplateDefinition::validate` (run by
    `Templates::create` before any request) refuses a carousel whose
    cards carry a `PRODUCT` header unless it has exactly two cards, on
    `components[<i>].cards`, from one constant beside the media cards'
    2–10 range (`templates/validate.rs`); the page's words are "Define
    only two product cards when you create the template" and "An
    approved template with two product cards can be used to send up to
    10 cards" (`catalogs/product-card-carousel-template-messages`). The
    page states the count for creation only, so `TemplateEdit::validate`
    keeps 2–10 for both kinds. The decisive tests, in
    `client/src/templates/tests.rs`, are
    `a_product_card_carousel_is_created_with_exactly_two_cards` (0, 1,
    3, 10 and 11 cards refused with nothing sent; 2 created, its exact
    body asserted), `a_media_card_carousel_with_three_cards_is_created`
    (the media-card page's own three-card example) and
    `a_product_card_carousel_send_takes_up_to_ten_cards` (10 sent, 11
    refused with nothing sent), with
    `a_product_card_carousel_edit_keeps_two_to_ten_cards` and
    `a_product_header_on_any_card_makes_a_product_card_carousel` (a
    `PRODUCT` header on any card and in any case counts; mixed,
    headerless and unknown-format carousels are refused by the card
    checks). Disabling the check fails the first; applying it to edits
    fails the edit test; applying it to every carousel fails the
    media-card tests; detecting the header on the first card only, or
    on every card, fails the header test. The service runs the same
    check (`a_product_card_carousel_is_created_with_two_cards`,
    `meta-whatsapp-server/tests/templates.rs`: Meta's example created,
    1, 3 and 10 cards `422` on `components[1].cards`). No in-repo
    construction needed changing: the only product-card carousels
    (tests) had two cards. OPEN_QUESTIONS #22 is closed and deleted
    (CHANGELOG). No parity row changes: rows 51 and 99 already counted
    product-card carousels.
- [x] **L21a. Dead-lettering a permanently failing event**
  (`meta-whatsapp-webhooks`, `meta-whatsapp-adapters`; OPEN_QUESTIONS
  #30): sink errors classified transient or permanent (unclassified
  stays transient, today's behaviour); a permanent one written to a
  dead-letter typed store on `KvStore` (bounded by count and age,
  alerted, replayable) before the delivery is acknowledged. `KvStore`
  has no scan, so L5's `ConversationStore::erase_all` cannot reach it:
  the store keeps its own per-contact index and an `erase`, and a
  facade-level eraser calls it beside the conversation store's.
  Companion: architecture.md § Stable identifiers for the dead-letter
  namespace.
  - **Kind:** additive.
  - **After:** L5.
  - **Decisive:** a batch with one permanently failing event delivers
    the others and dead-letters that one (removing the dead-letter write
    makes the handler answer 500 again, and the test fails); the
    facade-level erasure of the contact removes their dead-lettered
    events (removing the dead-letter store's `erase` from it fails the
    test).
  - **Landed:** the signal is a sink's own: `SinkError::Permanent`
    (`SinkError::permanent`, `is_permanent`; every other variant stays
    transient), added to core's existing leaf, with `FanoutSink`
    returning a transient error before a permanent one. The store is
    `meta_whatsapp_webhooks::deadletter::DeadLetters` on `KvStore`,
    wired by `WebhookHandlerBuilder::dead_letters`: a permanent error is
    written there, the event's dedup claim marked done, the rest of the
    batch delivered and the delivery acknowledged, counted in
    `DeliveryReport::dead_lettered` and alerted by a `tracing::error!`
    naming the letter, kind and number only; a failed write releases the
    claim and answers `500`, and without a store a permanent error
    answers `500` as before. Bounds: a ring of 1000 slots (letter `id` in
    slot `id % capacity`, the oldest overwritten without a scan), 14 days
    per record and index, 256 KiB per stored event (a larger one kept
    without its content). Replay (`replay`, `replay_all`) goes past the
    dedup guard. Erasure: a per-contact index (every identifier the
    event gives, BSUIDs, parent BSUIDs and phone numbers, since an
    erasure may know the person by one only; keyed by business number
    and contact, an event with no number under the empty one, which every
    number's erasure reads) and `DeadLetters::erase`, called by the new
    facade `meta_whatsapp_rs::erasure::Eraser` beside
    `ConversationStore::erase_all`. The namespaces `wa.webhook.deadletter`
    and `wa.webhook.deadletter.contact` and the records are pinned
    (`the_keys_are_pinned_and_hashed`, `the_record_bytes_are_pinned`) and
    pending the owner's confirmation (§ Owner touchpoints). The decisive
    tests are
    `a_batch_with_one_permanent_failure_delivers_the_others_and_dead_letters_it`
    (`webhooks/tests/deadletter.rs`; with
    `post_is_200_when_a_permanent_failure_is_dead_lettered` in
    `tests/router.rs` for the status) and
    `erasing_a_contact_removes_their_dead_letters`
    (`meta-whatsapp-rs/src/erasure.rs`). Removing the dead-letter write
    fails the first two, removing `erase` from the eraser the third;
    treating a permanent error as transient fails the first, dropping
    the count bound fails `the_count_bound_evicts_the_oldest`, and not
    updating the index fails the erasure tests. Not done: no sink of the
    library classifies an error as permanent yet (the inbox's U+0000 in
    a Meta-assigned id on Postgres still answers `500`); the service
    does not wire a store (its sink classifies nothing). No parity row
    changes status: row 8 already counted delivery semantics as done.
    The sabotage review indexed every identifier (the draft took the
    BSUID alone when an object had one and missed `phone_number`,
    `input`, `participant_recipient_id` and parent BSUIDs:
    `every_person_a_fixture_event_names_is_indexed` walks every fixture),
    pinned the write-before-index order and the erasure/append race
    (`an_erasure_that_reads_an_index_entry_finds_its_letter`,
    `an_erasure_racing_an_append_leaves_it_to_the_next_erasure`), the
    head's compare-and-swap (`racing_appends_take_distinct_ids_and_keep_every_letter`)
    and the take-back of an unindexed letter
    (`a_letter_that_cannot_be_indexed_is_taken_back_and_fails_the_delivery`).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable as noted): the
    core change, `SinkError::Permanent` with `SinkError::permanent` and
    `is_permanent` (additive, the enum `#[non_exhaustive]`; a sink
    classifies its own errors); a new `meta_whatsapp_rs::erasure::Eraser`
    rather than a change to `Inbox::erase_all`, whose rustdoc says it does
    not reach the dead letters, and the guides and the cms-inbox skill
    point to it; the defaults of 1000 letters, 14 days and 256 KiB, an
    oversized event kept without its content (`with_capacity`,
    `with_max_age`, `with_max_event_bytes`); a dead-lettered event's claim
    marked done, and replay past the dedup guard (at least once); an event
    naming no business number erased by any number's erasure of the
    contact (over-deleting diagnostic copies is the safe direction for an
    erasure); `FanoutSink` returning a transient error before a permanent
    one; and the known limits, documented in the `deadletter` module
    (an erasure racing an append leaves that letter to the next erasure;
    a replay racing an erasure hands the event back to the sink; more than
    `capacity` appends at once may evict a newer letter; a capacity change
    strands letters from `get`, `list` and `replay` until they age out;
    a letter naming nobody is reached by the bounds only). The namespaces
    `wa.webhook.deadletter` and `wa.webhook.deadletter.contact`, their
    keys' derivations and their records' shapes stay pending the owner's
    confirmation before the first release (§ Owner touchpoints).
- [x] **L21b. Live events shared, not cloned** (`meta-whatsapp-webhooks`
  `sse`, `meta-whatsapp-adapters` `BroadcastSink`; OPEN_QUESTIONS #31):
  both over `Arc<WebhookEvent>`.
  - **Kind:** breaking (their types change).
  - **After:** nothing.
  - **Decisive:** ten subscribers share one allocation per event
    (`Arc::ptr_eq` across them); reverting to a clone per subscriber
    fails it.
  - **Landed:** `BroadcastSink<E>` wraps each delivered event in one
    `Arc` and publishes `Arc<E>` (`from_sender`, `receiver` and
    `BroadcastSubscription` carry `Arc<E>`; `E` need not be `Clone`,
    but must be `Sync`); `sse` reads a
    `broadcast::Receiver<Arc<WebhookEvent>>`, its filter still a
    `Fn(&WebhookEvent) -> bool`. The `EventSink` port is unchanged:
    the sink still takes an owned `E` and does the wrapping. The
    decisive tests are
    `ten_subscribers_share_one_allocation_per_event`
    (`adapters::sink::broadcast`: ten subscriptions and ten raw
    receivers) and `ten_sse_subscribers_share_one_allocation_per_event`
    (`webhooks::server`, on the stream `sse` is built on), with
    `sse_filters_see_the_shared_event` checking that the public `sse`
    hands each of ten filters the sent allocation. A subscription that
    clones the event per subscriber fails the first; a stream that
    clones it fails the second, and the third too when the copy comes
    before the filter. `sse_body_is_byte_exact` (`tests/router.rs`)
    pins the wire output, the same bytes as before L21b. Not pinned: a
    copy inside `sse`'s encoding step, after the filter, which only an
    allocation counter would see (`unsafe_code` is forbidden, so a
    counting allocator would need a new dev-dependency).
    `WebhookEvent` is asserted `Send + Sync` at compile time.
    OPEN_QUESTIONS #31 is closed and deleted (CHANGELOG). No parity row
    changes: parity row 10 already counted `webhooks::sse` and
    `BroadcastSink` as done.
- [x] **L21c. `rediss://`** (`meta-whatsapp-adapters`, feature `redis`;
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
  - **Landed:** redis-rs 1.7 cannot take a provider (its rustls path
    always calls `ClientConfig::builder()`) but takes any stream, so
    `store::RedisTls` does the TLS itself: a `ClientConfig` on
    `aws_lc_rs::default_provider()` (the platform's roots, or the CAs of
    `root_certificates_pem`; `client_auth_pem` for mutual TLS;
    `from_client_config` for your own), the handshake with tokio-rustls,
    the stream to `MultiplexedConnection::new_with_config`, and a
    `RedisTlsConnection` for `RedisKvStore::new` that reconnects on its
    own; none of redis-rs's TLS features is enabled. The test binaries
    link `ring` too (an adapters dev-dependency, rustls with both
    providers). The decisive test is
    `live_rediss_store_connects_with_both_providers_and_no_default`
    (`tests/redis_tls.rs`: the key/value conformance suite over TLS,
    asserting before and after that no process default is installed),
    against compose.test.yaml's `redis-tls` (TLS only, client
    certificates required, test-only certificates generated by
    `tools/test-tls/generate.sh`). Building on the process default
    (`ClientConfig::builder()`) panics it and every other test of that
    binary, the non-live
    `builds_on_aws_lc_rs_with_both_providers_linked_and_no_default`
    included; the library installing a default fails them on the
    no-default assertion; taking the process default when one is
    installed (reqwest's rule) fails
    `uses_aws_lc_rs_when_the_process_default_is_ring`
    (`tests/redis_tls_ring_default.rs`, which installs `ring`). The
    sentinel `tests/redis_tls_process_default.rs` (`#[should_panic]`)
    proves those binaries are the hostile case: without `ring` linked,
    `builder()` would install aws-lc-rs quietly and it fails. Also live:
    an untrusted server and a missing client certificate are refused, and
    a killed connection is replaced on the next command. Not covered:
    Cluster and Sentinel over TLS.
  - **Review (2026-11-01):** the live tests share one Redis whose
    certificate names every host they use, so a verifier that ignored
    the name passed them all. `tests/redis_tls_handshake.rs` runs
    `RedisTls` against an in-process TLS server (certificates made per
    test by rcgen, a dev-dependency): a trusted certificate for another
    name is refused, an IP host is checked against IP SANs only
    (`an_ip_host_is_checked_against_ip_sans_only`), an untrusted issuer
    is refused, the connection timeout bounds a stalled handshake and a
    stalled setup, a reconnect sends `AUTH` and `SELECT` again and
    replays nothing, and eight commands that find the connection gone
    share one failed attempt
    (`concurrent_commands_share_one_failed_reconnect_attempt`). PEM
    errors no longer quote the offending line (a client chain can be a
    combined file holding the key). Left untested: the generation check
    that keeps a late error from an old connection from discarding its
    replacement (no deterministic way to deliver that error late).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation; each swappable as noted): TLS through a stream handed to
    `MultiplexedConnection::new_with_config`, rather than leaving
    `rediss://` to integrator-built connections (an integrator's own
    connection still goes to `RedisKvStore::new`); the platform's roots
    by default (`root_certificates_pem` replaces them,
    `from_client_config` takes any configuration); TLS under the
    existing `redis` feature, not a feature of its own; `ring` allowed in
    the workspace's test builds through the adapters' rustls
    dev-dependency only, never in a library build (`cargo tree -e normal
    -p meta-whatsapp-adapters --features redis -i ring` finds nothing);
    the test Redis requires client certificates; its certificates are
    generated, none committed (OPEN_QUESTIONS #19).
- [x] **L22a. `llms.txt` and a `doctor`** (docs; the service's CLI in
  `meta-whatsapp-server`; row 117): `llms.txt` for the docs, and a
  `doctor` command checking the configuration, the webhook subscription
  fields and the token's reach.
  - **Kind:** additive.
  - **After:** nothing.
  - **Decisive:** `doctor` reports each scripted misconfiguration by
    name and exits non-zero; `llms.txt` links only files that exist (a
    test reads it).
  - **Landed:** row 117 done in the service; the library's side stays
    partial for the MCP server (L22b). `docs/public/llms.txt`, served at
    the docs site's root (VitePress copies `docs/public/`), indexes the
    guides, design, planning docs and every consumer skill, each link
    the file's Markdown on `main`; `crates/meta-whatsapp-rs/tests/llms_txt.rs`
    refuses a link to a missing file or off the repository, and an
    unlisted guide or skill. `meta-whatsapp-server doctor`
    (`meta_whatsapp_server::doctor`) prints one line per check (`OK`,
    `WARN`, `FAIL`, `SKIP`) and exits non-zero on a `FAIL`: every
    refusal of the configuration by variable (a refused variable is
    replaced and the reading repeated, so one does not hide the next),
    the backend opened as `serve` opens it, the migration histories read
    by `SELECT` (pending service migrations fail with
    `WA_SERVER_MIGRATE=skip`, warn with `auto`), the app's
    `whatsapp_business_account` subscription and its fields
    (`GET /{app-id}/subscriptions`: `messages` and `account_update`
    required, eleven more recommended), and each bound WABA's token
    (`debug_token`: valid, this app's, not expired, both permissions, a
    grant naming the WABA) and the app's subscription to it
    (`GET /{waba-id}/subscribed_apps`), plus the Solution Partner's
    system token. Read-only (no migration, no re-encryption on read,
    `GET`s only); no secret printed. `tests/doctor.rs` scripts each
    misconfiguration (36 tests, four live) and runs the binary for its
    exit code; its test backend panics on any key/value write and on
    the ports doctor has no use for (migrator, locks, janitor, outbox),
    so a vault re-encrypting on read fails it, and the live tests show
    `WA_SERVER_MIGRATE=auto` warns without migrating and that a changed
    checksum, a half-applied migration (the service's or the library's),
    a newer and a pending version are each reported.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each a constant or a format in
    one place, swapped by changing it): `llms.txt` at the docs site's
    root (`docs/public/`), its links the raw Markdown on GitHub's
    `main`; a fourth status, `SKIP`, for a check that could not run;
    every refusal found by replacing the refused variable and reading
    again, the limit documented (a replacement can hide a problem only
    the intended value would show); `messages` and `account_update`
    required (`REQUIRED_FIELDS`), the other fields only warning
    (`RECOMMENDED_FIELDS`); pending service migrations warning with
    `WA_SERVER_MIGRATE=auto` and failing with `skip`; at most 100 WABAs
    a run (`MAX_WABAS`) and a warning 7 days before a token expires
    (`EXPIRY_WARNING`); no permission check on the Solution Partner's
    system token; the `GET /{app-id}/subscriptions` read stays in the
    service until the client has it.
  - **Left:** a JSON report (`doctor --json`) for deploy jobs; the
    library's migration versions compared, not only counted (their list
    is private to the adapters crate: it needs a public accessor there);
    the subscriptions read moved into the client.
- [x] **L22b. A docs MCP server** (a tool under `tools/`, outside the
  library; row 117): search and read the docs and skills over MCP's
  stdio transport.
  - **Kind:** additive.
  - **After:** L22a.
  - **Decisive:** a scripted MCP client lists the tools, searches
    `Idempotency-Key` and gets the server guide's section; a missing
    page is an MCP error, not a panic.
  - **Landed:** row 117 done in the library too. `tools/docs-mcp`
    (package and binary `meta-whatsapp-docs-mcp`, run by `just
    docs-mcp`) serves three read-only tools over stdio: `list` (every
    page, with its title and a skill's description), `search` (the
    sections holding every term of a query, best first, each with its
    heading, line and snippet) and `read` (a page, or one section by its
    heading or anchor, to the next heading of its level). It speaks MCP
    revision 2026-07-28, the current one on 2026-11-01 (no handshake:
    `_meta` on every request, `server/discover`, `resultType`, the
    caching hints, `-32022` for a version it does not speak) and, after
    an `initialize`, 2025-11-25 and 2025-06-18, the revisions clients
    shipped with until then (dual-era, as 2026-07-28's versioning page
    defines it). A missing page, a missing section, a path outside the
    corpus and bad arguments are tool execution errors (a result with
    `isError: true`, as the tools page sorts them: the model can correct
    them); an unknown tool or malformed params are JSON-RPC `-32602`.
    `tools/docs-mcp/tests/stdio.rs` runs the binary as a subprocess:
    a legacy client and a modern one each list the tools, search
    `Idempotency-Key`, read `docs/guides/server.md`'s `Idempotency keys`
    section and get a missing page as `isError: true` with the server
    still answering; twelve escapes (`..`, absolute paths, repository
    files outside the corpus, `.meta-docs/`, `\`) and symbolic links out
    of a corpus tree are refused, and the corpus covers every Markdown
    page `llms.txt` links. Each guard was removed in turn and a test
    failed: the path syntax check, the listing membership, the
    resolved-path check, the walk's refusal of links and of hidden
    directories, the missing page's `isError`, the read and search
    bounds.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each a constant or a list in one
    file of the tool, swapped by changing it): Rust, in the root workspace as a non-default member
    (its tests, clippy, rustdoc and `cargo deny` run in `just ci`'s
    `--workspace` recipes), a binary only so no crate can depend on it,
    with no MCP SDK and no dependency but `serde_json` (no new crate in
    `Cargo.lock`); dual-era rather than 2026-07-28 alone; the corpus
    (`corpus::ROOT_FILES` and `corpus::TREES` in the tool): the root's
    README, AGENTS, CONTRIBUTING, CHANGELOG and OPEN_QUESTIONS, `docs/`'s
    Markdown and text, `skills/`'s Markdown and example sources, never
    `.meta-docs/`, hidden directories, `node_modules/`, generated
    `*.d.ts` or a symbolic link, read from disk on every call (no
    state, no stale index); the bounds: 40 KiB a read (a longer one ends
    with its `from_line` to continue and the page's headings), 10 hits
    a search by default and 25 at most, 240-character snippets, files
    over 1 MiB and messages over 1 MiB refused; search ASCII
    case-insensitive, every term required, ranked by occurrences with a
    bonus for the heading; no `.mcp.json` committed (registering the
    server stays each developer's choice: docs/dev-environment.md shows
    how).
  - **Left:** MCP resources and prompts (the pages as `resources/read`
    too); a search that folds Unicode case or ranks by more than counts;
    `subscriptions/listen` for a changed page (the tool list never
    changes).
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
  - **Partly landed** (left open, row 153 partial): row 101 done in the
    library; row 153's create, get, update and delete. Meta's pages as
    served on 2026-10-26 document both: `templates/template-archival` now
    shows `POST /{WABA_ID}/message_templates/archive` and `…/unarchive`,
    and `templates/template-groups` is in Meta's page list.
    `Templates::archive` and `unarchive` send `{"hsm_ids": [..]}` (1 to
    100 ids, each digits) and return `ArchiveResult` / `UnarchiveResult`
    (the ids done, a reason for each that failed);
    `Templates::create_group`, `get_group`, `update_group` and
    `delete_group` take `TemplateGroupCreate` / `TemplateGroupUpdate`
    (template ids written as JSON integers, as the page's examples do) and
    return `TemplateGroup` with its WABA and the first page of its
    templates. A group id that is not digits is a `Validation` error
    before any request (`group_ids_are_checked_as_digits_before_any_request`
    fails with the check removed); none of the POSTs is replayed on a
    timeout. Archive and unarchive go to the Graph endpoint
    (`graph.facebook.com/v25.0`), but the only page that shows them
    prints `https://api.facebook.com/{WABA_ID}/…` with no version, and
    no page shows them on Graph. A probe on 2026-10-26 with a template
    id that does not exist got the same answer from both hosts, where an
    edge that does not exist gets Graph's `2500` "Unknown path
    components": Graph routes them, and no new credential rule is
    needed. A real archive has not been run live.
    **Left:** the list of a WABA's groups
    (`GET /{WABA_ID}/template_groups`) and the pages after the first of a
    group's templates: Meta's changelog names the list, but no page
    documents its parameters or response (its Graph API reference page
    shows no operation), nor how to request a group's next page of
    templates.
- [x] **L24. App deep links on template URL buttons**
  (`meta-whatsapp-client`, `client::templates::Button`; row 155): the
  Android deep link, its fallback URL and the Meta app id of
  `marketing-messages/deep-links`, which the service refuses today
  (coverage row 33).
  - **Kind:** breaking (a new field on `Button::Url`).
  - **After:** nothing.
  - **Decisive:** the page's request example round-trips byte for byte
    (as JSON values); dropping the field makes the service's
    lost-key check refuse it again.
  - **Landed:** row 155 done on both sides. `Button::Url` gains
    `app_deep_link: Option<AppDeepLink>` (`meta_app_id` a `u64`, a JSON
    number on the wire; the two strings checked non-empty), and
    `Button::url_with_app_deep_link` builds one. The page's example
    round-trips (enum values in the module's upper case, the deep link
    object as it stands); with the field's serialization skipped the
    service answers `422` on `components[2].buttons[0].app_deep_link`
    again and the round-trip, create and service tests fail. Not
    checked locally: the marketing category, which the page uses but
    does not state as a rule, and any length or URI syntax (none
    documented).
- [ ] **L25. The launch option shape, checked in Meta's Integration
  Helper** (`client::embedded_signup`, OPEN_QUESTIONS #12): a
  verification before production relies on pre-fill; if the helper
  shows another shape, the serializer follows it.
  - **Kind:** additive if the shape holds; breaking if it changes.
  - **After:** nothing.
  - **Decisive:** the pre-fill test's expected JSON is the helper's
    output, recorded with its date; a serializer that departs from it
    fails the test.
- [x] **L26. Number settings left** (`meta-whatsapp-client`,
  `client::phone_numbers`; row 156, category 20, coverage row 10): what L9 left
  of the number's settings. The payload-encryption settings of
  `/{PHONE_NUMBER_ID}/settings` (`payload_encryption`: `status` and
  `client_encryption_key` to set; `client_encryption_key_fingerprint`
  and `cloud_encryption_key` read back,
  `reference/whatsapp-business-phone-number/settings-api`), and the
  number's own `POST` fields `connection_status` and `webhook_url`
  (`whatsapp-business-account-phone-number-api`). The mirror documents
  them by schema only, without an example: the bodies follow the
  schemas, and the module docs name each gap. The encryption key goes
  through the security review (`wa-security-reviewer`).
  - **Kind:** additive, but for one field: `PhoneNumberSettings`'s
    `payload_encryption`, raw JSON until now, is typed.
  - **After:** L9.
  - **Decisive:** exact JSON per field from the schemas; dropping any one
    field from its body fails its test; reading the settings parses a
    `payload_encryption` answer carrying every field of the response
    schema.
  - **Landed:** row 156 done in the library; the service's side stays
    a gap (M5c3). `PhoneNumber::enable_payload_encryption` sends
    `status` and `client_encryption_key` (a `ClientEncryptionKey`,
    checked to be base64 and not an obvious private key, `Debug`
    showing its length only), `disable_payload_encryption` the status
    alone, and `settings()` reads `PayloadEncryption` back (`status`,
    optional so that an answer without it still parses;
    `client_encryption_key_fingerprint`; and `cloud_encryption_key`, a
    `CloudEncryptionKey` that `Debug` shows as a length), with decode
    errors that withhold the body. `set_connection_status`
    (`ConnectionStatus`, the schema's eleven values) and
    `set_webhook_url` (an absolute `https` URI, no user info, no
    fragment, sent exactly as given) post on the number, one field per
    call. The four setters are not replayed after a timeout, only after
    what Meta provably refused (`Error::may_resend`). Skipping any
    one field, request or response, fails its test. None of these has
    an example on Meta's pages: the bodies and the read answer follow
    the schemas, and the module docs name what they leave out (the
    key's format, the fingerprint's algorithm, what the cloud key is,
    which connection statuses a business may set and to what effect,
    how `webhook_url` relates to the override).
    **Left:** `StorageConfiguration::status` (from L9) is still required,
    so a `storage_configuration` answer without a status fails the whole
    `settings()` read, as `payload_encryption` did before this item
    made its status optional (a status Meta adds parses: L20a made
    `StorageStatus` an open enum, the value kept in `Other`);
    and `CallbackOverride` does not check its URI's scheme, where
    `set_webhook_url` requires `https`.

## 4. Service milestones

Re-scoped on the modular architecture (design §8, §9). Each item's
routes are `meta-whatsapp-server-core` services exposed through both API
adapters (`meta-whatsapp-server-api-axum`, and
`meta-whatsapp-server-api-cratestack` from S13 on, § Routes; streaming routes
through `meta-whatsapp-server-http`), and run against every backend the
conformance suite covers. The acceptance tests M2.1–M5.3 are the
design's (§9).

### M2. Inbox, live updates, webhooks out

- [x] **M2a. Inbox routes** (rows 91, 111, 119): conversation list,
  history, a reply checked against the window, all filtered by the
  number's binding epoch; retention per store (D10): the inbox store
  built `with_retention` from a setting, and `apply_retention` run by
  the service's housekeeping. The reply goes through `Inbox::reply`, so
  its ownership refusal (`ErrorKind::ThreadOwnedElsewhere`, L7) answers
  `409 thread_owned_elsewhere` (already in the error table, §5.2), and
  the service exposes `ReplyChecks` as a per-number setting (e.g. the
  number of a tenant that is the account's escalation partner turns the
  ownership check off; one that receives standby copies turns the
  handover trust off).
  - **After:** S2b, S7, L5, L7.
  - **Decisive:** M2.1; removing the binding-epoch filter shows a moved
    number's history to its new tenant, and a test fails; with a
    retention set, housekeeping purges an inbox message older than it
    (removing the `apply_retention` call fails the test).
  - **Also, after the first release** (from S7, which landed before
    it): drop the legacy fingerprint (S6's window:
    `Fingerprint::legacy_v1_json`, `legacy_v1_upload`, `or_legacy`, and
    the handlers' `Uri`), once every deployment has run a release with
    S6 for one `WA_SERVER_IDEMPOTENCY_TTL`. If M2a lands before the first
    release, the next item of the service's to land after it carries
    this line.
  - **Landed:** three routes in `meta-whatsapp-server-api-axum`
    (`api::inbox`, tag and scope `inbox`), each a core service
    (`meta_whatsapp_server_core::inbox::Conversations`, opened on an
    `OwnedNumber` its `Authorizer` admits, so key, tenant, scope,
    ownership, then the vault, as every number route): `GET
    /v1/numbers/{pn}/conversations` (`list_conversations`, read class)
    and `GET /v1/numbers/{pn}/conversations/{contact}/messages`
    (`list_conversation_messages`, read class), `?limit=&cursor=` and
    `next_cursor` as every list but the events (the store's exclusive
    `(time, id)` cursor, wrapped opaquely), and `POST
    /v1/numbers/{pn}/conversations/{contact}/messages`
    (`reply_to_conversation`, send class, `Idempotency-Key` over
    `Fingerprint::reply_to_conversation`: the number, the contact decoded,
    the canonical body; no legacy fingerprint), whose body is
    `ReplyMessage` (a `SendMessage` without `to`) and whose answer is `202
    MessageAccepted`. The reply goes through the library's `Inbox::send`
    (what `Inbox::reply` calls, so `reply_to` and `callback_data` pass),
    addressed to `Inbox::recipient`: a free-form reply outside the window
    is `409 customer_service_window_closed`, one while another app owns
    the thread `409 thread_owned_elsewhere` (`ErrorKind::ThreadOwnedElsewhere`,
    already in §5.2), both before any request; a template is exempt. The
    binding epoch is `authz::history_from` (a WABA with a previous
    tenant shows its holder nothing dated before `attached_at`; `None`, all
    of it, for a first onboarding, whose synced history predates its
    binding), carried by the `OwnedNumber` from the binding it was made
    from; a page is answered only if that binding still holds once read
    (`503 storage_unavailable` otherwise); a conversation whose customer
    last wrote before the cut shows no `last_inbound_at` and no `unread`.
    Retention: `WA_SERVER_INBOX_RETENTION` (`keep` by default, or a
    positive duration), the inbox store built with it
    (`MemoryBackend::with_inbox_retention`, `PgBackend::with_inbox_retention`,
    through `BackendFactory::open_with`, which `serve` calls), and
    `housekeeping::round` runs `ConversationStore::apply_retention` under
    its turn; `serve` refuses a backend whose inbox applies another
    retention than the setting. `ReplyChecks` per number:
    `WA_SERVER_INBOX_REPLY_CHECKS` (`inbox::ReplyChecksByNumber`). The
    OpenAPI document is additive only (796 lines added, none removed:
    two paths, the tag `inbox`, six schemas). Decisive tests:
    `conformance::inbox` (in `conformance::run`: M2.1 with no HTTP layer,
    the binding epoch, `control_taken`, the order's own number, the
    retention) on the core's memory backend, the memory crate's (built
    with a 30-day retention, and without one) and live on Postgres; over
    HTTP, the service's `tests/inbox.rs` (M2.1, the epoch, the reply's
    key, the ownership refusal and the setting, the scope, pages) and
    `live_postgres_m2_1_an_inbound_message_is_listed_read_and_answered`
    and `live_postgres_a_moved_numbers_history_is_hidden_from_its_new_tenant`;
    the start's refusal (`composed.rs`), the settings (`config.rs`), the
    M1.3 table, M1.5's sentinel and M1.7's log capture over the new
    routes. By mutation: the epoch filter removed fails the memory and
    core suites and the HTTP epoch test; the `apply_retention` call
    removed fails the retention case on the memory backend and live on
    Postgres; the window check, or the ownership refusal's mapping,
    removed fails M2.1 and the ownership cases. **With M2h, in the same
    pull request:** a number's own move record, so that a number Meta
    migrated from one tenant's WABA to another tenant's, neither WABA
    moving, is cut too. `NumberBinding` gains `attached_at` (when the
    number came under its tenant: a refresh, and a move between the
    tenant's own WABAs while bound, keep it) and
    `previous_tenant_released_at` (when another tenant last released it,
    from its `NumberTombstone`: a release is a binding that no longer
    lists it, or an unbind of its WABA, recorded in the same step; a
    deleted tenant counts as another), and `authz::history_from` is the
    later of the WABA's cut and the number's (a number another tenant
    released hides what is dated before its own `attached_at`; a first
    binding of a number no other tenant held keeps everything). The port
    rules are `RecordStore::bind_waba`'s and `unbind_waba`'s, applied by
    `NumberBinding::newly_bound` and `NumberTombstone` in memory and in the
    Postgres SQL. Postgres: migration 5, `number moves`
    (`0005_number_moves.sql`, additive, metadata only on Postgres 16:
    `wa_server_numbers.attached_at`, no default for the existing rows,
    which read their WABA's `attached_at`, and `now()` for the rows a
    replica of the previous release inserts;
    `wa_server_numbers.previous_tenant_released_at`, the Unix epoch
    (`UNRECORDED_MOVE`) by default, read as a move: fail closed; the table
    `wa_server_number_tombstones`, and the trigger
    `wa_server_numbers_released` with its function
    `wa_server_number_released()`), checksums 1 to 4 unchanged; a released
    number's tombstone is written in the releasing statement by the
    trigger, for every deleted number row (an unbind's cascade, a binding
    that no longer lists it, and the same deletions by a replica of the
    previous release during a rolling deploy, which writes none of its
    own), and a new
    number binding reads it in a statement of its own after its insert,
    its start `GREATEST(now(), clock_timestamp(), released_at)`, as S2b's
    review had a WABA's. The event outbox's routing needs no per-number
    rule: an event names the WABA of the entry Meta sent it under, and the
    routing gives a number's event to its tenant only when that WABA is
    the one the number is bound under, so the previous holder's events,
    dated or not, redelivered or late, name its WABA and reach nobody
    (S2b's window exists because a moved WABA keeps its id; a migrated
    number does not keep its WABA). Decisive tests:
    `conformance::inbox::a_number_migrated_between_two_tenants_wabas_hides_its_history`
    (the core's memory backend, the memory crate's, live on Postgres) and
    over HTTP `a_number_migrated_between_two_tenants_wabas_hides_its_history`
    and `live_postgres_a_number_migrated_between_two_tenants_wabas_hides_its_history`;
    `records::a_numbers_moves_are_recorded`,
    `races::a_number_taken_while_released_records_the_move` (and the
    number's assertions added to
    `races::a_binding_racing_an_unbind_records_the_move`), on memory and
    live; `live_postgres_migration_5_rewrites_no_table` (the numbers
    table's file unchanged by migration 5, a number from before it read as
    unrecorded, its start kept when another WABA of its tenant takes it);
    `live_postgres_a_release_by_the_previous_release_is_recorded` (the
    previous release's own deletions leave tombstones); the S7/S2b continuity test, which now adds migrations 4
    and 5 to `main`'s database and reads `main`'s number as unrecorded,
    its move then recorded. By mutation: the number's cut removed from
    `history_from` fails the migration case on the core's memory backend,
    the memory crate's and live on Postgres, and over HTTP on memory and
    live; the read of the tombstone in a new number binding removed
    (memory's `newly_bound` given none; Postgres's statement after the
    insert skipped) fails `records::a_numbers_moves_are_recorded` and the
    HTTP migration case, on memory and live; the trigger dropped fails
    them too, live, and the rolling deploy's case. **The sabotage
    review (2026-11-01), tenant isolation first,** added: a free-form
    reply on a number with a cut needs a window opened from the cut on
    (the customer's message, a call or a standby message dated from it,
    or, with the handover trusted, a handover recorded since), else `409
    customer_service_window_closed` with zero requests, so the new holder
    neither continues the previous holder's conversation nor learns from
    the answer that its customer wrote (`Conversations::reply`'s
    `refuse_across_the_cut`; templates exempt; nothing on a number without
    a cut or with the window check off); `unread` counts only the
    customer's messages dated from the cut on (`inbox::UNREAD_SCAN`
    latest messages read at most, only on a number with a cut); a page is
    answered only if the number's own binding also holds (its tenant, WABA
    and `attached_at`: a capability made before its number moved to
    another WABA of its tenant and on to another tenant, its first WABA's
    epoch unchanged, read the new holder's history);
    migration 5's trigger (above) for the rolling deploy; and a refresh
    that moves a number from before migration 5 to another WABA of its
    tenant writes its start down (its first WABA's) before the move.
    Decisive: `conformance::inbox::a_number_migrated_between_two_tenants_wabas_hides_its_history`
    (the reply's refusal, `unread`) and
    `a_capability_made_before_its_number_left_reads_nothing`, on memory
    and live, the HTTP migration case, and the two live migration tests;
    the WABA's cut, redundant with the number's for every move recorded
    since migration 5, is decisive for a WABA moved before it
    (`live_postgres_a_waba_moved_before_migration_5_still_cuts_its_numbers`).
    Left, and why: a reply is dated by the service's clock, so one recorded in
    the seconds after a move by a replica whose clock is behind the
    database's could be hidden from its own tenant; the reads open an
    `OwnedNumber`, so they read the vault and answer `409
    reconnect_required` for a number whose token lapsed, although they
    ask Meta nothing; the design's window and read routes, a message by
    id, and the list's `window` are M2g's; `ReplyChecks` and the
    retention are per deployment (by number id, and for every number),
    not per tenant. No new stored identifier: the two settings are
    environment variables.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable):
    1. `ReplyChecks` per number as a deployment setting keyed by phone
       number id, `WA_SERVER_INBOX_REPLY_CHECKS`, in the format as built,
       not a field on the binding (no schema change; to swap, a column
       read into `ReplyChecksByNumber`, which `Conversations::new` takes
       as a value).
    2. A binding made before migration 4 (and, with M2h, a number bound
       before migration 5), which reads `UNRECORDED_MOVE`, counts as
       moved: fail closed, hiding a pre-migration coexistence history
       older than its binding (to swap, `authz::history_from`).
    3. The inbox's reads go through the whole authorization order, the
       vault included, so a number whose token lapsed answers `409
       reconnect_required` (to swap, a token-less capability for reads).
    4. `serve` refuses a backend whose inbox applies another retention
       than `WA_SERVER_INBOX_RETENTION`, rather than warning.
    5. M2g kept as an item of its own (the window and read routes, a
       message by id).
    6. A page read while the binding moved (the WABA's epoch, or, since
       the review, the number's own binding) answers `503
       storage_unavailable`, which is retryable (to swap,
       `OwnedNumber::still_held`).
    7. The reply goes through `Inbox::send` rather than `Inbox::reply`,
       to carry `reply_to` and `callback_data`: the review confirmed that
       `Inbox::reply` is `Inbox::send` on the conversation's recipient, so
       every check (the window with its calls and standby messages, the
       handover trust, the thread's owner, the template and Direct Send
       exemptions, a recipient outside the conversation refused) runs the
       same; the service never sets a Direct Send `category` (`422
       unsupported_message_type`). The service adds its own check before
       them on a number with a cut (the review's, above).
    8. On a number with a cut, a free-form reply needs a window opened
       since the cut (an inbound message or window event dated from the
       cut on, or, with handover trust, a `ThisApp` handover recorded
       since it); otherwise `409 customer_service_window_closed` with no
       request, templates and Direct Send categories exempt. A previous
       holder's window never opens one for the new holder, and the 409
       cannot tell B whether A's customer wrote. The cost: the new holder
       starts with a template. Swap: drop `Conversations::reply`'s
       `refuse_across_the_cut` call.
  - **Also, after the first release** (from S7, carried by M2a, which
    landed before it): drop the legacy fingerprint (S6's window:
    `Fingerprint::legacy_v1_json`, `legacy_v1_upload`, `or_legacy`, and
    the handlers' `Uri`), once every deployment has run a release with
    S6 for one `WA_SERVER_IDEMPOTENCY_TTL`. The next item of the
    service's to land after the first release carries this line if it
    lands before it.
- [x] **M2b. Live updates** (row 10): SSE through an `EventNotifier`
  port (Postgres `LISTEN/NOTIFY`, memory broadcast; resume by
  `Last-Event-ID`); `GET /v1/events/{id}`.
  - **After:** S7.
  - **Decisive:** M2.2, M2.3, M2.5.
  - **Landed:** the core's port `notify::EventNotifier` (object-safe:
    `notify(&Wakeup)`, `subscribe(&TenantId)` to a `Subscription`; a
    replica's wake-ups through a `Hub`, a channel per tenant from its
    first subscription to its last), in the `Backend` bundle
    (`Backend::notifier`): memory's `LocalNotifier`, the process's hub;
    Postgres' `PgNotifier`, `NOTIFY` on the channel `wa_server_events`
    with `<sequence> <tenant>` and one `LISTEN` connection per replica,
    of its own outside the pool, which wakes every stream of the replica
    when it listens again. The webhook pipeline notifies once a row it
    routed to a tenant committed (a failure logged, the delivery still
    `200`). The streams are the core's `live::Streams`: a stream is a
    cursor in its tenant's stream, reading the outbox after it, with
    polling's filters, when woken and every 15 s whatever it heard (the
    re-poll), so a notification is a wake-up and nothing else and a lost
    one loses nothing; bounded per tenant and replica
    (`WA_SERVER_STREAMS_PER_TENANT`, 20, `WA_SERVER_STREAMS_PER_REPLICA`,
    1,000, `429 too_many_streams`), 1 MiB and 64 frames held per stream,
    a client taking nothing for 30 s dropped with `event: lagged`
    (`{"after": n}`), as is a stream behind a purge, a heartbeat every
    15 s, and `event: shutdown` for every stream when the replica stops
    (`AppState::begin_shutdown`). Routes (additive in `openapi/v1.json`):
    `GET /v1/events/stream` and `GET /v1/numbers/{pn}/events/stream`
    (the HTTP crate's `api::events::stream_routes`, behind scope
    `events`; `Last-Event-ID` or `?after=`, `410`/`422` on the cursor
    before the stream, `503 shutting_down`; the number's stream step 4
    only, no vault) and `GET /v1/events/{id}` (the axum adapter's
    `get_event`, over `Outbox::get` and `events::get_event`: the
    tenant's own only, `404` alike for another's, an operator-only row's,
    a purged or malformed id); operations `get_event`, `stream_events`,
    `stream_number_events`, rate class `read`; the event envelope moved
    to the HTTP crate (`api::events::EventEnvelope`, schema unchanged).
    Conformance: `conformance::live` in `conformance::run` (memory and,
    live, Postgres), and
    `live::an_event_through_one_replica_wakes_a_stream_on_another` over
    two Postgres pools. Decisive: M2.2 live over HTTP on two replicas
    (`live_postgres_a_webhook_into_one_replica_reaches_a_stream_on_another`:
    within 2 s, `Last-Event-ID` replaying exactly the missed events,
    another tenant's number's stream seeing none); M2.3 through the core's
    pipeline and over HTTP (`no_unknown_or_unparsed_event_reaches_a_stream`);
    M2.5 on memory and live (200 streams, half never read, 100 events a
    second for 3 s: every read stream gets its tenant's events in order,
    the data held never past each stream's budget, 0.9 MB at peak live);
    a lost notification still delivers (`a_lost_notification_still_delivers`,
    core and HTTP, with its control); `Last-Event-ID` resumes exactly
    after (`last_event_id_resumes_exactly_after_it`). Kept as tests: a
    notifier new on each call, or waking every tenant, fails
    `conformance::run`, and an outbox whose pages leave the tenant out
    fails the tenant case (the memory crate's `tests/conformance.rs`).
    **The sabotage review (2026-11-01), tenant isolation and resource
    bounds first,** added: a stream's place among the 20 and the 1,000
    is held by its producer as well as its body, and given back when the
    stream ends (lagged, shut down, revoked, its client gone), whatever
    the client does, and a lagged stream drops what it held then, so a
    client stalled on a full TCP window keeps neither its place nor its
    buffer (`lagged` names the last sequence its client took); the
    listeners close a connection that takes no byte of its answer for a
    minute (`listen::Limits::write_timeout`, both listeners), so the
    stalled connection goes too; a stream asks again, before it reads (at
    most every second, at least every re-poll), whether its key would
    still be admitted (`authz::Recheck`: the key stored, neither revoked
    nor expired, the scope, a platform key's tenants, the tenant stored
    and not suspended), and ends with no last frame once not: before, a
    revoked key's stream went on for as long as its connection lasted;
    `429 too_many_streams` carries `Retry-After: 5`, as the service's
    other limit does; the open streams are a metric,
    `wa_server_event_streams_open` (design §7.3); and the Postgres
    listener listens again on a lost connection itself, waking every
    stream once it listens rather than before. Decisive:
    `conformance::live::a_slow_client_is_dropped_with_its_buffer_bounded`
    (no place nor buffer held while the client never reads; the frames
    held counted apart from the budget's own count, `LiveStream::pending`)
    and `a_stream_ends_once_its_key_or_tenant_is_no_longer_admitted`, on
    memory and live on Postgres; over HTTP `a_slow_client_is_dropped_with_lagged`,
    `a_stream_ends_once_its_key_is_revoked`, `streams_are_bounded_per_tenant`
    (`Retry-After`) and `the_open_streams_are_a_metric`; the HTTP crate's
    `a_client_that_stops_reading_is_closed_after_the_write_timeout`, with
    its control; `live_postgres_a_killed_listener_listens_again` (the
    listening session terminated on a channel of the test's own, so no
    other test is disturbed). Each guard was removed or bent in turn and
    a named test failed; two that survived gained a test (the HTTP
    crate's `a_literal_segment_beats_a_parameter_in_either_order`: the
    stream's operation held only by the table's order; the core's
    `an_id_of_no_events_shape_is_not_looked_up`), and one is redundant by
    construction (`Producer::end` gives the slot back, and so does the
    producer's first field when it drops). Checked and
    left as built: the `NOTIFY` payload is parsed as a sequence and a
    `TenantId` or ignored, never trusted beyond a wake-up (a forged one,
    which any role of the database may send, costs its tenant's streams
    a read each); a cursor is a number of the caller's own tenant's
    stream (another tenant's sequence names nothing of theirs; a negative
    one is `422`, one past the newest `422`); `Operations::of` and
    `telemetry::most_specific` change only `/v1/events/stream` against
    `/v1/events/{id}`, the one pair of the document's templates that
    overlap; the `Event` schema is byte-identical to main's.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable): D39 (each stream
    reads, as built; one reader shared per replica is a later
    optimisation behind `live::Streams`, not a roadmap item), D40 (the
    channel `wa_server_events` and the payload `<sequence> <tenant>`, as
    built, both stable identifiers pending the owner's confirmation
    before the first release), D41 (no cursor starts at the newest
    sequence; `Last-Event-ID` wins over `after`; a malformed event id is
    `404`) and D42 (1 MiB and 64 frames per stream, a 30 s drop, a 15 s
    re-poll), as design §10 records; and:
    1. A number's stream takes step 4 of the authorization order only
       (the number bound to the tenant), no vault read (to swap,
       `Authorizer::owned_number` in `Streams::open_number_stream`).
    2. The webhook pipeline notifies after the outbox insert committed,
       for a row it routed to a tenant (to swap, `ServiceSink`'s call in
       `inbound.rs`).
    3. The event envelope's schemas move to the HTTP crate, the schema
       unchanged (`api::events::EventEnvelope`).
    4. tokio's `macros` feature in the service's core (`select!`).
    5. `Backend::notifier()` is required, with no default: a backend
       built outside this repository must add one (CHANGELOG, breaking
       for it).
  - **Confirmed 2026-11-01** (the coordinator, under the owner's
    delegation): D43, as the review proposed it (the re-check before a read, the frames dropped on a lag, `Retry-After: 5`, the
    one-minute write timeout).
  - **Left, and why:** a delete and re-create of a tenant within the
    re-check's second could let a stream read one event of the new
    tenant before it checks (the tenant's keys go with it, so the next
    check ends it); the re-check reads the key and the tenant, two
    record reads per stream and second at most, for a busy tenant's 20
    streams; webhooks-out (M2c) will need the notifier's wake-ups too,
    and gets them through the same port.
- [x] **M2c. Webhooks out** (row 10): endpoints, dispatcher and retries
  (Standard Webhooks, a destination allow list).
  - **After:** M2b.
  - **Decisive:** M2.4.
  - **Landed:** the core's `webhooks_out` (design §4.4's *as built*):
    the port `WebhookStore` in the bundle (`Backend::webhooks`, required:
    breaking for a backend built outside the repository), memory's
    `MemoryWebhookStore` under its store's lock and Postgres'
    `PgWebhookStore` on migration 9, `webhooks out` (two tables,
    `wa_server_webhook_endpoints` and `wa_server_webhook_deliveries`;
    numbered 9, after M2f's 8, kept at the merge of PR #115), with the suite `conformance::webhooks_out` in
    `conformance::run`. Nine routes in the axum adapter
    (`api::webhook_endpoints`, scope `webhooks`, additive in
    `openapi/v1.json`): `POST` and `GET /v1/webhook-endpoints`, `GET`,
    `PATCH` and `DELETE /v1/webhook-endpoints/{id}`,
    `POST …/{id}/rotate-secret`, `POST …/{id}/test` (`202`, a ping),
    `GET …/{id}/deliveries` and `POST …/{id}/deliveries/{delivery_id}/retry`
    (`202`); two new codes, `409 webhooks_not_configured` and
    `409 webhook_endpoint_limit_reached`. An endpoint is a cursor in its
    tenant's stream; the dispatcher (`webhooks_out::dispatch::Dispatcher`,
    spawned by `serve` on every replica when a data key is set) fans the
    outbox out to it (once: the cursor moves from the value it read, with
    the deliveries), claims each tenant's oldest due delivery under a
    60 s lease (`FOR UPDATE SKIP LOCKED`), reads its event in the
    endpoint's tenant's stream, leaves `data` out past 256 KiB, checks the
    destination again on a fresh lookup and connects to the checked
    address only (`destination::pin`; the HTTP crate's `HyperSender`:
    TCP, TLS on aws-lc-rs and the platform's store, HTTP/1.1, 10 s, 1 KiB
    read, no redirect), signs with the endpoint's secrets (Standard
    Webhooks, both during a 24 h rotation), retries 10 s doubling to an
    hour with a fifth of jitter for 72 h (`WA_SERVER_WEBHOOK_RETRY_WINDOW`),
    then `failed`, disabling an endpoint failing the whole window; bounded
    to 32 attempts a replica, 4 an endpoint, 8 a tenant. Secrets are
    `whsec_` and 32 random bytes, sealed with AES-256-GCM under
    `WA_SERVER_DATA_KEY` (`WA_SERVER_DATA_KEY_ID`,
    `WA_SERVER_DATA_PREVIOUS_KEYS`), shown once. Housekeeping purges the
    settled deliveries after 30 days. The M2d types and the number events
    are delivered as every tenant type
    (`every_tenant_event_type_is_delivered`). Decisive tests (M2.4): the
    Standard Webhooks libraries' known-answer vectors
    (`webhooks_out::signature::the_standard_webhooks_test_vectors`, and
    the receiver's check `a_receiver_verifies_as_the_libraries_do`); the
    schedule under a fake clock on memory and live on Postgres
    (`conformance::webhooks_out::the_retry_schedule_under_a_fake_clock`:
    attempts at 0, 10, 30, 70 and 150 s, none earlier, `failed` at the
    window, the endpoint disabled, a manual retry delivering it; and
    `schedule`'s own waits and window); redirects not followed
    (`server_http::webhooks_out`'s
    `an_attempt_goes_to_the_pinned_address_and_follows_no_redirect`, the
    `Location` never asked); a destination refused at creation and at
    delivery (`a_destination_is_refused_at_creation_and_at_delivery`: the
    stub's second lookup turns private, nothing is sent,
    `destination_refused`; `destination::each_pin_checks_a_fresh_lookup`;
    over HTTP `a_destination_outside_the_allow_list_is_refused_at_creation`);
    two signatures during a rotation (`a_rotation_signs_twice_for_24_hours`,
    over HTTP `a_rotation_signs_with_both_secrets`); over 256 KiB
    truncated and fetched by its tenant only
    (`a_large_event_is_delivered_truncated`, over HTTP
    `a_large_event_is_delivered_truncated_and_fetched_by_its_tenant_only`).
    Also: each delivery its tenant's event only
    (`a_delivery_is_signed_and_carries_its_tenants_events_only`, over HTTP
    `a_metas_webhook_reaches_its_tenants_endpoint_signed`, a Meta webhook
    through `/webhooks/meta`); two dispatchers send each event once; a
    slow endpoint holds its own bound only
    (`a_slow_endpoint_does_not_starve_another`); the TLS name checked at
    the pinned address; 1 KiB of an answer read; the sealed format pinned
    (`seal::the_sealed_format_is_pinned`); a delivery's body the polled
    envelope (`server_http`'s `a_deliverys_body_is_the_events_envelope`).
    By mutation, each failing a named test: `pin` checking the first
    address only, a non-public address admitted for a listed name, the
    metadata addresses not forbidden, plain `http` outside development, an
    IPv6 form not judged as the IPv4 address it carries, credentials in
    the URL accepted (`destination`'s tests); the attempt's check against
    another list (`a_destination_is_refused_at_creation_and_at_delivery`),
    the creation's check skipped (the same case); no truncation
    (`a_large_event_is_delivered_truncated`); the previous secret dropped,
    or kept past its 24 hours (`a_rotation_signs_twice_for_24_hours`); the
    waits uncapped, the window ignored, the jitter unbounded (`schedule`'s
    tests); an endpoint never disabled
    (`the_retry_schedule_under_a_fake_clock`); a claim ignoring its lease
    or a suspended tenant (`a_claim_is_exclusive_until_its_lease_ends`), a
    fan-out ignoring its cursor (`a_fan_out_moves_its_cursor_once`), an
    endpoint read for any tenant (`an_endpoint_is_its_tenants_own`), the
    endpoint bound ignored (`a_slow_endpoint_does_not_starve_another`); the
    sealed secret not bound to its endpoint (`seal`'s tests); the whole
    answer read, the sender resolving the host itself
    (`server_http::webhooks_out`'s tests); the signature over another
    layout (the vectors). One survived and is redundant by construction:
    a ping's "tried once" rule (its `give_up_at` is its creation, so the
    schedule fails it after one attempt anyway). The MongoDB test bundle
    of S9 (`SpikeBackend`) serves the memory backend's `WebhookStore`, as
    it does every port but the outbox (M2c adds no method to `Outbox` or
    `RecordStore`).
  - **Decided** (design D105–D113, the coordinator's decisions of
    2026-11-02 under the owner's delegation, each swappable, as the
    implementer proposed them): the port and its tables (D105), an
    endpoint as a cursor with no backfill (D106), the dispatcher's clock,
    replicas' clocks agreeing within the lease (D107), the claim and its
    bounds, settings since the review (D108), the allow-list's rules,
    tightened by the review (D109), the secrets and an optional data key
    (D110), retries and disabling, the window a setting (D111), the
    routes' small choices (D112), the attempt's wire (D113). Every stored
    name stays pending the owner's confirmation before the first release
    (§ Owner touchpoints).
  - **Reviewed** (adversarial, security lens, 2026-11-02): an attempt
    cut by its time limit left its connection's task running, and a TLS
    one kept its socket for as long as the receiver stopped reading (a
    slow-loris receiver held one socket per attempt); the connection's
    task is now aborted with the attempt
    (`server_http::webhooks_out`'s
    `a_timed_out_attempt_closes_its_connection`, failing before the fix).
    hyper buffered up to about 400 KiB of an answer's head; 16 KiB now
    (`an_oversized_answer_head_is_refused`). An IPv6 address outside
    `2000::/3` was public unless listed special: the IPv4-translated
    `::ffff:0:127.0.0.1` passed for a listed name; every address outside
    global unicast is now non-public, and `3fff::/20` too
    (`destination`'s `non_public_addresses_need_a_listed_cidr_and_some_never_pass`).
    The purge counted 30 days from a delivery's creation, not its
    settlement, as its documentation said
    (`deliveries_go_with_their_endpoint_and_tenant`). The dispatcher's
    bounds and lease became settings (D108). A URL's token no longer
    reaches `Debug` through the creation's and the change's types.
    Tests added for what survived a mutation: a `*.` pattern matching
    without its dot (`destination::host_spellings_are_judged_as_what_they_parse_to`),
    a manual retry naming another endpoint's delivery, the tenant's own
    or B's (`a_settlement_records_its_attempt`,
    `a_delivery_is_signed_and_carries_its_tenants_events_only`), and a
    retried ping tried for a whole window (the same case: the
    implementer's surviving mutation, killed). The M1.7 log capture now
    runs a dispatcher round too.
  - **Left, and why:** `tenants` on a platform key's endpoint (an endpoint
    is one tenant's: D112; M2m); a walk re-sealing the endpoints under a
    new data key (a previous key keeps opening them; M2i); a connection
    pool to the receivers (each attempt connects to the address it
    checked; M2j); the pending webhooks-out metric of design §7.3 (M2k);
    a fan-out reads each page's event data (up to 8 MiB) to learn ids,
    which an id-only outbox read would spare (a port change; M2l);
    replicas' clocks must agree within the lease (D107); a suspended
    tenant's deliveries are still recorded, and wait (D111).
- [x] **M2d. The reviewed event types** (rows 84, 137, 139; D25):
  `standby_observed`, `thread_control_changed` and
  `user_action_reported` made tenant-visible, with L7's inbox answer to
  OPEN_QUESTIONS #44 (the inbox already records ownership and window
  events from the first two; a tenant reading them through M2a's routes
  sees the same `ReplyChecks` per-number setting and `409
  thread_owned_elsewhere`); the service's number events.
  - **After:** S7, L7.
  - **Decisive:** M2.6; taking one of the three types out of
    `TENANT_EVENT_TYPES` fails it.
  - **Landed:** the three types move from `OPERATOR_EVENT_TYPES` (now
    `unknown`, `unparsed`, `partner_solution_updated`) to
    `TENANT_EVENT_TYPES` (34 types), routed by their number as every event
    naming one (the binding epoch, the same-second rule after a move and
    D36's window unchanged), so `GET /v1/events`, `GET /v1/events/{id}`,
    both streams and the `types` filter carry them for their number's
    tenant only. The OpenAPI document is additive only (six values
    appended to `KnownEventType`; the one line removed is a comma); no
    event data snapshot changed or was added (the 11 fixtures of the three
    types, from five of Meta's pages, were pinned already). Meta's click
    payload (`marketing-messages/track-click-events`, read again on
    2026-11-02) names neither the user nor the message: its `click_id`,
    `tracking_token` and `product_id` are served whole and reach no log
    line (M1.7's capture now delivers the three types with sentinels). The
    service's number events are the core's new `number_events`:
    `number_connected` (the admin attach, once the token is stored, and,
    merged with M3a, an Embedded Signup completed or resumed),
    `number_disconnected` (`reason`: `tenant_disconnected` once Meta
    unsubscribed the app, before M3a's revocation in Solution Partner
    mode; `operator_unbound` with or without a usable token; with M3a,
    `partner_app_uninstalled` and `account_deleted` for Meta's
    `account_update` offboarding, confirmed with Graph; recorded before
    the binding goes) and
    `number_reconnect_required` (a `190` marking a number anew, best
    effort), one per number, `data` `{event, phone_number_id, waba_id}`
    and `reason`, keyed by the binding's epoch (once per generation), the
    outbox's own `RouteGuard` on the number under the epoch's WABA dated
    the second after that binding began (no port change, no backend
    change), each waking the tenant's streams; the `Authorizer` records
    them once `AppState` gives it `Inbound::number_events`. Decisive tests:
    `conformance::webhooks::the_reviewed_types_reach_their_numbers_tenant_and_no_other`
    (Meta's 11 examples, `conformance::webhooks::REVIEWED_EXAMPLES`, on
    A's number: A's rows, poll and lookups; B none; an unbound number's
    and A's number under B's WABA operator-only),
    `the_reviewed_types_of_a_previous_holder_reach_no_tenant` (after a
    move, dated before it or in the binding's second: no tenant; the next
    second: the new holder) and
    `conformance::live::the_reviewed_types_reach_their_numbers_tenants_streams_only`,
    in `conformance::run` (the memory crate's and, live, Postgres), the
    first two also on the core's own memory backend; the suite
    `conformance::number_events` (their tenant only on poll, streams and by
    id, a number not the binding's and a moved or re-made binding giving
    the row to nobody, once per generation, the new holder's own at once
    after a move); over HTTP `tests/reviewed_events.rs` (M2.6: poll in
    order with the snapshots' data, by id, both streams of each tenant,
    `types`; `KnownEventType` equal to `TENANT_EVENT_TYPES`; the inbox
    answering what the events say: after Meta's standby copy no unread
    message, a recorded window, `409 thread_owned_elsewhere`, and with the
    number's ownership check off the reply going on the window the copy
    opened, `409 customer_service_window_closed` without the copy; after
    Meta's `control_taken`, the same) and `tests/number_events.rs` (each
    change of a binding its tenant's event only, on the poll, the stream,
    the number's stream and by id; the operator's unbind the previous
    holder's, the next holder starting anew; `data` holding no token,
    display number or Meta text). By mutation, each failing
    a named test: each of the three types in turn moved back to
    `OPERATOR_EVENT_TYPES` (the two webhook cases, on the core's memory
    backend; for `user_action_reported` also HTTP M2.6 and the memory
    crate's `conformance::run`); `user_action_reported`'s date dropped from
    `meta_time` (the previous holder's case: undated, the new holder no
    longer gets the next second's); the attach's `number_connected`, the
    disconnect's and both unbind paths' `number_disconnected`, the `190`'s
    `number_reconnect_required` each removed, and the disconnect's moved
    after the binding goes (operator-only then), over HTTP; the guard
    dropped (operator-only rows) and the guard on the WABA alone (another
    tenant's number reaching the epoch's tenant), the date in the binding's
    own second, the date by the clock and the clock-or-binding date (a
    tenant created again getting its namesake's), the generation left out
    of the key (`conformance::number_events`, memory); the tenant event's
    `data` added to the pipeline's log line (M1.7's capture, over the
    three types' sentinels); after the merge with M3a, the signup's `number_connected` and the `account_update` offboarding's `number_disconnected` each removed (`tests/signup.rs`'s uninstall case). The `190`'s read of the numbers not marked
    yet is an optimisation the key makes safe: a repeated refusal of a
    marked token is refused before Meta is asked, so no test tells it.
  - **Decided by the coordinator on 2026-11-02** (design §10, D80–D85,
    under the owner's delegation, swappable): the number events' `data`
    (D80), when each is recorded (D81), their guard and date (D82),
    their outbox key (D83), their place in `TENANT_EVENT_TYPES` (D84) and
    `user_action_reported`'s `data` served whole (D85). Each swaps where
    its row says.
  - **Review** (adversarial, tenant isolation, 2026-11-02): an Embedded
    Signup's completion or resume dropped its kept attempt before
    recording `number_connected`, so an outbox failure answered `503`
    and the resume that was to record them found nothing to resume; now
    recorded before the attempt is dropped. Meta's `account_update`
    offboarding recorded `number_disconnected` after the token was
    deleted, so an outbox failure left the token deleted and the binding
    kept (and, not `Busy`, for good); now recorded once Graph confirmed,
    before anything is revoked or deleted. New HTTP tests:
    `tests/signup.rs`'s
    `an_account_deleted_offboards_and_is_its_tenants_number_disconnected`
    (the `ACCOUNT_DELETED` path, M3a's first test of it, with a busy
    outbox redelivered), `an_offboarding_whose_events_fail_for_good_deletes_nothing`
    and `a_resume_whose_number_events_fail_keeps_its_attempt_and_records_them_next`;
    `tests/number_events.rs`'s
    `a_number_event_the_outbox_refuses_is_503_and_its_repeat_records_it`
    (the attach, the disconnect and both unbind paths `503`, their repeat
    recording each; a `190`'s refusal answering its own `409`) and
    `after_a_move_each_holder_sees_its_own_number_events_only` (the next
    holder's number stream from the start, its `phone_number_id` filter,
    the previous holder's ids `404`); the core's
    `conformance::number_events::a_number_event_is_recorded_for_its_bindings_numbers_only`
    (a previous binding's event writes no row about the new holder's
    numbers: the `Authorizer`'s tenant filter, which survived every other
    test). Each failing on its mutation (the review's table). M3a's
    `m3_4_uninstall_then_removal_end_revoked_then_deleted` now redelivers
    the same body (dated anew, a redelivery crossing a second was a second
    `account_updated`: a flake).
  - **Pending the owner's confirmation before the first release**
    (architecture § Stable identifiers): the three type names, the
    `reason` values `tenant_disconnected`, `operator_unbound`,
    `partner_app_uninstalled` and `account_deleted`, and the label
    `meta-whatsapp-server/number-event/v1` with its key layout.
  - **Left, and why:** the coexistence offboarding (`account_update`'s
    offboarded event, design §3.4) marks nothing and records no event
    (M3b's routes); a number
    a refresh drops from its WABA gets no `number_disconnected` (its
    binding is gone when the refresh lands, so the outbox would keep the
    row operator-only); a tenant deleted, created again and bound again
    within one second could get its namesake's late event, and an event of
    a binding's previous generation recorded after its tenant refreshed
    it reaches that tenant (D82: the guard compares no generation); a
    disconnect that fails after its events (`503`, the WABA moved or
    refreshed, a revocation that fails) leaves them for a binding that
    stays (D81); a first Embedded Signup completion whose events the
    outbox refuses kept no attempt to resume, and loses them with its
    `503` (D81); a `190` whose event the outbox refuses loses it (best
    effort, D81).
- [x] **M2e. Received media** (row 45; OPEN_QUESTIONS #43): a media id
  the service recorded as received on the tenant's number is asked from
  Meta without `phone_number_id`; every other id keeps the check; a live
  check against Meta records which behaviour Meta has.
  - **After:** S7.
  - **Decisive:** another tenant's received media id is `404` with zero
    requests (removing the "recorded on this tenant's number" lookup
    fails it); the live check's result is recorded in OPEN_QUESTIONS #43.
  - **Landed** (ticked for the first half of its decisive line; **the
    live check against Meta is still to run**, OPEN_QUESTIONS #43 says
    how): no route added; `GET` and `DELETE
    /v1/numbers/{pn}/media/{media_id}` read, before Meta is asked, where
    the id was recorded as received. The record is the outbox's
    (D77): `NewEvent::received_media` (the core's `received_media_id`:
    a `message_received` event's image, video, audio, document or
    sticker `{type}.id`, or an edit's, digits only) is kept beside the
    row at insert, on Postgres in `wa_server_events.media_id` with the
    partial index `wa_server_events_media_idx` (migration 7, expand-only,
    no table rewrite; a previous release's rows record none), in memory
    beside the row; `Outbox::received_media` (a port change, required:
    D79) answers the rows of an id, every stream, the operator-only one
    included, newest first, at most 16 (`RECEIVED_MEDIA_ROWS`). The core's
    `Media::received` reads them for the number (`media::Received`): a
    row of this number routed to its tenant, received from the binding
    epoch on (`authz::history_from`, on the row's `created_at`) is
    `Here`, and a download asks `GET /{media_id}` without
    `phone_number_id`, once `OwnedNumber::still_held` (`503` otherwise),
    with the number's token, then Meta's URL through the library's
    allow-list, within the same limits and verification; any other row
    makes it `Elsewhere`, `404 not_found` for a download and a deletion
    with nothing asked of Meta (D78; one `info` line, no id); no row,
    `Never`, Meta's check as in M1b. A deletion is never unscoped.
    `Media::new` takes the bundle's outbox (breaking for an adapter
    calling it; the HTTP crate passes the state's). The OpenAPI document
    is unchanged. Decisive tests, on the core's memory backend, the
    memory crate's and live on Postgres:
    `conformance::media::a_media_id_received_elsewhere_is_not_found_with_no_request`
    (the received id downloaded unscoped with the number's token, each
    request with the client's time limit; `404` with zero requests on
    another tenant's number, the tenant's other number, the number's new
    holder after a move, an operator-only row on it, and before the cut
    once the number came back; `503` with zero requests for a capability
    made before the move; a deletion and an upload's id keep the check;
    Meta's URL off its host `502 upstream` with the token sent nowhere;
    a reported size over `max_bytes` `413`) and
    `conformance::outbox::received_media`; over HTTP,
    `tests/media.rs`'s
    `another_tenants_received_media_id_is_not_found_with_no_request`
    (Meta's image webhook through `/webhooks/meta`; also live,
    `live_postgres.rs`); `metas_media_messages_record_their_media_id_and_nothing_else_does`
    over every fixture of the library; the log capture
    `a_received_media_refusal_logs_no_url_id_or_token`; and on Postgres
    `live_postgres_migration_7_rewrites_no_table_and_records_from_its_release_on`.
    By mutation, each failing a test: the lookup removed (any recorded
    row exempting), its tenant, its number or its cut left out; an id
    received elsewhere sent to Meta's check instead, for a download or a
    deletion; a received id kept scoped; the binding not read again; the
    memory or Postgres backend recording no id; an edit not followed, the
    type not matched, the id not checked; the library's host allow-list
    and its request time limit removed. The security review (2026-11-02)
    found the lookup walking the whole outbox on Postgres while
    `media_id` had no statistics (from migration 7 until the next
    analyze; a million rows read per request) and made the partial index
    `(media_id, created_at)`, pinned by its plan in the migration's live
    test; it pinned the cut's instant and the "one row here is enough"
    rule (`Received::among`, sharing the inbox's `visible`), each media
    type's own object, and the `RECEIVED_MEDIA_ROWS` bound in the outbox
    suite; it documented the refusal's timing (D78). D77–D79 are the
    coordinator's decisions (2026-11-02, under the owner's delegation,
    swappable). Migration 7's names are stable identifiers pending the
    owner (§ Owner touchpoints).
  - **Left, and why:** the live check (no real account here: run by the
    owner or the coordinator, OPEN_QUESTIONS #43); media of a coexistence
    history sync (`history` webhooks) and of the business's own echoes
    are not recorded (they are not a customer's message on a live
    number), so they keep the check; a received id older than the
    outbox's retention keeps the check (Meta keeps it about as long).
- [x] **M2f. Erasure in the service** (row 91; D10, D30, D31): an
  admin- or tenant-scoped route that erases a customer on a number
  through the library's `identities` and `erase_all`; deletes the
  tenant's outbox rows for them, through an expand-only `contacts
  TEXT[]` column on `wa_server_events` with a GIN index, filled at
  insert; and writes an erasure-journal entry (an HMAC of
  `phone_number_id|contact` and the time) and an audit event, neither
  naming the customer in clear. Which requests to honour stays the
  deployer's (D10, legal acts).
  - **Kind:** additive (a new route; an expand-only column).
  - **After:** M2a.
  - **Decisive:** after the route, no row of the inbox tables nor of
    `wa_server_events` mentions the customer (a sweep of every table,
    as the library's live test does), and removing the outbox clause
    fails it; the journal and the audit event hold no identity in
    clear; a tenant's key cannot erase on another tenant's number.
  - **Landed:** `POST /v1/numbers/{pn}/erasures` (`erase_customer`,
    tag and new scope `erasure`, rate class `send`) in
    `meta-whatsapp-server-api-axum` (`api::erasure`), over the core's
    `erasure::Erasures` on an `OwnedNumber` (the whole authorization
    order, the vault included): `{confirm: <pn>, contacts: [1..16]}`
    (`422` on `confirm`, `contacts`, `body`, never quoting a contact;
    `409 erasure_not_configured` without `WA_SERVER_ERASURE_KEY`, a new
    code), the binding read again (`OwnedNumber::still_held`, `503`),
    a number with a history cut refused whole (`409
    erasure_crosses_binding`, a new code: the library's `erase_all` has
    no time bound, so it would reach a previous holder's hidden records),
    the identities closed over `Inbox::identities`, a journal entry
    (`ErasureJournal::record`: `JournalEntry`, the hex HMAC-SHA256 of
    `<phone_number_id>|<contact>` under the key for each identity, the
    time, the tenant, the number; by default `LogJournal`, target
    `erasure_journal`), then the tenant's own outbox rows naming one of
    them, never an operator-only row (`Outbox::erase_contacts`, a new
    required port method), then `Inbox::erase_all`, then the audit event
    `customer_erased` (`audit::erasure_audit`: ids and counts); `200
    {erasure_id, phone_number_id, erased_at}`, nothing of what was
    found, no `Idempotency-Key`. Postgres migration 8 (`erasure`; numbered
    after M2e's 7): `wa_server_events.contacts TEXT[]` (no default, no
    rewrite), filled at insert from `NewEvent::contacts`
    (`outbox::event_contacts`: every value under the keys Meta's webhooks
    name a customer by, the business's own numbers left out) and a partial
    GIN index, `wa_server_events_contacts_idx`; the erasure's statement
    (`ERASE_SQL`) reads the tenant by `tenant_id`, which no index leads
    with, and `tenant_id = $1` never matches an operator-only row, so its
    plan is that index or a scan: pinned live with sequential scans off,
    and on 20,000 of the tenant's rows never analyzed, then analyzed, with
    them on. The memory
    backend keeps the contacts beside each row. The decisive tests:
    `live_postgres_after_an_erasure_no_row_of_any_table_mentions_the_customer`
    (every table of the schema swept, as the library's live test does, for
    the customer's phone number, BSUID, parent BSUID, username and name,
    after an erasure by the BSUID alone, every row a tenant may see; before
    it, the inbox's tables and `wa_server_events` held them; the number's
    operator-only row naming them (a message Meta dated before the
    binding) stays; removing the outbox's
    clause, the closure over the identities, or `ERASE_SQL`'s tenant clause
    fails it, and its memory twin
    `an_erasure_by_bsuid_leaves_nothing_of_the_customer`);
    `an_erasure_on_a_moved_number_erases_nothing` (memory and live: on a
    number whose WABA moved from another tenant, `409
    erasure_crosses_binding`, the previous holder's hidden message, the
    new holder's own and both tenants' events kept) and its core twin
    `conformance::inbox::an_erasure_on_a_number_with_a_previous_holder_erases_nothing`
    (both backends, no journal entry);
    `conformance::inbox::an_erasure_failing_part_way_is_completed_by_a_repeat`
    (the outbox before the inbox: with the inbox first, a repeat after a
    failing outbox leaves the phone number's row);
    `the_erasure_service_refuses_another_authorizers_caller_and_number`
    (the core's `tests/capabilities.rs`: each of `Erasures::new`'s
    admissions); `reads_an_erasure_key_of_at_least_32_bytes` (`config`:
    the key's length, `_FILE`, no derivation) and
    `erasure_is_set_up_by_its_key_or_by_a_throwaway_on_memory` (`serve`);
    `the_journal_and_the_audit_event_hold_no_identity_in_clear`
    (`erasure_logs.rs`: no log line at `TRACE` names the customer, the
    journal's HMACs computed apart from the code, the audit event's ids
    and counts); `an_erasure_stays_on_its_number_and_tenant` (memory and
    live: another tenant's key with every scope is `404` with nothing
    erased, a key without the scope `403`, the same customer on the
    tenant's other number and on another tenant's number kept);
    `conformance::outbox::erase_contacts` (both backends, and, since the
    merge of PR #91, S9's `MongoOutbox`, live: the tenant's own
    rows, never the number's operator-only ones, whose dedup keys stay
    taken, nor another number's or another tenant's stream, a repeat
    deleting nothing, the dedup key freed);
    `conformance::inbox::an_erasure_reaches_every_identity_and_journals_first`
    (a failing journal erases nothing) and the stale capability's refusal
    in `a_capability_made_before_its_number_left_reads_nothing`; the
    extractor's `every_identifier_an_event_holds_is_one_of_its_contacts`
    over every fixture; `the_journal_and_the_audit_event_hold_no_identity_in_clear`
    also sends malformed bodies naming the customer (a number for a
    string, an unknown field named after them, a cut body, their number as
    `confirm`): `422`, quoting and logging nothing of them. Left, and why:
    the number's operator-only outbox rows naming the person (the
    operator's, D89: the outbox's retention takes them); a number with a
    history cut cannot be erased on through the service at all (D90: the
    operator erases there through the library, until the owner decides
    the reach and the library has a time-bounded erasure); the 24-hour
    idempotency answers of
    sends to the person keep their recipient (deleting the records would
    let a repeat send twice; a redaction would change a replay's answer);
    a previous holder's own outbox rows of the number (another tenant's
    stream); rows recorded before migration 8 (the outbox's retention
    takes them); the journal's replay after a restore is the operator's,
    as the production guide describes it (no command does it); `doctor`
    does not check `WA_SERVER_ERASURE_KEY`; a number whose token lapsed
    cannot be erased on until it is reconnected (D90).
  - **Decided** (the coordinator's, 2026-11-02, under the owner's
    delegation, each swappable: design §10): D86 (a tenant or platform
    key with the new scope `erasure`, which no other scope grants; no
    admin route), D87 (the route's shape as proposed: identities in the
    body only, `confirm`, no counts, no `Idempotency-Key`), D88 (the
    journal as a port with a log default, under the dedicated secret
    `WA_SERVER_ERASURE_KEY`; `409` without it, a throwaway key on memory
    storage), D89 changed to narrow by default (the calling tenant's own
    outbox rows only, never the number's operator-only ones), D90
    changed to narrow by default (the outbox before the inbox, the
    binding read again first, the inbox's erasure only from the tenant's
    binding epoch on, which the library cannot bound, so a number with a
    cut is refused; the whole authorization order), D91
    (`Outbox::erase_contacts` required, no default). The wider reach of
    D89 and D90 stays the owner's, and every new stored name is pending
    the owner (§ Owner touchpoints).

- [x] **M2g. The inbox's other routes** (row 111; added by M2a, kept as
  an item by the coordinator's decision of 2026-11-01): the design's `GET
  /v1/numbers/{pn}/conversations/{contact}/window` (`{open, closes_at}`,
  `Inbox::check_reply`'s answer beside the recorded window) and `POST
  …/{contact}/read` (unread reset, `notify_customer` marking the latest
  inbound message read on Meta), the list's `window`, and a message by
  id on the number (`ConversationStore::message`), each from the binding
  epoch on.
  - **After:** M2a.
  - **Decisive:** a message of the number's previous holder is `404` by
    id to the new holder (removing the epoch from the lookup fails it);
    after the read route, `unread` is 0 and Meta got one read receipt for
    the latest inbound message only with `notify_customer`.
  - **Landed:** three routes in `meta-whatsapp-server-api-axum`
    (`api::inbox`, tag and scope `inbox`), each over the core's
    `inbox::Conversations` on an `OwnedNumber` (the whole authorization
    order, the vault included, as M2a's decision 3), each answered only if
    the number's binding still holds once read (`OwnedNumber::still_held`,
    `503 storage_unavailable`): `GET
    /v1/numbers/{pn}/conversations/{contact}/window`
    (`get_conversation_window`, read class: `Conversations::window`,
    `{open, closes_at}`, `open` the reply's own checks run with nothing
    sent, the cut's then the library's `Inbox::check_reply`; `closes_at`
    the recorded window, `null` for one opened before the cut); `POST
    /v1/numbers/{pn}/conversations/{contact}/read` (`mark_conversation_read`,
    send class: `Conversations::mark_read`; an optional body
    `{notify_customer}`, unknown fields `422 body`; the receipt first, with
    the number's token, for the latest inbound message among the
    conversation's latest 1000 (`UNREAD_SCAN`), a revoke's tombstone
    skipped, none for one dated before the cut or older than Meta's 30 days,
    `inbox::READ_RECEIPT_MAX_AGE`; then `ConversationStore::mark_read`; a
    refused receipt resets nothing; an optional `Idempotency-Key` over
    `Fingerprint::mark_conversation_read`, the number, the contact decoded
    and the body, `null` when empty; `204` with no body, kept and replayed
    as such by the HTTP crate's `Success::no_content`); and `GET
    /v1/numbers/{pn}/messages/{message_id}` (`get_inbox_message`, read
    class: `Conversations::message`, the store's lookup on the number,
    `{contact, message}`; dated before the cut, `404 not_found`, the body of
    an id the number never had; `422 message_id` for a blank id, one with
    a control character or over 256 bytes). The list's items gain
    `window`, the window route's answer for each
    (`Conversations::list_with_windows`; `Conversations::list` unchanged).
    The reply's cut check is now `Conversations::cut_refuses_free_form`
    (M2a's `refuse_across_the_cut`, split so the window shares it: the
    swap of M2a's decision 8 drops its calls, the reply's and the window's).
    Three tenant operations (`operation::Operation::GetConversationWindow`,
    `MarkConversationRead`, `GetInboxMessage`). The OpenAPI document is
    additive only (513 lines added: three paths, three schemas,
    `Conversation`'s required `window`; the one line removed is a comma).
    No new setting and no new stored name. Decisive tests, on the core's
    memory backend, the memory crate's and live on Postgres:
    `conformance::inbox::the_previous_holders_message_window_and_receipt_never_reach_the_new_holder`
    and `marking_a_conversation_read_resets_unread_and_receipts_the_latest_inbound_message`,
    with `the_window_is_what_a_reply_would_meet`,
    `a_message_is_found_by_id_on_its_own_number` and the stale capability's
    new reads in `a_capability_made_before_its_number_left_reads_nothing`;
    over HTTP, `tests/inbox.rs`'s
    `the_previous_holders_message_window_and_receipt_never_reach_the_new_holder`
    and `the_read_route_resets_unread_and_receipts_the_latest_inbound_message`
    (also live, `live_postgres.rs`), `the_window_route_answers_what_a_reply_would_meet`,
    `a_refused_read_receipt_resets_nothing`, the scope test, M1.3's tables,
    M1.5's sentinel and M1.7's log capture over the new routes. By
    mutation, each failing a test: the cut removed from the lookup by id
    (the core's and the HTTP decisive case), from the receipt's choice
    (the same), from the window's `closes_at` or from its `open` (the
    core's decisive case); the store's reset removed, the receipt sent
    without `notify_customer`, the receipt's message not required inbound,
    the receipt's age limit removed (the core's read case); the binding
    not read again in the window, the lookup or before the receipt (the
    stale capability's case); `open` from the recorded window alone, no
    `check_reply` (`the_window_is_what_a_reply_would_meet`); the read
    route's `Idempotency-Key` ignored (the HTTP read case); the window's
    operation under another scope (the axum crate's
    `each_tenant_route_is_one_core_operation_with_its_groups_scope`); and
    step 4's tenant filters removed from `Authorizer::owned_number` (M1.3
    and the HTTP scope case, over the new routes too). Left, and why: a group conversation
    gets one receipt, for its latest message (Meta marks group messages
    one by one); the window names no refusal reason (D62); the list reads a
    few rows per conversation for its windows (D63). D60–D63 are the
    coordinator's decisions of 2026-11-02, each swappable.
  - **Review** (sabotage, security lens): the binding is read again after
    the receipt, before the reset, so a number that moved while Meta
    answered resets nothing (`503`; the next holder's count was the stale
    request's to zero); three guards no test held are pinned, the receipt's
    tombstone skip (without it the receipt named a message never
    received), its scan past the history's first page and the 30 days'
    bound, to the second
    (`conformance::inbox::a_read_receipt_skips_a_tombstone_and_reads_past_the_first_page`,
    `a_number_that_moves_during_the_receipt_resets_nothing`, in `run`);
    the idempotency claim corrected: a `503` after the receipt went
    releases its key, and its repeat sends the receipt again. Left: a
    receipt for a message its customer revoked after it arrived (status
    `deleted`) is still sent, and Meta documents neither its answer nor a
    repeat's; the read reset and the window emit no live event (M2b's
    stream carries Meta's events only).
- [x] **M2h. A number's own move record** (row 111; added by M2a, and
  folded into it by the coordinator: the inbox never ships with the leak):
  the binding epoch for a number Meta migrated from
  one tenant's WABA to another tenant's WABA, the WABA not moving (M2a
  cuts by the WABA's move record only): a per-number record of the last
  tenant that held it, written with every number a binding drops (an
  unbind, a refresh that no longer lists it), read when a binding takes
  it, on both backends; its names are stable identifiers, the owner's
  to confirm (§ Owner touchpoints).
  - **Kind:** port change (`RecordStore`, an additive migration).
  - **After:** M2a.
  - **Decisive:** a number dropped by tenant A's WABA and listed by
    tenant B's WABA, bound long before, shows B none of A's history;
    removing the number's record from the cut fails it, on memory and
    live on Postgres.
  - **Landed:** with M2a, in the same pull request: see M2a's Landed
    note ("With M2h"). Migration 5's names are stable identifiers pending
    the owner (§ Owner touchpoints).

- [ ] **M2i. Re-sealing the webhook secrets** (added by M2c; D110): a
  walk that re-seals every endpoint's secret and previous secret under
  the active `WA_SERVER_DATA_KEY`, so that a previous key can leave
  `WA_SERVER_DATA_PREVIOUS_KEYS`: an operator command beside `vault
  rotate`, batch by batch, each row written only if its sealed value is
  still the one read.
  - **After:** M2c.
  - **Decisive:** after the walk, with the new key alone configured,
    every endpoint's next delivery verifies with the secret its creation
    or rotation showed; a rotation landing between the walk's read and
    write is not overwritten; skipping the write fails it.

- [ ] **M2j. A connection pool for webhooks out** (added by M2c; D109,
  D113): connections kept between attempts to one endpoint, reused only
  for the address the attempt's own fresh lookup checked and the same
  host name, never past the allow-list.
  - **After:** M2c.
  - **Decisive:** two attempts to one endpoint within the idle time make
    one TCP connection; when the lookup answers another address, the
    attempt opens a connection to it, never the pooled one; a pooled
    connection to an address the list no longer admits is never used.

- [ ] **M2k. The pending webhooks-out metric** (design §7.3; added by
  M2c): the pending deliveries and the age of the oldest due one, on
  `/metrics`, labelled by no endpoint, URL or tenant (unless the
  `tenant` labels are on).
  - **After:** M2c.
  - **Decisive:** the gauge rises with the deliveries a silent receiver
    holds and falls to 0 once they are delivered; no label names an
    endpoint id or a URL.

- [ ] **M2l. An id-only outbox read for the fan-out** (D106; added by
  M2c): an `Outbox` method answering the sequences, ids and types after
  a cursor without their data, which `Dispatcher::fan_out` uses (a port
  change: memory, Postgres and MongoDB implement it).
  - **After:** M2c.
  - **Decisive:** a fan-out over 100 events of 256 KiB each reads none
    of their data (a counting outbox asserts it), and the conformance
    suite's fan-out cases pass on every backend.

- [ ] **M2m. `tenants` on a platform key's webhook endpoint** (design
  §4.4's `{url, types, tenants?}`; D112; added by M2c): an endpoint made
  with a platform key may name several tenants, whose events it
  receives, each delivery read in its own tenant's stream.
  - **After:** M2c.
  - **Decisive:** a platform key's endpoint naming tenants A and B
    receives A's and B's events and none of C's; `tenants` from a tenant
    key is `422`; dropping the tenant filter from its fan-out fails it.

### M3. Embedded Signup, OTP, coexistence

- [x] **M3a. Embedded Signup routes** (rows 120, 142): both partner
  modes, persisted attempts, resume, disconnection, credit lines in
  Solution Partner mode, and vault rotation over the records partner
  mode keeps past a binding. Companion: `docs/guides/production.md`
  gains the vault key cadence of OPEN_QUESTIONS #9 (at least yearly, at
  once on a suspected exposure, the old key dropped only when a rotation
  reports no failure).
  - **After:** S7, L4.
  - **Decisive:** M3.1–M3.4.
  - **Landed:** rows 120 and 142 done on the service's side. Four
    routes on a new tag, `signup` (scope `signup`): `POST
    /v1/signup/sessions` (`start_signup`, rate class `send`), `POST
    /v1/signup/complete` (`complete_signup`, `send`; bodies of the group
    at most 16 KiB), `GET /v1/signup/attempts` (`list_signup_attempts`,
    `read`), `POST /v1/signup/resume` (`resume_signup`, `send`), and one
    admin route, `POST /v1/admin/wabas/{waba_id}/credit-line/reshare`
    (the operator's re-funding of a revoked business, audited
    `credit_line_reshared`). Each is the core's `signup` module
    (`Signups`, built by `AppState::from_backend` on the bundle's
    `KvStore` and the service's clock with the deployment's
    `SignupDeployment`, set by `serve` when `WA_APP_ID` and
    `WA_ES_CONFIG_ID` are; `Signups::open` admits the `Caller`) over the
    library's `EmbeddedSignup`, which makes every Meta call: the attempt
    is the library's `SignupSessions` (`wa.es.session`, unchanged); a
    completion checks every input before the redemption, refuses a
    claimed WABA another tenant holds before it (D4, `409`, the attempt
    kept), redeems for the caller's tenant (`403 stale_attempt`, another
    tenant's never burnt), then runs `onboard_with_approval`, whose
    approval binds the verified WABA and all its numbers (D4 again,
    atomically, before anything is stored, subscribed or shared; in
    Solution Partner mode also remembering the ledger for a rotation); a
    step after `store_token` that fails keeps the attempt (`502
    onboarding_failed`, `resumable: true`), resumed from the stored token
    by `resume_with_approval` (its approval re-checking the binding before
    any credit call; another tenant's resume `404 nothing_to_resume`,
    nothing read). The vault is the `Authorizer`'s
    (`Authorizer::vault_for_signup`, crate-private, a UI case). In
    Solution Partner mode the credit line is shared with the partner's
    system token only and the allocation recorded on the binding; a
    revoked business stops a resume and a new signup at
    `share_credit_line`, and only the admin route funds it again. The
    tenant's `DELETE /v1/wabas/{waba_id}` revokes before it deletes in
    Solution Partner mode (`numbers::Waba::offboarded_by`,
    `Signups::offboard`); the webhook pipeline gains an `AccountActions`
    hook (`Pipeline::with_account_actions`, run once an event is
    recorded; a retryable failure fails the delivery, any other is logged
    for the operator), which `signup::AccountUpdates` implements:
    `PARTNER_APP_UNINSTALLED` of the deployment's app and
    `ACCOUNT_DELETED` offboard (revoke, token, bindings), `PARTNER_REMOVED`
    revokes (D14), each confirmed with Graph first (`debug_token` on the
    stored token; `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION`, `graph` or
    `signature`). The vault's rotation walks the records kept past a
    binding after the bindings (`rotate_vault`, the answer gaining
    `records` and `failed_records`). A `RecordStore` port change (seven
    methods: the attempts, the allocation, the vault entries; both
    backends; Postgres migration 6, `signup`, adding
    `wa_server_signup_attempts` and `wa_server_vault_records`,
    expand-only, its names pending the owner), held by the new
    conformance suite `signup`. A new error code, `409
    signup_not_configured`. The OpenAPI document is additive only (852
    lines added, one changed by a trailing comma: four paths and the admin
    one, the tag `signup`, eight schemas, the code, `VaultRotation`'s two
    fields). The production guide gains the vault key cadence (§ 2, "Vault
    key cadence"); the guide a section, Embedded Signup; a new skill,
    `meta-whatsapp-rs-server-onboarding`. Decisive tests: `tests/signup.rs`
    (`m3_1_the_happy_path_binds_the_verified_waba_and_all_its_numbers`,
    `m3_2_another_tenants_state_is_stale_and_the_rightful_tenant_still_completes`,
    `m3_3_a_133005_at_registration_resumes_in_a_new_process` and its
    `live_postgres_m3_3_…` twin on two pools of one database, which also
    sweeps every table for the token, the PINs and the code,
    `m3_4_the_credit_line_request_carries_the_partners_system_token`,
    `m3_4_missing_partner_settings_refuse_the_start`,
    `m3_4_d4_is_enforced_before_any_credit_call`,
    `m3_4_a_revoked_business_is_refunded_only_by_the_operator`,
    `m3_4_a_cms_disconnect_revokes_first_and_deletes_second`,
    `m3_4_uninstall_then_removal_end_revoked_then_deleted`,
    `m3_4_removal_then_uninstall_end_revoked_then_deleted`), with
    `an_unconfirmed_or_foreign_account_update_changes_nothing`,
    `a_removal_naming_only_the_owner_revokes_by_business`,
    `a_tech_providers_offboarding_revokes_nothing`,
    `a_completion_over_16_kib_is_too_large`,
    `a_coexistence_signup_syncs_and_never_registers`,
    `a_cancelled_flow_redeems_nothing`,
    `a_resume_whose_waba_moves_meanwhile_acts_on_nothing`,
    `the_signup_routes_need_the_signup_scope`; `tests/signup_logs.rs`
    (a partner's whole signup at `TRACE`: no token, system token, PIN,
    code, state or Meta text); the core's `conformance::signup` (the
    records' contract, M3.4's D4 case and a rotation reaching an
    offboarded WABA's ledger and marker, on the core's memory backend,
    the memory crate's and live on Postgres); M1.3's table, M1.5's
    sentinel and M1.7's capture over the new routes. Left, and why: no
    `number_disconnected` event (the service's number events are
    M2c–M2e's); no per-signup currency, no two-call credit method, no
    route clearing a lost credit share (`clear_pending_share`); a
    Multi-Partner Solution's `PARTNER_REMOVED` is not acted on (no
    setting names the deployment's partner business); the admin unbind
    revokes nothing (D75: the operator revokes explicitly); hosted
    signup, several WABAs and pre-verified numbers wait for M3f. The
    review (2026-11-02) made a partner disconnection delete only the
    token and the binding it started from (the library's
    `EmbeddedSignup::offboard_if_unchanged`;
    `an_uninstall_whose_waba_moves_mid_revocation_leaves_the_new_holder`,
    `a_disconnect_whose_waba_moves_mid_revocation_leaves_the_new_holder`)
    and a Solution Partner's tenant deletion revoke first
    (`deleting_a_partners_tenant_revokes_its_wabas_lines_first`). D70–D76
    decided by the coordinator on 2026-11-02; migration 6's stored names
    stay pending the owner.
- [ ] **M3b. Coexistence** (rows 69, 124): onboarding, the automatic
  sync (D7), the synced contacts stored and readable by their tenant:
  M3b turns the inbox's contacts recording on
  (`meta-whatsapp-server-core`'s `inbound::INBOX_RECORDING`, off since
  L8: the library's `RecordingSwitches::ALL` with `contacts(false)`),
  with the routes that read them and their erasure.
  - **After:** M3a, L8, L11d.
  - **Decisive:** a coexistence onboarding over HTTP starts the sync
    exactly once, and a second onboarding of the number does not;
    another tenant's key reads none of the synced contacts (`404`).
- [x] **M3c. OTP and authentication templates** (row 98): OTP with
  per-tenant settings (the sending number, D8), authentication
  templates.
  - **After:** S7.
  - **Decisive:** M3.5.
  - **Landed:** four routes on two tags. `POST /v1/otp/issue`
    (`issue_otp`, scope `otp`, its own rate class `otp_issue`, 5/s and a
    burst of 5, the design's; an `Idempotency-Key` over
    `Fingerprint::issue_otp`, keyed: an HMAC-SHA256 under a key the
    service derives from `WA_OTP_PEPPER`, since the body names a phone
    number and design §6 keeps numbers only as keyed hashes) and `POST
    /v1/otp/verify` (`verify_otp`, scope `otp`, class `otp_verify`, 20/s
    and a burst of 20, the service's default: the design names none),
    each a core service (`meta_whatsapp_server_core::otp`: `OtpCodes`,
    built by `AppState::from_backend` on the bundle's `KvStore` and the
    service's clock, opened for an admitted `Caller` only), over the
    library's `OtpService`, which keeps every rule of its own (the codes,
    stored as keyed hashes under `wa.otp` and `wa.otp.rate`, unchanged;
    the constant-time comparison, the attempt counted before it, the
    expiry, the cooldown and the issue limit per recipient and purpose).
    The namespace is the calling tenant's id, never a setting or a
    request's value; the sending number comes from the tenant's own OTP
    settings, which may only name one of its own numbers, else from those
    of the deployment's default OTP tenant (`WA_SERVER_OTP_DEFAULT_TENANT`:
    the platform's number, D8's default), opened for the tenant whose
    settings name it through the authorization order's steps 4 and 5
    (`Authorizer::otp_sender`, crate-private, a UI case pins it); none,
    or a number no longer that tenant's, is `409 otp_not_configured` (a
    new code of §5.2), with the vault unread and Meta unasked. Outcomes
    are `200` with a status, as the design says (`sent` with
    `challenge_id` and `expires_at`, `cooling_down` and `rate_limited`
    with `retry_after_seconds`; `verified`, `invalid` with
    `attempts_left`, `expired`, `too_many_attempts`, `not_found`); a
    failed send keeps Meta's code, subcode and trace id and never its
    text or `details`; `phone` is strict E.164 with its `+` (a BSUID, a
    `wa_id` and separators refused, `422`); `purpose` is 1 to 64 of
    `[A-Za-z0-9._:-]`, `login` when absent. The tenants' settings:
    `GET`, `PUT` and `DELETE /v1/admin/tenants/{id}/otp` (admin key;
    audited `otp_settings_set` and `otp_settings_cleared`), the number,
    the template and its language, and the library's limits, the issue
    limit never off; stored in `wa_server_tenants.settings` under the key
    `otp` (`OtpSettings::to_stored`, version 1, no migration: the column
    exists since migration 1) through two new `RecordStore` methods,
    `otp_settings` and `set_otp_settings` (a port change: both in-tree
    backends, and the conformance suite `otp`). `POST
    /v1/wabas/{waba_id}/templates/authentication`
    (`create_authentication_template`, scope and class `templates`, an
    `Idempotency-Key` over `Fingerprint::create_authentication_template`):
    Meta's bulk management (`upsert_message_templates`) through the
    library's `Authentication::upsert`, copy code or one-tap, one or more
    languages, its body Meta's fields flattened; zero-tap is refused
    (`422` on `otp_type`): creating one accepts Meta's zero-tap terms, a
    legal act (M3g, § Owner touchpoints). The OTP outcomes are counted
    (`wa_server_otp_outcomes_total`, `Observer::otp`). The OpenAPI document
    is additive only (1086 lines added, none removed: four paths, the tag
    `otp`, fourteen schemas, the code `otp_not_configured`). Decisive
    tests: M3.5 over HTTP, `tests/otp.rs` (`m3_5_every_outcome`,
    `m3_5_a_tenants_code_verifies_at_no_other_tenant_on_the_same_number`,
    `m3_5_a_sentinel_in_a_graph_error_on_issue_reaches_no_response`) and
    `tests/otp_logs.rs` (`m3_5_logs_hold_neither_code_nor_number`), live on
    Postgres
    (`live_postgres_m3_5_a_code_verifies_at_no_other_tenant_on_the_same_number`,
    which also finds no phone number in the key/value, idempotency and
    tenant rows), and the core's `conformance::otp` with no HTTP layer
    (the core's memory backend, the memory crate's and live on Postgres);
    the sending number's holder check, the rate limits per tenant and per
    recipient, the scope, the issue's key, the admin's settings on the
    tenant's own number only and Meta's documented upserts in
    `tests/otp.rs`; M1.3's table, M1.5's sentinel and M1.7's capture over
    the new routes. By mutation (each run, then reverted): a namespace
    shared by every tenant fails both HTTP namespace cases, the core's
    `conformance::otp` case and its unit test; `Authorizer::otp_sender`
    opening the number for whoever holds it fails
    `the_sending_number_is_its_holders_own` and the core's case; the
    admin's own-number check removed fails
    `the_admin_sets_a_tenants_otp_settings_on_its_own_number_only`; the
    `otp_issue` class charged as `send` fails
    `rate_limits_are_per_tenant_and_per_recipient` and the class table;
    the issue limit dropped (`issue_limit: None`) fails
    `m3_5_every_outcome`; Meta's `details` kept on the OTP route fails
    the sentinel case; the fingerprint left unkeyed fails
    `the_otp_fingerprint_is_keyed`; a phone with separators let through
    fails `requests_are_checked_first_and_need_the_otp_scope`. Left,
    and why: the design's verify takes the phone number and purpose, not a
    challenge id (the library's `verify` is keyed so), so another tenant's
    code is `200 not_found`, the design's answer, not a `404`; per-tenant
    rate limit overrides are not settable (the deployment's
    `WA_SERVER_RATE_*` for every tenant); the design's "one `OtpService`
    per (tenant, sending number), cached" is built per request instead (it
    holds no connection: an allocation); template previews and zero-tap
    wait for M3g.
  - **Review (security lens), in the same PR:** five fixes. The generic
    `POST /v1/wabas/{waba_id}/templates` passed a zero-tap button's
    `zero_tap_terms_accepted: true` through to Meta since M1b (a tenant
    accepting Meta's terms): now `422 invalid_request` on the key's path
    (`templates::ZERO_TAP_TERMS`), the only other route that could carry
    it. A suspended or deleted default OTP tenant still lent its settings
    and number: now `409 otp_not_configured`, issue and check (fail
    closed). The library's issue limit is per namespace, so N tenants on
    the platform's number (or N purposes) could send one recipient N
    times it: the default tenant's number now has one issue limit per
    recipient, shared by every tenant and purpose sending from it
    (`otp::SharedLimit` on the bundle's `KvStore`, namespace
    `wa_server.otp.shared_rate`, the key an HMAC of the number and the
    recipient under a key derived from the pepper; reserved before the
    library's call, given back when nothing was sent; design D59,
    proposed). A failed OTP send logged Meta's texts at `debug`, as
    every route does, and they can quote the template's parameter, the
    code: withheld at every level on the OTP route
    (`ServiceError::from_library_withholding_texts`). `doctor` warns
    (`otp.default_tenant`) when the default
    tenant is missing, suspended, without settings or no longer holding
    its number. Decisive:
    `templates::tests::a_definition_accepting_the_zero_tap_terms_is_refused`
    and `tests/templates.rs`'s
    `a_template_accepting_the_zero_tap_terms_is_refused`;
    `a_suspended_or_deleted_default_tenant_lends_nothing`;
    `m3_5_logs_hold_neither_code_nor_number` (a sentinel in each of
    Meta's texts, captured at `TRACE`); `a_malformed_check_counts_no_attempt`;
    `the_platform_numbers_issue_limit_per_recipient_is_shared` and the
    core's `conformance::otp::the_default_numbers_issue_limit_is_shared_and_fails_closed`
    (memory, and live on Postgres); `a_default_otp_tenant_that_cannot_lend_warns`.
    Left: on a tenant's own number the library's limit stays per
    purpose, so a tenant can still send one recipient its limit once per
    purpose it invents (its own number and bill: a per-number limit
    there is a roadmap item if wanted); a tenant suspended between the
    settings' read and the send still sends that one code.
- [ ] **M3g. Zero-tap templates and template previews** (row 98; added
  by M3c): zero-tap authentication templates once the owner decides who
  accepts Meta's zero-tap terms (`zero_tap_terms_accepted`, a terms
  acknowledgement: a legal act, § Owner touchpoints), and `GET
  /v1/wabas/{waba_id}/templates/authentication/previews` over the
  library's `Authentication::previews`; a setting that swaps D59's
  shared issue limit on the default OTP tenant's number for the
  library's per-namespace one; and a borrowed send's delivery statuses
  (which carry the recipient's number) routed to the borrowing tenant
  rather than the default tenant.
  - **After:** M3c.
  - **Decisive:** with the deployment's setting off, a zero-tap creation
    makes no request to Meta; on, one request and one audit event; a
    preview answers Meta's documented example.
- [x] **M3d. Registration and the PIN** (rows 108, 128): request and
  verify a code, register, deregister; the PIN typed per attempt and
  never stored (D6).
  - **After:** S7.
  - **Decisive:** a sentinel PIN is found in no stored row and no log
    capture after a registration (storing it anywhere fails the test).
  - **Landed:** five routes in `meta-whatsapp-server-api-axum`
    (`api::registration`, tag `registration`, scope `numbers`, the send
    class), each over the core's `registration::Registration`, opened on
    an `OwnedNumber` and a `Caller` its `Authorizer` admits (key, tenant,
    scope, ownership, the vault, then the body): `POST
    /v1/numbers/{pn}/request-code` (`request_verification_code`),
    `…/verify-code` (`verify_code`), `…/register` (`register_number`),
    `…/deregister` (`deregister_number`) and `…/two-step-pin`
    (`set_two_step_pin`, the PIN row 128 counts), each `200
    {phone_number_id, status}` and Meta's documented request with the
    number's own token. The PIN and the code are read into the library's
    `TwoStepPin` and `VerificationCode` before anything else
    (`registration::pin`, `registration::verification_code`; the bodies
    that hold them have no `Debug`, and `ApiJson` never quotes a body);
    a failed call keeps Meta's code, subcode and trace id and none of its
    texts, in the answer or any log (`OwnedNumber::failed_withholding_texts`
    and `ServiceError::from_library_withholding_texts`, carried over
    byte for byte from M3c's pull request). Attempts are counted per
    number on the bundle's `KvStore`, shared by every replica, after
    ownership and the local checks and before Meta
    (`registration::RegistrationAttempts`, under
    `wa.server.registration`, a stable identifier pending the owner):
    register and deregister 5 in 72 hours, the code's request and check
    10 an hour (`WA_SERVER_REGISTRATION_*`, `WA_SERVER_VERIFICATION_*`;
    design D66). `Idempotency-Key` on the request of a code, a
    registration and a deregistration, a registration's fingerprint
    never over the PIN (`Fingerprint::register_number`, D67). A
    deregistration takes the number's id again (`confirm`,
    `registration::confirmed`, D68) and is audited
    (`number_deregistered`), as the PIN change is (`two_step_pin_set`;
    the audit `Subject` gained `phone_number_id`). The OpenAPI document
    is additive only (912 lines added, none removed: five paths, the tag
    `registration`, eight schemas). Decisive tests:
    `a_registrations_pin_and_code_are_in_no_store_and_no_answer` (memory:
    every answer, every value written to the `KvStore`, the idempotency
    records and their fingerprints, the outbox),
    `live_postgres_a_registration_leaves_its_pin_in_no_table` (every
    table of the schema, as text and as `bytea` hex) and
    `a_registration_logs_no_pin_no_code_and_none_of_metas_texts` (`TRACE`,
    every target), with M1.7's capture over the new routes; the tenant's
    isolation, the budgets across two replicas, the key's replay with
    another PIN, the malformed bodies and the confirmation in
    `tests/registration.rs`. By mutation: the PIN logged at `TRACE`, the
    PIN in the kept answer, the fingerprint over the PIN, Meta's texts
    logged, `details` answered, the budget's charge removed or moved
    before the confirmation, the confirmation removed, axum's own JSON
    extractor, the deregistration's audit removed: each fails a test.
  - **Decided:** D65–D69, the coordinator's of 2026-11-02 under the
    owner's delegation, each swappable (design §10); the namespace
    `wa.server.registration` and its record wait for the owner, and so
    does a scope of deregistration's own (§ Owner touchpoints).
  - **Review (security lens), in the same PR:** one fix and the tests a
    guard lacked. A counter record of another `v` whose fields parsed
    was read as version 1: now refused (`503`, fail closed;
    `a_record_that_does_not_read_refuses_the_attempt`). Pinned by new
    tests, each by mutation: concurrent charges on one record pass the
    limit exactly (`concurrent_charges_never_pass_the_limit`; a plain
    write, or a plain first write, lets all through), and so on Postgres
    across two replicas
    (`live_postgres_a_numbers_budget_holds_across_racing_replicas`); a
    store whose reads or writes fail refuses
    (`a_store_that_fails_refuses_the_attempt`); a moving 72 hours never
    holds more than 10 accepted attempts
    (`two_back_to_back_windows_hold_at_most_ten`); `Registration::new`'s
    tenant check, which HTTP never reached (the extractor refuses
    first), in `conformance::numbers`
    (`the_registration_services_open_on_the_callers_own_number_only`); the
    region in the fingerprint; an encoded path's confirmation
    (`deregistering_an_encoded_path_takes_the_decoded_id`); a timeout's
    kept key, replayed with another PIN and no attempt spent; Meta's
    refusal of a coexistence number (`502 unknown`, the key freed, no
    texts; `metas_refusal_to_deregister_maps_cleanly`); callers without a
    valid key. The shared flow now carries the PIN in malformed shapes
    (a number, too long, signed, padded, other digits, a body that is not
    JSON) through the store sweep, the `TRACE` capture and the live
    sweep, keys and replays its deregistration (audited once), and the
    metrics hold no secret. Docs: Meta limits registering and
    deregistering to 10 each per moving 72 hours, not 10 shared; the
    library's `PhoneNumber::deregister` said it turned local storage off,
    which Meta's page says it does not. Left: the windows are measured on
    each replica's clock, so a skew between replicas shortens one by the
    skew; the settings accept budgets past Meta's limit without a
    warning; a verification code has no length bound but the body's.
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
  is S15's, generated by CrateStack (D20 (a), decided by the owner on
  2026-11-02: yes; the `api-axum` client of D26's swap stays generated
  from `openapi/v1.json` by openapi-typescript).
  - **After:** M2c and S15 (D20 (a) decided by the owner on 2026-11-02:
    yes).
  - **Before the first release** (P2's security review, finding M2: a
    v1 client generated from the document before P2 types the admin key
    answers' `scopes` as the closed `Scope`, so `payments` falls outside
    them; 4 of the 271 operation checks): `Key.scopes` in responses
    becomes an open `KnownScope | string`, as `ErrorCode` and `EventType`
    already are, while the request's scopes (a key's minting) stay
    closed, so a scope added later is additive.
  - **Decisive:** M4.1–M4.3; the image's contents list matches the
    image (an extra binary fails it).

### M5. Routes for the modules parity requires

One PR per family, each a thin route over an existing library module
and each object id checked to be the path's number's or WABA's own
(architecture.md § Service). For every family: M5.1 and M5.2 over its
routes (M1.3's table covers each new `{pn}` and `{waba_id}` route by
construction; a route whose object id is not checked against the number
or WABA fails the family's own cross-tenant test).

- [x] **M5a. The send union's other types** (rows 22, 29, 37, 51, 53,
  56, 57, 59, 60, 104): pin, request contact info, Direct Send,
  interactive carousels, voice call, location request, address, call
  permission request, Flow, product messages.
  - **After:** S7.
  - **Decisive:** M5.1, M5.2; each type's documented example is sent
    byte for byte; one left out of the send union fails its test.
  - **Landed:** `POST /v1/numbers/{pn}/messages` and the inbox reply take
    `pin` (`meta_whatsapp_server_core::messages::MessageType::Pin`, to a
    group), the `interactive` types `location_request_message`,
    `request_contact_info`, `call_permission_request`, `voice_call`,
    `flow`, `address_message`, `carousel` (media cards or product cards),
    `product`, `product_list` and `catalog_message`
    (`messages::InteractiveType`, fifteen in all), and Direct Send's
    `category`, `ttl_seconds` and `direct_send_config` on any type
    (`messages::DirectSendCategoryObject`, Meta's three categories). Each
    is read into the library's own type (`Pin`, `LocationRequest`,
    `RequestContactInfo`, `CallPermissionRequest`, `VoiceCall`,
    `FlowMessage`, `AddressMessage`, `MediaCarousel`, `ProductCarousel`,
    `SingleProduct`, `ProductList`, `CatalogMessage`, the parameters
    objects as Meta's JSON, as P2 read the payment messages'), then the
    request's object, written back under Meta's names, is compared with
    what the library would send (P2's `request::dropped_key`): a key it
    would drop is `422 invalid_request` on its path, nothing sent; the
    values the library writes itself (an action's `name`, a card's
    `card_index` and `type`, `flow_message_version`) must be its own.
    The types before M5a are built as they were, a key new with M5a
    refused on them. The design's M5.2 for this family: a Flow's
    `flow_id` and a product message's `catalog_id` are checked to be the
    number's WABA's with its token before the send
    (`messages::admit_named_object`: `GET /{flow_id}`, its
    `whatsapp_business_account`; `GET /{waba_id}/product_catalogs`),
    another WABA's or one Meta refuses `404 not_found` with nothing sent,
    a failed check `may_have_been_sent: false`; on an inbox reply after
    the window's check (`Conversations::reply`), so a closed window asks
    Meta nothing. A `flow_token` reaches no log (the request's `Debug`
    shows a parameters object's keys only). The window: every new type is
    free-form, as is Direct Send `service`; `utility` and `authentication`
    are sent by Meta as templates, and the library's `Inbox::send` and
    the reply's cut exempt them. No new route, error code, setting or
    stored name. The OpenAPI document is additive only (596 lines added;
    the 11 removed are descriptions, `interactive.body` leaving
    `required`, a `$ref` widened to an `anyOf` and two enums' last
    lines): `type` gains `pin`, `interactive.type` ten values,
    `action.parameters` four `anyOf` alternatives (`FlowParameters`,
    `VoiceCallParameters`, `AddressParameters`, `CatalogParameters`),
    `action.sections` `ProductSection`, `action` `cards`, `catalog_id`
    and `product_retailer_id`, `SendMessage` and `ReplyMessage` `pin`,
    `category`, `ttl_seconds` and `direct_send_config`; seventeen schemas.
    Tests: the service's `tests/send_union.rs` (each type's example from
    its page sent as printed with the tenant's token, never the
    operator's,
    `each_types_example_is_sent_as_its_page_prints_it_with_the_tenants_token`;
    `a_key_the_library_would_drop_is_refused_on_its_path_before_any_request`;
    `direct_send_is_checked_by_the_librarys_rules_before_any_request`;
    M5.2's `a_flow_or_a_catalog_of_another_waba_is_not_found_and_nothing_is_sent`;
    M5.1's `another_tenants_number_is_not_found_before_the_vault_whatever_the_type`,
    sends and inbox replies; `an_inbox_reply_meets_each_types_window_rule`),
    M1.7's capture with a Flow's token (`tests/logs.rs`, live on Postgres
    `tests/live_logs.rs`), the core's `a_flow_token_never_reaches_debug`
    and the adapter's `a_flow_token_never_reaches_the_requests_debug`. By
    mutation, each failing a test: a type out of the send union (one per
    type), the comparison with the library's message removed (for the
    interactive types, for `pin`), the `card_index` check, the action
    name check, the `flow_message_version` check, the `flow_id` digits
    check, the closed Direct Send category, the named object's check
    removed from the send or from the reply, the WABA comparison of a
    Flow or a catalog, the reply's window check before the named
    object's, the failed check marked sent, and the redacted `Debug`.
    M5a's review added `each_value_the_library_writes_is_checked_and_a_carousels_catalogs_are_one`
    (each type's action `name`, a URL card's `name`, a quick-reply
    button's `type`, a product carousel's second catalog) and
    `an_inbox_replys_cut_exempts_only_what_meta_sends_as_a_template`
    (Direct Send under a migrated number's cut; the cut's exemption
    survived mutation before it), a Flow answered under another id, a
    `utility` Flow outside the window still checked, and the library's
    `a_flow_token_is_redacted_from_debug`.
  - **Decided 2026-11-02** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable, design §10, D100
    to D104): the comparison with the library's message, on M5a's types
    only (D100); Direct Send by the library's rules with Meta's three
    categories (D101); the `flow_id` and `catalog_id` checks before each
    send, no cache (D102); the `flow_token` out of every log, the
    library's `FlowParameters` `Debug` redacted with it (D103); Meta's
    per-user limits left to Meta (D104). No stored name; no legal act
    (no type accepts terms).
  - **Not done here:** Direct Send's interactive authentication message
    (a copy-code OTP button) and mixed URL and reply buttons, which the
    library does not model (its `MessageContent::Raw`, which the service
    refuses); the catalog a template's product cards name (M5b's); a
    group id checked against the number's groups (M5f's); the comparison
    on the types before M5a (D100).
  - **Also, after the first release** (from S7, carried by M2a, P2 and
    now M5a, which landed before it): drop the legacy fingerprint (S6's
    window: `Fingerprint::legacy_v1_json`, `legacy_v1_upload`,
    `or_legacy`, and the handlers' `Uri`), once every deployment has run
    a release with S6 for one `WA_SERVER_IDEMPOTENCY_TTL`.
- [ ] **M5b. Templates** (rows 95, 96, 99, 100, 101, 153): edit,
  library, migrate, compare, unpause, archive and unarchive, template
  groups, each kind of row 99 created through the service (app deep
  links pass through since L24).
  - **After:** S7, L23.
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
- [ ] **M5c3. Number settings** (rows 66, 109, 128, 129–133, 151, 156):
  the settings (payload encryption among them, after L26), the username, the messaging limit tier on
  `GET /v1/numbers/{pn}`, the Official Business Account, compliance
  information, health status, search visibility, notifications, the
  contact book, conversational components.
  - **After:** S7, L9.
  - **Decisive:** M5.1, M5.2; `GET /v1/numbers/{pn}` reports the
    messaging limit tier (dropping it from the fields Meta is asked for
    fails the test).
- [ ] **M5c4. WABAs and accounts** (rows 126, 127, 140, 148, 152).
  Row 152's call goes to `api.facebook.com`, not Graph: the service's
  egress list ([design §6](design/server.md#6-security)) needs
  `api.facebook.com:443`, and an egress rule is per host, so the path
  restriction lives only in the client's credential rules (L10a). The
  devcontainer firewall lacks that host too (left as is: a live call
  from the sandbox fails until it is added).
  - **After:** S7, L10a, L10b, L16b.
  - **Decisive:** M5.1, M5.2.
- [ ] **M5c5. Messaging Accounts in the service** (row 140; design
  D37): a send from a bound number names its binding's WABA id as
  `messaging_account_id`, behind the setting `send_messaging_account_id`
  (on by default), and the guide says how a number shared by several
  Messaging Accounts is held (one binding; the latest same-tenant attach
  is the one billed; another tenant's is refused). Several bindings per
  number wait for the owner's answer to OPEN_QUESTIONS #46.
  - **After:** S7, L16a.
  - **Decisive:** every send route's body carries the binding's id (the
    setting off: none); after a same-tenant attach of a second WABA
    holding the number, the sends name the second; another tenant's
    attach of it stays `409 waba_owned_by_another_tenant`; a send body
    naming `messaging_account_id` itself stays refused.
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
  - **After:** B1, B2, B2b (the replicas' shared budget), B3, B4.
  - **Decisive:** M5.1, M5.2; a broadcast over HTTP never exceeds its
    tenant's configured rate under a fake clock.
- [ ] **M5l. Conversation routing** (row 139): the thread control API.
  - **After:** S7, L15.
  - **Decisive:** M5.1, M5.2.

## 5. Payments (last)

A `payments` client module (`meta-whatsapp-client`; rows 38, 141):
India (UPI, payment links, order details and order status, onboarding)
and Brazil (Pix, Boleto, payment links, one-click), typed, instead of
`MessageContent::Raw`; the payment buttons of templates (`order_details`,
`payment_request`). Too large for one pull request, it lands in three:

- [x] **P1a. India: the order messages, the lookup, refunds and
  onboarding** (`meta-whatsapp-client`; rows 38, 141, partly).
  `client::messages::Interactive::OrderDetails` and `OrderStatus`
  (UPI intent links, payment links, payment gateways and the older
  payment configuration, every documented rule checked first), all
  re-exported from `client::payments`, with `Client::payments` (the
  payment lookup, refunds) and `Client::payment_configurations` (list,
  get, create, regenerate the OAuth link, delete).
  - **After:** nothing.
  - **Kind:** additive.
  - **Decisive:** exact JSON, both ways, from
    `payments/payments-in/upi-intent/dynamic-vpa`,
    `payments-in/payment-links`, `payments-in/pg`,
    `payments-in/upi-intent` and `payments-in/onboarding-apis`; a
    refund, a configuration's creation and an OAuth link are never
    replayed after a timeout.
  - **Landed:** `client::payments` (the messages in
    `client::messages::payments`, re-exported), every example of the
    five pages an exact-JSON test (`payments/tests.rs`), every rule
    the pages state checked first. Not checked locally: the subtotal,
    the expiration's 300-second minimum and the beneficiaries of
    shipped goods (the module docs say why).
  - **Review (money, wire format, secrets):** an `order_status`
    message refuses `pending`, which the `payments-in/pg` parameters
    table excludes, and checks its `reference_id` as an
    `order_details` message's; the items' origin and importer are not
    required with the older `payment_configuration` alone (its page
    makes them optional); a total above `u64::MAX` is refused as such
    (`Order::total` sums in `u128`, so nothing overflows; the message
    no longer says "negative"); a refusal of the two OAuth link
    endpoints that is not Meta's envelope keeps its status, never its
    body, and `PaymentOauthLink`'s `Debug` hides the link's path as
    well as its query (no page says where its credential sits). Tests
    added for what no test caught: every limit exactly reached, in
    two-byte characters (`every_limit_is_inclusive_and_counts_characters`),
    amounts at and beyond `u64::MAX`, the lookup's error example, `.`
    and `..` as lookup names, a refund not replayed on a server error.
    Documented: a replayed deletion can answer an error when the first
    went through, a refund's amount is not compared with what was
    paid, and the onboarding calls are refused with `131005`.
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions): where the pages contradict
    themselves, the examples win (`payment_settings` an array,
    `importer_address` an object, `partially_shipped`, `parameters` an
    object); the subtotal is not checked locally, the total equation
    is; the header image's `provider` is not modelled; the
    `order_details` header is an image only; the answers' fields the
    tables call required but some example leaves out, or another table
    calls optional, are optional (`merchant_category_code`,
    `purpose_code`, a transaction's `pg_transaction_id`);
    `purpose_code` is sent as a string, as both examples do, and a
    configuration's name has at most 60 characters, an order's limit;
    the tests put numeric ids where the examples print placeholder
    names (`the-catalog_id`, `your-media-id`).
- [x] **P1b. Brazil** (`meta-whatsapp-client`; rows 38, 141): `BRL`
  orders (`payment_type` `br`, the `order` object optional, a
  `reference_id` of 60), the payment settings `pix_dynamic_code`,
  `payment_link`, `boleto` and `offsite_card_pay`, and the `payment`
  object of an `order_status` message (whose status may be `pending`
  in Brazil, which India's check refuses), on P1a's types
  (`payments/payments-br/orders`, `offsite-pix`, `payment-links`,
  `boleto`, `one-click-payments`).
  - **After:** P1a.
  - **Kind:** additive.
  - **Decisive:** exact JSON from `payments/payments-br/*`; a `BRL`
    order checked by Brazil's rules, an `INR` one by India's.
  - **Landed:** `Currency::Brl` selects Brazil's rules
    (`OrderDetailsParameters::brl`, `brl_simplified`,
    `OrderStatus::brl` with a `PaymentUpdate`), India's unchanged; the
    settings `PixDynamicCode` (with `PixFlowType`, Pix with redirect),
    `PaymentSetting::boleto`, `OffsiteCardPay` and the `payment_link`'s
    `uri` alone; every message example of the five pages an exact-JSON
    test (`payments/tests/brazil.rs`; the one-click page's
    `payment_method` webhook is P1c's), the decisive pair
    (`a_brl_order_is_checked_by_brazils_rules_and_an_inr_one_by_indias`,
    `an_order_status_is_checked_by_its_currencys_rules`).
  - **Review (fields, limits, mutations):** a `BRL` header by media id
    is refused (Brazil's image object has `link` alone, required; the id
    is India's), and the item image's refusal is pinned by its reason
    (`an_item_image_is_refused_in_a_brl_order_as_indias`: Brazil's
    required `retailer_id` conflicts with an image anyway, so deleting
    the refusal left every test green), as is the simplified order's
    header refusal being Brazil's alone
    (`the_header_rules_are_brazils_only`).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions):
    1. A field documented for one currency only is refused in the
       other's orders (India's `payment_configuration`, beneficiaries,
       header by media id, item images, importers, `quick_pay`, a link's
       success and cancel URLs and settings in `BRL`; Brazil's settings,
       `payment_type` `br` and `payment` update in `INR`).
    2. Brazil's subtotal is checked against `Order::items_sum` (its
       page says the sale price counts, its examples keep the rule);
       India's stays unchecked (P1a).
    3. `payment_settings` are optional in `BRL`: every Brazil table
       says Optional.
    4. A `BRL` `order_status` with neither `order` nor `payment` is
       accepted: no page requires either.
    5. `last_four_digits` must be exactly four ASCII digits.
    6. Pix codes and keys, Boleto digitable lines, a card's last four
       digits and `credential_id` are payment data shown to the buyer
       or echoed to the merchant, not secrets: no secret type, and
       `Debug` shows them. Meta's page calls `credential_id` "the ID of
       the credential" the merchant looks the card up by; the module
       docs (not Meta's) say never to put a card number or PSP token
       there.
    7. The `header` Brazil's `order_status` table lists, with no fields
       and no example, is not modelled (a gap).
- [x] **P1c. The payment template buttons and the payment webhooks**
  (`meta-whatsapp-client`, `meta-whatsapp-webhooks`; rows 38, 141): a
  template's `order_details` button and `order_status` component
  (`payments-in/orderdetailstemplate`, `orderstatustemplate`,
  `payments-br/orderdetailstemplate`), its `payment_request` button
  (`payments-br/payment-request-cta`), the creation of these templates
  (which the service refuses until then), and the status of type
  `payment` (`payments-in/pg`) and the `payment_method` reply
  (`payments-br/one-click-payments`) typed.
  - **After:** P1a, P1b.
  - **Kind:** additive.
  - **Decisive:** the payments pages' webhook examples join the
    conformance sweep (their manifest status moves from `OutOfScope` to
    `Typed`), with new event data snapshots only (no changed one); exact
    JSON of the template pages' examples.
  - **Landed:** webhooks: a status's `type` and `payment`
    (`webhooks::fields::Status::payment`, `StatusPayment`: the
    transaction, its method and error, the refunds, the gateways' echoed
    fields), `MessageStatus::Captured` and `Pending`, and
    `InteractiveReply::PaymentMethod` (`PaymentMethodReply`); both pages
    `Typed` in the conformance manifest (the `OutOfScope` status, with no
    page left, is gone), two fixtures, two new event data snapshots, no
    existing one changed, no new event kind. Client: the
    `ORDER_DETAILS` and `PAYMENT_REQUEST` buttons of a definition
    (`client::templates::Button::OrderDetails`, `Button::PaymentRequest`),
    `TemplateDefinition::sub_category` and `display_format`, and the send
    side (`TemplateMessage::order_details_button`,
    `payment_request_button`, `order_status`;
    `client::payments::PaymentRequest`, `PaymentRequestSetting`,
    `PaymentRequestCard`, `TemplateOrderStatus`), the order checked by
    its currency's rules (P1a's and P1b's, with the template's field
    paths), every example of the four pages an exact-JSON test both ways
    (`payments/tests/templates.rs`). The order types now also
    deserialize (the service reads a send through them). The service's
    send refusal is lifted here, not in M5b: it read sends through the
    library type, so typing them lifts it; creation was never refused
    (the buttons parsed as `Button::Other`), and is now checked
    (`request::tests::the_payment_templates_pass_with_no_key_dropped`,
    the service's `payment_templates_are_sent_as_metas_objects_and_checked`).
    Not done here: India's checkout button templates
    (`payments-in/checkout-button-templates`, their checkout endpoint and
    the configuration's data endpoint) and the alpha third-party
    validation, P1d's (row 141 stays partial in the library).
  - **Review (fields, parsing, the send path, mutations):** a payment
    object in a catch-all was sent unchecked: a component `type` spelled
    `Button` or `Order_Status`, an `index` that is not a number, a
    parameter `type` `ACTION` or `ORDER_STATUS` land in
    `SendComponent::Other` or `Parameter::Other`, which reach Meta as
    written, so an order in `USD` (or a `pending` order status) skipped
    every rule, through the service too. One holding `order_details`,
    `payment_request` or `order_status` at any depth is now refused, as
    is a definition's `ORDER_DETAILS` or `PAYMENT_REQUEST` button that
    did not parse typed
    (`a_payment_object_in_a_catch_all_is_refused`; the service's
    `payment_templates_are_sent_as_metas_objects_and_checked`). And a
    payment webhook that failed its typed parse turned an event tenants
    received before P1c (`status_updated`, `message_received`) into an
    operator-only `unknown`: the `payment`, and its amount, method and
    error, are `None` when they do not parse, the reply an unknown reply,
    `refunds` may be `null` and `notes` `[]`
    (`a_payment_that_does_not_parse_typed_never_fails_its_change`).
    Three guards survived their deletion and are now pinned
    (`a_payment_button_carries_its_own_object_in_a_known_currency`): a
    payment button's `action` without its own object, and `Currency`
    refusing an unknown code (the catch-all tests' `USD` order lacked
    required fields, so it failed to parse whatever its currency).
  - **Decided 2026-11-01** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions):
    1. No new event kind. India's payment status stays a
       `status_updated` event (tenants of the service receive it so
       today, as a status `captured` or `pending`: a new kind, which
       would start operator-only, would take it from them), with `type`
       and `payment` added (`Status::is_payment` tells it from a
       message's); the `payment_method` reply stays a `message_received`
       event. Both already were tenant types.
    2. `MessageStatus` gains `Captured` and `Pending`, the payment
       statuses; the inbox ignores them (no delivery status maps), the
       bot's 131049 feedback needs an error code they never carry.
    3. The webhook types require only the identifiers (`reference_id`,
       a transaction's and a refund's `id`, the reply's
       `payment_method`); Razorpay's `notes` keep a non-string value as
       its JSON text, as `open_enum!` does. Tightened by the review: no
       payment object can fail the change it arrives in (a `payment`, an
       amount, a method or an error that does not parse typed is `None`,
       a reply that does not is `InteractiveReply::Unknown`), so a new
       field or value never costs the event.
    4. An order details template is `UTILITY` or `MARKETING` with a
       `TEXT`, `IMAGE` or `DOCUMENT` header, whatever its currency
       (Brazil's page says "must"; India's is silent and its examples
       comply); its label is not checked (India fixes `Review and Pay`,
       Brazil's example prints a placeholder).
    5. A payment request button: the label its method requires; at most
       three, each a different method, and then three buttons in all; a
       sample value, no card at creation, the card at send
       (`credential_id` optional, as this page says); `currency` and
       `total_amount` required for a card, refused for Boleto and links,
       and together or not at all for Pix ("used with").
    6. An order status template is checked by India's rules (a
       `reference_id` of 35, no `pending`): India alone documents it.
    7. A payment button or order status component whose parameter does
       not parse typed (a currency or payment method the client has no
       rules for, a required field missing) is refused by `validate`,
       not sent unchecked as the catch-all; so is a payment object in
       any catch-all component or parameter, and, at creation, a payment
       button that did not parse typed (the review's extension).
    8. The button `index` stays a string, as every template button's,
       though the order details pages print a number.
- [x] **P1d. India's checkout button templates** (`meta-whatsapp-client`;
  row 141): the checkout button templates
  (`payments/payments-in/checkout-button-templates`: one `order_details`
  button labelled `Buy now` per product, in a single product template or
  a carousel, each sent with its order), the checkout endpoint they call
  (its `get_coupons`, `apply_coupon`, `remove_coupon` and
  `apply_shipping` sub-actions, encrypted as a Flows endpoint is), the
  link of that endpoint to a payment configuration
  (`payments/payments-in/onboarding-apis`, "Link or update data
  endpoint"), and the alpha third-party validation
  (`payments/payments-in/pg`, "Third party validation": a Razorpay or
  `PayU` setting's `encrypted_payment_gateway_data`).
  - **After:** P1c.
  - **Kind:** additive.
  - **Decisive:** exact JSON of the checkout button page's creation and
    send examples; each endpoint sample request decrypted and answered
    with its sample response through the Flows endpoint's crypto; the
    two third-party validation payloads of `pg` as exact JSON.
  - **Landed:** the checkout button (`client::templates::Button::checkout`,
    `Button::CHECKOUT_LABEL`: an `ORDER_DETAILS` button labelled `Buy now`
    is checked as one: a `MARKETING` template with an `IMAGE` or `VIDEO`
    header, one checkout button and up to 9 quick replies, or the first
    button of each media card), sent with
    `TemplateMessage::order_details_button` and an order that may offer
    `client::payments::ShippingInfo` (INR, physical goods; refused in an
    `order_details` message), each order details button of a message
    with its own `reference_id`; the checkout endpoint
    (`client::payments::checkout`, feature `flows-endpoint`:
    `FlowEndpointKey::decrypt_checkout_request` on the Flows endpoint's
    `decrypt_request`, made generic inside the crate, `CheckoutRequest`,
    `CheckoutSubAction`, `CheckoutResponse` checked by `validate` before
    `ResponseSealer::seal`: India's rules, no payment settings, the total
    less the `AppliedCoupon`'s discount, a coupon after `apply_coupon` and
    none after `remove_coupon`); the link
    (`PaymentConfigurations::link_data_endpoint`: an `https` URL,
    replayed, its answer undocumented); the third party validation
    (`PaymentSetting::third_party_validation`, `TpvGateway`,
    `EncryptedPaymentGatewayData`, redacted from `Debug`). An
    expiration's description is now optional
    (`OrderExpiration::description` an `Option`, `OrderExpiration::at`),
    still required in an `order_details` message. Every example of the
    three pages an exact-JSON test (`payments/tests/checkout.rs`, the
    endpoint's in `payments/tests/checkout/endpoint.rs`: each sample
    request encrypted as the WhatsApp client does, decrypted, answered
    with the typed sample response, sealed and opened again to the page's
    JSON). Not wrapped, being undocumented: unlinking the data endpoint,
    a shipping address's floor number (its key is never shown), and a
    health check or error answer at the checkout endpoint (a request
    without a sub-action parses, to be answered as a Flow endpoint's).
  - **Review (crypto, secrets, money, the URL, mutations):** the
    checkout endpoint is the Flows endpoint's crypto, not a copy
    (`decrypt_request` and `decrypt_checkout_request` both call
    `decrypt_as`, whose behaviour and error are `decrypt_request`'s
    before: one `CryptoError::Decrypt` for a payload that does not
    decrypt and one that does not parse, so no answer tells them
    apart); the AES and private keys reach no `Debug` or error, nor
    does `EncryptedPaymentGatewayData`, through a setting, an order, a
    template message or a refusal. `link_data_endpoint` accepted what
    the WHATWG parser repairs and sent it unrepaired (`https:host`,
    `https:/host`, `https:///host`, backslashes, an empty `@`): it now
    requires `https://` then a host, without `@` before the path or a
    backslash (`link_data_endpoint_checks_first_and_reads_a_refusal`);
    hosts stay unjudged, private addresses and IDNs included, since
    only the deployment knows
    (`link_data_endpoint_takes_any_https_host_as_given`). Nine guards
    survived their deletion and are pinned: a checkout answer's
    `payment_configuration`, Brazil's `payment_type` and India's
    beneficiaries, the coupon's code, the shipping country and the
    version (`a_checkout_answer_names_no_way_to_pay_and_is_whole`),
    `shipping_info` in a BRL order
    (`shipping_info_is_a_template_buttons_and_physical_goods_only`),
    `expected_total` for a coupon worth more than the order
    (`a_coupon_never_takes_the_total_below_zero`), and the exact label
    (`a_checkout_template_is_marketing_with_an_image_or_video_header`,
    `a_carousel_card_starts_with_its_checkout_button`). A checkout
    button's own category check could never fire (the single product
    rule and the carousel rule refuse first) and is gone. The carousel
    rules hold on Meta's media card example with a button replaced
    (`a_checkout_carousel_is_metas_media_card_example_with_a_button_replaced`).
  - **Decided 2026-11-02** (the coordinator, under the owner's
    delegation, AGENTS.md § Decisions; each swappable):
    1. The exact label `Buy now` (`Button::CHECKOUT_LABEL`, the case and
       spacing the page prints, "non-customizable") marks a checkout
       button; any other label, `buy now` or `Buy now ` among them, keeps
       P1c's order details rules. Side effect: a Brazil order details
       template labelled `Buy now` (Brazil's example prints a
       placeholder) is checked as a checkout button, so it must be
       `MARKETING` with an `IMAGE` or `VIDEO` header. Swap: an explicit
       marker (a `Button::Checkout` variant, or a flag on
       `Button::OrderDetails`) read by `templates::validate::is_checkout`
       and built by `Button::checkout`, in place of the label.
    2. An expiration's description is optional
       (`OrderExpiration::description` an `Option`, `OrderExpiration::at`)
       except in an `order_details` message, where `payments-in/pg` says
       the business "must define" it. Swap: the `(None,
       OrderContext::Message)` arm of `Order::validate`, one arm per
       context.
    3. `shipping_info` is accepted in a template button's INR order of
       physical goods and in the checkout endpoint's exchange, refused in
       an `order_details` message, in BRL and for digital goods; its
       absence for physical goods is not refused (a send cannot tell a
       checkout button template from an order details template); its
       `selected_address` only in a checkout answer; every address limit
       of the page; the floor number, whose key no example shows, is not
       modelled. Swap: `OrderDetailsParameters::validate_checkout_parts`
       and `ShippingInfo::validate`, by `OrderContext`.
    4. `coupon` (`AppliedCoupon`) only in a checkout answer, refused in a
       send; its discount comes off `subtotal + tax + shipping -
       discount` (`OrderDetailsParameters::expected_total`: the page's
       examples, no sentence). Swap: `expected_total` and the coupon arm
       of `validate_checkout_parts`.
    5. A checkout answer refuses payment settings (`payment_settings`,
       the older `payment_configuration`, Brazil's `payment_type`),
       requires INR, and `CheckoutResponse::validate` refuses a
       sub-action this client does not know (`CheckoutSubAction::Other`)
       and data its sub-action does not take. Swap:
       `CheckoutResponse::validate` and
       `OrderDetailsParameters::validate_checkout_payment`; an answer to
       a new sub-action can be sealed without `validate` meanwhile.
    6. `CheckoutRequest::sub_action` and `data` are `Option` (a request
       without them, a health check should one come, parses); a request
       that decrypts but does not parse typed is `CryptoError::Decrypt`
       (HTTP 421), the same error as one that does not decrypt, so no
       answer tells the two apart. Swap: the crate-internal
       `FlowEndpointKey::decrypt_as` made public (or a
       `decrypt_as::<serde_json::Value>` entry), for a caller that wants
       the untyped payload.
    7. Signatures: the page does not say whether a checkout request
       carries `X-Hub-Signature-256`; the module docs say to verify it
       when present (`flows::endpoint::verify_request_signature`) and
       never to price from the request's amounts, since anyone can
       encrypt to a public key. Nothing enforces it. Swap: a caller-side
       check; a signature required by `decrypt_checkout_request` would
       be a new entry point beside it.
    8. `link_data_endpoint` is replayed after a timeout (it sets a
       value), sends `data_endpoint_url` (the example's spelling, not the
       table's `data-endpoint-url`), and takes any 2xx JSON object but
       `"success": false` as done. Swap: `.idempotent(true)` and the
       `Body` field in `PaymentConfigurations::link_data_endpoint`.
    9. Third party validation is `{"type": <gateway>, <gateway>:
       {"encrypted_payment_gateway_data": …}}`, `TpvGateway` the page's
       two (Razorpay, `PayU`; `#[non_exhaustive]`), INR only, with no
       `payment_gateway` setting required beside it, the encryption the
       caller's. Swap: `PaymentSetting`'s serde impls and
       `only_currency`; a new gateway is a `TpvGateway` variant.
    10. `reference_id`s are unique across every order details button of
        one message, carousel cards included (the page says each
        carousel button; a single product template has one). Swap:
        `templates::send::unique_reference_ids`.
- [x] **P2. Payment routes in the service** (rows 38, 141).
  - **After:** P1a, P1b, P1c.
  - **Decisive:** M1.3 over the new routes; an order-details send
    carries the tenant's token.
  - **Found already working before P2:** the payment templates' sends
    and creations (P1c: `type` `template`, read through the library's
    types); the payment webhooks, routed to the tenant like every event
    (`payment_configuration_updated`, already a tenant type, by WABA;
    India's payment status in `status_updated` and Brazil's
    `payment_method` reply in `message_received`, by number). Missing:
    the `order_details` and `order_status` interactive messages (`422
    unsupported_message_type`: design §4.3 listed `button`, `list`,
    `cta_url` only) and every payment call (configurations, lookup,
    refunds).
  - **Landed:** the send union's `interactive` gains `order_details` and
    `order_status` (`meta_whatsapp_server_core::messages::InteractiveType`,
    `ActionParameters::Order`: Meta's `action.parameters` read into the
    library's `OrderDetailsParameters`, or the order status's
    `reference_id`, `order` and `payment`, with no key dropped, then the
    library's checks; an `order_details` header an image only; an
    `order_status` checked by Brazil's rules, its wire naming no
    currency), for sends and inbox replies alike. Seven routes in
    `meta-whatsapp-server-api-axum` (`api::payments`, tag and scope
    `payments`, a new scope), each a core service
    (`meta_whatsapp_server_core::payments`: `PaymentConfigurations` on an
    `OwnedWaba`, `Payments` on an `OwnedNumber`, each opened only for the
    admitted caller's own tenant, with the binding's vault token):
    `GET`, `POST /v1/wabas/{waba_id}/payment-configurations`
    (`list_payment_configurations`, read class;
    `create_payment_configuration`, send class, an `Idempotency-Key` over
    `Fingerprint::create_payment_configuration`), `GET`, `DELETE
    …/{configuration_name}` (`get_payment_configuration`, read: `404`
    for Meta's empty `data` or its refusal of the name;
    `delete_payment_configuration`, send), `POST
    …/{configuration_name}/oauth-link` (`regenerate_payment_oauth_link`,
    send, a key over `Fingerprint::regenerate_payment_oauth_link`), `GET
    /v1/numbers/{pn}/payments/{configuration_name}/{reference_id}`
    (`get_payment`, read) and `POST /v1/numbers/{pn}/payments/refunds`
    (`refund_payment`, send, a key over `Fingerprint::refund_payment`).
    A lookup and a refund look the configuration they name up in the
    number's own WABA first (`GET /{waba_id}/payment_configuration/{name}`):
    one the WABA has none of is `404`, nothing read or refunded. The
    OAuth link is answered and never kept: the idempotency engine keeps a
    second body when the handler gives one (`idempotency::Answer::keeping`,
    the HTTP adapter's `Success::json_keeping`), the link's answer
    without it. A refund, a creation, a removal and a new link are tenant
    audit events (`payment_refunded`, `payment_configuration_created`,
    `payment_configuration_deleted`, `payment_oauth_link_regenerated`).
    Meta's `details` are kept on every payment route (the terms of
    service's link is there); no route accepts Meta's terms. No new error
    code; the OpenAPI document is additive (five paths, the tag
    `payments`, eighteen schemas; `interactive.type` and `Scope` gain
    values, `action.parameters` an `anyOf` alternative). Tests: the
    service's `tests/payments.rs` (every route's exact request and its
    page's sample response, the decisive token case
    `an_order_details_send_carries_the_tenants_token_never_the_operators`,
    the configuration check
    `another_wabas_configuration_is_not_found_and_nothing_is_read_or_refunded`,
    the refund's key `a_refund_is_never_made_twice`, the link never kept
    `a_created_configurations_oauth_link_is_answered_never_kept`, the
    scope, the terms' refusal), the M1.3 table pinning the seven routes
    (`tests/authz.rs`, and live on Postgres
    `live_postgres_another_tenants_number_is_not_found_before_the_vault`
    with four of them, and
    `live_postgres_a_payments_key_reaches_its_wabas_configurations`),
    M1.5's sentinel over them (`tests/errors.rs`), M1.7's capture (every
    payment route exercised, the OAuth links among the secrets, the audit
    events in order), the core's
    `payment_interactives_are_read_whole_and_checked`,
    `a_configuration_is_checked_before_any_request`,
    `a_refund_is_checked_before_any_request`, the conformance case
    `templates::the_payment_services_open_for_their_own_tenant_only` (in
    `conformance::run`: the core's memory backend, the memory crate's,
    live on Postgres), and the classes' and scopes' tables. By mutation,
    each failing its test: the configuration check dropped from the
    lookup or the refund; the link kept for a repeat (in the handler, or
    the engine keeping the answered body); the refund outside its key;
    the send through the authorizer's tokenless client instead of the
    number's; `order_details` or `order_status` out of the send union;
    the payment routes' `details` dropped; a payment message's button
    keys, or its parameters' dropped-key check, skipped; the provider
    list unchecked; the payment group behind the `send` scope, or a
    payment operation's scope moved; the tenant check of
    `PaymentConfigurations::new` or `Payments::new` dropped; and step 4
    of `owned_waba` dropped (the M1.3 table, the payment routes
    included).
  - **Decided** (the coordinator's decisions of 2026-11-01 under the
    owner's delegation, each swappable: design §10, D50 to D58): a
    `payments` scope (D50; its stored name pending the owner's
    confirmation); the configuration check before a lookup or a refund
    (D51); the OAuth link never kept (D52); an `order_status` checked by
    Brazil's rules (D53); times as RFC 3339 strings under Meta's names
    (D54); providers among Meta's four (D55); the refund's body (D56);
    Meta's `details` kept (D57); the four audit events (D58).
  - **Review** (adversarial, security lens): a configuration name holding
    `/` or `\` is refused (Meta could read the library's `%2F` as
    several segments); the check trusts only an answer of the very name
    asked; a check that fails was not sent (`may_have_been_sent: false`,
    the refund's key released, where it was kept as a refund that may
    have been made); the payment changes' audit events name what
    reconciles them, and a change cut by a timeout is audited, its
    outcome `unknown`. New decisive tests:
    `a_configuration_meta_answers_under_another_name_is_not_found`,
    `a_refunds_amount_is_checked_before_any_request`,
    `a_refund_whose_check_failed_was_not_sent_and_frees_its_key`,
    `an_inbox_reply_carries_a_payment_message_checked_and_within_its_window`,
    and M1.7's capture (`check_audit`).
  - **Follow-ups decided on 2026-11-02** (the coordinator, the owner's
    delegation): a refund requires an `Idempotency-Key` (D56 amended:
    `428 idempotency_key_required`, a new code, additive; decisive
    `a_refund_without_an_idempotency_key_sends_nothing`); the OAuth
    link's answers carry `Cache-Control: no-store`; the open `Scope` of
    the answers waits for the first release (M4).
  - **Not done here:** the data endpoint link of a configuration and
    India's checkout button templates (the library's P1d first); the
    alpha third-party validation (P1d).
  - **Also, after the first release** (from S7, carried by M2a and now
    by P2, which landed before it): drop the legacy fingerprint (S6's
    window: `Fingerprint::legacy_v1_json`, `legacy_v1_upload`,
    `or_legacy`, and the handlers' `Uri`), once every deployment has run
    a release with S6 for one `WA_SERVER_IDEMPOTENCY_TTL`. The next item
    of the service's to land after the first release carries this line
    if it lands before it.
- [ ] **P2b. The checkout endpoint's link in the service** (row 141;
  added by P1d's review, kept by the coordinator's decision of
  2026-11-02).
  - **After:** P1d, P2.
  - **Kind:** additive.
  - **Decisive:** M1.3 over the new route; the link
    (`PaymentConfigurations::link_data_endpoint`) carries the tenant's
    token and Meta's body, `{"data_endpoint_url": …}`, exactly; a URL
    the library refuses is `422` before any request.
  - **Why:** with P1d merged, the service sends checkout button
    templates and third party validation settings through
    `POST /v1/numbers/{pn}/messages`, read through the library's types
    (`checkout_templates_and_third_party_validation_are_sent_as_their_pages_print_them`
    in `tests/payments.rs`), but has no route for the data endpoint's
    link. Whether the service also hosts a tenant's checkout endpoint
    (its private key in the vault, its pricing the tenant's) is this
    item's design question, not decided here.

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

- **D20 (a)**, the BlueOak-1.0.0 licence of `minicbor`: **decided by
  the owner on 2026-11-02: yes**, for `minicbor` and `minicbor-serde`
  only. S10 added the scoped exception to `deny.toml` when CrateStack
  entered the workspace; nothing waits on the owner for it any more.
- **The merges of U1–U3** in the owner's cratestack repository. S16's
  default flip waits on U1.
- **Publishing**: to crates.io (parity row 115) and to npm (D11, M4).
  Crate and package names and versions are permanent, and the tokens
  are the owner's.
- **D21, D22 and D24**, confirmed before the first release: after it,
  D21's per-tenant sequences are a released contract, and D22's and
  D24's deletions cannot be undone.
- **L5's erasure and its stored names**, confirmed before the first
  release: D30's scope and D31's default for a person's group messages
  (redacted, keeping their ids) decide what an erasure deletes for good
  and what it leaves, D35 what a retention keeps, and the stable
  identifiers L5 adds (D33's lock SQL, `wa_identity_links`,
  `wa_messages.sender` and its rule, the `erased` kind, migration 4's
  names: architecture.md § Stable identifiers) become permanent with
  the release. Each ships swappable today; changing one after the
  release is a data migration.
- **New stable identifiers**, confirmed before the first release: each
  store namespace or stored name an item adds after the table in
  architecture.md § Stable identifiers was last confirmed (P2's key
  scope `payments`, stored in `wa_server_api_keys.scopes`, added and
  awaiting confirmation; B2b's
  `wa.bot.pacer` and its record layout, added and awaiting
  confirmation; B3's `wa.bot.jobs`, `wa.bot.jobs.due`,
  `wa.bot.jobs.sent`, `wa.bot.jobs.message` and `wa.bot.jobs.limited`,
  with their keys' derivations and their records' shapes, added and
  awaiting confirmation before the first release; M2h's Postgres
  migration 5, "number moves": the columns
  `wa_server_numbers.attached_at` (a row from before it read as its
  WABA's start) and `wa_server_numbers.previous_tenant_released_at` with
  its default, the Unix epoch read as "a move before the records began"
  (`UNRECORDED_MOVE`, failing closed), and the table
  `wa_server_number_tombstones` with its columns and its `ON DELETE SET
  NULL` rule, and the trigger `wa_server_numbers_released` with its
  function `wa_server_number_released()` (added by M2a's review), added
  and awaiting confirmation; S2b's Postgres
  migration 4, "waba moves": the columns `wa_server_wabas.generation` and
  `wa_server_wabas.previous_tenant_unbound_at` with its default, the Unix
  epoch read as "a move before the records began" (`UNRECORDED_MOVE`),
  and the table `wa_server_waba_tombstones` with its columns and its
  `ON DELETE SET NULL` rule, added and awaiting confirmation; L11d's
  `wa.es.coexistence` and its record layout, added and awaiting
  confirmation; L8's SQLite migration 1, its tables and columns, and its
  Redis conversation layout, added and awaiting confirmation; and L21a's
  `wa.webhook.deadletter` and `wa.webhook.deadletter.contact`, with their
  keys' derivations and their records' shapes, added and awaiting
  confirmation before the first release; M2e's Postgres migration 7,
  "received media": the column `wa_server_events.media_id` (no default:
  `NULL` for the rows before it, and for those a previous release's
  replica inserts) and its partial index `wa_server_events_media_idx`,
  added and awaiting confirmation; M3d's `wa.server.registration`
  namespace, its keys `<budget>:<phone_number_id>` and its record
  `{"v":1,"since","count"}`, ephemeral, added and awaiting confirmation;
  M2b's Postgres channel
  `wa_server_events` and its payload `<sequence> <tenant>`, not stored
  but what replicas of two releases must agree on, added and, the
  coordinator having decided them as built (D40), awaiting confirmation
  before the first release; M3c's tenant OTP settings, the
  key `otp` of `wa_server_tenants.settings` and its version 1 layout,
  and the label `meta-whatsapp-server/otp-fingerprint/v1` the issue's
  idempotency fingerprint is keyed with, added and awaiting
  confirmation; M3c's review's shared issue log, the namespace
  `wa_server.otp.shared_rate`, its key's derivation and record, and the
  label `meta-whatsapp-server/otp-shared-limit/v1`, added and awaiting
  confirmation; M3a's migration 6, `signup`, its tables
  `wa_server_signup_attempts` and `wa_server_vault_records`, their
  columns and the kinds `waba` and `business`, added and awaiting
  confirmation; M2d's number events, their types `number_connected`,
  `number_disconnected` and `number_reconnect_required`, the `reason`
  values `tenant_disconnected`, `operator_unbound`,
  `partner_app_uninstalled` and `account_deleted`, and their outbox
  key's label `meta-whatsapp-server/number-event/v1` with its layout
  (design D80, D83, decided by the coordinator), added and awaiting
  confirmation; M2f's migration 8, `erasure`: the column
  `wa_server_events.contacts` and its partial GIN index
  `wa_server_events_contacts_idx`, the key scope `erasure`, the erasure
  journal's target `erasure_journal`, its layout, its HMAC's input
  `<phone_number_id>|<contact>`, the `era_` id and the audit action
  `customer_erased`, added and awaiting confirmation;
  M2c's migration 9, `webhooks out` (numbered after M2f's
  8), its tables `wa_server_webhook_endpoints` and
  `wa_server_webhook_deliveries`, their columns, indexes and stored
  values, the sealed secret's format and its associated data
  `wa-server/webhook-secret/v1`, the ids `whe_` and `whd_`, the ping's
  body and `WA_SERVER_DATA_KEY_ID`'s default (design D105, D110, decided by
  the coordinator,
  architecture.md § Stable identifiers), added and awaiting
  confirmation; S9's MongoDB outbox
  collections `wa_server_events`, `wa_server_event_streams` and
  `wa_server_wabas`, their fields and their index names, added by a spike
  composed into no binary and awaiting confirmation, which S18 may still
  change before then; S10's binary feature `api-cratestack`, the crate
  `meta-whatsapp-server-api-cratestack` and its procedure's wire name and
  fields (`POST /$procs/version`: `server`, `meta_whatsapp_rs_revision`,
  `graph_api_version`, `api_version`), added and served by nothing yet,
  awaiting confirmation), for the same reason as L5's.
- **How far an erasure reaches in the service** (M2f, design D89 and
  D90): the coordinator decided the narrow default, the calling tenant's
  own data only. Two wider reaches are irreversible deletions beyond the
  caller's own data, so whether to add either is the owner's open
  choice: (a) the number's operator-only outbox rows naming the person
  (today left to the outbox's retention, 7 days by default); (b) on a
  number moved from another tenant, the previous holder's hidden inbox
  records of the person (today such a number is refused, `409
  erasure_crosses_binding`, and the operator erases there through the
  library). Either could ship swappable, as a deployment setting off by
  default or as an operator-only route over the same `Erasures`; and
  erasing from the binding epoch on, without (b), needs a time-bounded
  erasure in the library first (a new `ConversationStore` method, every
  adapter's).
- **CC0-1.0 for the MongoDB driver** (S9, then S17 and S18): the
  `mongodb` crate reaches `tiny-keccak` (CC0-1.0, a public-domain
  dedication, compiled into a proc-macro that runs at build time)
  through `macro_magic` and `const-random`. **Decided by the owner on
  2026-11-02: yes**, CC0-1.0 allowed in `deny.toml`'s `allow` list (S9
  adds it, with the decision's date beside it), so `just deny` passes
  with the driver a normal dependency.
- **A scope of its own for deregistering a number** (M3d, design D68):
  the coordinator decided the scope `numbers`, a confirmation in the body
  and an audit event; a separate scope would be a new stored name (keys
  keep their scopes), so whether to add one is the owner's.
- **Zero-tap authentication templates** (M3c, M3g): creating one sets
  Meta's `zero_tap_terms_accepted`, an acknowledgement of the WhatsApp
  Business terms for zero-tap and of the customers' expectation of an
  automatic fill. M3c refuses it on the service's authentication route;
  whether a tenant may accept them through the service, as an explicit
  and audited action off by default, is the owner's (AGENTS.md §
  Decisions, legal acts). The generic `POST /v1/wabas/{waba_id}/templates`
  passed Meta's JSON through, a zero-tap button with the field included,
  from M1b; M3c's review closed it (`422` on
  `zero_tap_terms_accepted`), so neither route accepts the terms until
  the owner decides.
- **OPEN_QUESTIONS #46**, a number under several Messaging Accounts
  (Meta's account model evolution): keying the service's numbers by
  `(waba_id, phone_number_id)` is a primary-key migration of stored
  data. Until the owner answers, one binding holds a number (design
  D37, M5c5). Asked with the release questions, or when Meta publishes
  phase 2's webhooks (L16b), whichever comes first.
- **The token vault's stored format**, if S2's last attach window is to
  close: a vault record naming the binding it was stored under would let
  a take-back tell its own token from another tenant's stored in the
  microseconds between an attach's binding check and its store
  (design §8.1, S2's "Left"). The record's format is stored data under
  `wa-rs/token-vault/v1`, so changing it is a data migration: the
  owner's, and asked with the release questions, not before.
