// WhatsApp login codes through meta-whatsapp-server, typed from its
// OpenAPI document. Generate the types next to this file first (once per
// service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type IssueOtp = components["schemas"]["IssueOtp"];
export type OtpIssued = components["schemas"]["OtpIssued"];
export type OtpVerified = components["schemas"]["OtpVerified"];
export type OtpSettingsRequest = components["schemas"]["OtpSettingsRequest"];
export type AuthenticationTemplateRequest =
  components["schemas"]["AuthenticationTemplateRequest"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key: a tenant key with scope `otp`, or a platform key
// naming the tenant (the store, the merchant) the codes are for.
export function whatsapp(baseUrl: string, key: string, tenant?: string) {
  const headers: Record<string, string> = { Authorization: `Bearer ${key}` };
  if (tenant !== undefined) {
    headers["WA-Tenant"] = tenant;
  }
  return createClient<paths>({ baseUrl, headers });
}

export type WhatsApp = ReturnType<typeof whatsapp>;

// What asking for a code means for your login form.
export type CodeRequest =
  | { kind: "sent"; challengeId: string; expiresAt: string }
  | { kind: "wait"; seconds: number } // cooling_down, rate_limited, 429
  | { kind: "maybe_sent" } // the code may be on its way: let the user type it
  | { kind: "not_configured" } // the operator sets the tenant's OTP sender
  | { kind: "bad_phone" }
  | { kind: "failed"; code: string };

// Send a code. `attempt` names this login attempt (`login:<user>:<n>`):
// the same key never sends twice, whatever the network does.
export async function requestCode(
  api: WhatsApp,
  phone: string,
  attempt: string,
): Promise<CodeRequest> {
  const { data, error, response } = await api.POST("/v1/otp/issue", {
    params: { header: { "Idempotency-Key": attempt } },
    body: { phone, purpose: "login" },
  });
  if (error) {
    return failedRequest(error.error, response.headers.get("Retry-After"));
  }
  return issued(data);
}

// An issue's outcome is a 200 with a status, never an error.
export function issued(data: OtpIssued): CodeRequest {
  switch (data.status) {
    case "sent":
      return {
        kind: "sent",
        challengeId: data.challenge_id ?? "",
        expiresAt: data.expires_at ?? "",
      };
    case "cooling_down":
    case "rate_limited":
      return { kind: "wait", seconds: data.retry_after_seconds ?? 30 };
  }
}

export function failedRequest(error: ErrorObject, retryAfter: string | null): CodeRequest {
  if (error.may_have_been_sent) {
    return { kind: "maybe_sent" };
  }
  switch (error.code as KnownErrorCode) {
    case "too_many_requests":
      return { kind: "wait", seconds: Number(retryAfter ?? "1") };
    case "otp_not_configured":
      return { kind: "not_configured" };
    case "invalid_request":
    case "recipient_not_supported":
      return { kind: "bad_phone" };
    default:
      return { kind: "failed", code: error.code };
  }
}

// What the code the user typed means.
export type CodeCheck =
  | { kind: "welcome" }
  | { kind: "wrong"; attemptsLeft: number }
  | { kind: "ask_for_a_new_code" };

export async function checkCode(api: WhatsApp, phone: string, typed: string): Promise<CodeCheck> {
  const { data, error } = await api.POST("/v1/otp/verify", {
    body: { phone, purpose: "login", code: typed.trim() },
  });
  if (error) {
    throw new Error(`meta-whatsapp-server: ${error.error.code}`);
  }
  return checked(data);
}

export function checked(data: OtpVerified): CodeCheck {
  switch (data.result) {
    case "verified":
      return { kind: "welcome" };
    case "invalid":
      return { kind: "wrong", attemptsLeft: data.attempts_left ?? 0 };
    case "expired":
    case "too_many_attempts":
    case "not_found":
      return { kind: "ask_for_a_new_code" };
  }
}

// The operator's side (an admin key): the number a tenant's codes are sent
// from, one of its own, and its approved authentication template.
export async function setSender(
  admin: WhatsApp,
  tenant: string,
  phoneNumberId: string,
): Promise<void> {
  const settings: OtpSettingsRequest = {
    phone_number_id: phoneNumberId,
    template: { name: "login_code", language: "en_US" },
    ttl_seconds: 600,
  };
  const { error } = await admin.PUT("/v1/admin/tenants/{id}/otp", {
    params: { path: { id: tenant } },
    body: settings,
  });
  if (error) {
    throw new Error(`meta-whatsapp-server: ${error.error.code}`);
  }
}

// The template itself, copy code, in three languages (scope `templates`).
export async function createLoginTemplate(api: WhatsApp, wabaId: string): Promise<string[]> {
  const template: AuthenticationTemplateRequest = {
    name: "login_code",
    languages: ["en_US", "es_ES", "fr"],
    otp_type: "copy_code",
    add_security_recommendation: true,
    code_expiration_minutes: 10,
  };
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/templates/authentication", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": "template:login_code:v1" } },
    body: template,
  });
  if (error) {
    throw new Error(`meta-whatsapp-server: ${error.error.code}`);
  }
  return data.data.map((created) => created.id);
}
