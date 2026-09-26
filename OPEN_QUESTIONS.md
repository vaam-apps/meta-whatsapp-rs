# Open questions

Decisions the implementation did **not** make when it was written. Each
entry says what the code does today, so nothing here is a hidden default.

**From 2026-09-26 the owner delegates these decisions**
([design §10](docs/design/server.md#10-decisions-for-the-owner)): an
entry is decided when its choice can be made swappable, and the decision
is recorded in the entry, with how to swap it and, when it needs code,
its item in [docs/roadmap.md](docs/roadmap.md). Legal and
terms-of-service questions, and those whose answer would be an
irreversible data migration, stay open for the owner. A decided entry
stays here, marked, until its roadmap item lands and nothing cites it;
then it is deleted.

On 2026-09-26, 29 entries are decided and 2 stay open: #26 (a legal
decision) and #33 (a data migration).

Numbers are permanent (the code, docs and skills cite them) and not in
order within a section. Markdown renders a list with its first item's
number and counts up from there, so an entry out of sequence starts a list
of its own, after an HTML comment; keep that when you add one.

## Naming and publishing

2. **`meta_whatsapp_rs::client` is both a module and a function** (the
   production client shortcut). Rustdoc links need `mod@`/`fn@`.
   Alternative: `meta_whatsapp_rs::connect(token)`.

   **Decided 2026-09-26 (coordinator, owner's delegation):** keep
   `meta_whatsapp_rs::client` as both the module and the shortcut; rustdoc
   links keep `mod@` and `fn@`. The safest choice: nothing breaks.
   Swappable by adding `connect(token)` later as an alias, which is
   additive. No code.

## Embedded Signup (onboarding merchants)

4. **Two-step PIN policy.** The caller supplies the 6-digit PIN on every
   `onboard`/`resume`; nothing generates or stores PINs. Numbers that already
   have a PIN need the merchant's current one.

   **Decided 2026-09-26 (coordinator, owner's delegation):** the caller
   supplies the PIN on every attempt; neither the library nor the service
   generates or stores one (design D6): one leak must not expose every
   number's PIN. Swappable by the integrator, who may keep PINs in its own
   secret store and pass them in. No code.

5. **Multi-WABA signups.** Only the claimed `waba_id` (else the first of
   `waba_ids`; with no claim, the newest granted WABA) is onboarded.

   **Decided 2026-09-26 (coordinator, owner's delegation):** one WABA per
   signup stays the default (the claimed one, which the merchant chose);
   onboarding every WABA the token grants becomes an opt-in option of the
   onboarding request, each WABA onboarded, bound and resumable on its
   own. Swappable by that option. Roadmap L11.

6. **One WABA shared by several tenants.** The vault is keyed by WABA and
   knows no tenants; the last onboarding wins. `onboard_with_approval`
   lets an integrator refuse (or apply any other policy) before anything
   is stored; which policy meta-whatsapp-rs itself should default to is open. A
   Solution Partner deployment must approve (plain `onboard` is refused,
   and `resume` shares only for a WABA whose stored token record has a
   recorded approval; the owner decided on 2026-09-25 that it stays
   required), but what the approval checks is still the integrator's:
   meta-whatsapp-rs decides no tenant policy.

   **Decided 2026-09-26 (coordinator, owner's delegation):**
   meta-whatsapp-rs keeps no tenant policy of its own.
   `onboard_with_approval` stays the hook, and the reference policy is the
   service's (design D4: refuse a WABA another tenant holds, with an admin
   unbind). Swappable by the approval the integrator passes. No code.

7. **Coexistence sync.** Contacts/history sync (`smb_app_data`) must happen
   once, within 24 h of onboarding. `onboard` only flags it
   (`needs_coexistence_sync()`); should it trigger it?

   **Decided 2026-09-26 (coordinator, owner's delegation):** the sync runs
   automatically by default: onboarding gains a coexistence-sync step,
   right after a coexistence onboarding, that an option turns off (the
   service does the same, design D7). The safest choice: Meta allows the
   sync once, within 24 hours, and only offboarding and a new signup
   recover a missed one. Swappable by that option; until it lands,
   `needs_coexistence_sync()` stays the signal. Roadmap L11.

8. **Token expiry and refresh.** The expiry is recorded; nothing refreshes.
   Today the merchant redoes Embedded Signup.

   **Decided 2026-09-26 (coordinator, owner's delegation):** no refresh.
   Running Embedded Signup again stays the way back (the service answers
   `409 reconnect_required`), and the expiry is surfaced before it lapses:
   a signal a configurable time ahead in the library, an event and a
   metric in the service. `access-tokens` describes business tokens as
   needing no re-authentication and documents no refresh call, so what can
   still lapse is a grant the merchant removes or lets expire; should Meta
   add a refresh, a refresher behind a trait slots in. Roadmap L11 (the
   library's signal) and M3 (the service's event).

9. **Vault key custody and rotation cadence.** The integrator supplies the
   AES-256 key(s); rotation is supported but not scheduled.

   **Decided 2026-09-26 (coordinator, owner's delegation):** custody stays
   the integrator's (a secret manager; the service reads `WA_VAULT_KEY` or
   its `_FILE`), and rotation stays on demand (`TokenVault::rotate`,
   `meta-whatsapp-server vault rotate`). The documented cadence: at least
   yearly, and at once on a suspected exposure, dropping the old key only
   when the rotation reports no failure. Swappable: the schedule is the
   operator's. The cadence goes into `docs/guides/production.md` with M3's
   rotation work (roadmap M3).

10. **Resuming after a restart.** `resume` takes an `OnboardingRequest`
    whose `code` it ignores; after a process restart you can't rebuild one
    without a placeholder code. Option: a code-less
    `OnboardingRequest::for_resume(session)`.

    **Decided 2026-09-26 (coordinator, owner's delegation):** add a
    code-less `OnboardingRequest::for_resume(session)`; the existing
    constructor stays, so the change is additive. Roadmap L4.

11. **App-only install / Hosted Embedded Signup.** Not integrated; the
    launch options cover the default and coexistence flows.

    **Decided 2026-09-26 (coordinator, owner's delegation):** integrate
    both. App-only install is already a launch feature
    (`FeatureName::AppOnlyInstall`, refused with coexistence). Hosted
    Embedded Signup (`embedded-signup/hosted-es`) is a second way into the
    same onboarding: the `PARTNER_ADDED` `account_update` webhook starts
    it, the business token comes from `system_user_access_tokens` with an
    `appsecret_proof` instead of a code exchange, and the steps after it
    are the existing ones (verify, store, subscribe, register). Swappable:
    both are opt-in, and the code-exchange flow is unchanged. Roadmap L11.

12. **Launch option shape.** `pre-filled-data` shows
    `whatsAppBusinessAccount` both as `{ids: …}` and `{id: [...]}`; the code
    uses the worked example (`{id: [...]}`) and sends `business.id` as a
    string. Confirm in Meta's Integration Helper before relying on pre-fill.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep the
    worked example's shape (`whatsAppBusinessAccount: {id: [...]}`,
    `business.id` as a string), pinned by its test. A check in Meta's
    Integration Helper stays a verification task before production relies
    on pre-fill. Swappable: the shape is one serializer. No code.

## Authentication (OTP)

13. **Issue limit default.** 5 codes per sliding hour per recipient number
    and purpose, within one service scope (the sending `phone_number_id`
    plus `OtpConfig::namespace`, since e40b86f), plus a 30 s cooldown and 5
    attempts per code: ~0.06 %/day brute-force success on 6 digits against
    one scope. Confirm or tune; opting out is explicit.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep the
    defaults (5 codes per sliding hour per recipient and purpose within
    one scope, a 30 s cooldown, 5 attempts per code): conservative, and
    the brute-force odds above are small. Swappable by `OtpConfig`, where
    opting out stays explicit. No code.

14. **Pepper custody.** The HMAC pepper (`SecretBytes`) is supplied by the
    integrator; changing it invalidates outstanding codes.

    **Decided 2026-09-26 (coordinator, owner's delegation):** the pepper
    stays the integrator's to hold (a secret manager; the service reads
    `WA_OTP_PEPPER` or its `_FILE`), with no rotation mechanism: codes
    live for minutes, so a rotation only invalidates the codes in flight.
    Swappable: the pepper is an input. No code.

## Webhooks

15. **Dedup lease length.** 60 s by default (`with_lease`): a delivery that
    finds another request mid-delivery gets 503 and Meta retries.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep 60 s:
    the sink path takes far less, and Meta's retry answers the `503`.
    Swappable by `with_lease`. No code.

16. **Blank verify token** fails when a verification request arrives, not at
    startup — failing early would change the handler builder's API.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep the
    builder infallible, and add a check that reports a blank verify token
    when the handler is built, next to it (additive; the service already
    refuses to start on one). Swappable: the check is the caller's to run.
    Roadmap L20.

## Storage

19. **Redis TLS.** `rediss://` is not wired (redis-rs + rustls + two crypto
    providers panics); integrators pass their own connection. Wire it in
    once the crypto provider question is settled workspace-wide.

    **Decided 2026-09-26 (coordinator, owner's delegation):** wire
    `rediss://` with an explicit aws-lc-rs provider handed to the
    connection, never the process default, which is the application's
    choice (the service installs aws-lc-rs as its default, design D20). If
    redis-rs cannot take an explicit provider, integrator-built
    connections stay the way, documented. Swappable: an integrator can
    still pass its own connection. Roadmap L21.

## Security posture

20. **`%2F` in ids.** Ids are percent-encoded as whole path segments, so an
    id containing `/` cannot change the path *client-side*. Whether Graph
    decodes `%2F` before routing is unverified; if it does, an attacker who
    controls an id could still reach another edge of an object the token can
    access. Option: reject `/` in ids outright (risk: a legitimate id with
    `/`, e.g. standard-base64 group ids, if Meta issues any).

    **Decided 2026-09-26 (coordinator, owner's delegation):** check each
    id against the shape Meta documents where it documents one (digits for
    phone number, WABA, media and template ids, as the service already
    does for media and template ids), and keep whole-segment
    percent-encoding for opaque ids (BSUIDs, group ids), so no legitimate
    id is refused. Swappable: one validator per kind of id. Roadmap L20.

21. **quick-xml advisories (RUSTSEC-2026-0194/0195)** are ignored in
    `deny.toml`, scoped and justified: reached only via typst's CSL parsing,
    which trusted templates alone can trigger. Remove when typst upgrades.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep the
    scoped ignore, and remove it in the PR that upgrades typst past the
    advisories (`just deny` then shows the entry unused). Reversible: one
    `deny.toml` entry. No other code.

## Product details

22. **Product-card carousel templates.** The page says "define exactly two
    cards"; the code allows 2–10 (the wording reads as guidance). Enforce 2?

    **Decided 2026-09-26 (coordinator, owner's delegation):** the page,
    now mirrored, settles it: a product-card carousel template is created
    with exactly two cards, and an approved one sends up to 10. So
    creation checks two cards when the cards carry a product header,
    media-card carousels keep 2–10, and sends keep at most 10. Swappable:
    one constant each. Roadmap L20.

23. **Groups `add_participants`.** In Meta's reference, but the groups guide
    says participants can't be added manually. Kept, with a warning.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep
    `add_participants`, with its warning: Meta's reference documents it,
    and Meta's refusal reaches the caller as an error. Swappable: callers
    need not call it. No code.

24. **`Analytics::set_button_click_tracking`** acts on a template but lives
    in analytics (where Meta documents it); move to templates later?

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep it in
    analytics, where Meta documents it. Swappable by an additive alias in
    templates, should one be wanted. No code.

25. **Typst default date.** `Renderer::new()` has none, so templates calling
    `datetime.today()` fail until `with_today` is set, rather than printing a
    wrong date on an invoice.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep failing
    until `with_today` is set: an invoice never carries a wrong date.
    Swappable: `with_today` sets the date. No code.

26. **In-App Signup Terms of Service.** Creating the first signup accepts
    Meta's marketing messages terms on the business's behalf — a legal
    decision, not a technical one.

    **Left open on 2026-09-26:** accepting Meta's terms on a business's
    behalf is a legal decision, which the owner's delegation leaves to the
    owner. The service will carry the In-App Signup routes and enable them
    only once this is answered (roadmap M5i).

## API conventions

Found by the conventions reviews of 8ee6fab (27–29; counts are from that
commit).

27. **One catch-all variant for every open enum.** Enums Meta may extend
    have a catch-all so a new value never fails parsing, but it comes in
    several shapes: `Other(String)` from `open_enum!` (meta-whatsapp-webhooks) and
    `string_enum!` (templates, case-insensitive); `Unknown(String)` from
    `wire_enum!` (flows and marketing, two copies of the macro) and in
    `business_profile::Vertical`; hand-written `#[serde(untagged)]
    Other(String)` (analytics, calling); and a unit `#[serde(other)]
    Unknown` (14 enums in phone_numbers, waba, signups, embedded_signup,
    and the send response's message status). The unit form drops Meta's
    value, and where the enum also derives `Serialize` it writes its own
    name back instead. Pick one name and one shape (and whether one macro
    in meta-whatsapp-core generates them all) before integrators pin a revision:
    changing it later breaks their `match`es. Until then new code follows
    its module and adds no new unit `Unknown`.
    One struct already mixes the meanings: in `PhoneNumberInfo`,
    `quality_rating` is the shared `common::QualityRating`, whose `Unknown`
    is Meta's documented `UNKNOWN` (its catch-all is `Other(String)`),
    while every other enum of the struct uses a unit `Unknown` as its
    catch-all.

    **Decided 2026-09-26 (coordinator, owner's delegation):** one shape,
    `Other(String)`, keeping Meta's value, generated by one macro in
    meta-whatsapp-core for every open enum; no unit `Unknown` catch-all (a
    documented `UNKNOWN` value, as in `QualityRating`, stays a real
    variant). A breaking change, made before the first release. Swappable:
    the macro is the one place to change it. Roadmap L20.

28. **`#[non_exhaustive]` policy.** 25 of the 144 `Deserialize` structs in
    meta-whatsapp-client have it (all in the onboarding modules), none of the 104 in
    meta-whatsapp-webhooks; every macro-generated enum has it, the hand-written
    `Other(String)` enums in analytics and calling do not, so naming a new
    value there is a breaking change. The attribute lets Meta's additions
    land without a major version, but integrators then cannot build these
    types with struct literals (for example in their own tests). Decide per
    kind (response structs, webhook payloads, enums) and write the rule in
    `docs/architecture.md`.

    **Decided 2026-09-26 (coordinator, owner's delegation):**
    `#[non_exhaustive]` on every response struct, webhook payload struct
    and open enum (Meta adds fields and values), never on request or
    builder types; integrators build test values through constructors or
    from JSON fixtures. The rule goes into `docs/architecture.md`. A
    breaking change for integrators (struct literals and exhaustive
    matches of those types stop compiling), made before the first
    release. Swappable per kind. Roadmap L20.

29. **axum and sqlx: re-exports or your own pins?** Both are re-exported
    (`meta_whatsapp_rs::webhooks::axum`, `meta_whatsapp_rs::adapters::store::postgres::sqlx`)
    and their rustdoc says to use the re-export; the README and examples
    now do the same, and the examples no longer pin sqlx. A consumer's own
    `axum = "0.8"` still unifies with it. The alternative, telling
    integrators to pin their own versions and dropping the re-exports, was
    not taken; confirm the direction.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep the
    re-exports (today's direction); an integrator's own compatible pin
    still unifies with them. Swappable: a re-export is one line. No code.

## Webhooks and live updates

Found by the security review of 8ee6fab.

30. **One permanent sink error fails the whole delivery.** When a sink
    fails, `WebhookHandler` answers 500 and Meta redelivers the whole POST
    (every event in it, possibly for several WABAs) for up to 7 days, then
    drops it. An event a sink fails on every time therefore holds back the
    events after it in the same body until all are lost. (The inbox's
    most likely case, U+0000 in a customer's message on Postgres, no
    longer fails: message content keeps it since the owner decided the
    former #18; a NUL in a Meta-assigned id, or any other permanent sink
    failure, still does this.) A dead-letter
    design would classify sink errors as transient or permanent,
    acknowledge a permanent one after writing the event (raw, size-bounded)
    to a dead-letter store with an alert and a replay path, and deliver the
    rest of the batch. Today the whole batch fails and Meta retries it.

    **Decided 2026-09-26 (coordinator, owner's delegation):** dead-letter.
    A sink error is classified transient or permanent; a permanent one
    writes the event (raw, size-bounded) to a dead-letter typed store on
    `KvStore`, raises an alert metric and keeps a replay path, and the
    rest of the batch is delivered and acknowledged; a transient one
    answers `500` as today. The delivery is acknowledged only after the
    dead-letter write succeeds, so nothing is dropped silently. An error
    a sink does not classify stays transient, so a sink that never opts
    in keeps today's behaviour. The raw events are personal data: the
    dead-letter store is bounded by count and age, and L5's erasure
    reaches it. Swappable: the sink classifies its own errors, and the
    store is a typed store like the others. Roadmap L21.

31. **SSE fan-out cost.** `webhooks::sse` reads a
    `broadcast::Receiver<WebhookEvent>`, and a broadcast receiver clones
    every event it receives: each open inbox copies every merchant's
    events before its filter drops them, including multi-megabyte
    `HistorySynced` bodies, so one coexistence history sync costs its size
    times the number of open inboxes. (The rustdoc of `sse` said rejected
    events "cost nothing"; it now says each one costs a clone.) Options:
    broadcast `Arc<WebhookEvent>` (changes `sse`'s and `BroadcastSink`'s
    types), or one channel per phone number id, created with its first
    subscriber.
    Today: a clone per event per subscriber, fine for a handful of open
    inboxes.

    **Decided 2026-09-26 (coordinator, owner's delegation):** broadcast
    `Arc<WebhookEvent>`, which changes the types of `sse` and
    `BroadcastSink` before the first release; the service's per-number
    channels (design §4.5) stay its own. Swappable: the sink is one of
    several. Roadmap L21.

## CMS inbox

Found while writing the integrator guides and checking them against
7940d15 (32, 33).

32. **A call reopens the window, the inbox cannot see it.** Meta starts or
    refreshes the 24-hour customer service window when the customer
    messages the business number, *calls* it (answered or not), or accepts
    the business's call (`calling/pricing`). `InboxSink` records messages
    only, and `Inbox::window` is computed from the last recorded inbound
    message. So after a call, `Inbox::reply` (and `Inbox::send` without a
    template or a Direct Send category) refuses locally a free-form reply
    that Meta would accept.
    Options: record calls (`CallUpdated` / `CallStatusUpdated`) as window
    events in the `ConversationStore` (a port change), let the caller
    override the check, or keep it and document the template fallback (what
    the guides and skills do today).

    **Decided 2026-09-26 (coordinator, owner's delegation):** record the
    calls that reopen the window (`calling/pricing`: a customer's call,
    answered or not, and the customer accepting the business's call;
    `CallUpdated`, `CallStatusUpdated`) as window events in
    `ConversationStore`, a port change that lands with L5, and let the
    caller override the local check explicitly. Meta enforces the window
    either way; the local check only saves a request Meta would refuse.
    Swappable by the override. Roadmap L7.

33. **Message ids are unique per store, not per business number.** The
    Postgres `messages.id` is the table's primary key on its own (and the
    memory store keys by id alone). If the same message id is ever
    delivered on two of an integrator's business numbers (for example a
    group both numbers are in, or one of its numbers writing to another,
    should Meta use one id on both sides), it is stored once, under the
    conversation that recorded it first: the second `append` returns
    `false` and changes nothing, and since `update_status` is scoped to the
    number (4b47bf7), the second number's statuses find no row either.
    Options: key messages by `(phone_number_id, id)` (a migration of the
    primary key; history cursors are already per conversation), or keep it
    and document it (what the guides and skills do today).

    **Left open on 2026-09-26:** the alternative to today's behaviour is
    keying stored messages by `(phone_number_id, id)`, a primary-key
    migration of stored data, which the owner's delegation leaves to the
    owner. Today's behaviour stays, documented as above.

<!-- 44 follows 33 in this section: a list of its own. -->

44. **Conversation Routing and the inbox.** Under Conversation Routing
    (`conversation-routing/*`) one responder owns a thread; the others may
    receive standby copies. `InboxSink` drops `StandbyObserved` and
    `ThreadControlChanged` (they arrive typed since the webhook
    conformance sweep), so: a merchant that observed a thread in standby
    and then gets `control_passed` has no recorded inbound message, and
    `Inbox::window` refuses a free-form reply Meta would accept; and the
    inbox does not know ownership, so after `control_taken` it still lets
    a reply through (Meta rejects Service sends from a non-owner). Options:
    record standby inbound messages as window events (not as unread
    messages), track ownership in the `ConversationStore` from the
    handovers, the `messages`/`standby` split, `release` and the 24-hour
    idle timeout (`conversation-routing/thread-control` § Tracking
    ownership), or keep it and document it (what the guides and skills do
    today).

    **Decided 2026-09-26 (coordinator, owner's delegation):** record
    standby inbound messages as window events (never as unread messages),
    and track thread ownership in `ConversationStore` from the handovers,
    `release` and the 24-hour idle timeout; the inbox refuses a reply
    locally when another app owns the thread, and the caller can override.
    It lands with L5 and #32, so adapters change once, and the service
    makes the two event types tenant-visible in M2 (design D25). Swappable
    by the override: ownership is advisory, Meta enforces it. Roadmap L7
    and M2.

## Service (meta-whatsapp-server)

Found in the review of milestone M1b (43).

43. **Media received by webhook, and the `phone_number_id` check.** The
    service's media routes ask Meta with `phone_number_id={pn}`, so that
    Meta acts only on that number's media and one tenant cannot reach
    another's files when one token reaches both tenants' numbers
    (`reference/media/media-api`). Meta documents that check for media
    *uploaded* on the number only. If Meta also refuses media a customer
    sent to the number (a media id received by webhook), the M2 inbox
    cannot fetch customers' files through the service: they answer `404`
    like another tenant's.
    Today: every media id goes through the check; nothing exempts a
    received one.
    Planned remedy: exempt a media id the service itself recorded as
    received on that tenant's number (from M1c's events), and ask Meta
    for it without `phone_number_id`. M1c records inbound events now; the
    remedy is a follow-up still to do before M2. Before M2 relies on
    either behaviour, a live check with a real received media id must say
    which one Meta has.

    **Decided 2026-09-26 (coordinator, owner's delegation):** adopt the
    planned remedy in M2: a media id the service itself recorded as
    received on the tenant's number is asked from Meta without
    `phone_number_id`, and every other id keeps the check. It is safe
    whichever way Meta behaves, since it widens the route only to ids the
    tenant's own events carried; the live check with a real received media
    id still says which behaviour Meta has. Swappable: the exemption is
    one lookup. Roadmap M2.
