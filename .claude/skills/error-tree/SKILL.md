---
name: error-tree
description: "How meta-whatsapp-rs errors are shaped — the wa_core::Error tree (thiserror nodes, anyhow opaque leaves), ErrorKind classification of Graph error codes, retry safety, in_step for multi-step flows. Use when handling, adding or classifying an error, or deciding whether something may be retried."
metadata:
  internal: true
---

# The error tree

```
Error ─ Api(GraphApiError) → .kind(): ErrorKind   (branch here)
      ─ Http{status, body_snippet}                non-Graph error body
      ─ Transport(TransportError)                 no response
      ─ Decode{context, source, body_snippet}
      ─ Validation(ValidationError{field, reason})
      ─ Webhook / Storage / Sink / Crypto / Config
      ─ Credit(CreditError)                       Solution Partner credit line: refused, busy,
                                                  reconcile, or a revocation part-way (with report)
      ─ Step{step, source}                        multi-step flow failed at `step`
      ─ Other(anyhow::Error)                      integrator code
```

## Rules

- **Classify by `code`** (Meta's guidance), in
  `crates/wa-core/src/error/graph.rs` `ErrorKind::from_code`. A new code →
  extend the match *and* the spot-check test; cite the doc row.
- `is_retryable()` = could succeed later. **Safe to replay** is separate:
  `ErrorKind::is_rejected_before_processing()` (throttling only). The client
  never replays a non-idempotent request on a timeout.
- Never retry `EcosystemEngagementLimit` (131049) or `SpamRateLimited`
  (131048) automatically — Meta says it makes things worse.
- `MarketingOptedOut` (131050): record the opt-out; never retry.
- `CustomerServiceWindowClosed` (131047): send a template instead.
- New leaf variants need a reason in `docs/architecture.md`. Prefer an
  existing leaf.
- **Every new `Error` variant decides `Error::may_have_been_sent`** (the
  match is exhaustive on purpose): `false` only if Meta provably did
  nothing, `true` if a send may have gone out. It is the shared rule:
  integrators branch on it ("fix and resend" vs "reconcile first") and
  the OTP service removes a challenge only when it is `false`. Add a row
  per new arm to `may_have_been_sent_only_when_meta_could_have_acted`
  (a `CreditError` arm: `credit_errors_say_whether_meta_could_have_acted`).
- A refusal that is not a plain input error goes in a typed node that
  decides retry and sent-ness itself, not in a `ValidationError` (never
  retryable, never sent): the precedent is `Error::Credit(CreditError)`,
  whose `RevocationIncomplete` carries the partial report instead of
  leaving it in a log line. Reach it through a helper that looks through
  `Step` (`Error::credit()`, like `Error::graph()`), and document the
  helpers in `docs/architecture.md` § Error tree.
- `anyhow` only wraps errors from code we don't own
  (`TransportError::Backend`, `StorageError::Backend`, `SinkError::Delivery`,
  `Error::Other`).
- Messages never contain secrets or full bodies (`snippet()` caps at 512
  bytes).
