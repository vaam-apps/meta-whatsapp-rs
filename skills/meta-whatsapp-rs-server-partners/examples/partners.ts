// Partner APIs through meta-whatsapp-server (scope `partners`), typed from
// its OpenAPI document. Generate the types next to this file first (once
// per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type BillingMigration = components["schemas"]["BillingMigration"];
export type SolutionMigrationIntent = components["schemas"]["SolutionMigrationIntent"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key: a tenant key with the scope `partners` for a WABA's
// own routes, an admin key for `/v1/admin/partners`. A platform key also
// names the tenant it acts for.
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

// Is the business behind this WABA verified? Meta's own word for it, as
// `status`, and whether it is `verified`.
export async function isVerified(api: WhatsApp, wabaId: string): Promise<boolean> {
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/business-verification", {
    params: { path: { waba_id: wabaId } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.verified;
}

// Start moving a WABA to another billing currency. Meta clones the WABA
// with its templates; poll `status` until READY_TO_COMPLETE, then resume.
// Taking the step is a legal act: with the deployment's legal acts off the
// answer is `feature_not_available` and nothing was asked of Meta.
export async function startBillingMigration(
  api: WhatsApp,
  wabaId: string,
  currency: string,
  reference: string,
): Promise<BillingMigration | "legal_acts_off"> {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/billing-migrations", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: { currency },
  });
  if (error) {
    if ((error.error.code as KnownErrorCode) === "feature_not_available") {
      return "legal_acts_off";
    }
    throw new WhatsAppError(error.error);
  }
  return data;
}

export async function billingMigrationStatus(
  api: WhatsApp,
  wabaId: string,
  migrationId: string,
): Promise<string> {
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/billing-migrations/{migration_id}", {
    params: { path: { waba_id: wabaId, migration_id: migrationId } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.status;
}

// Complete a migration that is READY_TO_COMPLETE: the clone becomes the
// billing account of the numbers, and the old WABA can no longer send.
export async function resumeBillingMigration(
  api: WhatsApp,
  wabaId: string,
  migrationId: string,
): Promise<BillingMigration> {
  const { data, error } = await api.POST(
    "/v1/wabas/{waba_id}/billing-migrations/{migration_id}/resume",
    { params: { path: { waba_id: wabaId, migration_id: migrationId } } },
  );
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Tag a WABA for migration to a Multi-Partner Solution. The WABA's client
// accepts in Meta Business Suite; read the intent until it is ACCEPTED.
export async function tagForMigration(
  api: WhatsApp,
  wabaId: string,
  solutionId: string,
  reference: string,
): Promise<string> {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/solution-migration-intents", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: { solution_id: solutionId },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.id;
}

export async function migrationIntent(
  api: WhatsApp,
  wabaId: string,
  intentId: string,
): Promise<SolutionMigrationIntent | null> {
  const { data, error } = await api.GET(
    "/v1/wabas/{waba_id}/solution-migration-intents/{intent_id}",
    { params: { path: { waba_id: wabaId, intent_id: intentId } } },
  );
  if (error) {
    // An intent that does not name this WABA is not_found, whoever's it is.
    if ((error.error.code as KnownErrorCode) === "not_found") {
      return null;
    }
    throw new WhatsAppError(error.error);
  }
  return data;
}

// The operator's side, with an admin key: a customer's documents for
// partner-led business verification (a legal act: the deployment must have
// turned the legal acts on), base64 in the body.
export async function submitVerification(
  admin: WhatsApp,
  customerBusinessId: string,
  documents: { file_name: string; content_base64: string }[],
): Promise<number | null> {
  const { data, error } = await admin.POST("/v1/admin/partners/business-verifications", {
    body: { client_business_id: customerBusinessId, documents },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.attempts_left;
}
