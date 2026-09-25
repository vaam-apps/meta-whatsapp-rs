---
name: meta-whatsapp-rs-send-messages
description: "Sending free-form WhatsApp messages with meta-whatsapp-rs - OutboundMessage text, image, video, audio, document, sticker, location, contact cards and reactions, choosing the recipient (Recipient::phone in E.164 with a plus, Recipient::user for a BSUID, groups), quoted replies with reply_to, callback_data for status webhooks, read receipts and typing indicators, the 24-hour customer service window, and what the send response means. Load when writing code that sends a WhatsApp message other than a template, answers a customer, or marks messages as read."
---

# meta-whatsapp-rs-send-messages

> **Verified against meta-whatsapp-rs d9f4c05393be9b6b7ce688efe1ad309b026fbd37 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/send.rs](examples/send.rs), compiled and tested
by meta-whatsapp-rs's own gate. Every constructor and content type:
[references/message-types.md](references/message-types.md).

## When to use

Sending anything but a template: answering a customer, sending media, a
location or a contact card, reacting, marking read. Module
`wa_rs::client::messages`, reached with `client.messages(phone_number_id)`.

## Who to send to

```rust
Recipient::phone("+16505551234"), // E.164 WITH the `+`: `to`
Recipient::user("US.13491208655302741918"), // BSUID from the webhook: `recipient`
Recipient::PhoneAndUser {
    phone: "+16505551234".into(), // Meta uses the phone number
    user: "US.13491208655302741918".into(),
},
Recipient::group("Y2FwaV9ncm91cDox"), // Groups API
```

A `&str` is not `Into<Recipient>`: build one of these. A webhook `wa_id`
is digits only — `Recipient::phone(format!("+{wa_id}"))`. Since 2026 a
webhook may carry only the BSUID: address the customer with it.

## Send

```rust
let messages = client.messages(phone_number_id);
let text = OutboundMessage::text(to, "Your order has shipped.")
    .reply_to(inbound) // shown as a quote
    .callback_data("order:860198"); // comes back as Status::biz_opaque_callback_data
let sent = messages.send(&text).await?;
Ok(sent.message_id().cloned()) // match status webhooks on this wamid
```

`OutboundMessage::new(to, content)` takes any content (`Text`, `Image`,
`Document`, `Location`, …); the named constructors are shortcuts:

```rust
let image = Image::new(photo).caption("Your parcel, packed");
messages
    .send(&OutboundMessage::new(to.clone(), image))
    .await?;
let invoice = Document::new(MediaSource::link("https://shop.example/i/860198.pdf"))
    .filename("invoice-860198.pdf");
messages.send(&OutboundMessage::new(to, invoice)).await
```

```rust
let shop = Location::new(52.520008, 13.404954)
    .name("Example Boutique")
    .address("Musterstraße 1, 10115 Berlin");
messages
    .send(&OutboundMessage::new(to.clone(), shop))
    .await?;
let card =
    Contact::new("Example Boutique support").phone(ContactPhone::new("+4930123456", "WORK"));
messages
    .send(&OutboundMessage::contacts(to.clone(), [card]))
    .await?;
messages.react(to, inbound, "👍").await?; // "" removes the reaction
```

Media ids come from an upload (`wa-rs-media`); a `MediaSource::link` must
be a public HTTPS URL.

## Read receipts and "typing…"

```rust
if messages
    .mark_read_with_typing_indicator(inbound)
    .await
    .is_err()
{
    messages.mark_read(inbound).await?; // idempotent: the client may replay it
}
```

`mark_read` marks a **received** message (and every earlier one) read; it
is replayed on transient errors. The typing indicator shows until you
reply or 25 seconds pass, only right before a reply; it is not replayed (a
late "typing…" after the answer looks broken).

## What `send` does and means

- It validates every documented limit first; a violation is
  `Error::Validation` naming the JSON path, and **nothing was sent**.
- `Ok(sent)` means Meta **accepted** the message; delivery, reads and
  failures arrive later as status webhooks keyed by `sent.message_id()`
  (`wa-rs-webhook-events`). For templates, `message_status` of
  `sent.messages[0]` may be `HeldForQualityAssessment` (Meta's pacing).
- It is not idempotent: a timeout or 5xx is returned, never replayed
  (`wa-rs-errors`). An `Error::Decode` means Meta answered 2xx: treat it as
  sent.

## Pitfalls

- **Outside the 24-hour customer service window** (no message from the
  customer in the last 24 hours) free-form messages fail with
  `ErrorKind::CustomerServiceWindowClosed` (131047): send a template
  (`wa-rs-send-templates`).
- **Never drop the `+`**: Meta prepends your business number's country
  code to a number without it, so the message reaches someone else.
- A reaction cannot be a quoted reply (refused locally, field `context`);
  pins go to groups only.
- Some sends refuse a BSUID (`ErrorKind::RecipientNotSupported`, 131062):
  authentication templates and marketing sends with a bid. Use the phone
  number there.
- Direct Send (`category(DirectSendCategory::…)`, `ttl_seconds`) is a Meta
  beta; the rest of the builder works without it.

## What meta-whatsapp-rs does not do

- No send queue, rate limiting across your fleet, or retry scheduler.
- Payment message types are not modelled: `MessageContent::Raw` is the
  escape hatch.
- The 24-hour window is not tracked here; the CMS inbox tracks it from
  recorded messages (`wa-rs-cms-inbox`,
  [open question 32](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#cms-inbox)).

## Related skills

`wa-rs-interactive-messages` (buttons, lists, Flows), `wa-rs-media`,
`wa-rs-send-templates`, `wa-rs-errors`, `wa-rs-webhook-events` (status
webhooks), `wa-rs-cms-inbox` (replies from an inbox).
