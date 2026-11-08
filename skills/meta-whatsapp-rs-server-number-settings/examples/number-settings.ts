// A number's settings through meta-whatsapp-server (scope `numbers`, and
// `erasure` for the contact book), typed from its OpenAPI document.
// Generate the types next to this file first (once per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type SettingsPatch = components["schemas"]["SettingsPatch"];
export type NumberSettings = components["schemas"]["NumberSettings"];
export type NumberHealthStatus = components["schemas"]["NumberHealthStatus"];
export type SetWebhookOverride = components["schemas"]["SetWebhookOverride"];
export type ObaApplicationBody = components["schemas"]["ObaApplicationBody"];
export type ComplianceInfoBody = components["schemas"]["ComplianceInfoBody"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scope `numbers`; the contact book also needs
// `erasure`). A platform key also names the tenant it acts for.
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

// The messaging limit tier is part of the number's details.
export async function messagingLimitTier(api: WhatsApp, pn: string): Promise<string | null> {
  const { data, error } = await api.GET("/v1/numbers/{pn}", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.messaging_limit_tier;
}

// One setting per call: `body` names exactly one of the patch's fields.
// Local storage can be changed only while the number is not registered.
export async function changeSetting(api: WhatsApp, pn: string, body: SettingsPatch) {
  const { data, error } = await api.PATCH("/v1/numbers/{pn}/settings", {
    params: { path: { pn } },
    body,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.setting;
}

// Hide the number from WhatsApp's business search.
export function hideFromSearch(api: WhatsApp, pn: string) {
  return changeSetting(api, pn, { search_visibility: "HIDDEN" });
}

// The settings, without key material: payload encryption's status and the
// fingerprint of the key you set, never Meta's key.
export async function settings(api: WhatsApp, pn: string): Promise<NumberSettings> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/settings", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Turn payload encryption on with your PUBLIC key (base64). The key is in
// the body only: the service keeps it out of every log, error and answer.
// On `504` the call may have reached Meta and is not replayed: read the
// settings before asking again.
export async function enablePayloadEncryption(api: WhatsApp, pn: string, publicKeyBase64: string) {
  const { error } = await api.PUT("/v1/numbers/{pn}/settings/payload-encryption", {
    params: { path: { pn } },
    body: { client_encryption_key: publicKeyBase64 },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// Adopt (or change to) a business username; `forceTransfer` moves it from
// another number of the same portfolio.
export async function setUsername(api: WhatsApp, pn: string, username: string, forceTransfer = false) {
  const { data, error } = await api.PUT("/v1/numbers/{pn}/username", {
    params: { path: { pn } },
    body: { username, force_transfer: forceTransfer },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.status;
}

// Whether messages can be sent through the number, and why not.
export async function healthStatus(api: WhatsApp, pn: string): Promise<NumberHealthStatus> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/health-status", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Ice breakers and commands (4 of 80 characters, 30 commands).
export async function setComponents(api: WhatsApp, pn: string) {
  const { error } = await api.PUT("/v1/numbers/{pn}/conversational-components", {
    params: { path: { pn } },
    body: {
      enable_welcome_message: true,
      prompts: ["Book a table"],
      commands: [{ command_name: "book", command_description: "Book a table" }],
    },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// The legal act: until the deployment sets WA_SERVER_LEGAL_ACTS=on, this is
// `feature_not_available` and Meta is not asked.
export async function declareComplianceInfo(api: WhatsApp, pn: string, info: ComplianceInfoBody) {
  const { error } = await api.PUT("/v1/numbers/{pn}/compliance-info", {
    params: { path: { pn } },
    body: info,
  });
  if (error) {
    if ((error.error.code as KnownErrorCode) === "feature_not_available") {
      return false;
    }
    throw new WhatsAppError(error.error);
  }
  return true;
}

// Not a legal act: it reaches Meta whatever the setting says. Keep the
// Idempotency-Key per application you mean to make.
export async function applyForOfficialBusinessAccount(
  api: WhatsApp,
  pn: string,
  application: ObaApplicationBody,
  reference: string,
) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/official-business-account", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: application,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.tracking_id;
}

// Irreversible: erase one user's contact book entry at Meta. Needs the
// scope `erasure` (and no setting); the id must be a BSUID (a phone number
// is `invalid_request`).
export async function eraseContactBookEntry(api: WhatsApp, pn: string, bsuid: string) {
  const { data, error } = await api.DELETE("/v1/numbers/{pn}/contact-book/{bsuid}", {
    params: { path: { pn, bsuid } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.deleted;
}

// A WABA's health status (roadmap M5c6): `can_send_message` and the nodes
// you own or already hold. Another tenant's WABA is `404`; an entity that
// is not yours is not in the list.
export async function wabaHealth(api: WhatsApp, wabaId: string) {
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/health-status", {
    params: { path: { waba_id: wabaId } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// The number's own webhook override, if any (`null`: Meta delivers its
// webhooks to the service). The WABA's and the app's callbacks are not
// answered.
export async function webhookOverride(api: WhatsApp, pn: string): Promise<string | null> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/webhook-override", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.url;
}

// Divert the number's `messages`, `calls`, `history` and the other
// supported webhooks to your URL: the service then no longer sees them.
// `403 forbidden` without the scope `webhook_override`, `403
// feature_not_available` until the deployment sets
// `WA_SERVER_NUMBER_WEBHOOK_OVERRIDE=on`; the URL must be one a webhook
// endpoint could have (`422` on `url` otherwise); the token is yours, never
// answered or logged.
export async function setWebhookOverride(
  api: WhatsApp,
  pn: string,
  body: SetWebhookOverride,
) {
  const { error } = await api.PUT("/v1/numbers/{pn}/webhook-override", {
    params: { path: { pn } },
    body,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// Restore delivery to the service. Always allowed, whatever the setting.
export async function clearWebhookOverride(api: WhatsApp, pn: string) {
  const { error } = await api.DELETE("/v1/numbers/{pn}/webhook-override", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}
