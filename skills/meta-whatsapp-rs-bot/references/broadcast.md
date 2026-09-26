# Paced broadcasts

> **Verified against meta-whatsapp-rs 35b6739412169ab3ab43bfcacea133295362cb9a (2026-09-26).** Also checked against Meta's
> `throughput`, `about-the-platform` (pair rate limits), `support/error-codes`,
> `templates/marketing-templates/per-user-limits` and `messaging-limits` pages
> as mirrored on 2026-09-24.

A `Broadcast` sends one message (`BroadcastBuilder::content`) or one per
recipient (`BroadcastBuilder::compose`) from one business number. Every
send, a retry too, first waits for a slot of the number's `Pacer`; up to
`BroadcastBuilder::concurrency` sends (default 32) are in flight at once,
so the rate is reached even when each send takes a while. `run` drives it
in your task: spawn it and keep the `BroadcastHandle`
(`BroadcastHandle::progress`, `BroadcastHandle::cancel`). Don't drop the
`run` future to stop it (the sends in flight and the report are lost):
cancel, and let `run` return.

**Each person once**, by default: a recipient listed again (same phone
number by its digits, same BSUID, same group, or a phone and a BSUID one
`Recipient::PhoneAndUser` carried) gets no second message; its line is
`SendOutcome::Duplicate`. `BroadcastBuilder::dedupe(false)` turns it off.

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
  set it yourself (`TokenBucket::rate_for`), and change it while running
  (`TokenBucket::set_rate`, on a bucket kept in an `Arc` and given to
  `Pacer::shared`), for example when a quality webhook says
  `PhoneNumberQualityEvent::ThroughputUpgrade`.
- **Leave room for inbound messages**: Meta counts them in the same
  throughput, and the pacer only sees your sends. Expecting one reply per
  four messages on a default number: `Rate::per_second(64)`.
- At `Rate::HIGHER_THROUGHPUT`, raise `BroadcastBuilder::concurrency` to
  about 1,000 times a send's duration (400 at 400 ms): 32 sends in
  flight make 80 a second.
- `TokenBucket` spaces sends evenly: any one-second window holds at most
  the rate. `TokenBucket::burst` lets a few go at once, at the cost of
  up to `burst - 1` more in a window.
- **One pacer per process**, shared: `BroadcastBuilder::pacer` for every
  broadcast and `BotBuilder::pacer` for the bot (its replies, refusals,
  read receipts and typing indicators, through `PacedOutbound`; handlers
  get it from `Ctx::pacer`). Group operations:
  `PacedGroups::new(client.groups(number), pacer)` and its `PacedGroup`
  (Meta documents no rate for those; pacing them in the same budget is a
  choice). Any other call: `Pacer::acquire` first.
- **Several replicas** each have their own budget: give each its share
  (`Rate::per_second`), or implement `RateLimiter` (`RateLimiter::reserve`
  takes a `SlotRequest` and returns a `Reservation`,
  `RateLimiter::release` takes an unused one back,
  `RateLimiter::slow_down` hears throttling) over a store they share.
  None ships yet.
- Don't give a broadcast a `PacedOutbound`: it paces itself, and each
  send would take two slots. An outbound of yours must not retry inside
  a call, and must be truthful about `Error::may_have_been_sent`.
- Zero where it means nothing is a `ConfigError` when given
  (`TokenBucket::burst`, `TokenBucket::recovery`, `Backoff::max_attempts`,
  a concurrency of zero at `BroadcastBuilder::build`).

## What a failed send becomes

`BroadcastPolicy` decides from a `SendFailure`; the default is `Backoff`,
read from the error's kind, never its text:

| Meta answers | `Backoff` does |
| --- | --- |
| `131056`, pair rate limit | defers that recipient 1, 4, 16, 64 s (Meta's `4^X`); the others go on |
| `130429` throughput, other `RateLimited` codes | retries after 1, 2, 4 … s |
| `131057`, number in maintenance (a throughput upgrade, up to a minute) | retries every 20 s (`Backoff::MAINTENANCE_RETRY`) |
| `131049`, per-user marketing limit | reports it for that recipient, never retries (Meta: wait 24 hours) |
| `Backoff::STOPS`: token, permission, account, classification limit, payment, spam limit (`131048`), registration, marketing turned off | the run stops (`BroadcastEnd::Stopped`); the rest are skipped |
| `Backoff::CONTENT_STOPS` (template not found, paused, disabled, wrong parameters), same message for all | the run stops; per recipient with `compose` |
| anything else not retryable | reports it (`SendOutcome::Failed`) |

Every delay is at most `Backoff::max_delay` (64 s); `Backoff::stops` and
`Backoff::content_stops` replace the lists. The pacer slows the number
down on its `SlowDownRule` (`ThrottlingErrors`: `130429` and the other
`RateLimited` codes, `131048`, `131057`), halving the rate and doubling
it back per quiet 30 s (`TokenBucket::slow_down_factor`,
`TokenBucket::slow_down_spacing`, `TokenBucket::recovery`;
`TokenBucket::adaptive(false)` turns it off).

**The library's rule, whatever the policy:** a recipient is resent only
when `Error::may_resend` holds, the client's own rule: Meta provably
refused the send (throttling on any status, an HTTP 429, `131057` on a
4xx). A timeout, a 5xx that is not a throttling refusal, or a `131000`
is `SendOutcome::Failed`, never resent: when `may_have_been_sent()` is
true, reconcile with the status webhooks (`OutboundMessage::callback_data`
in `compose`) before sending again. `BroadcastBuilder::client` turns the
client's own replays off (`Client::with_retry(RetryPolicy::NONE)`) so
every retry is paced; a paced bot built on a client does the same and
retries in its `PacedOutbound` (`PacedOutbound::retry`).

`SendOutcome::Sent` is Meta's acceptance, not delivery: a `131049` can
also arrive later as a `failed` status webhook. Meta's daily messaging
limit (unique users per 24 hours, per portfolio) is Meta's to enforce;
nothing here counts it.

## Cancel, stop, report

`BroadcastHandle::cancel`: no send starts afterwards (a wait for a slot
or a retry ends at once, the slot going back to the pacer); sends in
flight finish and are reported. Recipients never sent are
`SendOutcome::Skipped` (with `RecipientReport::last_error` when a retry
was waiting). The `BroadcastReport` has one `RecipientReport` per
recipient, in order (`index`, `attempts`, `outcome`), its counts
(`BroadcastReport::progress`) and `BroadcastEnd` (`Stopped`,
`LimiterFailed` and `SinkFailed` apart). For a long list,
`BroadcastBuilder::report_to(tx)` sends each line to a `ReportSink` (a
`tokio::sync::mpsc::Sender`) as it settles and keeps none: the run waits
for the sink, and one that fails stops the run. A restart loses a run in
progress: durable broadcasts and scheduling are planned, not built.

## Testing

A `ManualClock` is a `Timer`: waiting until a deadline moves it there at
once, never back, so a whole paced run takes no real time and each
send's start is exact, however many senders share it. A send takes no
time on it; to test slow sends overlapping, implement `Timer` over
Tokio's paused clock.

```rust
fn pacer(clock: &ManualClock) -> Pacer {
    shared_pacer("999".into()).with_timer(clock.clone())
}
```

Guide: [docs/guides/bots.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/bots.md).
