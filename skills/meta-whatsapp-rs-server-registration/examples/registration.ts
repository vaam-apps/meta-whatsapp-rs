// Registering a WhatsApp number through meta-whatsapp-server, typed from
// its OpenAPI document. Generate the types next to this file first (once
// per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type CodeRequestBody = components["schemas"]["CodeRequestBody"];
export type RegistrationBody = components["schemas"]["RegistrationBody"];
export type RegistrationAnswer = components["schemas"]["RegistrationAnswer"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scope `numbers`). A platform key also names the
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

// What a registration attempt's outcome means for the merchant's screen.
export type Registered =
  | { kind: "registered" }
  | { kind: "wrong_pin" } // ask the merchant for the PIN again
  | { kind: "verify_first" } // request and submit a code first, or wait out Meta's lock
  | { kind: "wait"; seconds: number } // the number's attempts are spent for now
  | { kind: "check_status" }; // may have registered: read the number before retrying

// Step 1: Meta sends the number a code by SMS (or VOICE). The key makes a
// retry after a timeout safe: it never asks Meta twice.
export async function requestCode(api: WhatsApp, pn: string, attempt: string): Promise<void> {
  const body: CodeRequestBody = { code_method: "SMS", language: "en_US" };
  const { error } = await api.POST("/v1/numbers/{pn}/request-code", {
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:code:${attempt}` } },
    body,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// Step 2: the code the merchant read ("123-830" works as typed).
export async function verifyCode(api: WhatsApp, pn: string, code: string): Promise<boolean> {
  const { error } = await api.POST("/v1/numbers/{pn}/verify-code", {
    params: { path: { pn } },
    body: { code },
  });
  if (!error) {
    return true;
  }
  // A wrong code is Meta's refusal of the parameter; anything else throws.
  if (error.error.code === "invalid_parameter") {
    return false;
  }
  throw new WhatsAppError(error.error);
}

// Step 3: register with the PIN the merchant just typed. Never store it,
// log it or put it in a URL: pass it through, then drop it.
export async function register(
  api: WhatsApp,
  pn: string,
  pin: string,
  attempt: string,
): Promise<Registered> {
  const body: RegistrationBody = { pin };
  const { error, response } = await api.POST("/v1/numbers/{pn}/register", {
    // The key covers the number (and the region), never the PIN: a wrong
    // PIN frees it, so the corrected PIN may reuse the same attempt id.
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:register:${attempt}` } },
    body,
  });
  if (!error) {
    return { kind: "registered" };
  }
  if (error.error.may_have_been_sent) {
    return { kind: "check_status" };
  }
  switch (error.error.code as KnownErrorCode) {
    case "two_step_verification":
      return { kind: "wrong_pin" };
    case "registration":
      return { kind: "verify_first" };
    case "too_many_requests": {
      const seconds = Number(response.headers.get("Retry-After"));
      return { kind: "wait", seconds: seconds > 0 ? seconds : 60 };
    }
    default:
      throw new WhatsAppError(error.error);
  }
}

// Deregistering stops the number until it is registered again: the body
// names it once more, and the service audits the call.
export async function deregister(api: WhatsApp, pn: string): Promise<RegistrationAnswer> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/deregister", {
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:deregister` } },
    body: { confirm: pn },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}
