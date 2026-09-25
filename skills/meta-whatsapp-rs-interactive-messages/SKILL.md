---
name: meta-whatsapp-rs-interactive-messages
description: "Interactive WhatsApp messages with meta-whatsapp-rs - reply buttons (ReplyButtons, up to 3), list messages (ListMessage, up to 10 rows), CTA URL buttons (CtaUrl), location requests, WhatsApp Flow messages (FlowParameters, FlowRef, flow token, first screen), media card carousels (MediaCarousel), their documented limits checked locally, and how the customer's tap comes back in the webhook. Load when building a WhatsApp message with buttons, a menu or list, a link button, a location request, a Flow or a carousel, or handling the reply to one."
---

# meta-whatsapp-rs-interactive-messages

> **Verified against meta-whatsapp-rs 6d04f3da9c504cffac32f7dbe05869adcaf1957e (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/interactive.rs](examples/interactive.rs),
compiled and tested by meta-whatsapp-rs's own gate.

## When to use

A message the customer answers with a tap: a choice of buttons or list
rows, a link, "share your location", a Flow (a form), a carousel. All are
free-form messages: inside the 24-hour window only. Outside it, use a
template with buttons (`meta-whatsapp-rs-send-templates`). Types live in
`meta_whatsapp_rs::client::messages`.

## Buttons and lists

```rust
let buttons = ReplyButtons::new(
    "Keep your delivery slot tomorrow, 9–12?",
    [
        ReplyButton::new("slot-keep", "Keep"), // id (≤ 256) comes back; title ≤ 20
        ReplyButton::new("slot-change", "Change"),
    ],
)
.header(Header::text("Order 860198"))
.footer("Example Boutique");
OutboundMessage::new(to, buttons)
```

```rust
let list = ListMessage::new(
    "Pick a delivery slot",
    "Slots", // the button that opens the list, ≤ 20
    [
        ListSection::new(
            "Tomorrow",
            [ListRow::new("t9", "9–12").description("Morning")],
        ),
        ListSection::new("Friday", [ListRow::new("f14", "14–18")]),
    ],
);
OutboundMessage::new(to, list)
```

The tap arrives as `WebhookEvent::MessageReceived` whose content is
`MessageContent::Interactive(InteractiveReply::ButtonReply(b))` or
`ListReply(r)` (`meta_whatsapp_rs::webhooks::fields`): match on `b.id` / `r.id`,
never on the title.

## Link, location request, Flow

```rust
let track = CtaUrl::new(
    "Track your parcel",
    "Track",
    "https://shop.example/t/860198",
)
.header(Header::text("Order 860198"));
messages
    .send(&OutboundMessage::new(to.clone(), track))
    .await?;
let ask = OutboundMessage::location_request(to.clone(), "Where should we deliver?");
messages.send(&ask).await?;
let fitting = FlowParameters::new(FlowRef::Name("fitting_v1".into()), "Book")
    .token("session-42") // your correlation id, echoed in the Flow's reply
    .navigate("WELCOME", Some(serde_json::json!({"customer": "Alex"})));
let flow = OutboundMessage::flow(to, "Book a fitting", fitting);
messages.send(&flow).await?;
```

A shared location comes back as `MessageContent::Location`; a completed
Flow as `InteractiveReply::NfmReply` (read it with `.response::<T>()`).
`FlowRef::Id` survives renames, `FlowRef::Name` does not. `.draft()`
sends the draft; `.data_exchange()` asks your Flow endpoint for the first
screen instead of `navigate` (`meta-whatsapp-rs-flows`).

## Carousels

```rust
let card = |image: &str, sku: &str| {
    MediaCard::new(
        CardHeader::Image(format!("https://shop.example/img/{image}.jpg")),
        CardAction::QuickReplies(vec![QuickReply::new(format!("buy-{sku}"), "Buy")]),
    )
    .body("Linen, navy")
};
```

Two to ten cards; every card uses the same button kind and count
(`CardAction::Url` or `QuickReplies`). Product carousels come from a
catalog instead (`meta-whatsapp-rs-commerce`).

## Limits are checked before sending

```rust
let message = OutboundMessage::new(to, buttons);
message.validate().err().map(|e| e.field) // Some("interactive.action.buttons")
```

`send` runs the same `validate()`; the error's `field` is the JSON path.
Checked locally: body 1024 (list 4096; a Flow message's body only
non-empty, Meta documents no limit), footer 60, list and CTA header
text 60; buttons 1–3 with unique ids and titles, title 20, id 256; lists 1–10
sections and 1–10 rows in total, row title 24, description 72, section
titles required with several sections; CTA display text 20; carousel
cards 2–10, card body 160 with at most 2 line breaks.

## Pitfalls

- Free-form: outside the 24-hour window Meta answers 131047
  (`ErrorKind::CustomerServiceWindowClosed`).
- Button and row ids come back verbatim: keep them short, stable and
  meaningful to your code (not user text).
- A Flow token is echoed to your endpoint and in the reply: it is a
  correlation id, not a secret.

## What meta-whatsapp-rs does not do

- It does not remember what you asked: correlate replies yourself, by the
  button or row id, the Flow token, or the quoted message
  (`InboundMessage::context`).
- Payment messages are not modelled (`MessageContent::Raw` is the escape
  hatch); `AddressMessage` is typed but Meta offers it in India only.

## Related skills

`meta-whatsapp-rs-send-messages` (sending, recipients), `meta-whatsapp-rs-webhook-events` (the
replies), `meta-whatsapp-rs-flows`, `meta-whatsapp-rs-commerce` (product messages),
`meta-whatsapp-rs-send-templates` (buttons outside the window).
