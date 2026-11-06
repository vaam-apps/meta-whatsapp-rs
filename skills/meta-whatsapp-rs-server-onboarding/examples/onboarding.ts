// A merchant connects their WhatsApp number from your CMS through
// meta-whatsapp-server (Embedded Signup), typed from its OpenAPI document.
// Generate the types next to this file first (once per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type SignupSession = components["schemas"]["SignupSession"];
export type CompleteSignup = components["schemas"]["CompleteSignup"];
export type SignupResult = components["schemas"]["SignupResult"];
export type SignupAttempt = components["schemas"]["SignupAttempt"];
export type ErrorObject = components["schemas"]["ErrorObject"];

// One client per key: a platform key with scope `signup` naming the
// merchant (your CMS's tenant id), or that merchant's own tenant key.
export function whatsapp(baseUrl: string, key: string, tenant?: string) {
  const headers: Record<string, string> = { Authorization: `Bearer ${key}` };
  if (tenant !== undefined) {
    headers["WA-Tenant"] = tenant;
  }
  return createClient<paths>({ baseUrl, headers });
}

export type WhatsApp = ReturnType<typeof whatsapp>;

// Your "Connect WhatsApp" button's backend: a new attempt bound to the
// merchant, and what the page hands FB.init and FB.login.
export async function startSignup(api: WhatsApp, coexistence: boolean): Promise<SignupSession> {
  const { data, error } = await api.POST("/v1/signup/sessions", {
    body: { coexistence },
  });
  if (error) {
    throw new Error(`signup not started: ${error.error.code}`);
  }
  return data;
}

// What the page posts back: the state, FB.login's code, the
// WA_EMBEDDED_SIGNUP message event as it arrived, and the PIN the merchant
// typed (never store it, never log it).
export type PagePost = {
  state: string;
  code: string;
  event: CompleteSignup["event"];
  pin?: string;
};

// What a completion (or a resume) means for your CMS.
export type Connection =
  | { kind: "connected"; wabaId: string; numbers: string[]; allocation: string | null }
  | { kind: "cancelled"; screen: string | null } // the attempt may still complete
  | { kind: "stale" } // expired, used, or another merchant's: start again
  | { kind: "taken" } // the WABA is another merchant's (D4): the operator decides
  | { kind: "invalid"; field: string } // fix the post: the attempt survives
  | { kind: "resumable"; step: string } // the token is stored: resume later
  | { kind: "failed"; step: string | null; code: string }; // start again

// Relay the page's post; the tenant is your credential's, never the body's.
export async function completeSignup(api: WhatsApp, post: PagePost): Promise<Connection> {
  const body: CompleteSignup = { state: post.state, code: post.code, event: post.event };
  if (post.pin !== undefined) {
    body.pin = post.pin;
  }
  const { data, error } = await api.POST("/v1/signup/complete", { body });
  if (error) {
    return failed(error.error);
  }
  return connection(data);
}

export function connection(data: SignupResult): Connection {
  if (data.status === "cancelled") {
    return { kind: "cancelled", screen: data.current_step ?? null };
  }
  return {
    kind: "connected",
    wabaId: data.waba_id ?? "",
    numbers: data.phone_number_ids ?? [],
    allocation: data.credit_allocation_id ?? null,
  };
}

export function failed(error: ErrorObject): Connection {
  switch (error.code) {
    case "stale_attempt":
      return { kind: "stale" };
    case "waba_owned_by_another_tenant":
      return { kind: "taken" };
    case "invalid_request":
      return { kind: "invalid", field: error.field ?? "body" };
    case "onboarding_failed":
      if (error.resumable === true) {
        return { kind: "resumable", step: error.step ?? "" };
      }
      return { kind: "failed", step: error.step ?? null, code: error.code };
    default:
      return { kind: "failed", step: error.step ?? null, code: error.code };
  }
}

// The merchant's attempts that stopped after their token was stored.
export async function attempts(api: WhatsApp): Promise<SignupAttempt[]> {
  const { data, error } = await api.GET("/v1/signup/attempts", {
    params: { query: { limit: 100 } },
  });
  if (error) {
    throw new Error(`attempts not listed: ${error.error.code}`);
  }
  return data.data;
}

