// Broadcasts through meta-whatsapp-server (scope `broadcasts`), typed from its
// OpenAPI document. Generate the types next to this file first (once per
// service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type Broadcast = components["schemas"]["Broadcast"];
export type BroadcastRecipient = components["schemas"]["BroadcastRecipient"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scope `broadcasts`). A platform key also names the
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

// A person by BSUID where you have one (Meta's preferred identifier, stable
// across number changes), with their phone number too when you hold it (Meta
// then uses the phone number). Never key a person by phone number alone.
export function person(userId: string, phone?: string): BroadcastRecipient {
  return { user_id: userId, phone: phone ?? null };
}

// A person you know by phone number only: E.164 with its `+`.
export function byPhone(phone: string): BroadcastRecipient {
  return { phone };
}

// Queue a template for many people from one of the tenant's numbers. Nothing
// is sent by the call: the service's workers send at the number's rate. Keep
// one `Idempotency-Key` per campaign: a repeat of the same call is answered
// from the kept answer and queues nothing; the same key with another body is
// `422 idempotency_key_reused`. A refusal (`422` on `recipients[i]` or the
// template's fields, `429` at the tenant's limit of broadcasts at once) queued
// nothing.
export async function start(
  api: WhatsApp,
  pn: string,
  reference: string,
  template: { name: string; language: { code: string }; components?: unknown[] },
  recipients: BroadcastRecipient[],
): Promise<Broadcast> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/broadcasts", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { template, recipients },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Where it stands: `queued`, `running`, `completed`, `cancelled` or `stopped`,
// and how many people are in each outcome. `failures` counts the refusals by
// their error code (`marketing_opted_out`, `template_paused`, ...), never by
// Meta's text. `unknown` people are never sent again: reconcile them with the
// message status events, whose `biz_opaque_callback_data` is
// `wa-broadcast:<id>`.
export async function progress(api: WhatsApp, pn: string, id: string): Promise<Broadcast> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/broadcasts/{broadcast_id}", {
    params: { path: { pn, broadcast_id: id } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Whether it is over: nothing more will be sent.
export function finished(broadcast: Broadcast): boolean {
  return broadcast.state === "completed" || broadcast.state === "cancelled" || broadcast.state === "stopped";
}

// Stop it before its next send (a send already in flight goes out). Answers
// the broadcast as it stands; repeating it is harmless. It needs no working
// token, so it works after the number's token expired.
export async function cancel(api: WhatsApp, pn: string, id: string): Promise<Broadcast> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/broadcasts/{broadcast_id}/cancel", {
    params: { path: { pn, broadcast_id: id } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Another tenant's broadcast, another number's and one never made are the same
// `404 not_found`.
export function missing(error: unknown): boolean {
  return error instanceof WhatsAppError && error.error.code === "not_found";
}
