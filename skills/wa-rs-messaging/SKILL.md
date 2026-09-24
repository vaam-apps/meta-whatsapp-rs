---
name: wa-rs-messaging
description: "Sending WhatsApp messages with wa-rs - OutboundMessage constructors (text, media, location, contacts, reactions), interactive buttons/lists/CTA/Flows, catalog and product messages, templates at send time (the TemplateMessage builder, positional vs named parameters), media upload and SHA-256-verified download, marketing via the Marketing Messages API vs the Cloud API, opt-outs (131050) and per-user marketing limits (131049), In-App Signup opt-in links, and why sends are never retried blindly. Load when writing any code that sends, uploads or downloads."
---

# wa-rs-messaging

> **Verified against wa-rs 91431ae (2026-09-24).** On another revision, trust
> the code over this page (see `skills/README.md`).

Modules: `wa_rs::client::{messages, media, templates, marketing, signups}`.
Errors, retries and recipients are in the `wa-rs` skill: read its "Retries"
and "Recipients" sections first.

## Send

```rust
use wa_rs::client::messages::{Image, OutboundMessage, ReplyButton};
use wa_rs::core::recipient::Recipient;

let messages = client.messages(phone_number_id); // merchant: client.with_token(..)
let to = Recipient::user("US.13491208655302741918"); // BSUID from the webhook
let sent = messages
    .send(&OutboundMessage::text(to.clone(), "Your order has shipped.")
        .reply_to(inbound_wamid)          // quoted reply
        .callback_data("order:860198"))   // echoed as Status::biz_opaque_callback_data
    .await?;
let wamid = sent.message_id();            // Option<&MessageId>: match status webhooks on it

messages.send(&OutboundMessage::reply_buttons(to.clone(), "Keep this delivery slot?", [
    ReplyButton::new("keep", "Keep"), ReplyButton::new("change", "Change"),
])).await?;
messages.send(&OutboundMessage::new(to, Image::new(media_id).caption("Your voucher"))).await?;
```

- `OutboundMessage::new(recipient, content)` takes any content type
  (`Text`, `Image`, `Document`, `ReplyButtons`, `ListMessage`, `CtaUrl`,
  `FlowMessage`, `ProductList`, `TemplateMessage`, …); the named constructors
  are shortcuts. Full list: [references/message-types.md](references/message-types.md).
- **Local validation first.** `send` checks every documented limit and fails
  with `Error::Validation(v)` whose `v.field` is the JSON path
  (`interactive.action.buttons[3]`) — nothing was sent. For templates it only
  checks the name: call `template.validate()?` yourself to check coupon,
  carousel and MPM limits before sending.
- **Not idempotent.** A timeout or 5xx is returned, not replayed (see `wa-rs`).
  Correlate with status webhooks via `callback_data` before re-sending.
- Outside the 24-hour window only templates go through
  (`ErrorKind::CustomerServiceWindowClosed`, 131047).
- Read receipts for a **received** message: `messages.mark_read(&inbound_id)`
  (idempotent); `mark_read_with_typing_indicator(&inbound_id)` only right
  before replying (not replayed: a late "typing…" after your reply looks
  broken). Reactions: `messages.react(to, inbound_id, "👍")` (`""` removes).

## Interactive, Flows, catalog

```rust
use wa_rs::client::messages::{
    CtaUrl, FlowParameters, FlowRef, Header, ListMessage, ListRow, ListSection,
    OutboundMessage, ProductSection,
};

let list = ListMessage::new("Pick a delivery slot", "Slots", [
    ListSection::new("Tomorrow", [ListRow::new("t9", "9–12").description("Morning")]),
]);
let cta = CtaUrl::new("Track your parcel", "Track", "https://shop.example/t/860198")
    .header(Header::text("Order 860198"));
let flow = OutboundMessage::flow(to.clone(), "Book a fitting",
    FlowParameters::new(FlowRef::Name("fitting_v1".into()), "Book").token("session-42"));
let bestsellers = OutboundMessage::product_list(to.clone(), "Bestsellers", "Tap to see more",
    catalog_id, [ProductSection::new("Shoes", ["SKU-1", "SKU-2"])]);
```

Replies arrive as `MessageReceived` with `InteractiveReply::ButtonReply` /
`ListReply` (`.id`), `NfmReply` (Flow response JSON), and carts as
`MessageContent::Order` (`wa-rs-webhooks`). Catalog inventory itself is
managed in Meta's Commerce Manager / Catalog API, not here;
`client.commerce(pnid)` only reads/sets the number's commerce settings.

## Templates at send time

```rust
use wa_rs::client::templates::{Parameter, TemplateMessage};

// Positional template: "Hi {{1}}, order {{2}} has shipped."
let t = TemplateMessage::new("order_update", "en_US")
    .header(Parameter::document_id(invoice_media_id, Some("invoice-860198.pdf".into())))
    .body([Parameter::text("Jessica"), Parameter::text("860198")])
    .url_button(0, "860198");                 // the {{1}} suffix of the URL button at index 0
// Named template ("Hi {{first_name}}"): one Parameter::named per placeholder.
let n = TemplateMessage::new("welcome", "en_US").body([Parameter::named("first_name", "Jessica")]);
client.messages(pnid).send(&OutboundMessage::template(to.clone(), t)).await?;
```

