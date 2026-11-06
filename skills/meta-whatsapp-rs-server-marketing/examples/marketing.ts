// The Marketing Messages API, max price and welcome message sequences
// through meta-whatsapp-server (scope `marketing`), typed from its OpenAPI
// document. Generate the types next to this file first (once per service
// upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type SendMarketingMessage = components["schemas"]["SendMarketingMessage"];
export type OptimizationSpec = components["schemas"]["OptimizationSpec"];
export type CreateWelcomeSequence = components["schemas"]["CreateWelcomeSequence"];
export type UpdateWelcomeSequence = components["schemas"]["UpdateWelcomeSequence"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scope `marketing`). A platform key also names the
// tenant it acts for. The operator's agreement needs an admin key instead.
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

// A marketing template through the Marketing Messages API. The key makes a
// retry safe: the same key and body never send twice.
export async function sendMarketing(
  api: WhatsApp,
  pn: string,
  body: SendMarketingMessage,
  idempotencyKey: string,
): Promise<string> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/marketing-messages", {
    params: { path: { pn }, header: { "Idempotency-Key": idempotencyKey } },
    body,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.message_id;
}

// Where the WABA stands: its eligibility, and its owner business's terms.
export async function onboarding(api: WhatsApp, wabaId: string) {
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/marketing/onboarding", {
    params: { path: { waba_id: wabaId } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Block (true) or allow (false) marketing templates on Cloud API.
export async function blockMarketingOnCloudApi(api: WhatsApp, wabaId: string, disabled: boolean) {
  const { error } = await api.PUT("/v1/wabas/{waba_id}/marketing/cloud-api-marketing", {
    params: { path: { waba_id: wabaId } },
    body: { disabled },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// An approved marketing template of the WABA, duplicated at another max
// price (per 1,000 deliveries, in the currency's smallest unit).
export async function duplicateAtPrice(
  api: WhatsApp,
  wabaId: string,
  sourceTemplateId: string,
  name: string,
  bidAmount: number,
  idempotencyKey: string,
) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/marketing/template-duplicates", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": idempotencyKey } },
    body: {
      source_template_id: sourceTemplateId,
      name,
      optimization_spec: { bid_amount: bidAmount },
    },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.id;
}

// Set a template's max price; `null` returns it to rate-card pricing.
export async function setMaxPrice(
  api: WhatsApp,
  wabaId: string,
  templateId: string,
  price: OptimizationSpec | null,
) {
  const path = { waba_id: wabaId, template_id: templateId };
  const url = "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price";
  const { error } =
    price === null
      ? await api.DELETE(url, { params: { path } })
      : await api.PUT(url, { params: { path }, body: price });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// Estimates at several max prices, from a history of L1D, L7D, L14D or L28D.
export async function reachEstimate(api: WhatsApp, wabaId: string, countries: string[]) {
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/marketing/reach-estimates", {
    params: {
      path: { waba_id: wabaId },
      query: { countries: countries.join(","), date_interval: "L7D" },
    },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.estimates;
}

// A Click-to-WhatsApp welcome message sequence; the id goes in the ad.
export async function createSequence(
  api: WhatsApp,
  wabaId: string,
  body: CreateWelcomeSequence,
  idempotencyKey: string,
) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/welcome-sequences", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": idempotencyKey } },
    body,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.sequence_id;
}

// Meta refuses to change or delete a sequence an active ad uses.
export async function deleteSequence(api: WhatsApp, wabaId: string, sequenceId: string) {
  const { error } = await api.DELETE("/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}", {
    params: { path: { waba_id: wabaId, sequence_id: sequenceId } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// The operator's explicit act, with an admin key: sign Meta's max price beta
// agreement for a business portfolio. `feature_not_available` means the
// deployment has not enabled the legal acts; nothing was asked of Meta.
export async function signMaxPriceAgreement(
  admin: WhatsApp,
  businessId: string,
  signerName: string,
  signerEmail: string,
  businessToken: string,
): Promise<string | null> {
  const { data, error } = await admin.POST("/v1/admin/businesses/{business_id}/max-price-agreement", {
    params: { path: { business_id: businessId } },
    body: { signer_name: signerName, signer_email: signerEmail, token: businessToken },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.status;
}
