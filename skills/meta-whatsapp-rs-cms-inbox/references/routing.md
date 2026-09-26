# Calls, standby and thread ownership in the inbox

> **Verified against meta-whatsapp-rs 97f0606fe4932c75fb6b2ba3c5d3668de6da7e5c (2026-09-27).** Source: the rustdoc of `meta_whatsapp_rs::inbox` (the module, `InboxSink`, `Inbox::thread_owner`, `ReplyChecks`) and `docs/guides/cms-inbox.md` section 5.

What `InboxSink` records besides messages, and what `Inbox` does with it.
Subscribe the webhook to `calls` and `user_id_update`, and, where the
merchant's account uses Conversation Routing, to `messaging_handovers`
and `standby`.

## The calls that reopen the window

Meta starts or refreshes the 24-hour window when the customer calls,
answered or not, and when they accept the business's call
(`calling/pricing`). `InboxSink` records a *window event* for:

- a `calls[]` event with `direction` USER_INITIATED and `event`
  `connect`, `call_created` (SIP) or `terminate`, at its `timestamp` (a
  terminate's `start_time` when it has one);
- a call status `ACCEPTED`, at its `timestamp`;
- a BUSINESS_INITIATED `terminate` with a `start_time` (the call was
  picked up), at the `start_time`.

Not a `RINGING` or `REJECTED` status, not your own call's `connect`, not
a recording or transcript notice, not an event without a `direction`.
A window event is never history, never unread, never the summary; one
call is recorded once (its id). `inbox.window(&key)` opens from the
latest of the last inbound message and the latest window event.

## Who owns the thread

| Signal | Recorded as |
| --- | --- |
| `control_passed` | `ThreadOwner::ThisApp`, with the new owner's role |
| `control_taken` | `ThreadOwner::AnotherApp` |
| a standby copy of the customer's message | `AnotherApp`, dated 1 ms before it (never over a handover of its second, even when replicas race), and a window event |
| a message on `messages` after the record | `ThisApp`, when read; not a call permission reply (Meta sends it to the Incoming Call primary and the standby partners too) |
| your own `release` | `inbox.record_release(&key)`: `Idle` |
| 24 hours without the customer | `Idle`, when read (`Inbox::THREAD_IDLE_AFTER`) |

`inbox.thread_owner(&key)` is `None` when nothing was ever recorded (no
routing). A handover names the customer by phone number only; it is
recorded under the conversation that number leads to: along the
identity links (a number whose message carried a BSUID leads to it, a
BSUID change on to the new one), else a synced contact's BSUID, else the
phone number. A recycled number can lead to its earlier owner: the check
is advisory. Standby echoes and receipts record nothing; a group's copy
no owner.

## The local refusal and the override

While another app owns the thread, `reply` and `send` refuse a service
message before any request: `Error::Validation` on field `THREAD_OWNER`
(`inbox::THREAD_OWNER`, kind `InvalidParameter`: Meta has no code of its
own for it); match with `is_thread_owned_elsewhere`. Templates and
Direct Send `utility` and `authentication` need no ownership (a Direct
Send `service` message does, and the window). An idle thread is not
refused. The designated escalation partner, whose service message takes
the thread, turns the check off for its inbox:

```rust
inbox.with_reply_checks(ReplyChecks::all().thread_owner(false))
```

`ReplyChecks::all().window(false)` turns the window check off the same
way; `ReplyChecks::none()` both. For one call, use a clone of the inbox.

The checks read what your app received. Turn one off where that is not
enough:

- an app that receives handovers **without standby visibility** (its
  `control_passed` carries a `conversation_context` summary instead)
  never saw the customer's messages to the previous owner: its window can
  read closed while Meta's is open. Turn the window check off there
  (`ReplyChecks::none()` for an escalation partner); Meta answers 131047
  when its window is closed;
- an app sharing the number with a Meta Business Agent and **no routing
  configuration** receives standby copies, yet its service message makes
  it the active handler: turn the ownership check off.

## Identity links

For erasures (`Inbox::identities`), `InboxSink` links a phone number to
the BSUID an inbound message carries with it (so a thread keyed by the
phone number from before BSUIDs is found), a previous BSUID to the
current one (`user_id_update`), and a number change's old identity to the
new one (`system` messages of type `user_changed_number` or
`user_changed_user_id`). Never an empty value, one with U+0000, or a value
to itself; parent BSUIDs are not linked.
