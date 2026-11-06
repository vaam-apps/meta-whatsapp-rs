---
name: meta-whatsapp-rs-server-calling
description: "WhatsApp Calling through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - the number's calling settings (call hours, icons, SIP, voicemail, restrictions), reading a user's call permission by phone number or BSUID, placing a call with your own SDP offer (201 and a call id, an Idempotency-Key), answering a user's call (pre-accept, accept, reject, terminate), per-call recording and transcription as the caller's choice, call analytics, the scope calls, why a call the number does not hold is not_found before anything reaches Meta, and what the logs never hold (SDP, SIP password). Load when a CMS or a Medusa store lets a merchant's agents call or take calls on their WhatsApp number through the service."
---

# meta-whatsapp-rs-server-calling

> **Verified against meta-whatsapp-rs 3cd24b38992fc5de77cffefaaf0edcf880b3e45f (2026-11-06).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/calls.ts](examples/calls.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#calling).

## When to use

A merchant's number is bound to their tenant and their agents place or
take WhatsApp voice calls. The service does **signalling only**: your own
WebRTC stack makes the SDP offer and consumes the answer, and the service
passes both to Meta unparsed. Deploying the service and its keys:
`meta-whatsapp-rs-server` first. The key needs the scope `calls`, which no
other scope grants (`send` does not); call analytics are the scope
`analytics`.

## The routes

| Route | Does |
| --- | --- |
| `GET`, `PATCH /v1/numbers/{pn}/call-settings` | the `CallingSettings` object (status, hours, icons, callback permission, SIP, audio, voicemail); the read adds `restrictions`; a change sets the fields it names, `204` |
| `GET /v1/numbers/{pn}/call-permissions` | `phone` (E.164 with its +) and/or `user_id` (a BSUID): `status`, `expiration_time`, `actions` |
| `POST /v1/numbers/{pn}/calls` | place a call (`ConnectCallRequest`): `201` with `call_id`; takes an `Idempotency-Key` |
| `POST /v1/numbers/{pn}/calls/{call_id}/pre-accept`, `POST /v1/numbers/{pn}/calls/{call_id}/accept` | answer a user's call with `sdp_answer` |
| `POST /v1/numbers/{pn}/calls/{call_id}/reject`, `POST /v1/numbers/{pn}/calls/{call_id}/terminate` | reject a user's call; end a call in either direction |
| `GET /v1/wabas/{waba_id}/analytics/calls` | `CallAnalytics`: `start`, `end`, `granularity`, `directions`, `dimensions`, `metric_types` |

## Placing a call

The user must have granted the number a call permission (the guide's
permission request is a message: `meta-whatsapp-rs-server-send`); without
it Meta refuses with its code 138006, answered as `409` with the code
`call_permission_missing` and Meta's code in `graph`. Read the permission
first.

```ts
export async function mayCall(api: WhatsApp, pn: string, user: CallParticipant): Promise<boolean> {
  const query = {
    ...(user.phone != null ? { phone: user.phone } : {}),
    ...(user.user_id != null ? { user_id: user.user_id } : {}),
  };
```

Keep the `Idempotency-Key` per call you mean to place: a repeat is
answered from the kept answer, never a second ring. A call is **never
repeated after a timeout**: on `504` the phone may be ringing, so wait for
the call's events. The call is the number's from the `201` on.

```ts
  const { data, error } = await api.POST("/v1/numbers/{pn}/calls", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { to, sdp_offer: sdpOffer, ...(recording !== undefined ? { recording } : {}) },
  });
```

## Answering a user's call

A user's call reaches you as a `call_updated` event in `/v1/events`
(`meta-whatsapp-rs-server-events`), a connect event with the user's SDP
offer. Answer within 30 to 60 seconds: pre-accept so that media can
connect, then accept, and start sending media only after it returns. Then
terminate when it ends, also after the media did (Meta prices the call
from it).

```ts
  const pre = await api.POST("/v1/numbers/{pn}/calls/{call_id}/pre-accept", {
    params,
    body: { sdp_answer: sdpAnswer },
  });
```

## A call the number does not hold is `not_found`

Meta documents no read of a call by its id, so the service keeps a record
of the calls each number holds, written by the number's call events and by
the call you place, and decides from it **before the tenant's token is
opened**: another tenant's call, another number's, an id no call has and a
call with no event for 24 hours are all `404 not_found`, and Meta is not
asked.

```ts
  if ((error.error.code as KnownErrorCode) === "not_found") {
    return false;
  }
```

## Recording and transcription are your choice, per call

`recording` and `transcription` (`CallAnnouncement`: `status`, `purpose`,
`announcement_language`) are sent only when you set them, on a call you
place and when you accept; nothing is recorded by default and the service
decides nothing about consent. When enabled, Meta plays the purpose to both
participants first, and a participant who does not consent ends the call;
the result arrives as an event with a media id. The service's audit event
says whether either was enabled.

## Settings

`call_hours` is replaced as a whole (a holiday schedule left out clears it),
and users' apps may take up to 7 days to show a change. A voicemail
announcement's `announcement_media_id` must be media uploaded **on this
number**: any other is `not_found`, and nothing changes.

```ts
  const settings: CallingSettings = {
    call_hours: {
      status: "ENABLED",
      timezone_id: timezoneId,
```

## What never reaches a log or an error

An SDP (addresses, ICE credentials) is in the request that carries it and
nowhere else. A SIP server's password is **never read from Meta and never
answered**: take it from WhatsApp Manager. A failed call answers Meta's
`code` and the service's `kind`, never Meta's text.

## What meta-whatsapp-rs does not do

The WebRTC media, the SIP password, uploading a voicemail announcement
(Meta wants a use case the media routes do not send), sending the call
permission request, call buttons and deep links. Meta's calling codes
(138xxx) answer their own kind (`calling_not_enabled`,
`call_permission_missing`, `call_limit_reached`, `call_connection_failed`,
...; `unknown` only for a code Meta's page does not list): branch on the
`error.code`, not the text.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-server-events` (the call events),
`meta-whatsapp-rs-server-send` (the permission request message),
`meta-whatsapp-rs-groups-and-calling` (the same calls in the Rust library
the service runs on).
