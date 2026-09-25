// WhatsApp events from meta-whatsapp-server, by polling GET /v1/events,
// typed from its OpenAPI document. Generate the types next to this file
// first (once per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type Event = components["schemas"]["Event"];
export type ErrorObject = components["schemas"]["ErrorObject"];
// The types a tenant receives today: switch on them, so TypeScript rejects
// a misspelled one; types only grow, so let any other fall to a default.
export type KnownEventType = components["schemas"]["KnownEventType"];
// Annotate the query with the generated type: TypeScript then rejects a
// misspelled parameter.
export type EventsQuery = NonNullable<paths["/v1/events"]["get"]["parameters"]["query"]>;

// One client per key (scope `events`). A platform key also names the
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
    super(`meta-whatsapp-server: ${error.code}`);
  }
}

// Where your backend keeps its cursor: a row of its own database, per
// tenant it polls for.
export interface Cursor {
  load(): Promise<number | undefined>;
  save(after: number): Promise<void>;
  clear(): Promise<void>;
}

// One poll. Handle each event, then save next_after. Handlers must be
// idempotent (skip an event id already handled): after a crash between
// the two, the next poll returns the same events, under the same ids. Returns whether more may follow at once (poll again without
// waiting).
export async function pollOnce(
  api: WhatsApp,
  cursor: Cursor,
  handle: (event: Event) => Promise<void>,
  resync: () => Promise<void>,
): Promise<boolean> {
  const after = await cursor.load();
  const query: EventsQuery = after === undefined ? { limit: 100 } : { limit: 100, after };
  const { data, error } = await api.GET("/v1/events", { params: { query } });
  if (error) {
    // Events after the cursor were purged (past retention, or deleted with
    // a tenant of the same id), or the cursor is past the tenant's newest
    // event (a restored database): rebuild what you derive from events,
    // then start again from the oldest kept.
    const expired = error.error.code === "cursor_expired";
    const unknown = error.error.code === "invalid_request" && error.error.field === "after";
    if (expired || unknown) {
      await resync();
      await cursor.clear();
      return true;
    }
    throw new WhatsAppError(error.error);
  }
  for (const event of data.data) {
    await handle(event);
  }
  await cursor.save(data.next_after);
  const last = data.data[data.data.length - 1];
  return last !== undefined && last.sequence === data.next_after;
}

// Poll until stopped: at once while pages follow, else every few seconds.
export async function pollForever(
  api: WhatsApp,
  cursor: Cursor,
  handle: (event: Event) => Promise<void>,
  resync: () => Promise<void>,
  signal: AbortSignal,
): Promise<void> {
  while (!signal.aborted) {
    const more = await pollOnce(api, cursor, handle, resync);
    if (!more) {
      await new Promise((resolve) => setTimeout(resolve, 5000));
    }
  }
}

// The fields of `data` this example reads: meta-whatsapp-rs's WebhookEvent
// JSON (Meta's fields, normalized). Read defensively: fields may be added.
type Received = {
  message?: { id?: string; type?: string; text?: { body?: string } };
  contact?: { user_id?: string | null; wa_id?: string | null } | null;
};

export async function handle(event: Event): Promise<void> {
  switch (event.type as KnownEventType) {
    case "message_received": {
      const data = event.data as Received;
      // Key the customer by BSUID: the phone number (wa_id) may be absent.
      const customer = data.contact?.user_id ?? data.contact?.wa_id ?? undefined;
      console.log(event.phone_number_id, customer, data.message?.type);
      return;
    }
    case "status_updated":
    case "template_status_updated":
      return;
    default:
      return; // a type you do not handle, or one added later
  }
}

// Only some types, and only one number's events.
export async function numberMessages(api: WhatsApp, pn: string, after: number) {
  const query: EventsQuery = {
    after,
    types: "message_received,status_updated",
    phone_number_id: pn,
  };
  const { data, error } = await api.GET("/v1/events", { params: { query } });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}
