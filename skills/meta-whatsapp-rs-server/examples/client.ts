// A thin client for meta-whatsapp-server, typed from its OpenAPI document.
// Generate the types next to this file first (once per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type ErrorObject = components["schemas"]["ErrorObject"];
export type ErrorCode = components["schemas"]["ErrorCode"];
export type NumberView = components["schemas"]["NumberView"];
export type ProfilePatch = components["schemas"]["ProfilePatch"];
// Annotate request literals with the generated types: TypeScript then
// rejects a misspelled field, which it does not through the generic call.
export type NumbersQuery = NonNullable<paths["/v1/numbers"]["get"]["parameters"]["query"]>;

// One client per key. A platform key also names the tenant it acts for;
// a tenant key never needs WA-Tenant.
export function whatsapp(baseUrl: string, key: string, tenant?: string) {
  const headers: Record<string, string> = { Authorization: `Bearer ${key}` };
  if (tenant !== undefined) {
    headers["WA-Tenant"] = tenant;
  }
  return createClient<paths>({ baseUrl, headers });
}

export type WhatsApp = ReturnType<typeof whatsapp>;

// A failed call, with the service's error object.
export class WhatsAppError extends Error {
  constructor(readonly error: ErrorObject) {
    super(`meta-whatsapp-server: ${error.code}`);
  }
}

export type Next = "fix_request" | "reconnect" | "retry_later" | "reconcile" | "give_up";

// What to do after a failure: branch on the code and on
// may_have_been_sent, never on the message or the status alone.
export function nextStep(error: ErrorObject): Next {
  if (error.may_have_been_sent) {
    return "reconcile"; // it may have taken effect: check before repeating
  }
  switch (error.code) {
    case "invalid_request":
    case "invalid_parameter":
      return "fix_request"; // error.field names the culprit
    case "number_not_connected":
    case "reconnect_required":
      return "reconnect"; // attach or onboard the WABA again
    case "unauthenticated":
    case "forbidden":
    case "tenant_suspended":
    case "not_found":
      return "give_up";
    default:
      return error.retryable ? "retry_later" : "give_up";
  }
}

// Every number of the tenant, following the cursors.
export async function allNumbers(api: WhatsApp): Promise<NumberView[]> {
  const numbers: NumberView[] = [];
  let cursor: string | undefined;
  do {
    const query: NumbersQuery = cursor === undefined ? { limit: 100 } : { limit: 100, cursor };
    const { data, error } = await api.GET("/v1/numbers", { params: { query } });
    if (error) {
      throw new WhatsAppError(error.error);
    }
    numbers.push(...data.data);
    cursor = data.next_cursor ?? undefined;
  } while (cursor !== undefined);
  return numbers;
}

// A number's live details from Meta: display number, verified name,
// quality rating, name status, throughput.
export async function numberDetails(api: WhatsApp, pn: string) {
  const { data, error } = await api.GET("/v1/numbers/{pn}", { params: { path: { pn } } });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Change the business profile; absent fields stay as they are.
export async function setAbout(api: WhatsApp, pn: string, about: string) {
  const body: ProfilePatch = { about };
  const { data, error } = await api.PATCH("/v1/numbers/{pn}/profile", {
    params: { path: { pn } },
    body,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}
