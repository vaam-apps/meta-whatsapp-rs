# Open questions

Decisions the implementation did **not** make when it was written. Each
entry says what the code does today, so nothing here is a hidden default.

**From 2026-09-26 the owner delegates these decisions**, under the rule
in [AGENTS.md § Decisions](AGENTS.md#decisions) (and
[design §10](docs/design/server.md#10-decisions) for the service): an
entry is decided when its choice ships swappable, or names the roadmap
item that adds the swap. Legal and terms-of-service choices, and what
cannot be undone (irreversible deletion or data migrations, released
contracts, stable identifiers), stay the owner's, even when swappable.
A legal act Meta ties to an API call is exposed as an explicit, audited
operator action and never taken automatically; performing it is the
deployer's decision.

**Recording format.** A decided entry keeps its text and gains one
paragraph: `**Decided <date> (<who>, owner's delegation):** <the
choice>. <why>. Swappable by <how>. Roadmap <item>.`, or `No code.` in
place of the roadmap item. An entry left for the owner gains
`**Left open on <date>:** <why it is the owner's>.` Design §10's rows
carry the same parts.

**Deleting an entry.** A decided entry stays here, marked, until its
roadmap item lands (at once for `No code.`), the CHANGELOG lists it
under "Open questions decided", and nothing cites it (guides, skills,
the design, the code); then it is deleted. A section heading stays when
its last entry goes, with a line saying so: guides and skills link to
its anchor.

On 2026-11-01, 25 entries are decided and 2 stay open: #33 and #46
(each a data migration).

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
   own. Swappable by that option. Roadmap L11b.

   **Landed with roadmap L11b (2026-11-01), the library's side, its
   choices the coordinator's (owner's delegation):** the option is `OnboardingRequest::every_granted_waba(true)`, off by
   default, acted on by two new calls, `EmbeddedSignup::onboard_wabas`
   and `onboard_wabas_with_approval` (its approval called once per WABA
   and, for a Solution Partner, recorded per WABA), because `onboard`'s
   result names one WABA; `onboard` and `onboard_hosted` refuse the
   option, and `resume` ignores it. The code is exchanged and the token
   inspected once; the WABAs onboarded are the token's grants
   (`granular_scopes`, `whatsapp_business_management`, newest first:
   `solution-providers/manage-accounts`), and a session info naming one
   (`waba_id`, `waba_ids`: `embedded-signup/implementation`) the token
   was not granted refuses the signup before anything is stored. The
   claimed WABA runs with the request as it is; each other grant runs its
   own `verify_assets`, `approve`, `store_token` and tail (its own vault
   record, phone index, subscription and credit line), registering no
   number and requesting no coexistence sync (the PIN and the finish are
   the claimed number's), only when Meta reports the claimed WABA's
   owner as its owner too (a token's grants outlast a signup and, for a
   user's token, may span businesses); one the vault already holds is
   left alone (`WabaOutcome::Held`), and one stored by another
   onboarding while this one runs is not replaced (`put_if_absent`).
   Per-WABA outcomes come back in `WabasOnboarded`; a WABA that fails
   does not fail the call or stop the next, and each is resumed on its
   own. Hosted signup stays one WABA per
   `PARTNER_ADDED` webhook. Nothing new is stored. Swappable by the
   option (off: one WABA, as `onboard`) and by calling `onboard` instead.
   Roadmap L11b (landed); the service's side, M3f.

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
   `needs_coexistence_sync()` stays the signal. Roadmap L11d.

   **Landed with roadmap L11d (2026-11-01), the library's side:** two
   steps, `sync_contacts` then
   `sync_history` (Meta's steps 1 and 2 of
   `onboarding-business-app-users`, each its own once-only call), the
   last of the tail: after `subscribe_app`, a Solution Partner's credit
   line and `register_phone` (which never runs for coexistence), because
   Meta's page starts the sync once the onboarding steps are done.
   They run when the request's `finish_kind` is coexistence and
   `OnboardingRequest::coexistence_sync` is on (the default); its
   setter, `coexistence_sync(false)`, turns them off, and
   `needs_coexistence_sync()` is unchanged. Meta's answers come back in
   `Onboarded::coexistence_sync` (`SyncOutcome::Requested` with the
   `request_id` Meta asks partners to keep, or `Refused`). Meta's
   errors page says a second request is refused (`2593107`, "exceeded
   the maximum number of times") and so is one past 24 hours
   (`2593108`); nothing documents a way to read whether a sync was
   made. So each request is claimed, before it is posted, in a plain
   JSON record (`wa.es.coexistence`, keyed by phone number id, scoped to
   the stored token record's WABA and `created_at`, so a new signup
   starts a new sync), and `resume` reads it: an accepted or refused
   request is never posted again; a Meta refusal (`SyncNotAllowed`) is
   reported, not failed (only a new signup changes it); a request Meta
   refused before doing anything (`may_have_been_sent` false) releases
   its claim and is asked again; one whose answer was lost keeps its
   claim, and `resume` fails at that step, before any request, until
   the integrator checks the webhooks and resumes with the sync off.
   Hosted signup syncs nothing (`hosted-es` onboards to Cloud API only;
   `for_hosted` names no finish). The new namespace is a stored
   identifier, pending the owner's confirmation (`docs/architecture.md`
   § Stable identifiers). M3b avoids a second call by building on these
   steps rather than calling `smb_app_data` itself: the record already
   refuses a repeat within one onboarding, and the service's own record
   of a synced number can turn the option off for a later one. Swappable
   by the option (and by calling `sync_smb_app_data` yourself). Roadmap
   L11d (landed); the service's side, M3b.

   **Confirmed 2026-11-01 (coordinator, owner's delegation; roadmap L11d
   landed):** decided for the library, the service's side carried by
   M3b, which builds on these steps rather than calling `smb_app_data`
   itself. The choices, each swappable, are the ones above and in
   roadmap L11d's Decided note: two steps after the credit-line steps,
   in Meta's order; Meta's refusals reported as `SyncOutcome::Refused`;
   a claim released only when `!may_have_been_sent()`; a lost answer's
   way out manual and documented (a call clearing a pending claim is a
   follow-up); the option `coexistence_sync(bool)`, on by default; the
   record never expires. The record's namespace, `wa.es.coexistence`,
   and its layout stay the owner's to confirm before the first release
   (`docs/architecture.md` § Stable identifiers). Swappable by the
   option. Roadmap L11d (landed), M3b.

8. **Token expiry and refresh.** The expiry is recorded; nothing refreshes.
   Today the merchant redoes Embedded Signup.

   **Decided 2026-09-26 (coordinator, owner's delegation):** no refresh.
   Running Embedded Signup again stays the way back (the service answers
   `409 reconnect_required`), and the expiry is surfaced before it lapses:
   a signal a configurable time ahead in the library, an event and a
   metric in the service. `access-tokens` describes business tokens as
   needing no re-authentication and documents no refresh call, so what can
   still lapse is a grant the merchant removes or lets expire; should Meta
   add a refresh, a refresher behind a trait slots in. Roadmap L11e (the
   library's signal) and M3e (the service's event).

   **Landed with roadmap L11e (2026-11-01), the library's side, its
   choices proposed to the coordinator (owner's delegation):**
   `TokenVault::token_expiry(waba_id, lead)` reports a stored token as
   `TokenExpiry::Never` (no `expires_at`), `Fresh`, `ExpiresSoon`
   (`now + lead` included: a boundary token is warned about, not missed)
   or `Expired` (`expires_at <= now`, as `is_expired`), on the vault's
   clock. The lead is the caller's, per call; `DEFAULT_EXPIRY_LEAD` is 7
   days: business tokens default to never expiring
   (`facebook-login/facebook-login-for-business`), and one generated to
   expire lasts 60 days, so a week leaves time to run Embedded Signup
   again. It reads the record's clear `expires_at`, not the sealed one:
   no key, no token in memory, no write, and the stored format
   unchanged; the clear copy is not authenticated (whoever writes the
   store can change the answer, as they can delete the record), so
   `StoredBusinessToken::expiry` answers on the decrypted record for a
   caller who wants the authenticated copy. A per-record check, not a
   listing: the vault cannot enumerate its records, so the caller walks
   its own WABAs (the service, its bindings). The answer is stateless:
   notifying once per `(waba_id, expires_at)` is the caller's (M3e's).
   Not recorded, so not reported: `debug_token`'s
   `data_access_expires_at` (0 for business tokens in Meta's examples);
   adding it would change the stored record. Swappable: the lead per
   call, and the classification is one function (`TokenExpiry::at`) a
   caller can replace on `expires_at`. Roadmap L11e (landed); the
   service's event, M3e.

   **Decided 2026-11-05 (coordinator, owner's delegation; roadmap M3e
   landed), the service's side: Coordinator's decision 2026-11-05 under
   the owner's delegation, swappable, as design D180 to D183:** a
   `token_expiring` event for the WABA's tenant only, `{event, waba_id,
   expires_at}` (D182), told only for an `ExpiresSoon` token, with one
   setting, `WA_SERVER_TOKEN_EXPIRY_LEAD` (7 days, or the outbox
   retention when that is shorter, `off`; an explicit lead past the
   retention is refused at start) (D180); once per token and expiry by
   the outbox's own key, no marker stored (D181); a task of its own under
   a leader-lock turn every 10 minutes, and a counter by outcome,
   `wa_server_token_expiry_events_total` (D183). A token found already
   expired is not told. The event type, setting, metric, key label and
   lock name stay the owner's to confirm before the first release
   (`docs/architecture.md` § Stable identifiers). Swappable by the
   setting and by `TokenExpiring::outbox_key`. Roadmap M3e (landed).

9. **Vault key custody and rotation cadence.** The integrator supplies the
   AES-256 key(s); rotation is supported but not scheduled.

   **Decided 2026-09-26 (coordinator, owner's delegation):** custody stays
   the integrator's (a secret manager; the service reads `WA_VAULT_KEY` or
   its `_FILE`), and rotation stays on demand (`TokenVault::rotate`,
   `meta-whatsapp-server vault rotate`). The documented cadence: at least
   yearly, and at once on a suspected exposure, dropping the old key only
   when the rotation reports no failure. Swappable: the schedule is the
   operator's. The cadence goes into `docs/guides/production.md` with M3's
   rotation work. Roadmap M3a.

<!-- 11 follows 9 in this section: a list of its own. -->

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
    both are opt-in, and the code-exchange flow is unchanged. Roadmap L11a.

    **Landed with roadmap L11a (2026-11-01), the library's side, its
    choices the coordinator's (owner's delegation):**
    `EmbeddedSignup::onboard_hosted` (and `_with_approval`) takes a `HostedSignup` (the webhook's
    `waba_info.waba_id` and `owner_business_id`) and the system user
    token as an explicit argument; the app secret is the
    `AppCredentials`' one. The token request is `fetch_only=true`, as
    `hosted-es` step 5 sends it (a fetch is replayed as a read, a
    generation never is); a WABA whose owner, as Meta reports it, is
    another business than the webhook's is refused (when Meta reports
    none, none is stored, as in the code flow); `debug_token` runs on
    the fetched token, assuming hosted signup's business tokens carry
    `granular_scopes` naming the WABA (one that does not fails closed);
    a WABA the vault already holds is refused before any request,
    because `PARTNER_ADDED` also comes for code-flow and coexistence
    onboardings and redeliveries, and storing again would replace the
    held token unasked. The integrator's, documented: re-onboarding a
    held WABA (delete it first), the tenant binding (in the approval:
    the webhook names no tenant) and the PIN (the hosted page asks for
    none; as #4 keeps, nothing generates or stores it). Hosted signup
    reuses the code flow's steps and vault record, so nothing new is
    stored. Swappable by `HostedSignup::token_request` (the request) and
    by not calling `onboard_hosted` (the code flow is unchanged); the
    held-WABA refusal by deleting the record first. Roadmap L11a
    (landed); the service's side, M3f.

12. **Launch option shape.** `pre-filled-data` shows
    `whatsAppBusinessAccount` both as `{ids: …}` and `{id: [...]}`; the code
    uses the worked example (`{id: [...]}`) and sends `business.id` as a
    string. Confirm in Meta's Integration Helper before relying on pre-fill.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep the
    worked example's shape (`whatsAppBusinessAccount: {id: [...]}`,
    `business.id` as a string), pinned by its test. A check in Meta's
    Integration Helper stays a verification task before production relies
    on pre-fill. Swappable: the shape is one serializer. Roadmap L25 (the
    check; code only if the helper shows another shape).

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
    still pass its own connection. Roadmap L21c.

    **Confirmed 2026-11-01 (coordinator, owner's delegation; roadmap
    L21c landed):** redis-rs 1.7 cannot take an explicit provider (its
    rustls path always calls `rustls::ClientConfig::builder()`, and
    nothing of it accepts a `ClientConfig` or a connector), but it takes
    any stream (`MultiplexedConnection::new_with_config`), so `rediss://`
    is wired anyway rather than left to integrator-built connections:
    `store::RedisTls` builds the `ClientConfig` on
    `rustls::crypto::aws_lc_rs::default_provider()`, does the handshake
    with tokio-rustls and hands the stream to that constructor, none of
    redis-rs's TLS features enabled. The platform's trust store
    (`rustls-platform-verifier`, as reqwest does) is the default; a
    private CA or a client certificate is given as PEM. It stays under
    the existing `redis` feature. `ring` is linked into the workspace's
    test builds only, through the adapters' rustls dev-dependency, so the
    tests run in the binary where the process default panics; no library
    build links it (`cargo tree -e normal -p meta-whatsapp-adapters
    --features redis -i ring` finds nothing). The test Redis requires
    client certificates, and its certificates are generated
    (`tools/test-tls/generate.sh`, and rcgen in the in-process tests),
    none committed. Why: the decision of 2026-09-26 asked for an explicit
    provider handed to the connection, and handing redis-rs a stream
    delivers exactly that without waiting on redis-rs. Swappable by
    `RedisTls::from_client_config` (any `ClientConfig`: your provider,
    your verifier) or by an integrator-built connection passed to
    `RedisKvStore::new`. Not covered: Cluster and Sentinel over TLS.
    Roadmap L21c (landed).

## Security posture

21. **quick-xml advisories (RUSTSEC-2026-0194/0195)** are ignored in
    `deny.toml`, scoped and justified: reached only via typst's CSL parsing,
    which trusted templates alone can trigger. Remove when typst upgrades.

    **Decided 2026-09-26 (coordinator, owner's delegation):** keep the
    scoped ignore, and remove it in the PR that upgrades typst past the
    advisories (`just deny` then shows the entry unused). Swappable by one
    `deny.toml` entry. No other code.

## Product details

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

    **Decided 2026-09-26 (coordinator, owner's delegation):** the code
    decides nothing legal, so the acceptance is exposed as an explicit
    operator action, and accepting is the deployer's act. The library
    already keeps it explicit (`SignupPolicy::accept_terms()`, which no
    other call sets); the service exposes it as an audited operator
    action, off until the deployment enables it, never taken
    automatically. Swappable: nothing accepts unless a deployment asks.
    Roadmap M5i.

<!-- 45 follows 26 in this section: a list of its own. -->

45. **The Marketing Messages API's max-price agreement.** Max price
    (`marketing-messages/pricing/enroll-max-price`) is a beta a WABA
    joins by signing Meta's beta agreement through the API: a legal act
    on the business's behalf. Nothing wrapped it before roadmap L13
    (parity row 137).

    **Decided 2026-09-26 (coordinator, owner's delegation):** the code
    decides nothing legal. The library exposes the agreement as its own
    explicit call, which no other call makes (a max-price send never
    signs it); the service exposes it as an audited operator action, off
    until the deployment enables it. Signing is the deployer's act, for
    their own deployment. Swappable: nothing signs unless a deployment
    calls it. Roadmap L13 and M5h.

    **Confirmed 2026-11-01 (coordinator, owner's delegation; roadmap L13
    landed):** decided for the library, the service's side carried by
    M5h. The library
    exposes the agreement only as the explicit
    `MarketingBusiness::sign_max_price_agreement` (`POST
    /{BUSINESS_ID}/max_price_agreements` with `signer_name` and
    `signer_email`), sent once whatever the retry policy, which no
    other call makes: no send, duplicate, reach estimate, template
    update or builder signs, and Meta's refusals for want of the
    agreement (`100`, a duplicate's HTTP 403) are returned, not
    answered by signing (`MarketingBusiness::max_price_agreement` only
    reads its state). The partner allow list no longer exists: Meta's
    changelog of 2026-10-01 removed it with the open beta (a partner's
    accepted agreement covers the client WABAs shared with or managed by
    it), so no per-client act remains to expose. M5h exposes signing as
    an explicit, audited operator action, off by default. Why: AGENTS.md
    § Decisions, "Legal acts": the code decides nothing legal, and
    signing is the deployer's act, for their own deployment. Swappable
    by not calling it: nothing signs unless a deployment calls it.
    Roadmap L13 (landed) and M5h.

## API conventions

Found by the conventions reviews of 8ee6fab (27–29; counts are from that
commit). #27 (one catch-all shape for open enums) landed with roadmap
L20a and is deleted (CHANGELOG, "Open questions closed").

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
    release. Swappable per kind. Roadmap L20b.

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
    dead-letter store is bounded by count and age, and keeps its own
    per-contact index and an `erase`, which a facade-level eraser calls
    beside the conversation store's (`KvStore` has no scan, so L5's
    `ConversationStore::erase_all` cannot reach it by itself; reworded on
    2026-09-26 after L5's privacy review, which found that "L5's erasure
    reaches it" could not hold). Swappable: the sink classifies its own
    errors, and the store is a typed store like the others. Roadmap
    L21a.

    **Confirmed 2026-11-01 (coordinator, owner's delegation; roadmap
    L21a landed):** a sink signals with `SinkError::Permanent`
    (`SinkError::permanent`, `is_permanent`; a variant of core's existing
    leaf, additive, the enum `#[non_exhaustive]`), and `FanoutSink` is
    permanent only when every failing sink was. The handler acknowledges
    a permanent error only after `DeadLetters::record` wrote the event and
    indexed it (a failed write or index releases the claim and answers
    `500`), then marks its dedup claim done; replay goes past the guard
    (at least once). The event is stored in its serialized form, the one
    replay reads back (the body itself for an unparsed one), rather than
    the raw body. Bounds: 1000 letters (a ring, the oldest overwritten),
    14 days, 256 KiB per stored event (a larger one kept without its
    content, still indexed). The contact index takes every identifier the
    event gives (BSUIDs, parent BSUIDs, phone numbers), keyed by business
    number and contact; an event naming no number is erased by any
    number's erasure of the contact (over-deleting diagnostic copies is
    the safe direction for an erasure). The facade's erasure is a new
    `meta_whatsapp_rs::erasure::Eraser` rather than a change to
    `Inbox::erase_all` (whose rustdoc says it does not reach the
    letters). Swappable: the bounds are settings (`with_capacity`,
    `with_max_age`, `with_max_event_bytes`), the store is opt-in
    (`WebhookHandlerBuilder::dead_letters`), and which errors are
    permanent is each sink's choice. The namespaces `wa.webhook.deadletter`
    and `wa.webhook.deadletter.contact`, their keys' derivations and their
    records' shapes stay the owner's to confirm before the first release
    (roadmap § Owner touchpoints). Roadmap L21a.

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
    `ConversationStore`, a port change that is part of L5, and let the
    caller override the local check explicitly. Meta enforces the window
    either way; the local check only saves a request Meta would refuse.
    Swappable by the override (`ReplyChecks::window(false)`), and by
    recording calls yourself (`RecordingSwitches::ALL.calls(false)`).
    Roadmap L5 (the port) and L7 (the inbox), both landed; which call
    webhooks count is in docs/guides/cms-inbox.md § 5. The entry stays
    until nothing cites it (the header's rule).

33. **Message ids are unique per store, not per business number.** The
    Postgres `messages.id` is the table's primary key on its own (and the
    memory store keys by id alone; the SQLite and Redis stores of roadmap
    L8 key a message by `(phone_number_id, id)` and keep the rule with a
    separate index on the id, which the other answer would drop:
    `conversation_conformance::ids_are_stored_once_per_store`). If the
    same message id is ever
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
    owner. Today's behaviour stays, documented as above. The roadmap's
    items that touch message ids (L5's lookup, L8's adapters) work with
    either answer.

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
    makes the two event types tenant-visible in M2d (design D25).
    Swappable by the override: ownership is advisory, Meta enforces it.
    Roadmap L5 (the port), L7 (the inbox) and M2d.
    Amended 2026-09-27 (coordinator, owner's delegation), with L7's
    review: after a handover to this app (a stored `control_passed`)
    newer than the last inbound message
    recorded for the conversation, the local window check treats the
    window as unknown and lets Meta decide, until the next inbound
    message or 24 hours after the handover, whichever comes first. An
    app that receives handovers without standby copies never saw the
    customer's messages to the previous owner
    (`conversation-routing/conversation-context`), and a thread is passed
    only while active (`conversation-routing/thread-control`, idle after
    24 hours: `thread-lifecycle`); losing a reply the customer is waiting
    for is worse than an occasional 131047 from Meta. The store keeps no
    source for an ownership record, so the code also trusts this app's
    own `Inbox::record_thread_owner(.., ThreadOwner::ThisApp, ..)` after
    a `take`, which Meta does not restrict to an active thread (after a
    take of an idle thread the reply meets Meta's 131047). Accepted as
    part of this decision (same day, coordinator): both cases err towards
    letting Meta decide, and the same switch turns both off; telling them
    apart needs a source on `ThreadOwnership`, a port change that would
    buy only a local refusal Meta makes anyway. Swappable by
    `ReplyChecks::trust_handover(false)` (an app with standby copies) or
    `ReplyChecks::window(false)`; the ownership refusal is its own
    `ErrorKind::ThreadOwnedElsewhere`, each recording a switch
    (`InboxSink::with_recording`) and the idle timeout a setting
    (`Inbox::with_thread_idle_after`). Roadmap L7 and M2d (both landed:
    the service's tenants receive the two event types the inbox records
    from, on M2a's routes the same checks).

## Service (meta-whatsapp-server)

Found in the review of milestone M1b (43), and with Meta's account
model evolution (46).

43. **Media received by webhook, and the `phone_number_id` check.** The
    service's media routes ask Meta with `phone_number_id={pn}`, so that
    Meta acts only on that number's media and one tenant cannot reach
    another's files when one token reaches both tenants' numbers
    (`reference/media/media-api`). Meta documents that check for media
    *uploaded* on the number only. If Meta also refuses media a customer
    sent to the number (a media id received by webhook), the M2 inbox
    cannot fetch customers' files through the service: they answer `404`
    like another tenant's.
    Until M2e: every media id went through the check; nothing exempted
    a received one (since M2e: "Built", below).
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
    one lookup. Roadmap M2e.

    **Built 2026-11-02 (roadmap M2e; its choices D77–D79 decided by the
    coordinator under the owner's delegation, swappable):** the outbox
    keeps beside each `message_received` row the media id its customer
    sent (`wa_server_events.media_id`, migration 7). A media id recorded
    on the caller's own number, routed
    to its tenant, from the number's binding epoch on, is downloaded with
    `GET /{media_id}` and no `phone_number_id` (the binding read again
    first); one recorded anywhere else (another number or tenant, an
    operator-only row, before the epoch) is `404` with nothing asked of
    Meta, for a download and a deletion, whatever Meta's check would have
    answered; an id never recorded keeps the check, and a deletion always
    does. So the route is safe whichever way Meta behaves for the ids the
    service saw; for an id it never saw (purged, or from before the
    release), it is what M1b was.

    **The live check is still to run** (nothing here calls Meta's API).
    The owner or the coordinator runs it once, with a test number of a
    WABA they control and its token, and records the four answers here:
    1. send the number an image from a phone; read its media id from the
       service (`GET /v1/events?types=message_received`:
       `data.message.image.id`) or from the webhook;
    2. `GET https://graph.facebook.com/v25.0/{media_id}?phone_number_id={pn}`
       with the token, `{pn}` the receiving number: `200` (Meta's check
       admits received media) or a refusal (it is what M2e works around);
    3. the same without `phone_number_id`: `200` expected (Meta's
       documented call; what the service now sends);
    4. the same with `phone_number_id` of **another** number the token
       reaches: a refusal means the M1b check alone protected received
       media too; a `200` means it did not, and M2e's local refusal is
       what keeps one tenant from another's customers' files;
    5. through the service, `GET /v1/numbers/{pn}/media/{media_id}` with
       the receiving tenant's key: `200`, the file verified.

<!-- 46 follows 43 in this section: a list of its own. -->

46. **A number under several Messaging Accounts.** Meta's account model
    evolution (`account-model-evolution`, phase 1 from H2 2026) splits a
    WABA in two: the Messaging Account keeps the WABA's id and holds its
    templates, billing and webhook subscriptions; a new WhatsApp account
    (WAAC) holds the phone number. One number can then belong to
    several Messaging Accounts: one per partner, or one per billing
    currency (`account-model-evolution/messaging`). The service binds a
    WABA id, today a Messaging Account id, to one tenant, and stores
    each number under exactly one binding (`wa_server_numbers` is keyed
    by `phone_number_id` alone). Two Messaging Accounts of one deployment
    that share a number cannot both hold it: attaching the second moves
    the number to it when the same tenant holds the first, and is refused
    when another tenant does (design D4).
    Options: keep one Messaging Account per number in a deployment (as
    built; design D37 records it, and from M5c5 its sends carry the
    binding's id), or key numbers by `(waba_id, phone_number_id)`, so one
    number can serve several bindings, each with its own billing, and
    route an inbound event by the Messaging Account its webhook names (a
    primary-key migration of stored data, and a routing rule for events
    Meta has not documented yet: its phase 2, H1 2027, adds a
    `whatsapp_account` webhook topic).

    **Left open on 2026-10-26:** the alternative to today's rule is a
    primary-key migration of stored data, which the owner's delegation
    leaves to the owner, and its routing depends on webhooks Meta has not
    published. Today's rule stays (design D37), with its cost: Meta keeps
    templates per Messaging Account, so after a same-tenant move the
    number's sends find only the new account's templates. Roadmap L16b
    reads the phase 2 webhooks either way.
