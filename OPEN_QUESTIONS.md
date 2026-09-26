# Open questions

Decisions the implementation deliberately did **not** make. Each entry says
what the code does today, so nothing here is a hidden default. Close an
entry by deciding it in an issue/PR and deleting it here.

Numbers are permanent (the code, docs and skills cite them) and not in
order within a section. Markdown renders a list with its first item's
number and counts up from there, so an entry out of sequence starts a list
of its own, after an HTML comment; keep that when you add one.

## Naming and publishing

2. **`meta_whatsapp_rs::client` is both a module and a function** (the
   production client shortcut). Rustdoc links need `mod@`/`fn@`.
   Alternative: `meta_whatsapp_rs::connect(token)`.

## Embedded Signup (onboarding merchants)

4. **Two-step PIN policy.** The caller supplies the 6-digit PIN on every
   `onboard`/`resume`; nothing generates or stores PINs. Numbers that already
   have a PIN need the merchant's current one.
5. **Multi-WABA signups.** Only the claimed `waba_id` (else the first of
   `waba_ids`; with no claim, the newest granted WABA) is onboarded.
6. **One WABA shared by several tenants.** The vault is keyed by WABA and
   knows no tenants; the last onboarding wins. `onboard_with_approval`
   lets an integrator refuse (or apply any other policy) before anything
   is stored; which policy meta-whatsapp-rs itself should default to is open. A
   Solution Partner deployment must approve (plain `onboard` is refused,
   and `resume` shares only for a WABA whose stored token record has a
   recorded approval; the owner decided on 2026-09-25 that it stays
   required), but what the approval checks is still the integrator's:
   meta-whatsapp-rs decides no tenant policy.
7. **Coexistence sync.** Contacts/history sync (`smb_app_data`) must happen
   once, within 24 h of onboarding. `onboard` only flags it
   (`needs_coexistence_sync()`); should it trigger it?
8. **Token expiry and refresh.** The expiry is recorded; nothing refreshes.
   Today the merchant redoes Embedded Signup.
9. **Vault key custody and rotation cadence.** The integrator supplies the
   AES-256 key(s); rotation is supported but not scheduled.
10. **Resuming after a restart.** `resume` takes an `OnboardingRequest`
    whose `code` it ignores; after a process restart you can't rebuild one
    without a placeholder code. Option: a code-less
    `OnboardingRequest::for_resume(session)`.
11. **App-only install / Hosted Embedded Signup.** Not integrated; the
    launch options cover the default and coexistence flows.
12. **Launch option shape.** `pre-filled-data` shows
    `whatsAppBusinessAccount` both as `{ids: …}` and `{id: [...]}`; the code
    uses the worked example (`{id: [...]}`) and sends `business.id` as a
    string. Confirm in Meta's Integration Helper before relying on pre-fill.

## Authentication (OTP)

13. **Issue limit default.** 5 codes per sliding hour per recipient number
    and purpose, within one service scope (the sending `phone_number_id`
    plus `OtpConfig::namespace`, since e40b86f), plus a 30 s cooldown and 5
    attempts per code: ~0.06 %/day brute-force success on 6 digits against
    one scope. Confirm or tune; opting out is explicit.
14. **Pepper custody.** The HMAC pepper (`SecretBytes`) is supplied by the
    integrator; changing it invalidates outstanding codes.

## Webhooks

15. **Dedup lease length.** 60 s by default (`with_lease`): a delivery that
    finds another request mid-delivery gets 503 and Meta retries.
16. **Blank verify token** fails when a verification request arrives, not at
    startup — failing early would change the handler builder's API.

## Storage

19. **Redis TLS.** `rediss://` is not wired (redis-rs + rustls + two crypto
    providers panics); integrators pass their own connection. Wire it in
    once the crypto provider question is settled workspace-wide.

## Security posture

20. **`%2F` in ids.** Ids are percent-encoded as whole path segments, so an
    id containing `/` cannot change the path *client-side*. Whether Graph
    decodes `%2F` before routing is unverified; if it does, an attacker who
    controls an id could still reach another edge of an object the token can
    access. Option: reject `/` in ids outright (risk: a legitimate id with
    `/`, e.g. standard-base64 group ids, if Meta issues any).
21. **quick-xml advisories (RUSTSEC-2026-0194/0195)** are ignored in
    `deny.toml`, scoped and justified: reached only via typst's CSL parsing,
    which trusted templates alone can trigger. Remove when typst upgrades.

## Product details

22. **Product-card carousel templates.** The page says "define exactly two
    cards"; the code allows 2–10 (the wording reads as guidance). Enforce 2?
23. **Groups `add_participants`.** In Meta's reference, but the groups guide
    says participants can't be added manually. Kept, with a warning.
24. **`Analytics::set_button_click_tracking`** acts on a template but lives
    in analytics (where Meta documents it); move to templates later?
25. **Typst default date.** `Renderer::new()` has none, so templates calling
    `datetime.today()` fail until `with_today` is set, rather than printing a
    wrong date on an invoice.
26. **In-App Signup Terms of Service.** Creating the first signup accepts
    Meta's marketing messages terms on the business's behalf — a legal
    decision, not a technical one.

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
28. **`#[non_exhaustive]` policy.** 25 of the 144 `Deserialize` structs in
    meta-whatsapp-client have it (all in the onboarding modules), none of the 104 in
    meta-whatsapp-webhooks; every macro-generated enum has it, the hand-written
    `Other(String)` enums in analytics and calling do not, so naming a new
    value there is a breaking change. The attribute lets Meta's additions
    land without a major version, but integrators then cannot build these
    types with struct literals (for example in their own tests). Decide per
    kind (response structs, webhook payloads, enums) and write the rule in
    `docs/architecture.md`.
29. **axum and sqlx: re-exports or your own pins?** Both are re-exported
    (`meta_whatsapp_rs::webhooks::axum`, `meta_whatsapp_rs::adapters::store::postgres::sqlx`)
    and their rustdoc says to use the re-export; the README and examples
    now do the same, and the examples no longer pin sqlx. A consumer's own
    `axum = "0.8"` still unifies with it. The alternative, telling
    integrators to pin their own versions and dropping the re-exports, was
    not taken; confirm the direction.

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
