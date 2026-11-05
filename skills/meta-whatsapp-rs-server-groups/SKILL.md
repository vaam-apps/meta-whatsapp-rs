---
name: meta-whatsapp-rs-server-groups
description: "Managing a business number's WhatsApp groups through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - asking for a group (202 and a request id, the group's id arriving in a webhook event), listing and reading groups, changing the subject and description, deleting, invite links (read, reset), participants (list, remove by phone, wa_id or BSUID), join requests (list, approve, reject), group analytics, the scope groups, why a group the number does not hold is not_found before anything reaches Meta, and what the logs never hold. Load when a CMS or a Medusa store creates or moderates WhatsApp groups for a merchant's number through the service."
---

# meta-whatsapp-rs-server-groups

> **Verified against meta-whatsapp-rs 69c142402b7adb076c5c42c42f22b8c925a9fd3a (2026-11-05).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/groups.ts](examples/groups.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#groups).

## When to use

A merchant's number is bound to their tenant and you create, moderate or
measure its groups (a support group per order, a community of buyers).
Sending a message *to* a group is a send: `meta-whatsapp-rs-server-send`,
with a `group_id` recipient. Deploying the service and its keys:
`meta-whatsapp-rs-server` first. The key needs the scope `groups`, which no
other scope grants: a key minted before it does not carry it.

## The routes

All under `/v1/numbers/{pn}/groups`, with the number's WABA token:

| Route | Does |
| --- | --- |
| `POST /v1/numbers/{pn}/groups` | ask for a group (`CreateGroupRequest`: `subject`, `description`, `join_approval_mode`); `202` with `request_id`; takes an `Idempotency-Key` |
| `GET /v1/numbers/{pn}/groups` | the number's groups (`limit`, `after`, `before`): `next_after` for the next page |
| `GET`, `PATCH`, `DELETE /v1/numbers/{pn}/groups/{group_id}` | read (`fields`), change `subject` and `description` (`204`), delete (`204`) |
| `GET`, `POST /v1/numbers/{pn}/groups/{group_id}/invite-link` | the invite link; replace it |
| `GET /v1/numbers/{pn}/groups/{group_id}/participants` | who is in the group, and how many |
| `POST /v1/numbers/{pn}/groups/{group_id}/participants/remove` | remove 1 to 8 participants |
| `GET /v1/numbers/{pn}/groups/{group_id}/join-requests` | the open join requests |
| `POST /v1/numbers/{pn}/groups/{group_id}/join-requests/approve`, `POST /v1/numbers/{pn}/groups/{group_id}/join-requests/reject` | 1 to 100 `join_request_ids` |
| `GET /v1/numbers/{pn}/groups/{group_id}/analytics` | a day a point: `start` within 90 days, `end`, `metrics` |

## A group is created asynchronously

Meta answers a creation with a request id only. The group's id and its
invite link come later, in a group lifecycle update event: poll
`GET /v1/events` (`meta-whatsapp-rs-server-events`) for `group_updated`
with type group_create and your `request_id`. That event is also what
makes the group the number's in the service, so the routes on its id work
from then on. A creation that failed arrives the same way, with its errors.

```ts
export async function createGroup(api: WhatsApp, pn: string, group: CreateGroupRequest, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/groups", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: group,
  });
```

Keep the `Idempotency-Key` per group you mean to make: a repeat is
answered from the kept answer, never a second group. On `504` the group
may exist: look for its event before asking again.

## A group the number does not hold is `not_found`

The service keeps a record of the groups each number holds, and decides
from it **before the tenant's token is opened**: another tenant's group,
another number's, an id no group has and a group the service has not
seen yet are all `404 not_found`, and Meta is not asked. A group made
before the service, or one whose event was missed, is found by listing
the number's groups: each page listed is recorded.

```ts
export async function holdsGroup(api: WhatsApp, pn: string, groupId: string): Promise<boolean> {
  const { error } = await api.GET("/v1/numbers/{pn}/groups/{group_id}", {
    params: { path: { pn, group_id: groupId }, query: { fields: "subject" } },
  });
  if (error === undefined) {
    return true;
  }
```

A group Meta no longer has is `not_found` too, with no Meta code. A
number that moved to another tenant keeps its groups' records under the
old tenant: list its groups to make them the new one's.

## Invite links, participants, join requests

The invite link (`GroupInviteLink`'s `invite_link`) is a credential: whoever
holds it can join. It is in the answer of its two routes and in no log of
the service. Resetting it stops every link sent before; reading it does not.

Participants are named one way each: `phone` (E.164 with its `+`),
`wa_id` (digits, as the participants route lists it) or `user_id` (a
BSUID); two at once, or a parent BSUID, is `422` on `users[i]`, and the
answer never quotes the value. Removal answers `partial`: true when Meta
says some were not removed (the participants update event says
which). Join requests list `join_request_id`, `wa_id` or `user_id`,
`username`; an approval or a rejection answers `done`, `failed` (each with
Meta's `reasons`: a `code` and a `kind`) and `reasons`. A partial success
is a `200`.

```ts
    for (const failed of data.failed) {
      refused.push(failed.join_request_id ?? "unknown");
    }
```

## What never reaches a log or an error

A participant's phone number, BSUID or username, a join request's id and
the invite link are in the answer that returns them and nowhere else: a
failed call answers Meta's `code` and the service's `kind`, **never Meta's
text** (it may quote them), and the changes' audit events in the service's
logs hold counts and outcomes only (who and which group are never in them).
Do the same on your side: log the request, not the people.

## What meta-whatsapp-rs does not do

The group's picture, pinning a message (a send), adding participants
(Meta: they join through the invite link), deleting the invite link and
leaving a group (Meta documents no call; delete the group instead, which
removes every participant and the business). Meta's refusals of a group
call answer `502 unknown` with its code in `graph`: read the code, not
the text.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-server-events` (the lifecycle events),
`meta-whatsapp-rs-server-send` (sending to a group),
`meta-whatsapp-rs-groups-and-calling` (the same calls in the Rust library
the service runs on).
