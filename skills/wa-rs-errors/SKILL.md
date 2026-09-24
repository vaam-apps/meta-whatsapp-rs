---
name: wa-rs-errors
description: "Handling wa-rs errors correctly - the Error tree (Api, Http, Transport, Decode, Validation, Step, ...), classifying failures with err.kind() and ErrorKind instead of message text, what each Graph error code means (131047 window closed, 131050 opted out, 131049 per-user limit, 131062 BSUID refused, 133016 registration lock, ...), why a failed send is never replayed after a timeout or 5xx, and how a job queue decides between resend, never and reconcile. Load when writing a match on a wa-rs error, a retry or job queue around sends, or when an error code from Meta needs interpreting."
---

# wa-rs-errors

> **Verified against wa-rs 4eb93c9bd63812221e75ad0920b6e2cb98ea0dd6 (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/handle.rs](examples/handle.rs), compiled and
tested by wa-rs's own gate. Every code, its `ErrorKind` and what to do:
[references/error-kinds.md](references/error-kinds.md).

## When to use

Whenever code matches on a `wa_rs::Error`, retries anything, or runs a job
queue that sends messages.

## The tree

Every fallible call returns `wa_rs::Result<T>` = `Result<T, wa_rs::Error>`:

```text
Error::Api(GraphApiError)   Meta's error object; .kind() classifies .code
Error::Http { status, .. }  a non-Graph error body (an HTML 502)
Error::Transport(..)        no answer: timeout, connect, integrity (media hash)
Error::Decode { .. }        a 2xx body of an unexpected shape
Error::Validation(v)        refused locally, NOTHING was sent; v.field is the JSON path
Error::Webhook / Storage / Sink / Crypto / Config
Error::Step { step, source } a multi-step flow (onboarding) stopped at `step`
Error::Other(anyhow)        your code, Typst's RenderError
```

`err.graph()` returns the `GraphApiError`, also through `Step`; its `code`
tells apart codes that share a kind. `ErrorKind` is `#[non_exhaustive]`:
keep a `_` arm.

## Branch on the kind

```rust
let template = TemplateMessage::new("order_shipped", "en_US").body([Parameter::text(order_no)]);
let msg = OutboundMessage::template(to, template).callback_data(format!("order:{order_no}")); // echoed on status webhooks
match messages.send(&msg).await {
    Ok(sent) => sent
        .message_id()
        .cloned()
        .map_or(Next::Reconcile, Next::Accepted),
    Err(Error::Validation(v)) => Next::FixInput(v.field),
    Err(e) => match e.kind() {
        ErrorKind::TemplateNotFound | ErrorKind::TemplateParameterMismatch => Next::FixTemplate,
        ErrorKind::MarketingOptedOut | ErrorKind::EcosystemEngagementLimit => {
            Next::StopMarketing
        }
        _ if !e.may_have_been_sent() => Next::Rejected(e),
        _ => Next::Reconcile,
    },
}
```

`Error::may_have_been_sent()` is the line between the two: `false` for a
Graph error on a 4xx response, a local validation or configuration error,
or a connection that never opened — nothing went out, fix and resend.
`true` for a timeout, a 5xx, an unreadable 2xx or anything unknown — the
message may be on its way; reconcile before resending.

## Retries: "could succeed later" is not "safe to repeat"

- `err.is_retryable()` answers *could the same request succeed later*.
- The client retries **idempotent** requests (GET, DELETE, POSTs that set
  a value) on any retryable error, with jittered backoff
  (`RetryPolicy::default()`; `RetryPolicy::NONE` turns it off).
- A **send** (`Messages::send`, `Marketing::send`, uploads) is replayed
  only when the error proves Meta did nothing: `RateLimited`,
  `PairRateLimited`, HTTP 429 (`ErrorKind::is_rejected_before_processing`).
  **A timeout or 5xx on a send is returned, never replayed**: a duplicate
  OTP or order confirmation is worse than an error. The example's tests
  prove both behaviours.
- From a job queue, resend only what Meta provably refused and may accept
  later; reconcile the rest with status webhooks first (match
  `biz_opaque_callback_data`, see `wa-rs-webhook-events`):

```rust
pub fn after_failed_send(e: &Error) -> Resend {
    if matches!(e, Error::Validation(_)) {
        return Resend::Never; // refused locally: nothing was sent
    }
    if e.may_have_been_sent() {
        return Resend::ReconcileFirst;
    }
    if e.is_retryable() {
        Resend::Later
    } else {
        Resend::Never // includes 131049, 131050 and 131048: never auto-retry
    }
}
```

## Pitfalls

- **Never match on message text or HTTP status**: Meta changes wording;
  the code is the contract.
- `Error::Decode` from `Messages::send` means Meta answered 2xx: treat the
  message as sent. Its snippet is withheld (the response names the
  recipient); only the error category and position remain.
- The inbox's local 24-hour refusal is `Error::Validation` whose kind is
  `ErrorKind::CustomerServiceWindowClosed`, the same as Meta's 131047.
  Branch on the kind, or on `ValidationError::is_customer_service_window_closed()`
  to tell the local refusal apart — not on `v.field`.
- `EcosystemEngagementLimit` (131049) and `SpamRateLimited` (131048) are
  not retryable on purpose; `Registration` includes 133016 (a 72-hour
  lock), which is never retried.
- The same `GraphApiError` arrives in webhooks: `Status::errors` of a
  failed status, `InboundMessage::errors`, `WebhookEvent::ErrorReported`.
  An opt-out can show up there instead of on the send.
- Every public `validate()` returns `Result<(), ValidationError>`; `?`
  lifts it into `wa_rs::Error`. `OtpService::new` reports a bad config as
  `Error::Config`.

## What wa-rs does not do

- No retry scheduler, outbox or dead-letter queue for sends: a job queue
  on top is yours, and it must be idempotent (tag sends with
  `callback_data`).
- Webhook sink errors fail the whole batch; a dead-letter design is
  [open question 30](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#webhooks-and-live-updates).

## Related skills

`wa-rs-send-messages`, `wa-rs-send-templates`, `wa-rs-marketing` (opt-outs),
`wa-rs-webhook-events` (errors in status webhooks), `wa-rs-embedded-signup`
(`Error::Step`), `wa-rs-testing` (scripting errors).