- The language code must be the one the template was **approved** in.
- Parameter style must match the approved template (`ParameterFormat`):
  positional → `Parameter::text` in placeholder order; named →
  `Parameter::named`. Mismatch → `ErrorKind::TemplateParameterMismatch`.
- Other parameters: `Parameter::{currency, date_time, image_id, image_link,
  video_id, document_link, location, product, payload, coupon_code}`; buttons:
  `quick_reply_button`, `copy_code_button`, `flow_button`, `catalog_button`,
  `mpm_button`, `voice_call_button`; `carousel`, `limited_time_offer`,
  `tap_target`. Button `index` is the button's position in the template.
- `Parameter`'s `Debug` redacts text and coupon codes; keep it that way in logs.
- Creating/approving templates, and OTP templates: `wa-rs-templates-otp`.

## Media

```rust
use futures::StreamExt;
use wa_rs::core::ids::MediaId;

let media = client.media(pnid);
let id = media.upload(bytes, "image/png", "voucher.png").await?;   // MIME + size checked first
let small = media.download_bytes(&MediaId::new(inbound_id), 5 * 1024 * 1024).await?; // verified
let mut dl = media.download(&MediaId::new(inbound_id)).await?.verified()?;
while let Some(chunk) = dl.body.next().await {
    let chunk = chunk?;   // the LAST item is an error if the SHA-256 does not match:
    write_to_temp(&chunk); // do not use the file until the loop ends without one
}
```

- Supported types/limits are Meta's table (`media::validate_upload`): images
  JPEG/PNG 5 MB, documents (PDF, Office, text) 100 MB, audio/video 16 MB,
  WebP only as stickers. Anything else is refused before upload.
- Uploaded ids live 30 days; ids from webhooks 7 days; a media URL 5 minutes.
- The token is only sent to Meta hosts over HTTPS; a download URL elsewhere
  fails with a validation error.
- A hash mismatch is `TransportError::Integrity` (retryable: fetch again).
- `media.restrict_to_phone_number()` makes Meta refuse media not uploaded on
  that number — a tenant guard for multi-merchant setups.
- Uploads are not replayed on transient errors (a replay creates a second
  object). Template *creation* needs a Resumable Upload handle instead of a
  media id: `wa-rs-templates-otp`.

## Marketing

Two routes for a **marketing template**:

| | Cloud API | Marketing Messages API (MM API) |
| --- | --- | --- |
| Call | `client.messages(pnid).send(&OutboundMessage::template(to, t))` | `client.marketing(pnid).send(&to, &t, &MarketingOptions::new())` |
| Templates | any approved | **only** approved `MARKETING` (`MarketingNotAllowed`: 131055, 134100) |
| Extras | — | `strict()` (no Cloud API fallback), `message_activity_sharing`, `bid_multiplier` (max-price beta) |
| Setup | none | WABA onboarded (`client.marketing_account(waba).onboarding_status()`) |

- A new or revived template needs ~10 minutes of ad sync on the MM API
  (`TemplateSyncing`, 134101): retry later from your queue.
- A bid multiplier / max price cannot go to a BSUID (`RecipientNotSupported`,
  131062); by BSUID alone, delivery optimization is off.
- `set_cloud_api_marketing_disabled(true)` on the WABA makes Cloud API refuse
  marketing templates (131063) — only do that once MM API works.

### Opt-outs and engagement limits — never retry these

- **131050 `MarketingOptedOut`**: the customer stopped your marketing. Record
  it; do not send marketing again until they opt back in. The same choice
  arrives as `WebhookEvent::UserPreferenceChanged` (`PreferenceValue::Stop` /
  `Resume`, category `MarketingMessages`).
- **131049 `EcosystemEngagementLimit`**: per-user marketing limit. Do not
  retry for at least 24 h; never auto-retry.
- Check both the send result **and** `failed` status webhooks
  (`status.errors[i].kind()`): these can arrive either way.

### In-App Signup: opt-in links

```rust
use wa_rs::client::signups::{NewSignup, SignupPolicy, deep_link};

let created = client.signups(waba_id)
    .create(&NewSignup::new(
        "Get our deals on WhatsApp", "You're in! Code {{promo_code}}", "https://shop.example/privacy")
        .promo_code("WELCOME10")
        .policy(SignupPolicy::accept_terms())) // FIRST create of a business only, after it agreed
    .await?;
let link = deep_link("+15551234567", &created.id)?; // wa.me/15551234567/signup/<id>; add https://
```

The first `create` of a business **must** carry `accept_terms()` (without:
2494168), every later one must **not** (with: 2494176, nothing created —
resend without `policy`). Only accept after the business actually agreed;
the library does not track consent. `deep_link` requires strict E.164 with
`+`. `update`, `enable`, `disable`, `list`, `default_messaging_customer_base`
are on `Signups` too.

## What the library does not do

- No send queue, rate limiting across your fleet, or retry scheduler.
- No opt-out registry: persisting 131050 / `user_preferences` is yours.
- No template-parameter check against the *approved* template (it does not
  know it); Meta answers `TemplateParameterMismatch`.
- Payments message types: only via `MessageContent::Raw` (unmodelled).
