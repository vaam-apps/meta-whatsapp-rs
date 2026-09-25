---
name: meta-whatsapp-rs-production
description: "Running meta-whatsapp-rs in production - the secrets (system user token, app secrets, verify token, vault key, OTP pepper) loaded once and checked at boot, what meta-whatsapp-rs logs and never logs, Meta's limits (throughput, pair rate, messaging tiers, management rate limits, registration budget), timeouts and retries, pinning and upgrading the Graph API version, several instances (shared stores, clocks, SSE relay), and the open product decisions to read before going live. Load when preparing a deployment, reviewing security or logging of a WhatsApp integration, scaling to several instances, or upgrading the meta-whatsapp-rs revision or Graph API version."
---

# meta-whatsapp-rs-production

> **Verified against meta-whatsapp-rs d9f4c05393be9b6b7ce688efe1ad309b026fbd37 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/production.rs](examples/production.rs),
compiled and tested by meta-whatsapp-rs's own gate. Longer walkthrough:
[production guide](https://github.com/vaam-apps/wa-rs/blob/main/docs/guides/production.md).

## When to use

Before the first deployment, and at every change of instances, keys,
wa-rs revision or Graph API version.

## Secrets: load once, fail at boot

```rust
let mut app_secrets = vec![AppSecret::new(required("WA_APP_SECRET")?)];
if let Ok(previous) = required("WA_APP_SECRET_PREVIOUS") {
    app_secrets.push(AppSecret::new(previous));
}
```

| Secret | Keep it | Rotate |
| --- | --- | --- |
| system user token | secret manager | new one in Business Settings, deploy, revoke the old |
| app secret | secret manager | list old and new in `SignatureVerifier::new` during the rollout |
| verify token | secret manager | change it in the App Dashboard and your config together |
| vault key | secret manager, **not** the vault's database | `VaultKeys::with_previous`; `vault.rotate(&waba_id)` for every WABA ever onboarded, offboarded ones too (a Solution Partner's credit ledger outlives the token), and `vault.rotate_business(&business_id)` for each business revoked by id alone; then drop the old key (`wa-rs-token-vault`) |
| OTP pepper | secret manager, **not** the OTP database | invalidates codes in flight |
| merchants' tokens | the vault only | the merchant reconnects |

`AccessToken`, `AppSecret`, `VerifyToken`, `SecretBytes`, `SignupCode`,
`TwoStepPin`, `OtpPepper` and `VaultKey` redact their `Debug`; the value
comes out only through `expose_secret()`: call it at the boundary that
needs it, nowhere near a log line.

## Logs

```rust
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    .init();
```

wa-rs logs through `tracing` (`RUST_LOG=info,wa_client=debug`): requests
and retries at `debug`, rejected webhooks and sink failures at `warn`,
unparseable signed bodies at `error` — sizes, digests, field names and
error kinds only. **Never logged**: tokens, secrets, codes, PINs, query
strings, payload values. Keep it that way: never log request bodies, the
signature header, `WebhookEvent`'s `Debug`, or `expose_secret()`.
`TracingSink::new()` logs kinds; `.with_payload(true)` logs customer data.
Worth a metric: `DeliveryReport` counts (`unparsed` > 0 → alert), webhook
answers by status (a run of 503s: sinks outlast the dedup lease),
`err.kind()` of failed sends, the rate of `Unknown` events.

## Solution Partner checklist

A deployment funding merchants with its credit line
(`wa-rs-embedded-signup`, `references/solution-partner.md` there):
onboarding only through `onboard_with_approval`; every `PartnerRemoved`
wired to `revoke_credit_line` at once, coexistence disconnections included
(unless its `solution_partner_business_ids` omit your business),
`PartnerAppUninstalled` of **your** app to `offboard`; the key rotation
above; an alert on `CreditError::Reconcile` and on a `RevocationIncomplete`
that is not retryable (`ErrorKind::Unknown`: a person checks Meta Business
Suite), and a retry of one that is; a staff-only admin action for a lost
share Meta never lists (`clear_pending_share`, with an operator id).

## Limits and retries

