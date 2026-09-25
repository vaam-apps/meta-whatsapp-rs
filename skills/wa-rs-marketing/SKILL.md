---
name: wa-rs-marketing
description: "WhatsApp marketing with wa-rs - collecting opt-ins with In-App Signup deep links (the terms rule 2494168 vs 2494176) and QR codes, sending marketing templates on the Cloud API or the Marketing Messages API (onboarding status, MarketingOptions, TemplateSyncing 134101, BSUID limits), honouring opt-outs (131050, user_preferences stop and resume) and the per-user marketing limit (131049) without ever retrying them, and messaging and template analytics. Load when building campaigns, promotions, newsletters, opt-in or opt-out handling, or marketing reports on WhatsApp."
---

# wa-rs-marketing

> **Verified against wa-rs 1e63b2ba9c94fb9a4f2895f0dc9efc27ee749274 (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/marketing.rs](examples/marketing.rs), compiled
and tested by wa-rs's own gate.

## When to use

Anything promotional: opt-in collection, campaign sends, consent
tracking, results. Templates themselves: `wa-rs-templates`; their
parameters: `wa-rs-send-templates`.

## Collect opt-ins

```rust
let mut signup = NewSignup::new(
    "Get our deals on WhatsApp",
    "You're in! Code {{promo_code}}",
    "https://shop.example/privacy",
)
.promo_code("WELCOME10");
if first_signup_of_this_business {
    // Accepts Meta's marketing terms for the business: only after it agreed.
    signup = signup.policy(SignupPolicy::accept_terms());
}
let created = client.signups(waba_id).create(&signup).await?;
deep_link(business_number, &created.id) // wa.me/<digits>/signup/<id>; add https://
```

The first In-App Signup of a business **must** carry `accept_terms()`
(without: 2494168), every later one must **not** (with: 2494176, nothing
created). `deep_link` needs strict E.164 with `+`. QR codes and short
links open a chat with a prefilled message (1–140 characters):

```rust
let request =
    CreateQrCode::new("Hi! Send me the autumn catalog").with_image(QrImageFormat::Svg);
let qr = client.qr_codes(phone_number_id).create(&request).await?;
Ok(qr.deep_link_url) // https://wa.me/message/<code>; qr.qr_image_url is the image
```

## Send: Cloud API or Marketing Messages API

```rust
let status = client
    .marketing_account(waba_id)
    .onboarding_status()
    .await?;
if status == Some(OnboardingStatus::Onboarded) {
    let options = MarketingOptions::new(); // .strict(): no Cloud API fallback
    client
        .marketing(phone_number_id)
        .send(&to, &offer, &options)
        .await
} else {
    let message = OutboundMessage::template(to, offer);
    client.messages(phone_number_id).send(&message).await
}
```

| | Cloud API | Marketing Messages API |
| --- | --- | --- |
| Call | `client.messages(pnid).send` | `client.marketing(pnid).send(&to, &template, &options)` |
| Templates | any approved | approved `MARKETING` only (`ErrorKind::MarketingNotAllowed`, 131055, 134100) |
| Extras | — | `strict()`, `message_activity_sharing(..)`, `bid_multiplier(..)`, `product_policy(..)` |

- A new template, or one unused for 7 days, needs about 10 minutes of ad
  sync on the MM API (`ErrorKind::TemplateSyncing`, 134101): retry later
  from your queue.
- A bid multiplier cannot go to a BSUID (131062); by BSUID alone,
  delivery optimization is off.
- `marketing_account(waba).set_cloud_api_marketing_disabled(true)` makes
  the Cloud API refuse marketing templates (131063): only once the MM API
  works. Partners list client WABAs by status with
  `marketing_business(business_id).client_wabas_with_status_stream(..)`,
  with a `ListClientWabas` (~~`(&statuses, after)` / `(&statuses)`~~:
  until 6034804, 2026-09-24).

## Opt-outs and per-user limits: never retry

```rust
WebhookEvent::UserPreferenceChanged { preference, .. } => match preference.value {
    PreferenceValue::Stop => vec![Consent::OptedOut(preference.user_id.clone())],
    PreferenceValue::Resume => vec![Consent::OptedIn(preference.user_id.clone())],
    _ => vec![],
},
```

- **131050, `MarketingOptedOut`**: the user stopped your marketing. Record
  it; no marketing until a `Resume`.
- **131049, `EcosystemEngagementLimit`**: WhatsApp's cap on marketing
  from all businesses to one user. Wait at least 24 hours; never
  auto-retry (`is_retryable()` is `false` on purpose).
- Both arrive either on the send or as a `failed` status webhook
  (`status.errors[i].kind()`): handle both (the example's
  `consent_changes` does).

## Measure

`client.analytics(waba_id)`: `messaging`, `conversation`, `pricing`,
`template` (call `enable_template_insights()` once first — it cannot be
undone), `template_group`. Every status webhook also carries `pricing` and
your `biz_opaque_callback_data`.

```rust
let analytics = client.analytics(waba_id).messaging(&query).await?;
let delivered = analytics
    .iter()
    .flat_map(|a| &a.data_points)
    .filter_map(|p| p.delivered)
    .sum();
```

## Pitfalls

- Opt-in first: message only people who agreed; record how and when.
- Marketing templates to US numbers (`+1` US area codes) are not
  delivered at the time of writing.
- Messaging limits (unique users per rolling 24 h) and throughput
  (80 msg/s by default; `130429`) are Meta's: pace campaigns in your queue.
- A quick-reply "Stop promotions" arrives as an inbound `Button` message:
  treat it as an opt-out in your code.

## What wa-rs does not do

- No consent registry, campaign scheduler, audience segmentation or
  conversion tracking.
- Accepting the In-App Signup terms accepts Meta's marketing terms for
  the business: a legal decision
  ([open question 26](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#product-details)).
- MM API max-price agreement, partner allowlist and reach estimates are
  not wrapped.

## Related skills

`wa-rs-templates`, `wa-rs-send-templates`, `wa-rs-errors`,
`wa-rs-webhook-events`, `wa-rs-commerce`, `wa-rs-documents` (voucher
images).
