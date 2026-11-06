---
name: meta-whatsapp-rs-server-partners
description: "Partner APIs through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - a WABA's business verification status, tagging a WABA for migration to a Multi-Partner Solution and reading the intent back, moving a WABA to another billing currency or payment method (start, poll, resume), the operator's partner-led business verification submissions, solutions (create, accept, reject, deactivate) and assigned WABAs, the scope partners, the legal acts the deployment must turn on (WA_SERVER_LEGAL_ACTS, feature_not_available when off) and why the solution token is never served. Load when a CMS or an onboarding back office changes a merchant's billing currency or partner, or an operator drives Multi-Partner Solutions."
---

# meta-whatsapp-rs-server-partners

> **Verified against meta-whatsapp-rs 81bf9e80b5160bd28344c5040ba34e640d7354a3 (2026-11-06).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/partners.ts](examples/partners.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#partner-apis).

## When to use

A merchant's WABA is bound to their tenant and you move it (billing
currency, payment method, Multi-Partner Solution), or you run the
partner side of the deployment (the Tech Provider or Solution Partner).
Deploying the service and its keys: `meta-whatsapp-rs-server` first.
Two kinds of key:

- A tenant key with the scope `partners`, which no other scope grants,
  acts on **its own WABAs**.
- An **admin key** drives the operator's routes (the admin partners routes below),
  with the deployment's own partner business and system token
  (`WA_PARTNER_BUSINESS_ID`; without them `signup_not_configured`).

## Legal acts are off until the deployment turns them on

Seven calls state something about a business, cannot be taken back or move
a business's assets: a billing migration's start and its resumption, a
solution migration intent, a business verification submission, and
accepting, rejecting or (accepting the deactivation of) a solution. With
`WA_SERVER_LEGAL_ACTS` off (the default) each is `403 feature_not_available`
and **nothing was asked of Meta**; on, each is audited. The code never
takes one by itself.

## The routes

| Route | Does |
| --- | --- |
| `GET /v1/wabas/{waba_id}/business-verification` | the WABA's `status` as Meta says it, and `verified` |
| `POST /v1/wabas/{waba_id}/solution-migration-intents` | tag the WABA for a solution (`solution_id`): `201` with the intent's id; an `Idempotency-Key`; a legal act |
| `GET /v1/wabas/{waba_id}/solution-migration-intents/{intent_id}` | the intent, if it names this WABA, else `404` |
| `POST /v1/wabas/{waba_id}/billing-migrations` | start a billing migration (`currency`, optional `extended_credit_id`): `201`; an `Idempotency-Key`; a legal act |
| `GET /v1/wabas/{waba_id}/billing-migrations/{migration_id}` | where it stands; only a migration this WABA started |
| `POST /v1/wabas/{waba_id}/billing-migrations/{migration_id}/resume` | complete it; a legal act |

The operator's, all with an admin key:

| Route | Does |
| --- | --- |
| `POST /v1/admin/partners/business-verifications` | submit documents for a customer business; a legal act |
| `GET /v1/admin/partners/business-verifications` | the submissions made |
| `POST /v1/admin/partners/solutions` | create a solution |
| `GET /v1/admin/partners/solutions/{id}` | read one with its owner, permissions and partners |
| `POST /v1/admin/partners/solutions/{id}/accept` | accept it; a legal act that cannot be undone |
| `POST /v1/admin/partners/solutions/{id}/reject` | decline it; a legal act, permanent |
| `POST /v1/admin/partners/solutions/{id}/deactivation-request/accept` | accept a deactivation request; a legal act |
| `GET /v1/admin/partners/users/{user_id}/wabas` | the WABAs assigned to a system user |

The list of solutions, a solution's partners, the client businesses and
the other two deactivation calls are in the operators' guide.

## Moving a WABA to another billing currency

Meta clones the WABA with its templates and the Flows they use; the numbers
stay where they are. Start, poll `status` until it says it is ready to complete (add a
payment method on the new WABA first: a credit card cannot be attached by
the API), then resume: the clone becomes the billing account and the old
WABA can no longer send.

```ts
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/billing-migrations", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: { currency },
  });
  if (error) {
    if ((error.error.code as KnownErrorCode) === "feature_not_available") {
      return "legal_acts_off";
    }
```

Keep the `Idempotency-Key` per migration you mean to start: a repeat is
answered from the kept answer, never a second clone. Neither call is
repeated after a timeout (`504`: read the migration, or the WABA's
numbers, before trying again). A migration id the WABA did not start
through the service is `404 not_found`, and Meta is not asked.

```ts
  const { data, error } = await api.POST(
    "/v1/wabas/{waba_id}/billing-migrations/{migration_id}/resume",
    { params: { path: { waba_id: wabaId, migration_id: migrationId } } },
  );
```

## Moving a WABA to a Multi-Partner Solution

The destination solution must be active. The intent asks the WABA's client
to accept in Meta Business Suite (Meta accepts for them after 90 days);
the destination solution's partner then reads the intent and goes on only
when it is `ACCEPTED`. The read answers only an intent that names your WABA.

```ts
    // An intent that does not name this WABA is not_found, whoever's it is.
    if ((error.error.code as KnownErrorCode) === "not_found") {
      return null;
    }
```

## The operator's side

A submission carries one to three documents (PDF, JPEG or PNG, 5 MiB each,
base64 in the body), checked before any request; Meta counts three per
customer, so it is **never repeated after a timeout** (list the submissions
first). Meta's decision arrives as an account update event.

```ts
  const { data, error } = await admin.POST("/v1/admin/partners/business-verifications", {
    body: { client_business_id: customerBusinessId, documents },
  });
```

Accepting a solution cannot be undone, so read it first
(`GET …/solutions/{id}` answers its owner, permissions and partners).

## What never reaches an answer, a log or an audit event

The **solution token** (a client's business token through a solution) is
never served; hosted Embedded Signup stores it in the vault
(`meta-whatsapp-rs-server-onboarding`). Documents, their names, the
partner's system token and Meta's texts are in no log, error or audit
event; the audit events name ids and counts.

## What meta-whatsapp-rs does not do

Moving a phone number between partners (Meta: not supported on the new
account model), a WABA's own solutions, and the solution token.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-server-onboarding` (Embedded Signup, pre-verified numbers).
