# Hosted signup, several WABAs per signup and pre-verified numbers

> **Verified against meta-whatsapp-rs d5b9444ed2f6b0c3dd75e33bbed6530bcd939425 (2026-11-07).** Also checked against Meta's `embedded-signup/hosted-es` and `embedded-signup/pre-verified-numbers` pages as fetched on 2026-11-06.

The code is in [examples/onboarding.ts](../examples/onboarding.ts) (the
functions below, plus one that adds a number to the pool), type-checked
against the service's OpenAPI document.

## Several WABAs in one signup

A merchant may grant several WABAs in one flow. By default the service
binds **one**, the primary: the event's `waba_id`, else the first WABA of
its list. The answer's `waba_id` is that WABA, and `not_bound_waba_ids`
lists the others, which are not bound:

```ts
export function notBound(data: SignupResult): string[] {
  return data.not_bound_waba_ids ?? [];
}
```

With `WA_SIGNUP_EVERY_GRANTED_WABA=on` every granted WABA is bound, each
under the ownership rule, and the answer gains `wabas`: one entry per
WABA (`primary`, `status` `connected`, `held`, `refused` or `failed`,
`phone_number_ids`, `failed_step`, `resumable`, `code`). A WABA the event
names that another merchant holds refuses the whole signup
(`waba_owned_by_another_tenant`, 409) with nothing bound, and the attempt
still completes without it; a grant the event did not name is `held` or
`refused`, never taken. Resume a `failed` one whose `resumable` is true
with its own `waba_id`:

```ts
export function wabasToResume(data: SignupResult): string[] {
  return (data.wabas ?? []).filter((waba) => waba.status === "failed" && waba.resumable).map((waba) => waba.waba_id);
}
```

## Hosted signup

With `WA_HOSTED_SIGNUP=on`, send the merchant to Meta's own onboarding
page (App Dashboard, WhatsApp, Quickstart): no code on your page, and
nothing comes back to your backend. Meta's account_update webhook
(PARTNER_ADDED) names the merchant's WABA and business; the service
remembers it for 7 days (a delivery that is unsigned or wrongly signed is
refused and remembered nowhere), and your operator (admin key), who knows
which merchant sent whom, claims it for that merchant's tenant with both
ids and the PIN you choose for their number:

```ts
export async function claimHostedSignup(
  admin: WhatsApp,
  tenantId: string,
  wabaId: string,
  ownerBusinessId: string,
  pin?: string,
): Promise<Connection> {
```

`not_found` (404): no such arrival is waiting (not yet delivered, claimed
already, lapsed, or another business), or no such tenant.
`waba_owned_by_another_tenant` (409): another merchant holds the WABA.
`onboarding_failed` is as for a completion: `resumable: false` puts the
arrival back to claim again; `resumable: true` leaves an attempt the
merchant's tenant resumes. The claim is the operator's, never a tenant's, because the
ids are no secrets: a route open to merchants would let one take
another's arrival. A tenant key gets `forbidden` (403). Off, the route
answers `signup_not_configured` (409).

Send an `Idempotency-Key` header with the claim (and with the pool's add and
code requests below): the same key and body after a `504` is one request to
Meta and gets the first answer back, marked as a replay; another
body is `idempotency_key_reused` (422). A claim Meta refused (nothing sent)
puts the arrival back and frees the key; one Meta may have received puts the
arrival back and keeps the `onboarding_failed` for that key, so claim again
under a new key. The PIN is not compared: another PIN under the same key
gets the first answer.

## Pre-verified numbers

Your operator (admin key) keeps a pool of numbers verified ahead of time
(`WA_PARTNER_BUSINESS_ID`, `WA_SYSTEM_TOKEN`). `POST /v1/admin/pre-verified-numbers`
adds one, `POST /v1/admin/pre-verified-numbers/{id}/code` has Meta send its
code, and `POST /v1/admin/pre-verified-numbers/{id}/verify` checks it. Reserve a number for a merchant and
their launch offers it, so they type no one-time code:

```ts
export async function reservePreVerified(admin: WhatsApp, id: string, tenantId: string): Promise<boolean> {
  const { error } = await admin.PUT("/v1/admin/pre-verified-numbers/{id}/reservation", {
    params: { path: { id } },
    body: { tenant_id: tenantId },
  });
```

```ts
export async function startWithPreVerified(api: WhatsApp, ids: string[]): Promise<SignupSession> {
  const { data, error } = await api.POST("/v1/signup/sessions", {
    body: { pre_verified_phone_ids: ids },
  });
```

An id not reserved for the merchant, another merchant's and a malformed
one are the same `invalid_request` on `pre_verified_phone_ids`. Tracking
who claimed which number is yours (Meta says so); sharing a number with
a partner is not a route yet. No step accepts terms.
