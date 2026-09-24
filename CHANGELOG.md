# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/) once published.

## [Unreleased] — 0.1.0

First feature set. See [docs/coverage.md](docs/coverage.md) for the full
matrix and [OPEN_QUESTIONS.md](OPEN_QUESTIONS.md) for decisions still open.

### Open questions closed

- #3 (Tech Provider or Solution Partner?): decided by the owner on
  2026-09-24, "support both, per deployment"; Solution Partner mode is
  below.
- #17 (coexistence echoes and history not recorded by the inbox):
  resolved in a3582b8; what that needs a port change for is #35.
- #34 (should `OtpConfig::namespace` be required?): decided yes, done in
  d67b3ac.

### Added

- **Solution Partner onboarding** (per deployment): configure
  `EmbeddedSignup::solution_partner(SolutionPartner::new(system_token,
  system_user_id, credit_line_id))` and every onboarding shares your
  credit line between `subscribe_app` and `register_phone`, Meta's
  order. `CreditSharing::ShareAndAttach` (default, Meta's current method)
  first adds your system user to the WABA (`assign_system_user`), then
  calls `whatsapp_credit_sharing_and_attach` with the system user token;
  `CreditSharing::ShareThenAttach` shares with the verified owner business
  and attaches with the merchant's token. The currency
  (`OnboardingRequest::currency`, else `SolutionPartner::default_currency`)
  is required before the code is exchanged, and the first one is sealed:
  another is refused later. `share_credit_line` checks before it posts,
  in `onboard` and `resume` alike (the owner's records and the recorded
  allocation, each with its `request_status`, against the WABA's
  `primary_funding_id`), under a per-WABA lease, so a timed-out share is
  resumed without posting it twice and two concurrent onboardings cannot
  both post (`refusals::CREDIT_STEP_BUSY`). **A revoked business is not
  funded again** (marked by `revoke_credit_line`, or only `DELETED`
  records on Meta's side: `refusals::CREDIT_LINE_REVOKED`,
  `EmbeddedSignup::is_credit_line_revoked`) unless the request says
  `OnboardingRequest::reshare_after_revocation()`. The allocation is
  returned (`Onboarded::allocation_config_id`) and kept in a sealed credit
  ledger (`TokenVault::credit` → `StoredCredit`,
  `TokenVault::revoked_business` → `RevokedBusiness`) that
  `TokenVault::delete` leaves, so the token record keeps its previous
  format. `EmbeddedSignup::revoke_credit_line(&waba, owner_business_id,
  &vault)` revokes from the recorded owner (or, when nothing is recorded,
  a signed webhook's `owner_business_id`; a contradicting one revokes
  nothing; an unreadable token or credit record, or a failed lookup, does
  not stop what the other sources can revoke), and
  `EmbeddedSignup::offboard` revokes first and deletes the token second
  (`Offboarded`), so `PARTNER_APP_UNINSTALLED` and `PARTNER_REMOVED` end
  revoked in either order. The Tech Provider flow is
  unchanged, request for request. `SolutionPartner`'s system token is
  private and never in `Debug`.
- **`EmbeddedSignup::onboard_with_approval`**: your check of the verified
  WABA, owner business and numbers (`VerifiedOnboarding`) runs after
  `verify_assets` and before `store_token`; a refusal is step `approve`
  and nothing is stored, subscribed or shared. For a Solution Partner it
  is where tenant checks belong: after `onboard` the line is attached.
- **`wa_client::credit_lines`**: `CreditLines` (`Client::credit_lines`)
  with `list`/`list_stream` (`extendedcredits`), `share_and_attach`,
  `share`, `attach`, `receiving_credential`, `primary_funding`,
  `allocations_for` (accepts the page's single object and a
  `{"data": [...]}` page, follows cursors, never quotes the business name
  in an error), `revoke`, `revoke_for_business` (→ `CreditRevocation`:
  only active records naming that business, each confirmed `DELETED`,
  every one attempted before a failure is returned, records naming no
  business reported rather than revoked), `allocation_status`, and
  `is_shared`;
  `WabaCurrency` (the six supported codes, `Other` only on purpose). New
  ids in `wa_core::ids`: `CreditLineId`, `AllocationConfigId`, `FundingId`
  (both sides of the `is_shared` comparison: a receiving credential and a
  WABA's `primary_funding_id`, so neither can be compared with an
  allocation or WABA id by mistake) and `SystemUserId`
  (`SolutionPartner::system_user_id`; `Waba::assign_user` still takes a
  `&str`, as it assigns any business user).
- **Coexistence in the CMS inbox**: `InboxSink` records the merchant's
  WhatsApp Business app messages (`MessageEchoed`: outbound, `Sent`, in the
  customer's conversation; an echoed revoke deletes the original) and the
  synchronized chat history (`HistorySynced`: each message in its documented
  direction and status, idempotent by message id, chunks in any order, a
  declined sync records nothing). One malformed history item is skipped
  and logged by position instead of failing the delivery, including when
  it made the whole `history` value arrive as `WebhookEvent::Unknown`.
  Synced inbound history still counts towards the local window and unread
  count, and media contents are not merged into recorded placeholders:
  both need a `ConversationStore` port change (`OPEN_QUESTIONS.md` #35).
- **Adoption helpers**: `Error::may_have_been_sent()` (whether a failed send
  could still have been delivered — the line between "fix and resend" and
  "reconcile first"), `Inbox::window_is_open` (the reply window by the
  inbox's own clock, the same check `reply` makes), a `testing` feature on
  `wa-rs` (no second pinned `wa-core` dev-dependency), and a `redis`
  re-export next to the `sqlx` one.
- **Skills gate, three holes closed**: a Rust block may no longer quote the
  inside of a string (raw or not) or a comment of an example, nor an item
  under a `cfg` that `--all-features` never enables (the
  `#[cfg(not(feature = …))]` arms of `crates/wa-rs/examples/*.rs`); and
  every `references/*.md` must carry a well-formed stamp under its title,
  like its `SKILL.md` (`just skills-check` alone passed a malformed date
  and missed a misspelled stamp).
- **Granular consumer skills**: 24 task-shaped skills with compiled example
  files, and a gate (`crates/wa-rs/tests/skills.rs`, `just skills-check`)
  that keeps snippets, API names, links, frontmatter and stamps true. The
  developer skills in `.claude/skills/` are marked internal so
  `npx skills add vaam-apps/wa-rs` offers only the consumer skills.

- **Workspace**: `wa-core` (error tree, ids, ports), `wa-client`,
  `wa-webhooks`, `wa-adapters`, `wa-typst`, and the `wa-rs` facade with a
  prelude and a `client(token)` shortcut. Graph API v25.0 by default.
- **Error tree**: `thiserror` nodes with `anyhow` opaque leaves;
  `ErrorKind` classifies every documented Graph error code; retry policy
  never replays a non-idempotent send on a timeout.
- **Embedded Signup**: launch options, session events, code exchange,
  `debug_token`, verification of browser-supplied ids (grants, phone
  ownership, owner business), encrypted token vault with key rotation and a
  phone → WABA index, tenant-bound signup sessions, `onboard`/`resume`,
  coexistence sync.
- **Webhooks**: verify-token and `X-Hub-Signature-256` checks (multi-secret,
  fail-closed), typed payloads for every documented field, BSUID
  identities, normalized events, leased dedup, PII-free logs, axum router
  and SSE.
- **Messages and media**: every message type incl. interactive, commerce,
  flows, calling and group sends; read receipts and typing indicators;
  verified streaming media downloads; Resumable Upload.
- **Templates and authentication**: management, library, migration, typed
  definition and send-time components; authentication templates; an OTP
  service bound to E.164 numbers with hashed storage, CAS-counted attempts,
  cooldown and a per-number issue limit.
- **Also**: In-App Signup, WABA and phone number management, business
  profile, commerce settings, Flows (management + data-endpoint crypto via
  aws-lc-rs), Marketing Messages API, analytics, QR codes, block users,
  Groups, Calling signalling, Direct Send.
- **CMS inbox** (`wa_rs::inbox`): webhook events into a conversation store,
  24-hour-window-checked replies with the merchant's token.
- **Adapters**: reqwest transport; memory, Postgres and Redis stores with
  executable conformance suites; channel, broadcast, fan-out, filter, fn and
  tracing sinks.
- **Typst**: deterministic, sandboxed invoice/receipt/voucher rendering to
  PDF/PNG.
- **Examples**: send a message, CMS inbox server, Embedded Signup server
  (both bearer-authenticated per tenant, on `127.0.0.1` by default), OTP
  login, invoice document.
- **Tooling**: `just ci` gate, CI running it, `cargo xtask meta-docs`,
  Claude Code dev container with a fail-closed default-deny firewall,
  project skills and agents, consumer skills (`npx skills add vaam-apps/wa-rs`).

### Deprecated

- `EsVersion::V2`, `V3`, `V2PublicPreview` and `V3PublicPreview`: Meta
  deprecates Embedded Signup v2 and v3, including their public previews,
  on 2026-10-15 (`embedded-signup/onboarding-customers-as-a-solution-partner`).
  Use v4, which needs no `version`.

### Changed

- **Breaking — `WebhookEvent::AccountUpdated` names the right WABA.** Its
  `waba_id` (and `WebhookEvent::waba_id()`) was the entry id, which in
  Meta's examples of every update with a `waba_info` (`PARTNER_ADDED`,
  `PARTNER_REMOVED`, `PARTNER_APP_INSTALLED`, `PARTNER_APP_UNINSTALLED`,
  `AD_ACCOUNT_LINKED`, `MM_LITE_TERMS_SIGNED`) is a business portfolio, not
  the WABA: a `PARTNER_REMOVED` routed by it found no merchant. It is now
  `Option<WabaId>`: `waba_info.waba_id` when the update has a `waba_info`,
  `None` if that names no WABA, and the entry id (as before) for updates
  without one, which is what every such example shows. The entry id is kept
  verbatim in the new `entry_id` field. Because the event's JSON changed,
  the `dedup_key` of an `account_update` delivered before the upgrade and
  redelivered after it differs once. The partner fixtures assert every
  documented `waba_info` field.
- **Breaking — one type per concept** (conventions review #9):
  `wa_client::common` defines `MediaSource`, `FlowAction` and
  `QualityRating` once; `messages`, `templates` and `phone_numbers`
  re-export them, so the old paths still name them. The Flow message's
  `FlowAction` was a closed `Copy` enum and is now the templates' open one
  (`Other(String)`, case-insensitive, not `Copy`). The phone number's
  `QualityRating` was `Copy` and turned any unknown value into `Unknown`;
  `Unknown` is now only the documented `UNKNOWN`, anything else is
  `Other(String)`. `marketing::OnboardingRequest` (the Intent API's
  answer) is now `marketing::OnboardingRequested`, so it no longer shares a
  name with `embedded_signup::OnboardingRequest`.
- **Breaking — typed ids** (conventions review #17): `FlowButton::flow_id`
  is an `Option<FlowId>` and `FlowButton::by_id` takes `impl Into<FlowId>`;
  `FlowMedia::media_id` is a new `FlowMediaId` (a Flow upload's UUID, not a
  Graph `MediaId`); `TemplateGroupAnalyticsQuery::template_group_ids` and
  `TemplateGroupDataPoint::template_group_id` use a new `TemplateGroupId`;
  `Client::business_profile_node` takes `impl Into<BusinessProfileId>` (new),
  and `BusinessProfileNode::id`, `Profile::id` and `ProfileNodeUpdated::id`
  return it.
- **Breaking — every list takes its cursor the same way** (conventions
  review #7): `after`/`before` in the list's query, sent by the one-page
  method and refused by its stream (which manages them; the stream's
  single item is a `ValidationError`). New query types: `ListSignups`
  (`Signups::list`/`list_stream` took an `Option<u32>`),
  `AssignedUsersQuery` (`Waba::assigned_users`/`_stream` took a
  `&BusinessId`), `ListFlows` and `ListFlowAssets` (`Flows::list` and
  `Flow::assets` took an `Option<&str>`), `ListClientWabas`
  (`MarketingBusiness::client_wabas_with_status` took
  `(&[OnboardingStatus], Option<&str>)`, its stream `&[OnboardingStatus]`).
  `PhoneNumbersQuery`, `WabaListQuery` (both with `after`/`before`
  builders), `TemplateAnalyticsQuery`, `TemplateGroupAnalyticsQuery` and
  `GroupAnalyticsQuery` (now with a `new`) gained the two fields.
  `Templates::list_stream` refuses a cursor it used to ignore.
  `Waba::subscribed_apps` and `Templates::library` take none: their pages
  document no pagination.
- **Breaking — the OTP namespace is required** (decided for
  `OPEN_QUESTIONS.md` #34, now closed): `OtpConfig::namespace` is a
  `String` (was `Option<String>`), `OtpConfig::new(namespace)` builds the
  defaults, and `OtpConfig` no longer implements `Default`, so no service
  can share a scope with another by forgetting it; a blank one is still an
  `Error::Config`. Migration: `OtpConfig { namespace: Some(ns),
  ..OtpConfig::default() }` becomes `OtpConfig::new(ns)` and derives the
  same store keys (outstanding codes stay valid); a service that used
  `None` must pick a namespace, and its codes in flight become `NotFound`
  once. The `otp_login` example reads `WA_OTP_NAMESPACE`.
- `Error::may_have_been_sent` is `false` for a throttling Graph error
  (`ErrorKind::is_rejected_before_processing`) on any status, as the retry
  policy already assumed when it replays a send; the OTP service uses it
  instead of a private copy. The only difference a send could reach is
  gone: a 1xx–3xx answer without a Graph error used to drop the challenge
  and now keeps it (unknown, so the code may be on its way).

Breaking for anyone pinned to an earlier revision (nothing is released
yet): `ConversationStore::update_status(phone_number_id, id, status, at,
error)`; the `validate()` of `TemplateDefinition`, `TemplateEdit`,
`TemplateMessage`, `AuthenticationTemplate`, `AuthenticationUpsert` and
`OtpConfig` returns `Result<(), ValidationError>` like every other public
`validate()`; `OtpConfig` has a `namespace` field; `.xtask` is a workspace
of its own (`cargo xtask …` still works through the alias). Added:
`ValidationError::{CUSTOMER_SERVICE_WINDOW, customer_service_window_closed,
is_customer_service_window_closed}`,
`MarketingBusiness::client_wabas_with_status_stream`, and
`wa_webhooks::SIGNATURE_HEADER` without the `axum` feature.

Consumer skills: the seven broad skills became 24 task-shaped ones
(`wa-rs` routes to the others; `wa-rs-messaging`, `wa-rs-templates-otp` and
`wa-rs-webhooks` are gone, split into `wa-rs-send-messages`,
`wa-rs-interactive-messages`, `wa-rs-media`, `wa-rs-templates`,
`wa-rs-send-templates`, `wa-rs-otp-login`, `wa-rs-webhook-endpoint`,
`wa-rs-webhook-events`, `wa-rs-live-updates`, …). Each ships compiled,
tested example code; `just ci` checks excerpts, frontmatter, links, names
and stamps (`just skills-check`). The developer skills in `.claude/skills/`
are marked `internal`, so `npx skills add vaam-apps/wa-rs` no longer offers
them.

### Security

Found and fixed during review, before any release: an OTP account takeover
through Meta's country-code prefixing (OTPs now require E.164 with `+`); an
inbox reply to a `wa_id` reaching the wrong person for the same reason;
id path injection into Graph URLs (segment-safe path builder); webhook dedup
markers that could swallow a retried event for 7 days (now a lease);
forged signatures accepted with a blank app secret; `client_secret` leaks via
reqwest error URLs, redirect `Referer` headers and `Debug` output; an
unverified `business_id` stored from the browser; a Postgres CAS that could
resurrect a used OTP challenge; a devcontainer firewall that failed open.
The first firewall fix covered only one trigger (an ad-blocking resolver's
sinkholed answers); any other error before the last lines, such as a
rate-limited `api.github.com/meta`, still left egress wide open. The script
now installs the default-deny policies first and fails closed on any error,
with a dated snapshot of GitHub's ranges for when the live list is
unavailable.

The example servers, which integrators copy, were open: both listened on
every interface, `cms_inbox` served any merchant's conversations and
replies to anyone, `embedded_signup` took the tenant from the query string
(one merchant could resume another's onboarding with their own PIN), and
one shared PIN registered every merchant's number. They now require a
tenant bearer token, check that the tenant owns the phone number before
reading the token vault, take the PIN from the merchant per attempt, and
bind to `127.0.0.1` unless told otherwise. The Flows endpoint's handler
example now verifies `X-Hub-Signature-256` before decrypting, and the
Redis store's docs require `noeviction` (a `volatile-*` policy silently
evicts OTP issue logs and dedup markers).

The final security review of 8ee6fab found, and fixed before 7940d15:

- **H1 — an OTP code was valid across merchants.** Store keys were
  HMAC(pepper, number | purpose), so `OtpService`s sharing a store and a
  pepper (several merchants of one integrator) shared records: a code
  merchant A sent verified at merchant B for the same number and purpose,
  an issue at A replaced B's code, and cooldowns and issue limits were
  pooled. Codes are now bound to the sending `phone_number_id` and an
  optional `OtpConfig::namespace` (new field; a blank one is a config
  error). Upgrading changes every key once: outstanding codes answer
  `NotFound` and issue logs restart.
- **M1 — one NUL in a customer's message blocked the whole webhook batch on
  Postgres.** Postgres cannot store U+0000, so `InboxSink` failed every
  delivery of that batch until Meta dropped it after 7 days, with every
  other event in it. `InboxSink` and `Inbox::send` now store U+0000 in
  message content as U+FFFD (lossy, and provisional: the choice was
  reserved for the maintainer, `OPEN_QUESTIONS.md` #18).
- **L1 — a status or revoke on one number could change another number's
  message.** `ConversationStore::update_status` matched the message id
  alone; it now takes the business `phone_number_id` first and both stores
  match on it (breaking for custom stores).
- **L3 — the body was read before the signature was looked at.** The axum
  route buffered an unsigned request up to the body limit (and answered
  `413` rather than `401` when it was larger). A missing or malformed
  `X-Hub-Signature-256` is now refused with `401` before the body is read.
- **Credential allowlist narrowed.** Tokens went to any `*.fbsbx.com`,
  `*.facebook.com` or `*.whatsapp.net` host on any port, and to
  `graph.facebook.com` even when the client was configured for a proxy.
  Now only the configured Graph endpoint's origin and
  `https://lookaside.fbsbx.com` on the default port (media downloads)
  receive one; the host is compared exactly, so subdomains, suffix and
  trailing-dot tricks and IDN look-alikes are refused before sending.
- **OTP codes go through `Messages::send`.** `OtpService` posted its own
  body, so `OutboundMessage::validate` never ran on it and it kept a
  private copy of the response decoding.
- **Send decode errors no longer quote the recipient.** An unreadable
  response of `Messages::send` or `Marketing::send` (which echo the
  recipient's number) is reported without the body snippet and without
  serde's message.
