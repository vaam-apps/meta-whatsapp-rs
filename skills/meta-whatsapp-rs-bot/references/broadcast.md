# Paced broadcasts

> **Verified against meta-whatsapp-rs 63eee9761b545a2d6ac0ef3507f358bb9c9ab28b (2026-09-26).** Also checked against Meta's
> `throughput`, `about-the-platform` (pair rate limits), `support/error-codes`,
> `templates/marketing-templates/per-user-limits` and `messaging-limits` pages
> as mirrored on 2026-09-24.

A `Broadcast` sends one message (`BroadcastBuilder::content`) or one per
recipient (`BroadcastBuilder::compose`) from one business number. Every
send, a retry too, first waits for a slot of the number's `Pacer`; up to
`BroadcastBuilder::concurrency` sends (default 32) are in flight at once,
so the rate is reached even when each send takes a while. `run` drives it
in your task: spawn it and keep the `BroadcastHandle`
(`BroadcastHandle::progress`, `BroadcastHandle::cancel`).

**Each person once**, by default: a recipient listed again (same phone
number by its digits, same BSUID, same group, or a phone and a BSUID one
`Recipient::PhoneAndUser` carried) gets no second message; its line is
`Outcome::Duplicate`. `BroadcastBuilder::dedupe(false)` turns it off.

## The pacer

```rust
pub fn shared_pacer(upgraded: PhoneNumberId) -> Pacer {
    Pacer::new(TokenBucket::new(Rate::DEFAULT).rate_for(upgraded, Rate::HIGHER_THROUGHPUT))
}
```

- Meta's throughput per registered number: `Rate::DEFAULT` (80 a second),
  `Rate::HIGHER_THROUGHPUT` (1,000, after Meta's automatic upgrade),
  `Rate::BUSINESS_APP` (20, a number also in the WhatsApp Business app).
  Meta documents no mapping from a number's reported level to a rate:
  set it yourself (`TokenBucket::rate_for`), for example when a quality
  webhook says `PhoneNumberQualityEvent::ThroughputUpgrade`.
- `TokenBucket` spaces sends evenly: any one-second window holds at most
  the rate. `TokenBucket::burst` lets a few go at once, at the cost of
  up to `burst - 1` more in a window.
- **One pacer per process**, shared: `BroadcastBuilder::pacer` for every
  broadcast and `BotBuilder::pacer` for the bot (its replies, refusals,
  read receipts and typing indicators, through `PacedOutbound`). Group
  operations: `PacedGroups::new(client.groups(number), pacer)` and its
  `PacedGroup` (Meta documents no rate for those; pacing them in the same
  budget is a choice). Any other call: `Pacer::acquire` first.
- **Several replicas** each have their own budget: give each its share
  (`Rate::per_second`), or implement `RateLimiter` (two methods,
  `RateLimiter::reserve` and `RateLimiter::slow_down`) over a store they
  share. None ships.
- Don't give a broadcast a `PacedOutbound`: it paces itself, and each
  send would take two slots.

## What a failed send becomes

`BroadcastPolicy` decides; the default is `Backoff`, read from the
error's kind, never its text:

| Meta answers | `Backoff` does |
| --- | --- |
| `131056`, pair rate limit | defers that recipient 1, 4, 16, 64 s (Meta's `4^X`); the others go on |
| `130429` throughput, other `RateLimited` codes | retries after 1, 2, 4 … s (up to 60), and the pacer halves the rate for 30 s |
| `131057`, number in maintenance (a throughput upgrade, up to a minute) | retries every 20 s (`Backoff::MAINTENANCE_RETRY`); the pacer slows down |
| `131048`, spam | reports it, never retries; the pacer slows down |
| `131049`, per-user marketing limit | reports it for that recipient, never retries (Meta: wait 24 hours) |
| `Backoff::STOPS`: token, permission, account, classification limit, payment | the run stops (`Ended::Stopped`); the rest are skipped |
| anything else not retryable | reports it (`Outcome::Failed`) |

**The library's rule, whatever the policy:** a recipient is resent only
when the error is retryable and `Error::may_have_been_sent` is false. A
timeout or a 5xx after the request reached Meta is `Outcome::Failed` with
`may_have_been_sent()` true: reconcile it with the status webhooks
(`OutboundMessage::callback_data` in `compose`) before sending again.
`BroadcastBuilder::client` turns the client's own replays off
(`Client::with_retry(RetryPolicy::NONE)`) so every retry is paced; do the
same for an outbound of yours over a client.

`Outcome::Sent` is Meta's acceptance, not delivery: most `131049`
refusals arrive later as a `failed` status webhook. Meta's daily
messaging limit (unique users per 24 hours, per portfolio) is Meta's to
enforce; nothing here counts it.

## Cancel, stop, report

`BroadcastHandle::cancel`: no send starts afterwards (a wait for a slot
or a retry ends at once); sends in flight finish and are reported.
Recipients never sent are `Outcome::Skipped`. The `BroadcastReport` has
one `RecipientReport` per recipient, in order (`index`, `attempts`,
`outcome`), its counts (`BroadcastReport::progress`) and `Ended`. For a
long list, `BroadcastBuilder::report_to(tx)` sends each line to a
`ReportSink` (a `tokio::sync::mpsc::Sender`) as it settles and keeps
none: the run waits for the sink, and one that fails stops the run. A
restart loses a run in progress: durable broadcasts and scheduling are
planned, not built.

## Testing

A `ManualClock` is a `Timer`: waiting on it moves it forward at once, so
a whole paced run takes no real time and each send's start is readable.
Its sleeps add up, so concurrent senders never overlap on it; to test the
rate under concurrency, implement `Timer` over Tokio's paused clock.

```rust
fn pacer(clock: &ManualClock) -> Pacer {
    shared_pacer("999".into()).with_timer(clock.clone())
}
```

Guide: [docs/guides/bots.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/bots.md).
