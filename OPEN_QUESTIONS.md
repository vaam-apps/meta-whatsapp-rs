# Open questions

Decisions the implementation deliberately did **not** make. Each entry says
what the code does today, so nothing here is a hidden default. Close an
entry by deciding it in an issue/PR and deleting it here.

## Naming and publishing

1. **Crate names.** `wa-rs` is taken on crates.io by an unrelated project,
   so the workspace is `publish = false`. Pick names (e.g. a common prefix)
   before the first release.
2. **`wa_rs::client` is both a module and a function** (the production
   client shortcut). Rustdoc links need `mod@`/`fn@`. Alternative:
   `wa_rs::connect(token)`.

## Embedded Signup (onboarding merchants)

3. **Tech Provider or Solution Partner?** Only the Tech Provider flow is
   implemented (exchange → verify → store → subscribe → register). A
   Solution Partner must also share its credit line with each onboarded
   business (system user token, `receiving_business_id` = the verified owner
   business id, which is already stored). Merchants of a Tech Provider must
   add their own payment method before they can message.
4. **Two-step PIN policy.** The caller supplies the 6-digit PIN on every
   `onboard`/`resume`; nothing generates or stores PINs. Numbers that already
   have a PIN need the merchant's current one.
5. **Multi-WABA signups.** Only the claimed `waba_id` (else the first of
   `waba_ids`; with no claim, the newest granted WABA) is onboarded.
6. **One WABA shared by several tenants.** The vault is keyed by WABA and
   knows no tenants; the last onboarding wins.
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

13. **Issue limit default.** 5 codes per sliding hour per number and purpose
    (plus a 30 s cooldown and 5 attempts per code): ~0.06 %/day brute-force
    success on 6 digits. Confirm or tune; opting out is explicit.
14. **Pepper custody.** The HMAC pepper (`SecretBytes`) is supplied by the
    integrator; changing it invalidates outstanding codes.

## Webhooks

15. **Dedup lease length.** 60 s by default (`with_lease`): a delivery that
    finds another request mid-delivery gets 503 and Meta retries.
16. **Blank verify token** fails when a verification request arrives, not at
    startup — failing early would change the handler builder's API.
17. **Coexistence echoes and history** are parsed but not recorded by the
    inbox.

## Storage

18. **Postgres and U+0000.** Postgres cannot store the NUL character; a
    webhook payload containing one is refused permanently (memory and Redis
    accept it). Options: strip/replace NUL (lossy) or store payloads as
    `bytea` (schema change).
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
