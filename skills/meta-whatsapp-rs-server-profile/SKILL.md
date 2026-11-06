---
name: meta-whatsapp-rs-server-profile
description: "Setting a business number's profile picture and changing its display name through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - the business profile (GET and PATCH /v1/numbers/{pn}/profile), the picture set from a completed resumable upload by its upload id (never Meta's handle; another tenant's or number's upload is not_found before anything reaches Meta), the display name request and its review status (POST and GET /v1/numbers/{pn}/display-name), why a new name shows only after Meta approves it and the number is registered again, Meta's 10 changes per 30 days, the audit event. Load when a CMS or a Medusa store edits a merchant's WhatsApp profile or renames their number through the service."
---

# meta-whatsapp-rs-server-profile

> **Verified against meta-whatsapp-rs 26bee03976b8a499f0fb1752d4158c7f30b47376 (2026-11-05).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/profile.ts](examples/profile.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#profile-picture-and-display-name).

## When to use

A merchant's number is bound to their tenant and you edit what customers
see of it: the business profile (about, address, description, email,
websites, vertical, picture) or the display name. Deploying the service
and its keys: `meta-whatsapp-rs-server` first. The key needs the scope
`numbers`, and `media` for the picture's upload.

## The routes

| Route | Does |
| --- | --- |
| `GET /v1/numbers/{pn}/profile` | the profile, with `profile_picture_url` |
| `PATCH /v1/numbers/{pn}/profile` | change the fields given (`ProfilePatch`); `profile_picture_upload_id` sets the picture |
| `POST /v1/numbers/{pn}/display-name` | ask Meta to review a new display name; `202`; takes an `Idempotency-Key` |
| `GET /v1/numbers/{pn}/display-name` | the display name and the review of the requested one |

## The picture is named by its upload

Open a resumable upload of the picture on the same number (a JPEG or a
PNG: `meta-whatsapp-rs-server-send` and the guide show the upload), send
its bytes, and when the answer says `complete`, pass the **upload's id**.
The service keeps Meta's handle itself and sends it as the profile's
picture handle: a handle is not a field of the change, because it could
not be checked against the tenant and number that obtained it.

```ts
export async function setPicture(api: WhatsApp, pn: string, uploadId: string) {
  const body: ProfilePatch = { profile_picture_upload_id: uploadId };
  const { data, error } = await api.PATCH("/v1/numbers/{pn}/profile", {
    params: { path: { pn } },
    body,
  });
```

- An upload of another tenant, of another number, an expired one (the
  service keeps an upload 24 hours) and an id nobody made are **all
  `404 not_found`**, the same answer, and Meta is not asked.
- An id that is no upload id, an upload that is not an image, or one not
  complete yet is `422 invalid_request` on `profile_picture_upload_id`:
  finish it (the upload's status says where it stands) or upload again.
- Meta's pages state no picture format or size: the types are the
  service's own bound, and the size is the upload's (5 MiB for an image).
- The answer is the whole profile; the new `profile_picture_url` is
  Meta's.

## The display name changes after a review

```ts
export async function requestDisplayName(api: WhatsApp, pn: string, name: string, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/display-name", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { new_display_name: name },
  });
```

The name is trimmed, 1 to 256 characters, with no control character and
no bidi override or zero-width character (`422` on `new_display_name`,
before Meta is asked). The answer is `202`:
Meta reviews the name, and the number's name does **not** change yet.

```ts
export async function displayName(api: WhatsApp, pn: string): Promise<DisplayName> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/display-name", {
    params: { path: { pn } },
  });
```

`new_name_status` says where the review stands (`PENDING_REVIEW`,
`APPROVED`, `DECLINED`, …). `verified_name` is the name WhatsApp holds and
does not say it is approved: `name_status` does. When the review is
`APPROVED` (the event `phone_number_name_updated`, in
`meta-whatsapp-rs-server-events`), **register the number again** within
the 14 days Meta allows, or the name is reviewed again; registering before
the approval has no effect. The service does not do it for you: it never
holds the two-step PIN, which the merchant types for the attempt
(`meta-whatsapp-rs-server-registration`).

```ts
  const { error } = await api.POST("/v1/numbers/{pn}/register", {
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:rename:${attempt}` } },
    body: { pin },
  });
```

- Meta allows **10 changes per 30 days**; the service keeps no count and
  passes Meta's refusal on. A number with Official Business Account
  status changes its name through Meta's support.
- Keep the `Idempotency-Key` per change you mean to make: a repeat is
  answered from the kept answer, never a second review. On `504` the
  request may have reached Meta: read the status before asking again (the
  request is not replayed, by the service or under its key).
- Each request is an **audit event** in the service's logs
  (action display_name_change_requested: tenant, WABA, number, your key's
  public id, the name's length and the outcome), because the change
  starts a review and later shows to every customer. The name is not in
  it.

## What never reaches a log or an error

Meta's texts are never copied into an answer: read `error.code` and
`error.graph.code`, not a message. The picture's handle, Meta's upload
session, the picture's URL and the upload ids are in no log line.

## What meta-whatsapp-rs does not do

The profile-node API (the profile by its own id), registering the number
for you after an approval, counting Meta's 30-day window, changing the
name of an Official Business Account, and uploading a picture in the same
call (the upload is its own route, then this one).

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-server-registration` (registering again with the PIN),
`meta-whatsapp-rs-server-events` (`phone_number_name_updated`),
`meta-whatsapp-rs-phone-numbers` (the same calls in the Rust library the
service runs on).
