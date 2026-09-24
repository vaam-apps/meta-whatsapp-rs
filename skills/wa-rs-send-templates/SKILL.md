---
name: wa-rs-send-templates
description: "Sending an approved WhatsApp template with wa-rs - TemplateMessage with its language code, positional (Parameter::text) vs named (Parameter::named) parameters, header media, currency and date parameters, button parameters by index (URL suffix, quick-reply payload, copy code, Flow, catalog, multi-product), carousels and limited-time offers, what is validated locally and what only Meta can check (132000, 132001). Load when sending a template message - order updates, notifications, campaigns, anything outside the 24-hour window."
---

# wa-rs-send-templates

> **Verified against wa-rs 4eb93c9bd63812221e75ad0920b6e2cb98ea0dd6 (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/send.rs](examples/send.rs), compiled and tested
by wa-rs's own gate.

## When to use

Sending a template that is already approved: the only messages that reach
a customer outside the 24-hour window. Defining and approving templates is
`wa-rs-templates`; OTP codes go through `wa-rs-otp-login`, never through
this builder.

## Fill the placeholders

```rust
TemplateMessage::new("order_update", "en_US") // the language it was APPROVED in
    .header(Parameter::document_id(
        invoice,
        Some("invoice-860198.pdf".into()),
    ))
    .body([Parameter::text("Jessica"), Parameter::text("860198")]) // placeholder order
    .url_button(0, "860198") // index = the button's position in the template
```

A named template takes one `Parameter::named` per placeholder:

```rust
TemplateMessage::new("welcome", "en_US").body([
    Parameter::named("first_name", "Jessica"),
    Parameter::named("total", "€134.80"),
])
```

Buttons are addressed by their position in the approved template:

```rust
TemplateMessage::new("autumn_offer", "en_US")
    .body([Parameter::named("first_name", "Jessica")])
    .limited_time_offer(expires_at_ms) // UNIX milliseconds
    .copy_code_button(0, "AUTUMN20") // at most 20 characters
    .quick_reply_button(1, "stop-promotions") // comes back in the Button webhook
```

Carousels take one entry per card, in card order:

```rust
.carousel([
    CarouselCardParameters::new(0, [SendComponent::header(Parameter::image_id(first))]),
    CarouselCardParameters::new(1, [SendComponent::header(Parameter::image_id(second))]),
])
```

## Send

```rust
template.validate()?; // `send` does this too; the error names the field
messages
    .send(&OutboundMessage::template(to, template))
    .await
```

Or through the Marketing Messages API for marketing templates
(`wa-rs-marketing`).

## The parameter kinds

| Placeholder | `Parameter` |
| --- | --- |
| text, positional / named | `text(v)` / `named(name, v)` |
| money | `currency(fallback_value, "EUR", amount_1000)` (amount × 1000) |
| date | `date_time(fallback_value)` |
| image / video / document header | `image_id(id)`, `image_link(url)`, `video_id`, `video_link`, `document_id(id, filename)`, `document_link(url, filename)` |
| location header | `location(TemplateLocation::new(lat, lon))` |
| product header | `product(product_retailer_id, catalog_id)` |

Button builders on `TemplateMessage`: `url_button`, `quick_reply_button`,
`copy_code_button`, `flow_button`, `catalog_button`, `mpm_button`,
`voice_call_button`; also `tap_target`. A media header at send time is a
normal media id (or link), not the Resumable Upload handle used to create
the template.

## What is checked where

- Locally, before sending (`TemplateMessage::validate`): the name, a
  non-empty language, coupon codes ≤ 20 characters, carousels ≤ 10 cards,
  multi-product sections (1–10 sections, 24-character titles, 30
  products), quick-reply payloads ≤ 512, call-button TTL 1–43200 minutes.
- Only by Meta, because wa-rs does not know the approved definition:
  parameter count and kind (`ErrorKind::TemplateParameterMismatch`,
  132000), the template existing in that language
  (`ErrorKind::TemplateNotFound`, 132001), paused or disabled templates
  (132015, 132016).

## Pitfalls

- The language code must be the one the template was approved in
  (`en_US` and `en` are different templates).
- Positional parameters go to a positional template in placeholder
  order; a named template needs `Parameter::named` for each placeholder.
  Mixing them is `TemplateParameterMismatch`.
- `Parameter`'s `Debug` redacts text and coupon codes; keep it out of logs
  anyway.
- Marketing templates may be refused per user (131049, 131050): see
  `wa-rs-marketing` and `wa-rs-errors`.

## What wa-rs does not do

- No lookup of the approved definition, so no local check of parameter
  counts; cache your approved templates (`wa-rs-templates`) if you want
  one.
- No campaign pacing or messaging-limit accounting.

## Related skills

`wa-rs-templates` (definitions), `wa-rs-send-messages` (recipients, send
semantics), `wa-rs-marketing`, `wa-rs-commerce` (catalog and multi-product
templates), `wa-rs-documents` (document headers), `wa-rs-errors`.
