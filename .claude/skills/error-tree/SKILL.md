---
name: error-tree
description: "How wa-rs errors are shaped — the wa_core::Error tree (thiserror nodes, anyhow opaque leaves), ErrorKind classification of Graph error codes, retry safety, in_step for multi-step flows. Use when handling, adding or classifying an error, or deciding whether something may be retried."
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
- `anyhow` only wraps errors from code we don't own
  (`TransportError::Backend`, `StorageError::Backend`, `SinkError::Delivery`,
  `Error::Other`).
- Messages never contain secrets or full bodies (`snippet()` caps at 512
  bytes).
