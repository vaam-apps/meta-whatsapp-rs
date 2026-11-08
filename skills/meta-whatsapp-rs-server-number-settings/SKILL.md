---
name: meta-whatsapp-rs-server-number-settings
description: "Managing a business number's settings through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - the messaging limit tier, the settings (local storage, identity key check, search visibility, notices; one setting per PATCH), payload encryption with the business's public key (never in a log, error or answer), the business username, the Official Business Account status and application, India's compliance information, the number's and the WABA's health status, the number's webhook override (off unless the deployment enables it, its URL checked as a webhook endpoint's is, clearing always allowed), conversational components and the contact book erasure; why the compliance declaration is the one legal act, 403 feature_not_available until WA_SERVER_LEGAL_ACTS=on. Load when a CMS or a Medusa store configures a merchant's WhatsApp number through the service."
---

# meta-whatsapp-rs-server-number-settings

> **Verified against meta-whatsapp-rs 6802f4d141d61d8f77e4afbe7b72f15c5f34f155 (2026-11-07).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/number-settings.ts](examples/number-settings.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#number-settings).

## When to use

A merchant's number is bound to their tenant and you configure it or read
how it stands. Deploy the service first (`meta-whatsapp-rs-server`). The
key needs the scope `numbers` (setting a webhook override, the scope `webhook_override`); the contact book's erasure needs `erasure`
too. Another tenant's number is `404 not_found` on every route.

## The routes

| Route | Does |
| --- | --- |
| `GET /v1/numbers/{pn}` | the number's details, with `messaging_limit_tier` |
| `GET`, `PATCH /v1/numbers/{pn}/settings` | local storage and payload encryption's status; **one** setting per change |
| `PUT`, `DELETE /v1/numbers/{pn}/settings/payload-encryption` | payload encryption on (your public key) or off |
| `GET`, `PUT`, `DELETE /v1/numbers/{pn}/username`, `GET …/username/suggestions` | the business username, and the ones WhatsApp reserved |
| `GET`, `POST /v1/numbers/{pn}/official-business-account` | the status; the application (audited) |
| `GET`, `PUT /v1/numbers/{pn}/compliance-info` | India's compliance information; the declaration (the legal act) |
| `GET /v1/numbers/{pn}/health-status` | whether messages can be sent, node by node |
| `GET`, `PUT /v1/numbers/{pn}/conversational-components` | welcome message flag, ice breakers, commands |
| `GET /v1/wabas/{waba_id}/health-status` | the same for your WABA (another's is `404`, Meta not asked); only nodes you own or hold ([references/webhook-override.md](references/webhook-override.md)) |
| `GET`, `PUT`, `DELETE /v1/numbers/{pn}/webhook-override` | the number's own webhook URL; divert its webhooks there (the scope `webhook_override`, which `numbers` does not grant, and `403 feature_not_available` unless `WA_SERVER_NUMBER_WEBHOOK_OVERRIDE=on`; the service then stops seeing them); restore delivery (always allowed): [the rules](references/webhook-override.md) |
| `DELETE /v1/numbers/{pn}/contact-book/{bsuid}` | erase a contact book entry at Meta (scope `erasure`, audited) |

## The limit tier, health and settings

```ts
export async function messagingLimitTier(api: WhatsApp, pn: string): Promise<string | null> {
  const { data, error } = await api.GET("/v1/numbers/{pn}", {
    params: { path: { pn } },
  });
```

The tier is Meta's name (TIER_250, TIER_1K, TIER_10K, TIER_100K,
TIER_UNLIMITED, …), or null when Meta answers none. The health status
gives `can_send_message` overall and one entry per node (the number, its
WABA, its business portfolio, the app) with the reasons it is LIMITED or
BLOCKED. A settings change names exactly one field, as Meta sets one value
per call, so a change either went through or did not:

```ts
export async function changeSetting(api: WhatsApp, pn: string, body: SettingsPatch) {
  const { data, error } = await api.PATCH("/v1/numbers/{pn}/settings", {
    params: { path: { pn } },
    body,
  });
```

- `local_storage` is `{enabled, data_localization_region}` (two uppercase
  letters, required with `enabled: true`), and Meta lets it change only
  while the number is **not** registered; `search_visibility` is
  `VISIBLE` or `HIDDEN`.
- Two fields, none, an unknown field or a bad value is `invalid_request`
  (`body` or the field), before Meta is asked.
- Each change is an **audit event** (number_setting_changed, with the
  setting's name, never a value).

## Payload encryption keys are secrets

```ts
export async function enablePayloadEncryption(api: WhatsApp, pn: string, publicKeyBase64: string) {
  const { error } = await api.PUT("/v1/numbers/{pn}/settings/payload-encryption", {
    params: { path: { pn } },
    body: { client_encryption_key: publicKeyBase64 },
  });
```

Send your **public** key, base64. A private key or a value that is not
base64 is `422 invalid_request` on `client_encryption_key`, never quoted.
The key is in the body only: in no log, no error, no answer and no debug
output of the service. Meta's own cloud key is **not** answered either:
`GET …/settings` gives the `payload_encryption` status and your key's
fingerprint (to check which key Meta holds). Meta's error texts on these
calls are withheld, and a call cut by a timeout (`504`) is not replayed:
read the settings before asking again.

## Username, components

```ts
export async function setUsername(api: WhatsApp, pn: string, username: string, forceTransfer = false) {
  const { data, error } = await api.PUT("/v1/numbers/{pn}/username", {
    params: { path: { pn } },
    body: { username, force_transfer: forceTransfer },
  });
```

A username is 3 to 35 characters of English letters, digits, dots and
underscores, with a letter, no dot first, last or doubled, and not
starting with "www" (`422` on `username` otherwise). If another number of
the portfolio holds it, Meta refuses (147005) unless `force_transfer`.
Conversational components: at most 4 ice breakers of 80 characters and 30
commands (names of 32 characters without a leading slash, unique; hints of
256); `422` names the entry (`prompts[0]`, `commands[1].command_name`).

## One legal act stays off until the deployment turns it on

The **compliance information declaration** (India) is a legal act. Until
the deployment sets `WA_SERVER_LEGAL_ACTS=on` it is
`403 feature_not_available` and nothing is asked of Meta; a read is never
gated. The **application** (a request for Meta's review) and the
**contact book erasure** (irreversible at Meta) are not gated.

```ts
export async function declareComplianceInfo(api: WhatsApp, pn: string, info: ComplianceInfoBody) {
  const { error } = await api.PUT("/v1/numbers/{pn}/compliance-info", {
    params: { path: { pn } },
    body: info,
  });
```

- Each is **audited** (official_business_account_requested,
  compliance_info_set, contact_book_entry_deleted: tenant, WABA, number,
  your key's public id, the outcome; never what a declaration says or
  whose entry it was); the declaration only when the setting is on.
- The application takes an `Idempotency-Key`, answers `202`, and on `504`
  is not replayed: read the status. Meta wants 5 to 10 supporting links.
- The declaration follows the page's rules (entity type OTHER needs
  `entity_type_custom`; `is_registered` only with OTHER or PARTNERSHIP;
  emails under 128 characters) and the service's own: no control or
  invisible character, texts at most 128 characters.
- The erasure takes a **BSUID** only (a phone number or a parent BSUID is
  `422` on `bsuid`, never quoted) and the scope `erasure`; no setting.

```ts
export async function eraseContactBookEntry(api: WhatsApp, pn: string, bsuid: string) {
  const { data, error } = await api.DELETE("/v1/numbers/{pn}/contact-book/{bsuid}", {
    params: { path: { pn, bsuid } },
  });
```

## What never reaches a log

Meta's texts are never copied into an answer: read `error.code` and
`error.graph.code`. Keys, usernames, declarations, application texts,
contact ids and components' texts are in no log line.

## What meta-whatsapp-rs does not do

Not offered: a number's webhook URL setter and connection status (the service
owns its events), the WABA-level callback override, bot details, calling
settings, the coexistence data sync. No call of these
routes was run against Meta live.

## Related skills

`meta-whatsapp-rs-server`, `meta-whatsapp-rs-server-profile`,
`meta-whatsapp-rs-server-registration`, `meta-whatsapp-rs-phone-numbers`.
