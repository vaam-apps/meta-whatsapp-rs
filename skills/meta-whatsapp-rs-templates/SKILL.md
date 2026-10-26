---
name: meta-whatsapp-rs-templates
description: "Managing WhatsApp message templates with meta-whatsapp-rs - TemplateDefinition and TemplateComponent (header, body, footer, buttons, carousel, limited-time offer), categories (utility, marketing, authentication), positional vs named placeholders and their examples, media header handles, creating, listing (list_stream), editing, deleting, the template library, archiving and unarchiving, template groups, local validation, and following review with TemplateStatusUpdated webhooks. Load when creating or changing templates, syncing a template catalog, or reacting to a template being approved, rejected, paused or re-categorized."
---

# meta-whatsapp-rs-templates

> **Verified against meta-whatsapp-rs b7211bc1f282f873b605e7a3a1126ce4e45e5677 (2026-09-26).** On another revision, trust the code over this page.

Reference code: [examples/manage.rs](examples/manage.rs), compiled and
tested by meta-whatsapp-rs's own gate.

## When to use

Defining what a template **is** and getting it approved. Sending one with
its values is `meta-whatsapp-rs-send-templates`; authentication (OTP) templates are
`meta-whatsapp-rs-otp-login`. Module `meta_whatsapp_rs::client::templates`, reached with
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
needs a Resumable Upload handle (`meta-whatsapp-rs-media`), not a media id:

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

- `list(&query)` returns one `Page<TemplateInfo>` (the next page: the
  same query with `after` = `page.next_cursor()`); `list_stream` follows
  the cursors itself and refuses a query that sets `after` or `before`
  (~~it ignored them~~: until 6034804, 2026-09-24).
  `get(&id)`, `get_fields(&id, &["status"])`. Cache the list: management
  endpoints are rate limited per WABA.
- `edit(&id, &TemplateEdit::components(vec![..]))` **replaces every
  component**. Only `APPROVED`, `REJECTED` or `PAUSED` templates can be
  edited; an approved one keeps its category and allows 10 edits in 30
  days (once per 24 h). Edits are not replayed on timeouts.
- `delete_by_name(name)` deletes every language; `delete_by_id(name, &id)`
  one; `delete_by_ids(&ids)` up to 100, all or nothing. An approved name
  cannot be reused for 30 days.
- Also: `library` / `create_from_library` (Meta's pre-approved library),
  `migrate_from`, `compare`, `unpause`.

## Archive, unarchive, group

- `archive(&ids)` / `unarchive(&ids)`: up to 100 template ids, each
  digits only. An archived template cannot be sent and is deleted 28 days
  later; unarchiving within that window restores its previous status.
  A partial failure is not an error: read `failed_templates` (id →
  reason) next to `archived_templates` / `unarchived_templates`. Not
  replayed on timeouts.
- Template groups gather templates for `Analytics::template_group`:
  `create_group(&TemplateGroupCreate::new(name, ids))` (unique name, at
  most 256 characters; `.description(..)`, at most 512),
  `get_group(&group_id)` (the group, its WABA and the first page of its
  templates), `update_group(&group_id, &update)` with a
  `TemplateGroupUpdate` (`name`, `description`, `add_templates`,
  `remove_templates`), `delete_group(&group_id)` (keeps the templates). A template is in at most one group. A group id
  that is not digits is a `Validation` error before any request.

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
  `meta_whatsapp_rs::webhooks::fields::TemplateCategory` is not
  `meta_whatsapp_rs::client::templates::TemplateCategory`. Alias one on import.

## What meta-whatsapp-rs does not do

- No list of a WABA's template groups, and no call for the pages after
  the first of a group's templates: Meta's changelog names the list
  endpoint, but no page documents it (roadmap L23). Keep the group ids
  `create_group` returns.
- It does not store which templates are approved: keep your own table,
  updated from the webhooks.
- Product-card carousels: the code allows 2–10 cards where Meta's page
  says "exactly two"
  ([OPEN_QUESTIONS.md #22](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#product-details),
  decided on 2026-09-26: creation will check exactly two, roadmap L20e).

## Related skills

`meta-whatsapp-rs-send-templates` (sending them), `meta-whatsapp-rs-otp-login` (authentication
templates), `meta-whatsapp-rs-media` (header handles), `meta-whatsapp-rs-webhook-events`,
`meta-whatsapp-rs-marketing`, `meta-whatsapp-rs-commerce` (catalog and multi-product templates).
