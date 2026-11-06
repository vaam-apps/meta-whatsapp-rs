// Conversation routing through meta-whatsapp-server (scope `routing`), typed
// from its OpenAPI document. Generate the types next to this file first
// (once per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type ThreadUser = components["schemas"]["ThreadUser"];
export type RoutingRoles = components["schemas"]["RoutingRoles"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// Meta's six roles for a pass (`target_role`). A role, never an app: the
// business that owns the number decides who holds each.
export type TargetRole = "ai_agent" | "ctwa" | "customer_service" | "escalation" | "marketing" | "utility";

// One client per key (scope `routing`). A platform key also names the
// tenant it acts for.
export function whatsapp(baseUrl: string, key: string, tenant?: string) {
  const headers: Record<string, string> = { Authorization: `Bearer ${key}` };
  if (tenant !== undefined) {
    headers["WA-Tenant"] = tenant;
  }
  return createClient<paths>({ baseUrl, headers });
}

export type WhatsApp = ReturnType<typeof whatsapp>;

export class WhatsAppError extends Error {
  constructor(readonly error: ErrorObject) {
    super(`${error.code}: ${error.message}`);
  }
}

// The user whose thread it is, by BSUID where you have one (Meta's preferred
// identifier, stable across number changes). Never key a person by phone
// number alone.
export function bsuid(userId: string): ThreadUser {
  return { user_id: userId };
}

// The roles the number's business holds, and its standby grant. Read it at
// start-up and now and then, not before each action. Empty `roles` is a
// success. A number without Conversation Routing is Meta's code 100.
export async function roles(api: WhatsApp, pn: string): Promise<RoutingRoles> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/routing-roles", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Pass the thread to a role (none: the escalation partner, which therefore
// cannot pass). Keep one `Idempotency-Key` per handover you mean to make:
// a repeat is answered from the kept answer, never a second pass. On `504`
// the thread may have changed hands: reconcile before asking again.
export async function pass(
  api: WhatsApp,
  pn: string,
  user: ThreadUser,
  reference: string,
  target?: TargetRole,
  note?: string,
): Promise<string | null> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/thread-control/pass", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { user, target_role: target ?? null, metadata: note ?? null },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.request_id;
}

// Take the thread: the escalation partner only. Any other caller gets Meta's
// code 2494191, in `error.graph.code`, with `502 unknown`.
export async function take(api: WhatsApp, pn: string, user: ThreadUser, reference: string): Promise<boolean> {
  const { error } = await api.POST("/v1/numbers/{pn}/thread-control/take", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { user },
  });
  if (error === undefined) {
    return true;
  }
  if (error.error.graph?.code === 2494191) {
    return false;
  }
  throw new WhatsAppError(error.error);
}

// Release the thread to idle (an idle thread's release succeeds). The next
// message from the user is routed by entry point. No event follows.
export async function release(api: WhatsApp, pn: string, user: ThreadUser, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/thread-control/release", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { user },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.request_id;
}
