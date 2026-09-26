# Marketing and commerce

**Goal:** collect opt-ins, send campaigns and order updates with approved
templates, respect opt-outs and per-user limits, show products from your
catalog, and measure what happened.

Example: [`send_message.rs`](../../crates/meta-whatsapp-rs/examples/send_message.rs).
Agent skills: [`meta-whatsapp-rs-marketing`](../../skills/meta-whatsapp-rs-marketing/SKILL.md)
(opt-ins, campaigns, opt-outs, analytics),
[`meta-whatsapp-rs-commerce`](../../skills/meta-whatsapp-rs-commerce/SKILL.md) (catalogs, orders),
[`meta-whatsapp-rs-templates`](../../skills/meta-whatsapp-rs-templates/SKILL.md) (template lifecycle) and
[`meta-whatsapp-rs-send-templates`](../../skills/meta-whatsapp-rs-send-templates/SKILL.md).

## 1. On Meta's side

- **Opt-in first.** You may only message people who agreed to hear from
  you on WhatsApp; record how and when.
- **Templates** are reviewed by Meta before use, and Meta may move one to
  another category (it is then billed differently). Utility templates
  follow a user's action (an order); anything promotional is marketing.
- **Messaging limits** are per business portfolio: the number of unique
  users you can reach with templates in a rolling 24 hours outside a
  customer service window. A new portfolio starts at 250; verifying the
  business raises it to 2,000, then Meta scales it with quality and use.
- **Throughput:** 80 messages per second per number by default (up to
  1,000 after an automatic upgrade); above it Meta answers `130429`.
  Sending too fast to *one* user trips the pair rate limit (`131056`).
- **Marketing Messages API (MM API)** is optional: the business accepts its
  terms and is onboarded (check below). Cloud API sends marketing templates
  without it.
- **Catalogs** are uploaded and connected to the WABA in Commerce Manager or
  Meta's Catalog API, outside the WhatsApp API.
- **Marketing templates are not delivered to US numbers** (`+1` with a US
  area code) at the time of writing: expect errors there.

