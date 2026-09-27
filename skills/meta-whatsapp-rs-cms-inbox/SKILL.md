---
name: meta-whatsapp-rs-cms-inbox
description: "The merchant-to-customer chat inbox of a multi-tenant CMS built on meta-whatsapp-rs (meta_whatsapp_rs::inbox) - InboxSink recording webhook messages, statuses, coexistence echoes and history, the calls and standby messages that reopen the window, Conversation Routing thread ownership and identity links into a ConversationStore, Inbox listing conversations and history and replying with the merchant's token, the tenant ownership check before the token vault, conversation keys (BSUID, wa_id, group), the 24-hour window with a template fallback, replies refused while another app owns the thread and the ReplyChecks override, quoted replies, unread counts, NUL handling on Postgres, erasing a customer and retention, and what the inbox does not record. Load when building inbox screens, reply endpoints, or the webhook-to-inbox pipeline of a CMS."
---

# meta-whatsapp-rs-cms-inbox

> **Verified against meta-whatsapp-rs 97f0606fe4932c75fb6b2ba3c5d3668de6da7e5c (2026-09-27).** On another revision, trust the code over this page.

Reference code: [examples/inbox.rs](examples/inbox.rs), compiled and tested by meta-whatsapp-rs's own gate.
The full server (webhook endpoint, SSE, bearer-token tenants), exercised in-process by meta-whatsapp-rs's tests:
[`cms_inbox.rs`](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/crates/meta-whatsapp-rs/examples/cms_inbox.rs).

## When to use

Merchants connected their number (`meta-whatsapp-rs-embedded-signup`) and chat with their customers in your
CMS. Module `meta_whatsapp_rs::inbox`; storage port `ConversationStore`.

```text
Meta ─webhook─► WebhookHandler ─► FanoutSink ─┬─► InboxSink ──► ConversationStore
                                              └─► BroadcastSink ─► sse() ─► merchant UI
merchant UI ─► Inbox::reply ─► client.with_token(merchant token) ─► Meta
```

## Record: the webhook side (from `cms_inbox.rs`)

```rust
let sink = FanoutSink::new()
    .with(InboxSink::new(conversations.clone()))
    .with(BroadcastSink::from_sender(live.clone()));
```

`InboxSink` records inbound messages and status updates, idempotently (a known message id is ignored; a
status never moves a message backwards), only on the business number they arrived on. Run
`postgres::migrate(&pool)` at startup (`meta-whatsapp-rs-storage`). Subscribe to `calls`, `user_id_update`,
and under Conversation Routing `messaging_handovers` and `standby`: these need `whatsapp_business_management`,
and no callback override reroutes `standby` or `user_id_update` ([references/routing.md](references/routing.md)).
To record a kind yourself, switch it off: `InboxSink::with_recording`.

Coexistence (the merchant keeps the WhatsApp Business app): `MessageEchoed` (sent from the app) is outbound
`Sent`, in the customer's BSUID (else phone) conversation; `HistorySynced` is recorded message by message,
outbound when `from` is the business number (status from `history_context`), else inbound, opens no reply
window, never unread (`append_synced`); a later media content fills its placeholder unless revoked
(`fill_media_placeholder`). A declined sync (2593109) records nothing; a malformed item is skipped, logged.

## Read and reply: ownership first

```rust
// Ownership first: the vault holds every merchant's token.
if !owned_numbers.contains(&number) {
    return Err(Refused::NotYourNumber);
}
let Some(merchant) = vault.get_by_phone_number(&number).await? else {
    return Err(Refused::NotConnected);
};
Ok(Inbox::new(
    platform.with_token(merchant.token),
    number,
    store,
))
```

`owned_numbers` is **your** tenant → phone number table, for the tenant **your** authentication says is
calling, never one the request names; the inbox only checks that a key belongs to its number.

```rust
let key = inbox.key(contact); // the `contact` of a ConversationSummary: BSUID, wa_id or group id
let page = inbox.history(&key, None, 50).await?; // next page: the last row's (timestamp, id)
inbox.mark_read(&key).await?; // your unread counter, not WhatsApp's blue ticks
```

`inbox.conversations(before, limit)` lists newest activity first (next page: the last row's
`(last_message_at, key.contact)`, exclusive). Blue ticks are `client.messages(pnid).mark_read(&id)`.

## The 24-hour window

