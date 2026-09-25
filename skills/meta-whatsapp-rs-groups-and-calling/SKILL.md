---
name: meta-whatsapp-rs-groups-and-calling
description: "What a merchant inbox built on meta-whatsapp-rs needs beyond one-to-one text - blocking and unblocking a customer (block_users, BSUID or phone, the 24-hour rule, partial failures 139100), WhatsApp group chats (Groups API - create by request id and webhook, invite links, join requests, participants, pins, group messages in the inbox), and WhatsApp calls (Calling API signalling only - settings, permissions, reject or accept with SDP, call webhooks; no audio). Load when adding a block button, group chats or call handling to a CMS or support inbox, or when group or call webhooks arrive."
---

# meta-whatsapp-rs-groups-and-calling

> **Verified against meta-whatsapp-rs 6d04f3da9c504cffac32f7dbe05869adcaf1957e (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/groups_calls.rs](examples/groups_calls.rs),
compiled and tested by meta-whatsapp-rs's own gate (Meta-shaped answers and
webhooks). Everything else: the rustdoc of `meta_whatsapp_rs::client::block_users`,
`meta_whatsapp_rs::client::groups` and `meta_whatsapp_rs::client::calling`
(`cargo doc -p meta-whatsapp-rs --all-features --open`).

## When to use

A merchant's inbox (`meta-whatsapp-rs-cms-inbox`) wants a "block" button, group
chats with customers, or to cope with customers calling the number. All
three act as the merchant: use the client from `with_token`
(`meta-whatsapp-rs-token-vault`), after your ownership check.

## Block a customer

```rust
let answer = merchant.block_users(number).block(&[customer]).await?;
// A 2xx can still list failures (with a top-level 139100 error).
let refused = answer
    .block_users
    .failed_users
    .iter()
    .flat_map(|user| {
        user.errors
            .iter()
            .map(meta_whatsapp_rs::GraphApiError::kind)
    })
    .collect();
Ok(refused) // empty: blocked. 131047: they have not written in the last 24 hours
```

`customer` is `inbox.recipient(&key)`: a BSUID goes as `user_id`, a
phone number as `user`. Checked locally: 1–1,000 users per request
(`MAX_USERS_PER_REQUEST`), no groups, no parent BSUIDs (`CC.ENT.…`).
Only Meta checks that the user wrote to you in the last 24 hours and the
64,000-entry cap. `unblock` takes the same list (answer in
`removed_users`); `list_stream(&ListBlockedUsers::default())` walks the
block list. A partial failure with a non-2xx status is an `Err` and the
per-user lists are lost.

## Group chats

```rust
let mut request = CreateGroup::new(subject);
request.join_approval_mode = Some(JoinApprovalMode::ApprovalRequired);
let created = merchant.groups(number).create(&request).await?; // not replayed after a timeout
Ok(created.request_id) // match it in the `group_lifecycle_update` webhook
```

Creation is asynchronous: the group id and its invite link arrive as
`WebhookEvent::GroupUpdated` (`GroupUpdateType::GroupCreate`, the same
`request_id`; a failed creation carries `errors`):

```rust
if update.update_type != GroupUpdateType::GroupCreate || !update.errors.is_empty() {
    return None; // a failed creation carries `errors`
}
```

Customers join through the invite link (`client.group(id).invite_link()`;
`reset_invite_link()` revokes the old one); with approval required,
`join_requests_stream`, `approve_join_requests`, `reject_join_requests`.
`remove_participants` takes 1–8 users per request. Subscribe the app to
`group_lifecycle_update`, `group_participants_update`,
`group_settings_update` and `group_status_update`.

Messages to a group are ordinary sends to `Recipient::group(id)`. The CMS
inbox keys group messages by the group id and `Inbox::reply` answers the
group; `pin_message(group, message, days)` pins for 1–30 days (group
admins only, three pins at most). Checked locally: subject 1–128
characters, description ≤ 2,048, JPEG picture ≤ 5 MiB.

## Calls

meta-whatsapp-rs wraps the Calling API's **signalling**: settings, call
permissions, and the actions `connect`, `pre_accept`, `accept`,
`reject`, `terminate`, with the SDP passed through unparsed. The audio
(WebRTC or SIP) is yours. Calling is off until
`calling(pnid).update_settings(..)` sets `status` to
`FeatureStatus::Enabled` (it needs a messaging limit of at least 2,000).
Without a media stack, leave it off, or reject what rings:

```rust
if call.event != CallEventType::Connect || call.direction != Some(CallDirection::UserInitiated)
{
    return Ok(false);
}
merchant
    .calling(phone_number_id.clone())
    .reject(&call.id)
    .await?;
```

Calls arrive as `WebhookEvent::CallUpdated` and `CallStatusUpdated`
(field `calls`); calling is not supported in groups.

## Pitfalls

- **Ownership first**: `block_users(number)` and `groups(number)` act on
  whatever number you pass; check that the calling tenant owns it.
- `create` and `reset_invite_link` are not idempotent (a second group, a
  dead link): the client replays them only on throttling, never after a
  timeout or 5xx. Do not retry them yourself either.
- `add_participants` is wrapped, but Meta's guide says participants join
  through invite links: expect a refusal unless Meta enabled it for you.
- A call, answered or not, opens the 24-hour window on Meta's side, but
  the inbox records no calls: `Inbox::reply` still refuses free-form text
  unless the customer wrote in the last 24 hours
  ([open question 32](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#cms-inbox)).

## What meta-whatsapp-rs does not do

- No audio: no WebRTC or SIP media stack, no storage of call recordings
  or transcripts.
- The inbox records neither group lifecycle events nor calls: handle
  `GroupUpdated` and `CallUpdated` in your own sink (`meta-whatsapp-rs-live-updates`).
- No block list sync into your tables: `list_stream` is the source.

## Related skills

`meta-whatsapp-rs-cms-inbox` (conversations, `recipient`), `meta-whatsapp-rs-token-vault`
(the merchant's client), `meta-whatsapp-rs-webhook-events` (group and call events),
`meta-whatsapp-rs-live-updates` (sinks), `meta-whatsapp-rs-send-messages` (sending to a group),
`meta-whatsapp-rs-errors`.