// Resume one, with a corrected PIN when the registration refused the
// first (Meta's 133005): from the stored token, no code, any replica.
export async function resumeSignup(api: WhatsApp, wabaId: string, pin?: string): Promise<Connection> {
  const { data, error } = await api.POST("/v1/signup/resume", {
    body: pin === undefined ? { waba_id: wabaId } : { waba_id: wabaId, pin },
  });
  if (error) {
    if (error.error.code === "nothing_to_resume") {
      return { kind: "stale" };
    }
    return failed(error.error);
  }
  return connection(data);
}

// The merchant disconnects in your CMS (scope `numbers`): unsubscribed,
// a Solution Partner's credit line revoked, then the token and bindings
// deleted. A revocation that fails deletes nothing: call it again.
export async function disconnect(api: WhatsApp, wabaId: string): Promise<boolean> {
  const { error } = await api.DELETE("/v1/wabas/{waba_id}", {
    params: { path: { waba_id: wabaId } },
  });
  return error === undefined;
}

// Your operator funds again a business whose credit line was revoked
// (admin key): no resume and no new signup ever does.
export async function fundAgain(admin: WhatsApp, wabaId: string): Promise<Connection> {
  const { data, error } = await admin.POST("/v1/admin/wabas/{waba_id}/credit-line/reshare", {
    params: { path: { waba_id: wabaId } },
  });
  if (error) {
    return failed(error.error);
  }
  return connection(data);
}

// Hosted Embedded Signup (WA_HOSTED_SIGNUP=on): the merchant used Meta's own
// page, which returned nothing; Meta's account_update PARTNER_ADDED named
// their WABA and business to the service. Your operator (admin key) claims
// it for the tenant that merchant is, with the PIN chosen for their number
// (never stored by the service): the ids are no secrets, so no tenant key
// claims. A 404 `not_found` means no such arrival is waiting (or no such
// tenant); 409 `waba_owned_by_another_tenant` that another merchant holds
// the WABA.
export async function claimHostedSignup(
  admin: WhatsApp,
  tenantId: string,
  wabaId: string,
  ownerBusinessId: string,
  pin?: string,
): Promise<Connection> {
  const { data, error } = await admin.POST("/v1/admin/signup/hosted", {
    body:
      pin === undefined
        ? { tenant_id: tenantId, waba_id: wabaId, owner_business_id: ownerBusinessId }
        : { tenant_id: tenantId, waba_id: wabaId, owner_business_id: ownerBusinessId, pin },
  });
  if (error) {
    return failed(error.error);
  }
  return connection(data);
}

export type SignupWaba = components["schemas"]["SignupWaba"];

// A completion on a deployment that binds every granted WABA
// (WA_SIGNUP_EVERY_GRANTED_WABA=on) lists each WABA and what became of it;
// one that failed after its token was stored is resumed with its own id.
export function wabasToResume(data: SignupResult): string[] {
  return (data.wabas ?? []).filter((waba) => waba.status === "failed" && waba.resumable).map((waba) => waba.waba_id);
}

// With the option off, the primary WABA (`waba_id`) is the only one bound;
// the others the page's event named are listed here, for a signup of their own.
export function notBound(data: SignupResult): string[] {
  return data.not_bound_waba_ids ?? [];
}

// The operator's pool of pre-verified numbers (admin key): add a number,
// have Meta send its code, check the code, then reserve it for a merchant,
// whose launch may then offer it (`pre_verified_phone_ids`).
export async function addPreVerified(admin: WhatsApp, phoneNumber: string): Promise<string> {
  const { data, error } = await admin.POST("/v1/admin/pre-verified-numbers", {
    body: { phone_number: phoneNumber },
  });
  if (error) {
    throw new Error(`number not added: ${error.error.code}`);
  }
  return data.id;
}

export async function reservePreVerified(admin: WhatsApp, id: string, tenantId: string): Promise<boolean> {
  const { error } = await admin.PUT("/v1/admin/pre-verified-numbers/{id}/reservation", {
    params: { path: { id } },
    body: { tenant_id: tenantId },
  });
  return error === undefined;
}

export async function startWithPreVerified(api: WhatsApp, ids: string[]): Promise<SignupSession> {
  const { data, error } = await api.POST("/v1/signup/sessions", {
    body: { pre_verified_phone_ids: ids },
  });
  if (error) {
    throw new Error(`signup not started: ${error.error.code}`);
  }
  return data;
}