```rust
let key = inbox.key(contact);
// `check_reply` makes `reply`'s own checks (its clock, its ReplyChecks) and sends nothing.
let content: MessageContent = match inbox.check_reply(&key).await {
    Ok(()) => Text::new(body).into(),
    Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => {
        TemplateMessage::new("reopen_conversation", "en_US").into() // an approved template
    }
    Err(e) => return Err(e), // ThreadOwnedElsewhere: another app answers them now
};
// Recorded as `Accepted` once Meta accepts it; never retry an `Ok`.
inbox.reply(&key, content).await
```

`reply`/`send` refuse free-form content outside the window **before any request**, with `Error::Validation`
whose `kind()` is `ErrorKind::CustomerServiceWindowClosed`, Meta's 131047's: branch on the kind. Templates and
Direct Send `utility`/`authentication` are exempt (not `service`). The window counts calls and standby messages
(window events: never history, never unread); after a handover to this app the customer has not written
since, Meta decides (`ReplyChecks::trust_handover`). `window_is_open` is the window alone. Quoted replies:

```rust
let message = OutboundMessage::new(inbox.recipient(&key), Text::new(body)).reply_to(quoted);
inbox.send(&key, message).await // any other recipient is refused
```

## Another app owns the thread (Conversation Routing)

`reply`/`send` refuse a service message, before any request, while another app owns the thread (after
`control_taken` or a standby copy; templates need no ownership): kind `ErrorKind::ThreadOwnedElsewhere`
(answer it `409`, like the window's).

```rust
use meta_whatsapp_rs::inbox::{ReplyChecks, is_thread_owned_elsewhere};

Err(e) if is_thread_owned_elsewhere(&e) => Ok(None),
```

The escalation partner turns it off: `ReplyChecks::ALL.thread_owner(false)`. Your own `pass` or `take`:
`inbox.record_thread_owner`. Who owns the thread, and why: [references/routing.md](references/routing.md).

## Conversation keys

`ConversationKey { phone_number_id, contact }`: the group id for group messages, else the BSUID, else the
`wa_id` (digits); a message with none is not recorded. A revoke of the same number and direction, whatever its
conversation, marks the original `Deleted` and keeps its content (both decided); one that comes first leaves a
history-only tombstone (`StoredMessage::REVOKED`). Replies to a `wa_id` go to `+<digits>`; a `.` means a BSUID.
Public rules: `meta_whatsapp_rs::inbox::conversation_key`, `meta_whatsapp_rs::inbox::preview`. A new BSUID
(`WebhookEvent::UserIdChanged`) starts a new conversation; `InboxSink` links the two for erasures.

## Erasing a customer

Erase the person (their BSUID, phone number, an earlier BSUID), behind the ownership check:

```rust
let key = inbox.key(contact); // any of their keys on this number
let ids: Vec<String> = inbox.identities(&key).await?.into_iter().collect();
inbox.erase_all(&ids).await // `Erased` holds counts only: log those, never the ids
```

Group messages are redacted in place (`ErasureMode::Delete` deletes them); `Inbox::erase` and
`Inbox::identities` refuse another number's key. Procedure and limits: [references/erasure.md](references/erasure.md).
History is kept unless the store has `with_retention` and you schedule `apply_retention`.

## Pitfalls

- A reply that returned `Ok` is recorded; a failure to record it is logged, never returned (an error would
  invite a second send). Never retry an `Ok`; treat a timeout as "may have been sent".
- Your own replies are not broadcast: push them to the UI yourself (`meta-whatsapp-rs-live-updates`).
- **U+0000 is content**: recorded exactly, so render or strip it in your UI (SQL or your own store:
  `meta-whatsapp-rs-storage`); the Postgres store refuses it in Meta-assigned ids.
- On an older pin, what was fixed when: [references/history.md](references/history.md).

## What meta-whatsapp-rs does not do

- Not recorded by `InboxSink`: synced contacts (`smb_app_state_sync`, L8), media bytes (rows keep the
  media id; download within 7 days), BSUID merges, `conversation_context`. No thread control API (L15).
- Message ids are unique per store, not per business number (open question 33). No Redis store.

## Related skills

`meta-whatsapp-rs-embedded-signup`, `meta-whatsapp-rs-token-vault`, `meta-whatsapp-rs-webhook-endpoint`,
`meta-whatsapp-rs-live-updates`, `meta-whatsapp-rs-send-templates` (the fallback), `meta-whatsapp-rs-storage`,
`meta-whatsapp-rs-testing` (the window with a `ManualClock`), `meta-whatsapp-rs-groups-and-calling` (calls).
