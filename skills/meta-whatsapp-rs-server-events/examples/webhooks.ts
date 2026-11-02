// WhatsApp events pushed by meta-whatsapp-server to a URL of yours
// (webhooks out), signed the Standard Webhooks way, typed from its OpenAPI
// document. Generate the types next to this file first (once per service
// upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`). Web Crypto only: Node 20+, Deno, Bun and
// workers have it.

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type Event = components["schemas"]["Event"];
export type CreateWebhookEndpoint = components["schemas"]["CreateWebhookEndpoint"];
export type WebhookEndpointCreated = components["schemas"]["WebhookEndpointCreated"];
// A delivery's body is the envelope `GET /v1/events/{id}` answers, but its
// `data` is `null` when the event was too large to send (`truncated`).
export type Delivery = Omit<Event, "data"> & { data: Event["data"] | null };

// One client per key (scope `webhooks` to manage endpoints, `events` to
// read an event by id). A platform key also names the tenant it acts for.
export function whatsapp(baseUrl: string, key: string, tenant?: string) {
  const headers: Record<string, string> = { Authorization: `Bearer ${key}` };
  if (tenant !== undefined) {
    headers["WA-Tenant"] = tenant;
  }
  return createClient<paths>({ baseUrl, headers });
}

export type WhatsApp = ReturnType<typeof whatsapp>;

// Make an endpoint once, and keep its secret: it is shown this once.
export async function createEndpoint(api: WhatsApp, url: string): Promise<WebhookEndpointCreated> {
  const body: CreateWebhookEndpoint = { url, types: ["message_received", "status_updated"] };
  const { data, error } = await api.POST("/v1/webhook-endpoints", { body });
  if (error !== undefined) {
    // 422 `invalid_request` on `url`: not https, or not in the operator's
    // WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS; 409 `webhooks_not_configured`.
    throw new Error(`meta-whatsapp-server: ${error.error.code} ${error.error.field ?? ""}`);
  }
  return data;
}

// Receivers refuse a timestamp further than this from their clock.
const TOLERANCE_SECONDS = 5 * 60;

function base64(bytes: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(bytes), (c) => c.charCodeAt(0));
}

// Whether `body` (the raw bytes, before any JSON parsing) is a delivery
// signed with `secret` (`whsec_…`): the timestamp within 5 minutes, and one
// of the header's `v1,` signatures HMAC-SHA256(key, "{id}.{timestamp}.{body}").
// During a rotation the header carries two: either one verifies.
export async function verifyDelivery(secret: string, headers: Headers, body: Uint8Array<ArrayBuffer>): Promise<boolean> {
  const id = headers.get("webhook-id");
  const timestamp = headers.get("webhook-timestamp");
  const signatures = headers.get("webhook-signature");
  if (id === null || timestamp === null || signatures === null) {
    return false;
  }
  // An integer of seconds: `Number("x")` is NaN, which no comparison refuses.
  const seconds = Number(timestamp);
  if (!Number.isSafeInteger(seconds) || Math.abs(Date.now() / 1000 - seconds) > TOLERANCE_SECONDS) {
    return false;
  }
  const key = await crypto.subtle.importKey(
    "raw", base64(secret.replace(/^whsec_/, "")), { name: "HMAC", hash: "SHA-256" }, false, ["verify"],
  );
  const prefix = new TextEncoder().encode(`${id}.${timestamp}.`);
  const signed = new Uint8Array(prefix.length + body.length);
  signed.set(prefix);
  signed.set(body, prefix.length);
  for (const entry of signatures.split(" ")) {
    if (!entry.startsWith("v1,")) {
      continue; // another scheme
    }
    let candidate: Uint8Array<ArrayBuffer>;
    try {
      candidate = base64(entry.slice(3));
    } catch {
      continue; // not base64: skipped, as the libraries do
    }
    // crypto.subtle.verify compares in constant time.
    if (await crypto.subtle.verify("HMAC", key, candidate, signed)) {
      return true;
    }
  }
  return false;
}

// What `handle` already did, by event id: deliveries come at least once.
export interface Seen {
  has(id: string): Promise<boolean>;
  add(id: string): Promise<void>;
}

// Your route's handler (a fetch-style Request): answer 2xx within 10 s, or
// the delivery is tried again (for 72 hours, then `failed`).
export async function receive(
  request: Request, secret: string, api: WhatsApp, seen: Seen, handle: (event: Event) => Promise<void>,
): Promise<Response> {
  const body = new Uint8Array(await request.arrayBuffer());
  if (!(await verifyDelivery(secret, request.headers, body))) {
    return new Response(null, { status: 401 });
  }
  const delivery = JSON.parse(new TextDecoder().decode(body)) as Delivery | { type: "ping" };
  if (delivery.type === "ping") {
    return new Response(null, { status: 204 }); // POST …/{id}/test
  }
  const event = delivery as Delivery;
  if (await seen.has(event.id)) {
    return new Response(null, { status: 204 });
  }
  let whole: Event;
  if (event.truncated || event.data === null) {
    // Over 256 KiB: read it whole, by id.
    const { data, error } = await api.GET("/v1/events/{id}", { params: { path: { id: event.id } } });
    if (error !== undefined) {
      return new Response(null, { status: 503 }); // try again later
    }
    whole = data;
  } else {
    whole = { ...event, data: event.data };
  }
  await handle(whole);
  await seen.add(event.id);
  return new Response(null, { status: 204 });
}
