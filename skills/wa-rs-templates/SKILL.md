---
name: wa-rs-templates
description: "Managing WhatsApp message templates with wa-rs - TemplateDefinition and TemplateComponent (header, body, footer, buttons, carousel, limited-time offer), categories (utility, marketing, authentication), positional vs named placeholders and their examples, media header handles, creating, listing (list_stream), editing, deleting, the template library, local validation, and following review with TemplateStatusUpdated webhooks. Load when creating or changing templates, syncing a template catalog, or reacting to a template being approved, rejected, paused or re-categorized."
---

# wa-rs-templates

> **Verified against wa-rs 4eb93c9bd63812221e75ad0920b6e2cb98ea0dd6 (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/manage.rs](examples/manage.rs), compiled and
tested by wa-rs's own gate.

## When to use

Defining what a template **is** and getting it approved. Sending one with
its values is `wa-rs-send-templates`; authentication (OTP) templates are
`wa-rs-otp-login`. Module `wa_rs::client::templates`, reached with
`client.templates(waba_id)` — the WABA, not the phone number.

Two builder families, never mixed: a **definition** (`TemplateDefinition`,
`TemplateComponent`, `Button`) creates or edits a template; an
**invocation** (`TemplateMessage`, `Parameter`) fills it when sending.

## Create

```rust
let definition = TemplateDefinition::new("order_update", "en_US", TemplateCategory::Utility)
    .component(TemplateComponent::body_positional(
        "Hi {{1}}, order {{2}} has shipped.",
        ["Pablo", "860198"], // one example per placeholder, in order
    ))
    .component(TemplateComponent::buttons([Button::url_with_example(
        "Track",
        "https://shop.example/track/{{1}}",
        "860198",
    )]));
templates.create(&definition).await // checked locally first; usually PENDING
```

Named placeholders need the named format declared, and a media header
needs a Resumable Upload handle (`wa-rs-media`), not a media id:

```rust
TemplateDefinition::new("autumn_sale", "en_US", TemplateCategory::Marketing)
    .parameter_format(ParameterFormat::Named) // required for {{first_name}}
    .component(TemplateComponent::header_image(header.as_str())) // a Resumable Upload handle
    .component(TemplateComponent::body_named(
        "Hi {{first_name}}, 20% off until Sunday.",
        [("first_name", "Alex")],
    ))
```

Categories: `Utility` follows a user's action (order updates), `Marketing`
is anything promotional or mixed, `Authentication` is OTP only (built with
`AuthenticationTemplate`). Meta may re-categorize a template
(`TemplateCategoryUpdated`); `allow_category_change(true)` lets it.

## Follow the review

```rust
match update.event {
    TemplateStatusEvent::Approved => Some((update.message_template_id.clone(), true)),
    TemplateStatusEvent::Rejected | TemplateStatusEvent::Disabled => {
        Some((update.message_template_id.clone(), false)) // `update.reason` says why
    }
    _ => None, // Paused, Pending, and values Meta adds later
}
```

`WebhookEvent::TemplateStatusUpdated` (field
`message_template_status_update`) arrives when review ends; only
`APPROVED` templates can be sent. Also watch `TemplateQualityUpdated` and
`TemplateCategoryUpdated`. Subscribe the app to those fields.

## List, edit, delete

```rust
let query = TemplateListQuery::new().status(TemplateStatus::Approved);
let mut stream = std::pin::pin!(templates.list_stream(&query));
let mut found = Vec::new();
while let Some(template) = stream.next().await {
    found.push(template?);
}
```

- `list(&query)` returns one `Page<TemplateInfo>`; `list_stream` follows
  the cursors. `get(&id)`, `get_fields(&id, &["status"])`. Cache the list:
  management endpoints are rate limited per WABA.
- `edit(&id, &TemplateEdit::components(vec![..]))` **replaces every
  component**. Only `APPROVED`, `REJECTED` or `PAUSED` templates can be
  edited; an approved one keeps its category and allows 10 edits in 30
  days (once per 24 h). Edits are not replayed on timeouts.
- `delete_by_name(name)` deletes every language; `delete_by_id(name, &id)`
  one; `delete_by_ids(&ids)` up to 100, all or nothing. An approved name
  cannot be reused for 30 days.
- Also: `library` / `create_from_library` (Meta's pre-approved library),
  `migrate_from`, `compare`, `unpause`.

## Pitfalls

- `create` validates locally (`TemplateDefinition::validate`): names,
  lengths, button counts, and placeholders **against the declared
  format**. Positional placeholders read `{{1}}, {{2}}, …` in order;
  every placeholder needs an example; `{{first_name}}` without
  `ParameterFormat::Named` is a `Validation` error (the test proves it).
- A template exists per **language**: create each language you send in.
- At most 100 creations per WABA per hour; the WABA total is limited too
  (`ErrorKind::TemplateLimitReached`, 2388019). Content rejections are
  `ErrorKind::TemplateRejected`.
- Webhook and client enums share names: the webhook's
  `wa_rs::webhooks::fields::TemplateCategory` is not
  `wa_rs::client::templates::TemplateCategory`. Alias one on import.

## What wa-rs does not do

- No archive/unarchive (Meta documents no endpoint for it).
- It does not store which templates are approved: keep your own table,
  updated from the webhooks.
- Product-card carousels: the code allows 2–10 cards where Meta's page
  says "exactly two"
  ([open question 22](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#product-details)).

## Related skills

`wa-rs-send-templates` (sending them), `wa-rs-otp-login` (authentication
templates), `wa-rs-media` (header handles), `wa-rs-webhook-events`,
`wa-rs-marketing`, `wa-rs-commerce` (catalog and multi-product templates).
