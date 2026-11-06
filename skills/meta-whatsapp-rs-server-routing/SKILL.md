---
name: meta-whatsapp-rs-server-routing
description: "Conversation routing through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - reading the routing roles a number's business holds, and passing, taking and releasing a user's thread (thread control) between responders such as a bot, an agent desk and the escalation partner, the scope routing, naming the user by BSUID, phone or wa_id, a pass naming a role and never an app, idempotency keys, why a timeout is never retried, and what the logs never hold. Load when a CMS or a Medusa store hands a customer's conversation between its bot and its human agents, or to Meta Business Agent, through the service."
---

# meta-whatsapp-rs-server-routing

> **Verified against meta-whatsapp-rs 0000000000000000000000000000000000000000 (2026-11-06).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/routing.ts](examples/routing.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#conversation-routing).

## When to use

The business turned on Conversation Routing for a number, and your app is
one of its responders (the primary of an entry point, or the escalation
partner): you hand a customer's thread to a human or a bot, take it back,
or let it go idle. Receiving the handover events:
`meta-whatsapp-rs-server-events`. Deploying the service and its keys:
`meta-whatsapp-rs-server` first. The key needs the scope `routing`, which
no other scope grants: a key minted before it does not carry it.

## The routes

All under `/v1/numbers/{pn}`, with the number's WABA token:

| Route | Does |
| --- | --- |
| `GET …/routing-roles` | `{roles, is_standby}`: the roles your business holds (`calls`, `ctwa`, `customer_service`, `escalation`, `marketing`, `utility`), unordered; empty is a success |
| `POST …/thread-control/pass` | `{user, target_role?, metadata?}`; without a role, to the escalation partner |
| `POST …/thread-control/take` | `{user, metadata?}`; the escalation partner only |
| `POST …/thread-control/release` | `{user}`; the thread goes idle, and an idle one succeeds too |

The three actions answer `200 {request_id}` (Meta's, for its support) and
take an `Idempotency-Key`. Meta tells the caller nothing more: the new
owner of a pass gets `control_passed`, the previous owner of a take gets
`control_taken`, a release has no event, and **no call says who owns a
thread** (keep that state yourself, from those events and your own calls).

## The user, and a role never an app

`user` holds exactly one of `phone` (E.164 with its `+`), `wa_id` (digits)
and `user_id` (a BSUID). Prefer the BSUID, which survives a number change;
never key a person by phone number alone. Two at once, a parent BSUID
(`US.ENT.…`) or none is `422` on `user…`, nothing sent.

```ts
export function bsuid(userId: string): ThreadUser {
  return { user_id: userId };
}
```

A pass hands the thread to a **role**: `target_role` is `ai_agent`, `ctwa`,
`customer_service`, `escalation`, `marketing` or `utility`, spelled so
(anything else is `422 target_role`). There is no app id in this API: the
business that owns the number configures which responder holds each role,
so a pass cannot reach another tenant's app. Without a role it goes to
the escalation partner, which therefore cannot pass. `ai_agent` exists
only where Meta Business Agent is enabled; Meta judges that.
`metadata` (2,000 characters at most) is forwarded to the new owner.

```ts
export type TargetRole = "ai_agent" | "ctwa" | "customer_service" | "escalation" | "marketing" | "utility";
```

## Never replayed, so keyed

None of the three actions is idempotent at Meta: a repeated pass is a
second transfer, and a repeated release can release a thread a new message
just routed back to you. The service never retries one: after a timeout it
answers `504 timeout` with `may_have_been_sent`, and the thread may have
changed hands, so reconcile from your own state before acting again.
Keep one `Idempotency-Key` per handover you mean to make: a repeat of the
same request is answered from the kept answer and sends nothing; the same
key with another body, or another action, is `422 idempotency_key_reused`.

```ts
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { user, target_role: target ?? null, metadata: note ?? null },
```

## What Meta refuses

Meta's code comes back in `error.graph.code` (the answer holds **no
Meta text** for an action: it may quote the user). A take by a caller that
is not the escalation partner is `2494191` (`502 unknown`); a pass on an
idle thread, or one you do not own, a target not enabled and an account
not enrolled for thread control are refused with `100` or its kin. The
thread-control page lists the cases; the roles read tells which actions
are open to you.

```ts
  if (error.error.graph?.code === 2494191) {
    return false;
  }
```

A number without Conversation Routing answers the roles read with Meta's
`100`. Another tenant's number is `404`, before anything reaches Meta.

## What never reaches a log

The user (phone, `wa_id`, BSUID) and `metadata` are in no log line, error or
audit event of the service. Each action is an audit event (`thread_passed`,
`thread_taken`, `thread_released`) holding the key, tenant, WABA, number
and, for a pass, the role. Do the same on your side: log the handover, not
the person.

## What meta-whatsapp-rs does not do

It does not accept Meta's Business Agents and Platform Terms of Service,
which thread control needs and only the business can accept in Meta
Business Suite. It does not report a thread's owner (Meta offers no call),
and the service does not record your own pass, take or release in its
inbox's owner state: after a pass, a free-form reply through the inbox
routes may still be let through and Meta then refuses it (`409
thread_owned_elsewhere`). It does not take a thread implicitly: the
escalation partner's service message does, with no call.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-server-events` (`thread_control_changed` and standby
events), `meta-whatsapp-rs-server-send` (replying), `meta-whatsapp-rs-cms-inbox`
(the same calls and the ownership record in the Rust library).