Meta's pages:
[templates/overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/overview),
[template-categorization](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/template-categorization),
[marketing-templates](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/marketing-templates),
[per-user limits](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/marketing-templates/per-user-limits),
[marketing-messages/overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/marketing-messages/overview),
[messaging-limits](https://developers.facebook.com/documentation/business-messaging/whatsapp/messaging-limits),
[throughput](https://developers.facebook.com/documentation/business-messaging/whatsapp/throughput),
[getting-opt-in](https://developers.facebook.com/documentation/business-messaging/whatsapp/getting-opt-in),
[in-app-signup](https://developers.facebook.com/documentation/business-messaging/whatsapp/in-app-signup),
[qr-codes](https://developers.facebook.com/documentation/business-messaging/whatsapp/qr-codes),
[catalogs/catalogs-overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/catalogs/catalogs-overview).

## 2. Collect opt-ins

**In-App Signup** gives you a deep link that subscribes a user and sends
them a confirmation, optionally with a promo code:

```rust
use meta_whatsapp_rs::client::signups::{NewSignup, SignupPolicy, deep_link};

let created = client.signups(waba_id.clone())
    .create(&NewSignup::new("Get our deals on WhatsApp", "You're in! Code {{promo_code}}", "https://shop.example/privacy")
        .promo_code("WELCOME10")
        .policy(SignupPolicy::accept_terms())) // FIRST signup of the business only
    .await?;
let link = deep_link("+15551234567", &created.id)?; // wa.me/15551234567/signup/<id>
```

The first `create` of a business must carry `accept_terms()` (without it:
`2494168`); later ones must not (with it: `2494176`, nothing created).
Accepting accepts Meta's marketing messages terms on the business's behalf:
a legal act, not a technical one. meta-whatsapp-rs decides nothing legal:
`accept_terms()` is the explicit call, and making it is your decision for
your deployment ([open question](../../OPEN_QUESTIONS.md#product-details)
26, decided on that basis). Meta's page says a webhook notifies you of each
subscription but does not document its payload; meta-whatsapp-rs has no typed event
for it, so it would arrive as `WebhookEvent::Unknown` (or an unknown
message type). Check on a test WABA before relying on it.

**QR codes and short links** open a chat with a prefilled message:

```rust
use meta_whatsapp_rs::client::qr_codes::{CreateQrCode, QrImageFormat};

let qr = client.qr_codes(phone_number_id.clone())
    .create(&CreateQrCode::new("Hi! Send me the autumn catalog").with_image(QrImageFormat::Svg)) // SVG for print
    .await?;
// qr.deep_link_url is https://wa.me/message/<code>; qr.qr_image_url the image
```

The prefilled message is 1–140 characters. The chat it opens also opens a
customer service window when the user sends it.

meta-whatsapp-rs keeps no consent registry: store opt-ins and opt-outs yourself, keyed
by the customer's business-scoped user id (BSUID) and phone number.

## 3. Create templates

```rust
use meta_whatsapp_rs::client::templates::{Button, ParameterFormat, TemplateCategory, TemplateComponent, TemplateDefinition};

let templates = client.templates(waba_id.clone());

// Utility: follows the customer's order. Positional placeholders need an example each.
templates.create(&TemplateDefinition::new("order_shipped", "en_US", TemplateCategory::Utility)
    .component(TemplateComponent::body_positional("Order {{1}} has shipped.", ["860198"]))
    .component(TemplateComponent::buttons([Button::url_with_example(
        "Track", "https://shop.example/track/{{1}}", "860198")])))
    .await?;

// Marketing, with named placeholders and an image header. A header example is a
// Resumable Upload handle, not a media id.
let handle = client.media(phone_number_id.clone())
    .resumable_upload(&app_id, "autumn.png", "image/png", png_bytes).await?;
templates.create(&TemplateDefinition::new("autumn_sale", "en_US", TemplateCategory::Marketing)
    .parameter_format(ParameterFormat::Named) // required for {{first_name}}
    .component(TemplateComponent::header_image(handle.as_str()))
    .component(TemplateComponent::body_named("Hi {{first_name}}, 20% off until Sunday.", [("first_name", "Alex")]))
    .component(TemplateComponent::buttons([Button::quick_reply("Stop promotions")])))
    .await?;
```

`create` validates locally first (names, lengths, button counts,
placeholders against the declared format). Approval is asynchronous: watch
`TemplateStatusUpdated` ([webhooks.md](webhooks.md#8-operational-alerts)).
A tap on a quick-reply button arrives as an inbound `Button` message with
its text: treat "Stop promotions" as an opt-out in your own code.

## 4. Send: Cloud API or MM API

```rust
use meta_whatsapp_rs::client::marketing::MarketingOptions;

let offer = TemplateMessage::new("autumn_sale", "en_US")
    .header(Parameter::image_id(voucher_media_id)) // at send time: a normal media id
    .body([Parameter::named("first_name", "Alex")]);

// Cloud API: any approved template.
client.messages(phone_number_id.clone()).send(&OutboundMessage::template(to.clone(), offer.clone())).await?;
// MM API: approved MARKETING templates only, with delivery optimization.
client.marketing(phone_number_id).send(&to, &offer, &MarketingOptions::new()).await?;
```

| | Cloud API | MM API |
| --- | --- | --- |
| Call | `messages(pnid).send` | `marketing(pnid).send` |
| Templates | any approved | approved marketing only (`MarketingNotAllowed`) |
| Extras | — | `strict()` (no Cloud API fallback), `message_activity_sharing`, `bid_multiplier` |
| Setup | none | `client.marketing_account(waba_id).onboarding_status()` |

- A new template (or one unused for a week) needs about 10 minutes of ad
  sync on the MM API: `ErrorKind::TemplateSyncing` (`134101`). Retry later
  from your queue.
- `marketing_account(waba_id).set_cloud_api_marketing_disabled(true)` makes
  Cloud API refuse marketing templates (`131063`); only once the MM API works.
- Order updates are utility templates on the Cloud API; add
  `.callback_data(format!("order:{id}"))` to find them again in status
  webhooks. Documents (invoices) are in [documents.md](documents.md).
- Pace campaigns with your own queue: the library replays throttled sends
  only within its retry budget, and never replays timeouts.

## 5. Opt-outs (131050) and per-user limits (131049)

Meta documents both as arriving **after** a successful send, as a `failed`
status webhook; handle them on the call too, since the same codes map to the
same `ErrorKind` there.

```rust
use meta_whatsapp_rs::webhooks::fields::PreferenceValue;

match &event {
    WebhookEvent::UserPreferenceChanged { preference, .. } => match preference.value {
        PreferenceValue::Stop => set_marketing_consent(preference.user_id.as_ref(), false).await,
        PreferenceValue::Resume => set_marketing_consent(preference.user_id.as_ref(), true).await,
        _ => {}
    },
    WebhookEvent::StatusUpdated { status, .. } => {
        for error in &status.errors {
            match error.kind() {
                ErrorKind::MarketingOptedOut => set_marketing_consent(status.recipient_user_id.as_ref(), false).await,
                ErrorKind::EcosystemEngagementLimit => hold_marketing_24h(status.recipient_user_id.as_ref()).await,
                _ => {}
            }
        }
    }
    _ => {}
}
```

- **131050, `MarketingOptedOut`:** the user stopped your marketing. Never
  retry; send marketing again only after a `Resume`.
- **131049, `EcosystemEngagementLimit`:** WhatsApp limits how many marketing
  messages one user receives from all businesses. Wait at least 24 hours
  before resending to that user; resending sooner prolongs it.
  `is_retryable()` is `false` on purpose: never auto-retry.
- Users' "not interested" feedback does not produce a webhook; only stop
  and resume do.

## 6. Catalogs and product messages

```rust
use meta_whatsapp_rs::client::messages::ProductSection;

let messages = client.messages(phone_number_id.clone());
messages.send(&OutboundMessage::product(to.clone(), catalog_id.clone(), "SKU-1")).await?;
messages.send(&OutboundMessage::product_list(to.clone(), "Autumn picks", "Tap to see more", catalog_id,
    [ProductSection::new("Shoes", ["SKU-1", "SKU-2"])])).await?;
client.commerce(phone_number_id).set_cart_enabled(true).await?;
```

- Product, product-list and catalog messages are interactive, so free-form:
  inside the 24-hour window only. Outside it, use catalog, multi-product or
  single-product *templates*.
- A cart comes back as an inbound message whose content is
  `MessageContent::Order` (`catalog_id`, `product_items` with retailer id,
  quantity, price, currency). Price and stock are yours to re-check.

## 7. Measure

- Every status webhook carries `pricing` (billable, category) and your
  `biz_opaque_callback_data`: the cheapest per-message record.
- `client.analytics(waba_id)` wraps messaging, conversation, pricing,
  template and template-group analytics; call
  `enable_template_insights()` once before `template(…)`.

## Pitfalls

- Sending a template in a language it was not approved in
  (`TemplateNotFound`), or positional parameters to a named template
  (`TemplateParameterMismatch`).
- Writing numbers without `+`: they go to your business number's country.
- Retrying 131049 or 131050 from a generic retry loop.
- Treating a successful send as delivered.

## Not handled

Consent registry, campaign scheduling and pacing, audience segmentation,
catalog inventory management, conversion tracking through Meta's
Conversions API, and payments ([coverage.md](../coverage.md)).
