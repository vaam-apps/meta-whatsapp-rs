# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/) once published.

## [Unreleased] — 0.1.0

First feature set. See [docs/coverage.md](docs/coverage.md) for the full
matrix and [OPEN_QUESTIONS.md](OPEN_QUESTIONS.md) for decisions still open.

### Open questions closed

- #17 (coexistence echoes and history not recorded by the inbox):
  resolved in a3582b8; what that needs a port change for is #35.
- #34 (should `OtpConfig::namespace` be required?): decided yes, done in
  d67b3ac.
- #35 (synced coexistence history went through `append` like live
  messages): resolved by the `ConversationStore` port change below.

### Added

- **Coexistence in the CMS inbox**: `InboxSink` records the merchant's
  WhatsApp Business app messages (`MessageEchoed`: outbound, `Sent`, in the
  customer's conversation; an echoed revoke deletes the original) and the
  synchronized chat history (`HistorySynced`: each message in its documented
  direction and status, idempotent by message id, chunks in any order, a
  declined sync records nothing). One malformed history item is skipped
  and logged by position instead of failing the delivery, including when
  it made the whole `history` value arrive as `WebhookEvent::Unknown`.
  Synced history opens no local reply window and is never unread, and the
  media content Meta sends after a `media_placeholder` fills it (see
  "Changed", `ConversationStore`).
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
  and missed a misspelled stamp). After the security review of b805dac:
  raw C strings (`cr"…"`, `cr#"…"#`) are strings to both lexers; a
  `cfg(` inside a `cfg_attr` removes its item; `cfg(test)` never holds in
  `crates/wa-rs/examples/*.rs` (built as examples), whose files are now
  checked for block comments, `macro_rules!` and any `cfg` but the
  `postgres` arms like the skills' own examples.
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

### Changed

- **Breaking — `ConversationStore` records coexistence history as history**
  (`OPEN_QUESTIONS.md` #35), and revokes are their own method; three
  required methods. `append_synced` stores a batch of messages, each like
  `append` (same id rule across both, same order, same latest-message
  preview; one answer per message), but never moves `last_inbound_at` nor
  the unread count: Meta opens no customer service window for a message
  sent before onboarding, and the merchant read it in the app. `revoke`
  (below, Security) replaces `update_status(.., Deleted, ..)` for
  revokes. `fill_media_placeholder`
  gives a stored `StoredMessage::MEDIA_PLACEHOLDER` row the media content
  Meta sends later (kind, text, payload; the preview follows when it is
  the latest message), once, on its own business number. `InboxSink`
  uses both for `HistorySynced`; the memory and Postgres adapters
  implement them (Postgres without a schema change; a history batch is
  one statement), and `conversation_conformance::run` checks them, so a
  custom store that treats synced history like live messages fails it.
  Custom stores must implement the three methods. `InboxSink::with_clock`
  is new.
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
  pooled. Codes are now bound to the sending `phone_number_id` and to
  `OtpConfig::namespace` (a new field, optional at first and required
  since d67b3ac; a blank one is a config error). Upgrading changes every key once: outstanding codes answer
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
- The security review of b805dac found (all Low or informational), and
  fixed:
  - **A revoke could delete a message of the other direction.** Revokes
    were applied by message id and business number; the customer's could
    delete the business's message and an echoed one the customer's.
    `ConversationStore::revoke` also matches the direction. It does not
    match the conversation (asked for too): see `OPEN_QUESTIONS.md` #37.
  - **A revoke that arrived before its message was dropped**, and the
    message, when it came (a later history chunk, a redelivery), was
    stored with the content its sender had deleted. The revoke now leaves
    a tombstone under the message's id (`StoredMessage::tombstone`), which
    keeps the content out.
  - **History was stored one round trip per message** while the webhook
    request waited: a large sync could outlast the 60-second dedup lease,
    and Meta's retry then ran a second pass concurrently. Each chunk is
    now one `append_synced` batch (one Postgres statement), and media
    contents find their contact in an index instead of a scan per item.
  - **A device clock in the future pinned a conversation to the top.**
    Synced device timestamps are bounded by `InboxSink`'s clock plus 5
    minutes (the payload keeps Meta's value).
  - **A storage error named a message id** (Meta's ids encode the
    customer's phone number) in text the webhook handler logs. The
    Postgres adapter's status errors no longer carry it.
  - **L6 — an OTP record copied to another key verified there.** The code
    hash covered the challenge id and the code, not the store key, so
    whoever could write the store (a shared Redis) without the pepper
    could ask for a code for their own number, copy their record over the
    victim's key (another number, purpose or namespace) and verify as the
    victim. The code hash now covers the store key. Upgrading: codes in
    flight answer `Invalid` once (10-minute TTL by default).
  - `OtpConfig::validate` refuses a namespace with edge whitespace or
    control characters (two tenants that print alike), and the rustdoc
    and guides say the namespace and the purpose are server-side
    constants, never request input.
  - `Error::may_have_been_sent` said "`true` for any non-4xx status" but
    answered `false` for a Graph error on a 1xx–3xx response; it now
    answers `true` there too (unknown), so the OTP service keeps the
    challenge. A Graph error built without a status stays `false`.
- **Send decode errors no longer quote the recipient.** An unreadable
  response of `Messages::send` or `Marketing::send` (which echo the
  recipient's number) is reported without the body snippet and without
  serde's message.
