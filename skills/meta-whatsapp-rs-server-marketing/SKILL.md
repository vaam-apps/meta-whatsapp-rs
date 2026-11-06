---
name: meta-whatsapp-rs-server-marketing
description: "Sending marketing messages and managing max price and Click-to-WhatsApp welcome message sequences through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - a marketing template sent through Meta's Marketing Messages API (product policy, activity sharing, per-message bid multiplier), onboarding state, the Cloud API marketing switch, duplicating a template at another max price, a template's max price, reach estimates, and welcome message sequences; why the max price agreement is a legal act only an operator's admin key signs, 403 feature_not_available until the deployment sets WA_SERVER_LEGAL_ACTS=on, and why no other route signs it. Load when a CMS or a Medusa store runs marketing campaigns through the service."
---

# meta-whatsapp-rs-server-marketing

> **Verified against meta-whatsapp-rs 1474437216b078be1fc45ae3e1ce391420f75a60 (2026-11-06).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/marketing.ts](examples/marketing.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#marketing-messages-api-max-price-and-welcome-message-sequences).

## When to use

A store sends promotions to opted-in customers, tunes what Meta charges
for them, or prepares the welcome a Click-to-WhatsApp ad opens. Deploy the
service first (`meta-whatsapp-rs-server`). The key needs the scope
`marketing`, which `send` and `templates` do not grant (a key made before
it has none). Another tenant's number or WABA is `404 not_found` on every
route, and so is a template or a sequence that is not your WABA's.

## The routes

See [references/routes.md](references/routes.md): the send, onboarding, the Cloud API switch, template duplicates, a template's max price, reach estimates, welcome message sequences, and the operator's agreement.

## Send a marketing template

```ts
export async function sendMarketing(
  api: WhatsApp,
  pn: string,
  body: SendMarketingMessage,
  idempotencyKey: string,
): Promise<string> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/marketing-messages", {
    params: { path: { pn }, header: { "Idempotency-Key": idempotencyKey } },
    body,
  });
```

The body is `to` and `template` as for a message send (E.164 with its
`+`, Meta's template object), plus `product_policy` (`CLOUD_API_FALLBACK`,
Meta's default, or `STRICT`), `message_activity_sharing`, and
`bid_spec.per_message_bid_multiplier`, a positive number scaling the
template's max price for this message. The answer is `202` as for any send.

- Only approved marketing templates are accepted (`marketing_not_allowed`
  otherwise); a new or long-unused template needs about ten minutes of ad
  sync. A catalog the template names must be your WABA's.
- A send cut by a timeout is `504` with `may_have_been_sent`: it is never
  replayed, and a retry under the same `Idempotency-Key` is not a second
  send. Meta's texts are not answered: read `error.code` and `error.graph.code`.

## Onboarding and the Cloud API switch

```ts
export async function onboarding(api: WhatsApp, wabaId: string) {
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/marketing/onboarding", {
    params: { path: { waba_id: wabaId } },
  });
```

`owner_business.terms_status` is Meta's word for the Terms of Service
state (signed, request sent, not started). The service cannot accept the
terms for you: someone with full control of the business portfolio does it
in WhatsApp Manager. `PUT …/cloud-api-marketing` with `disabled: true` makes
Cloud API refuse marketing templates, so they go through the Marketing
Messages API only; it is audited.

## Max price

```ts
export async function duplicateAtPrice(
  api: WhatsApp,
  wabaId: string,
  sourceTemplateId: string,
  name: string,
  bidAmount: number,
  idempotencyKey: string,
) {
```

`bid_amount` is per 1,000 deliveries in the smallest unit of the WABA's
currency (0.87 per delivery is `87000`), from 1 to 2147483647;
`bid_country_multiplier_overrides` maps countries (two capital letters) to a
multiplier above 0 and at most 10, 240 at most, and on a change replaces the
whole map. The duplicate is approved at once and counts toward your
template limit. A price applies within an hour; deleting it returns the
template to rate-card pricing.

```ts
export async function reachEstimate(api: WhatsApp, wabaId: string, countries: string[]) {
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/marketing/reach-estimates", {
    params: {
      path: { waba_id: wabaId },
      query: { countries: countries.join(","), date_interval: "L7D" },
    },
  });
```

## The agreement is a legal act, and the operator's

Max price needs Meta's beta agreement, signed once per business
portfolio. **Nothing you call as a tenant signs it**: not a duplicate, a
price change or a send. When it is missing you get Meta's refusal as it is.
The operator signs it, with an admin key, only when the deployment sets
`WA_SERVER_LEGAL_ACTS=on`:

```ts
export async function signMaxPriceAgreement(
  admin: WhatsApp,
  businessId: string,
  signerName: string,
  signerEmail: string,
  businessToken: string,
): Promise<string | null> {
```

- Off (the default) it is `403 feature_not_available` before the body is
  read, and nothing is asked of Meta. A tenant's key is `403 forbidden`.
- On, it is one request, never replayed (not after a timeout, not on Meta's
  `429` within five minutes of the last email), with the `token` of the
  body: a system user or business token allowed to manage the business, an
  admin of the portfolio's, used once and stored nowhere. Meta emails the
  signer an acceptance link and the answer is pending.
- It is **audited** with the admin key's id and the business
  (max_price_agreement_signed). The signer's name and email are in no log,
  error, audit event or answer; neither is Meta's text, which quotes them.

## Welcome message sequences

```ts
export async function createSequence(
  api: WhatsApp,
  wabaId: string,
  body: CreateWelcomeSequence,
  idempotencyKey: string,
) {
```

The body is `name` and `welcome_message_sequence` (`text`,
`autofill_message` with `content`, `ice_breakers` with `title`), at least
one part; the service bounds a name at 128 characters, texts at 1,024 and
quick replies at 20, where Meta's guide states none. The answer is the
`sequence_id` for the ad's creative. A change or a deletion is `404` for a
sequence that is not your WABA's and, for one an active ad uses, Meta's
refusal. The welcome text is in no log.

## What meta-whatsapp-rs does not do

Not offered: the agreement's state read, the partner Intent API (it emails
a business's admins an invitation to accept the terms), the client WABAs
listing, and a GIF header's send-time parameter (no page of Meta's shows
it; create the template with a `GIF` header format, send it as Meta's
object). No call of these routes was run against Meta live.

## Related skills

`meta-whatsapp-rs-server`, `meta-whatsapp-rs-server-send`,
`meta-whatsapp-rs-marketing`.
