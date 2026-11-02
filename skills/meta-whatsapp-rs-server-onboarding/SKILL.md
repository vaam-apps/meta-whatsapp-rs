---
name: meta-whatsapp-rs-server-onboarding
description: "Connecting a merchant's WhatsApp number (Embedded Signup) through meta-whatsapp-server over HTTP from a CMS not written in Rust - POST /v1/signup/sessions for the page's FB.login options, relaying the page's post to POST /v1/signup/complete (state, code, event, the merchant's own PIN), stale_attempt, waba_owned_by_another_tenant, onboarding_failed with step and resumable, GET /v1/signup/attempts and POST /v1/signup/resume, coexistence numbers, Tech Provider and Solution Partner modes (the credit line, its revocation, the operator's re-funding), and disconnecting. Load when a non-Rust backend lets its merchants connect WhatsApp through the service."
---

# meta-whatsapp-rs-server-onboarding

> **Verified against meta-whatsapp-rs d9bff1364441ad5a2493351eec08873623ac470c (2026-11-02).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/onboarding.ts](examples/onboarding.ts) (type-checked against the service's OpenAPI document). Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md).

## When to use

Your CMS is not Rust, reaches WhatsApp through meta-whatsapp-server
(`meta-whatsapp-rs-server`: deploying it, keys, errors), and lets each
merchant connect their own number (scope `signup`). The service keeps
the merchant's token in its vault: you never see it.

| Route | Does |
| --- | --- |
| `POST /v1/signup/sessions` | an attempt bound to the merchant (15 minutes), and what `FB.init` and `FB.login` need |
| `POST /v1/signup/complete` | the page's post: the merchant onboarded, the WABA and all its numbers theirs |
| `GET /v1/signup/attempts` | the merchant's attempts that stopped after the token was stored |
| `POST /v1/signup/resume` | one of them again, with a corrected PIN |
| `DELETE /v1/wabas/{waba_id}` | disconnect (scope `numbers`) |
| `POST /v1/admin/wabas/{waba_id}/credit-line/reshare` | the operator funds a revoked business again (admin key) |

## The flow

Your page loads Facebook's SDK. Before the click, your backend starts an
attempt for the merchant who is logged in (a platform key naming them
with `WA-Tenant`, or their tenant key):

```ts
export async function startSignup(api: WhatsApp, coexistence: boolean): Promise<SignupSession> {
  const { data, error } = await api.POST("/v1/signup/sessions", {
    body: { coexistence },
  });
```

`SignupSession`: `state` (keep it with the page, post it back in a
body, never a URL), `app_id` and `graph_api_version` for `FB.init`,
`launch_options` for `FB.login` as they are. `coexistence: true`
onboards a number the merchant keeps in the WhatsApp Business app.

`FB.login`'s callback gives a code (30 seconds, single use) and the page
receives a WA_EMBEDDED_SIGNUP message event. Post both, the state and
the merchant's two-step verification PIN (6 digits, typed by them) to
your backend, which relays them at once:

```ts
export async function completeSignup(api: WhatsApp, post: PagePost): Promise<Connection> {
  const body: CompleteSignup = { state: post.state, code: post.code, event: post.event };
  if (post.pin !== undefined) {
    body.pin = post.pin;
  }
  const { data, error } = await api.POST("/v1/signup/complete", { body });
```

- `200` `SignupResult`: `status` `connected`, with `waba_id`,
  `phone_number_ids`, `steps_completed`, `needs_coexistence_sync` and,
  for a Solution Partner, `credit_allocation_id`; or `cancelled` with
  `current_step` (the merchant left the flow: the same state still
  completes).
- `stale_attempt` (403): expired, used, or another merchant's (the
  tenant is your key's, never the body's): start again.
- `invalid_request` (422) on `state`, `code`, `pin`, `event` or `body`:
  fix the post; the attempt survives. Never echoed back.
- `waba_owned_by_another_tenant` (409): another merchant holds the WABA
  (decision D4); nothing was stored, subscribed or shared. Your operator
  decides (the admin unbind).
- `onboarding_failed` (502) with `step` and `resumable`, and Meta's code
  in `graph`. `resumable: true`: the token is stored, the attempt kept.

```ts
    case "onboarding_failed":
      if (error.resumable === true) {
        return { kind: "resumable", step: error.step ?? "" };
      }
```

## The PIN, and resuming

The PIN becomes a new number's two-step verification PIN, or must match
the one it has. It is the merchant's: ask for it on the page each time,
never store or log it, and post nothing when they leave it empty (the
number is then left unregistered). A coexistence number is registered
already. Meta refusing the PIN (`graph.code` 133005) stops the
registration step, resumable:

```ts
export async function resumeSignup(api: WhatsApp, wabaId: string, pin?: string): Promise<Connection> {
  const { data, error } = await api.POST("/v1/signup/resume", {
    body: pin === undefined ? { waba_id: wabaId } : { waba_id: wabaId, pin },
  });
```

Any replica resumes, after a restart too, from the stored token: no
code. `nothing_to_resume` (404): no attempt of this merchant's for that
WABA (another merchant's included), or it finished. List them with
`GET /v1/signup/attempts` (`SignupAttempt`: `waba_id`, `failed_step`,
`updated_at`).

## Solution Partner mode

With `WA_ONBOARDING_MODE=solution_partner` your credit line pays for
every merchant: the service adds your system user to their WABA and
shares the line with your system token (`WA_PARTNER_SYSTEM_TOKEN`), never
theirs, in `WA_WABA_CURRENCY`. Nothing changes in your calls; the answer
gains `credit_allocation_id`. A line cannot be taken back once attached,
which is why the WABA is the merchant's before anything is shared.

A merchant whose line was revoked (they unshared the WABA from you) is
not funded again by a resume or a new signup: both stop at the credit
step. Funding them again is your operator's decision:

```ts
  const { data, error } = await admin.POST("/v1/admin/wabas/{waba_id}/credit-line/reshare", {
    params: { path: { waba_id: wabaId } },
  });
```

## Disconnecting

```ts
  const { error } = await api.DELETE("/v1/wabas/{waba_id}", {
    params: { path: { waba_id: wabaId } },
  });
```

The app is unsubscribed, a Solution Partner's line revoked, then the
token and bindings deleted; a failure deletes nothing: call it again. Meta's
account_update webhooks do the same on their own: the merchant
uninstalling your app (PARTNER_APP_UNINSTALLED) or deleting the WABA
offboards them; unsharing it (PARTNER_REMOVED) revokes the line. The
service confirms each with Meta first, and your tenant still receives the
`account_updated` event (`meta-whatsapp-rs-server-events`).

## What meta-whatsapp-rs does not do

- It never shows you the merchant's token, and never stores the PIN.
- No hosted Embedded Signup, several WABAs in one signup or pre-verified
  numbers over HTTP yet; no per-merchant currency (one
  `WA_WABA_CURRENCY`); no route to clear a credit share whose answer was
  lost (the library's `EmbeddedSignup::clear_pending_share`).
- No browser access: your backend relays every call (keys stay on your
  servers).

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, the error body);
`meta-whatsapp-rs-server-events` (the webhook events);
`meta-whatsapp-rs-server-send` (sending once connected); from Rust:
`meta-whatsapp-rs-embedded-signup`.
