# Calls, standby and thread ownership in the inbox

> **Verified against meta-whatsapp-rs 888dec677cf5a56db041e56986e6ef6ee2e52b0a (2026-09-27).** Source: the rustdoc of `meta_whatsapp_rs::inbox` (the module, `InboxSink`, `RecordingSwitches`, `Inbox::thread_owner`, `ReplyChecks`) and `docs/guides/cms-inbox.md` section 5.

What `InboxSink` records besides messages, and what `Inbox` does with it.

## Subscriptions and permissions

Subscribe the webhook to `calls` and `user_id_update`, and, where the
merchant's account uses Conversation Routing, to `messaging_handovers`
and `standby`. These four fields need `whatsapp_business_management`
(Meta's `webhooks/overview`, "Permissions": `whatsapp_business_messaging`
covers `messages` only), which merchants grant in the Embedded Signup
popup next to `whatsapp_business_messaging` once your app has Advanced
access to both (`meta-whatsapp-rs-embedded-signup`).

A callback override (`webhooks/override`) reroutes `messages`, `calls`
and `messaging_handovers` of a number or WABA, never `standby` or
`user_id_update`: those always reach the app's default callback URL. An
inbox pipeline behind an override receives them only if that default
URL also delivers them to the same `InboxSink` and store; otherwise
standby copies (window events, ownership) and BSUID changes (identity
links) are silently never recorded.

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
| your own `pass`, or `take` as the escalation partner | `inbox.record_thread_owner(&key, owner, role)`: Meta reports neither to you |
| 24 hours without the customer | `Idle`, when read (`Inbox::THREAD_IDLE_AFTER`; `Inbox::with_thread_idle_after` sets another) |

`inbox.thread_owner(&key)` is `None` when nothing was ever recorded (no
routing). A handover names the customer by phone number only; it is
recorded under the conversation that number leads to
(`meta_whatsapp_rs::inbox::handover_key`): along the identity links (a
number whose message carried a BSUID leads to it, a BSUID change on to
the new one), else a synced contact's BSUID, else the phone number. That
is up to 16 identity link reads per handover, inside the webhook
request. A recycled number can lead to its earlier owner: the check is
advisory. Standby echoes and receipts record nothing; a group's copy no
owner.

## The local refusals and the switches

While another app owns the thread, `reply` and `send` refuse a service
message before any request: `Error::Validation` on field `thread_owner`
(`ValidationError::thread_owned_elsewhere()`), whose `kind()` is
`ErrorKind::ThreadOwnedElsewhere` (Meta has no code of its own for it
yet); `meta_whatsapp_rs::inbox::is_thread_owned_elsewhere` also looks
through `Error::Step`. A service answers it like the window's refusal:
`409`. Templates and Direct Send `utility` and `authentication` need no
ownership (a Direct Send `service` message does, and the window). An
idle thread is not refused. The designated escalation partner, whose
service message takes the thread, turns the check off for its inbox:

```rust
inbox.with_reply_checks(ReplyChecks::ALL.thread_owner(false))
```

`ReplyChecks::ALL.window(false)` turns the window check off the same
way; `ReplyChecks::NONE` every check. For one call, use a clone of the
inbox. `inbox.check_reply(&key)` runs the checks `reply` would, sending
nothing.

The checks read what your app received:

- An app that receives handovers **without standby copies** (its
  `control_passed` carries a `conversation_context` summary instead)
  never saw the customer's messages to the previous owner: its recorded
  window can read closed while Meta's is open. So after a handover to
  this app newer than the customer's last inbound message recorded here,
  the window check lets Meta decide (on by default:
  `ReplyChecks::trust_handover`), until the customer's next message or 24
  hours after the handover; Meta answers 131047 when its window is
  closed. An app that does receive standby copies knows the window:

```rust
inbox.with_reply_checks(ReplyChecks::ALL.trust_handover(false))
```

- An app sharing the number with a Meta Business Agent and **no routing
  configuration** receives standby copies, yet its service message makes
  it the active handler: turn the ownership check off.

## Recording it yourself

`InboxSink::with_recording(RecordingSwitches::ALL.handovers(false))`
(or `.calls(false)`, `.standby(false)`, `.identity_links(false)`) turns
one kind of record off, all on by default; you then record that kind
from the same events through the same `ConversationStore` methods. The
rules are public for that: `meta_whatsapp_rs::inbox::call_window`,
`meta_whatsapp_rs::inbox::call_key`, `meta_whatsapp_rs::inbox::call_status_key`,
`meta_whatsapp_rs::inbox::handover_key`.

## Identity links

For erasures (`Inbox::identities`), `InboxSink` links a phone number to
the BSUID an inbound message carries with it (so a thread keyed by the
phone number from before BSUIDs is found), a previous BSUID to the
current one and the update's `wa_id` to the current one
(`user_id_update`), and a number change's old identity to the new one
(`system` messages of type `user_changed_number` or
`user_changed_user_id`). Never an empty value, one with U+0000, or a value
to itself; always on the business number the event arrived on; parent
BSUIDs are not linked.
