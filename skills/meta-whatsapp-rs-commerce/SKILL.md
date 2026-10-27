---
name: meta-whatsapp-rs-commerce
description: "WhatsApp commerce with meta-whatsapp-rs - commerce settings (cart, catalog visibility), reading the WABA's connected catalog and its products page by page (Waba::product_catalogs, Catalog::products_stream), single-product, multi-product (up to 30 products) and catalog messages from a Meta catalog, catalog, multi-product and product-card carousel templates (two cards at creation, up to 10 at send) to reach customers outside the 24-hour window, and carts coming back as order webhooks (OrderContent, product_items) to re-check and fulfil. Load when listing a catalog's products, showing products or a catalog in WhatsApp, enabling the cart, handling orders placed in WhatsApp, or sending product templates."
---

# meta-whatsapp-rs-commerce

> **Verified against meta-whatsapp-rs 3c4a1504b94055b9ec17fbdfd4b4b933501a69b9 (2026-10-27).** On another revision, trust the code over this page.

Reference code: [examples/commerce.rs](examples/commerce.rs), compiled and
tested by meta-whatsapp-rs's own gate.

## When to use

A shop whose catalog is connected to its WhatsApp Business Account:
showing products, letting customers build a cart in WhatsApp, receiving
the order. Upload and manage the catalog itself in Meta's Commerce Manager
or Catalog API — meta-whatsapp-rs only reads it.

## Commerce settings

```rust
let commerce = client.commerce(phone_number_id);
commerce.set_catalog_visible(true).await?; // hidden by default
commerce.set_cart_enabled(true).await // Meta's default
```

`commerce.settings()` reads them; `update_settings(&update)` with a
`CommerceSettingsUpdate` sets both at once. Hiding the catalog makes
existing catalog links show an "invalid catalog link" warning.

## Reading the catalog

```rust
let catalogs = client.waba(waba_id).product_catalogs(&[]).await?;
let Some(catalog) = catalogs.data.into_iter().next() else {
    return Ok(Vec::new()); // no catalog connected yet
};
```

`client.catalog(catalog.id).products_stream(&query)` follows Meta's
cursors to the last page; `products(&query)` returns one `Page`, whose
`next_cursor()` is the next query's `after`. `ListProducts` carries
`return_only_approved_products`, and in `common` (`ProductFilters`)
`fields`, a JSON `filter` and `limit`; `product_sets_stream`,
`client.product_set(id)` and `client.product(id)` read the rest (numeric
ids, `ProductSetId` and `ProductItemId`); a product's `retailer_id` is its SKU.

## Product messages (inside the 24-hour window)

```rust
let one = OutboundMessage::product(to.clone(), catalog_id.clone(), "SKU-1");
messages.send(&one).await?;
let picks = OutboundMessage::product_list(
    to.clone(),
    "Autumn picks", // header, required
    "Tap to see more",
    catalog_id,
    [
        ProductSection::new("Shirts", ["SKU-1", "SKU-2"]),
        ProductSection::new("Belts", ["SKU-9"]),
    ],
); // at most 30 products; titles required with several sections
messages.send(&picks).await?;
messages
    .send(&OutboundMessage::catalog(to, "Browse the whole shop"))
    .await?;
```

Products are addressed by their retailer id (your SKU in the catalog).
`ProductCarousel` (2–10 `ProductCard`s) scrolls products horizontally.

## Product templates (any time)

```rust
TemplateDefinition::new("autumn_picks", "en_US", TemplateCategory::Marketing)
    .component(TemplateComponent::header_text("Autumn picks"))
    .component(TemplateComponent::body_positional(
        "Hi {{1}}, picked for you.",
        ["Alex"],
    ))
    .component(TemplateComponent::buttons([Button::mpm("View items")]))
```

```rust
TemplateMessage::new("autumn_picks", "en_US")
    .body([Parameter::text("Alex")])
    .mpm_button(0, "SKU-1", [MpmSection::new("Shirts", ["SKU-1", "SKU-2"])])
```

A catalog template has a `Button::catalog(text)`, filled at send time by
`catalog_button(index, thumbnail_product_retailer_id)`. A single-product
template has `TemplateComponent::header_product()` and `Button::spm(text)`;
at send time its header is
`Parameter::product(product_retailer_id, catalog_id)`. Creating and
approving them: `meta-whatsapp-rs-templates`.

A product-card carousel template is created with exactly two cards,
then sent with up to 10, one product each:

```rust
let card = || {
    CarouselCard::new([
        TemplateComponent::header_product(),
        TemplateComponent::buttons([Button::spm("View")]),
    ])
};

.component(TemplateComponent::carousel([card(), card()])) // two, not one per product

.carousel(skus.iter().zip(0..).map(|(sku, card_index)| {
    let product = Parameter::product(*sku, catalog_id.clone());
    CarouselCardParameters::new(card_index, [SendComponent::header(product)])
}))
```

## Orders

```rust
let WebhookEvent::MessageReceived { message, .. } = event else {
    return None;
};
match &message.content {
    Inbound::Order(order) => Some((&message.id, order)), // idempotency key: the message id
    _ => None,
}
```

`OrderContent` carries `catalog_id`, the customer's `text` and
`product_items` (`product_retailer_id`, `quantity`, `item_price`,
`currency`). Prices and stock are what the customer saw: re-check them
against your own system before charging. Webhooks can be delivered twice:
key the order by the message id.

## Pitfalls

- Product, list and catalog messages are interactive, hence free-form:
  outside the window they fail with 131047; use the templates.
- A retailer id that is not in the connected catalog is refused by Meta,
  not locally: meta-whatsapp-rs checks presence and counts only.
- `item_price` is a float in Meta's example: never compute money from it.
- A product-card carousel template (any card with
  `TemplateComponent::header_product()`) is created with exactly two
  cards: `create` refuses any other count with a `Validation` error on
  `components[<i>].cards`, before any request (the test proves it). Once
  approved, a send carries up to 10 cards (11 are refused locally). An
  edit (`TemplateEdit`) of an existing one keeps the general 2–10.

## What meta-whatsapp-rs does not do

- No catalog upload or inventory management, no payment collection
  (payments are out of scope, see
  [coverage](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/coverage.md)),
  no order state machine.

## Related skills

`meta-whatsapp-rs-send-messages`, `meta-whatsapp-rs-templates`, `meta-whatsapp-rs-send-templates`,
`meta-whatsapp-rs-webhook-events`, `meta-whatsapp-rs-documents` (receipts and invoices for the
order), `meta-whatsapp-rs-marketing`.