| Meta's limit | Value | In meta-whatsapp-rs |
| --- | --- | --- |
| throughput per number | 80 msg/s by default | `RateLimited` (130429), replayed within the retry budget |
| same user | about 1 message per 6 s | `PairRateLimited` (131056), replayed within the budget |
| new users per 24 h | the portfolio's messaging tier | not tracked: count in your queue |
| management endpoints | rate limited per app and WABA | cache template and number lists |
| number registration | 10 per 72 h | 133016, a 72 h lock, never retried |
| webhook answers | fast (median ≤ 250 ms) | keep sinks quick (`wa-rs-live-updates`) |

Defaults: 30 s timeout, 3 retries (250 ms base, 8 s cap). Set them once:

```rust
meta_whatsapp_rs::client_builder()?
    .access_token(settings.system_user_token.clone())
    .api_version(ApiVersion::new(25, 0)) // moves only when you change it
    .timeout(Duration::from_secs(15))
```

Sends are never replayed after a timeout or 5xx: a queue on top must be
idempotent (tag sends with `callback_data`, reconcile with status webhooks;
`wa-rs-errors`).

## Versions

- `ApiVersion::DEFAULT` is v25.0; pin meta-whatsapp-rs by `rev`, and the Graph
  version moves only with it — or hold one with
  `.api_version(ApiVersion::new(25, 0))`. Use the same version in the
  Embedded Signup page's `FB.init`.
- Before moving either: read Meta's changelog, run your tests against a
  test WABA, watch the `Unknown` event count.
- Each skill is stamped with the commit it was verified against: after
  moving the `rev`, re-read the skills whose stamps differ, and the
  struck-through notes in them.

## Several instances

Shared stores for everything (`wa-rs-storage`); hosts on NTP (Postgres and
Redis expire by their own clock); one `Client` per process, `with_token`
per merchant; the SSE broadcast channel is per process (relay events or
pin merchants to an instance); a `ChannelSink` queue dies with its
process. A load balancer in front of the webhook: HTTPS with a valid
certificate, body limit ≥ 3 MiB, no body rewriting or decompression,
timeouts longer than your slowest sink.

## Pitfalls

- A blank secret read from an unset variable: fail at boot (the example's
  `required`), not on the first webhook.
- Upgrade crossings (read the skill named before moving the `rev`):
  e40b86f invalidates OTP codes in flight once (`wa-rs-otp-login`);
  4b47bf7 changes a custom `ConversationStore`'s `update_status`
  signature (`wa-rs-cms-inbox`); 6d50701 and a9593f3 add three required
  `ConversationStore` methods, `append_synced`, `fill_media_placeholder`
  and `revoke` (`wa-rs-storage`), and af5b1f8 tightens their conformance
  suite; 8238853 makes OTP codes in flight
  answer `Invalid` once; 8238853 and 7e4801f refuse a namespace with edge
  whitespace, control or format characters at `OtpService::new`, and
  fixing it restarts codes and limits (`wa-rs-otp-login`). Lossless
  message content (PR #7, 2026-09-25) is Postgres migration 3, one-way:
  back up first (a rollback is a restore, losing what was recorded
  since), stop the older instances that write to the inbox tables, drop
  your own objects on the content columns, run `migrate` once from a job
  with a lock timeout, then start (steps: `wa-rs-storage`). An older
  instance left running fails on every content statement (500s Meta
  redelivers, replies sent but not recorded). Upgrades back-fill
  nothing: rows and summaries recorded before stay as written (a U+FFFD
  an older revision stored for a NUL stays one).

## What meta-whatsapp-rs does not do

Read [OPEN_QUESTIONS.md](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md)
before going live: the OTP issue limit, PIN policy, the missing
dead-letter path for webhook batches, token refresh, Redis TLS. No
metrics exporter, no health endpoint, no secret manager integration. A
revoked message keeps its content in the inbox (decided 2026-09-25;
`wa-rs-cms-inbox`). ~~Whether the OTP namespace becomes
required~~: decided in d67b3ac (2026-09-24), it is (`wa-rs-otp-login`).
~~The provisional U+0000 replacement~~: until the pull request that
made U+0000 lossless (PR #7, 2026-09-25); message content keeps it
(`wa-rs-storage`).

## Related skills

`wa-rs-setup`, `wa-rs-errors`, `wa-rs-storage`, `wa-rs-token-vault`,
`wa-rs-webhook-endpoint`, `wa-rs-live-updates`.
