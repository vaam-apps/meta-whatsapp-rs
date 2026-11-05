// WhatsApp Flows through meta-whatsapp-server (scope flows): create a
// Flow, fill it with its Flow JSON, publish it, preview it, typed from its
// OpenAPI document; the client is send.ts's. meta-whatsapp-rs's gate
// type-checks this file against the committed document (`just skills-ts`).

import type { components } from "./meta-whatsapp-server";
import { WhatsAppError } from "./send";
import type { WhatsApp } from "./send";

export type FlowCreateRequest = components["schemas"]["FlowCreateRequest"];
export type FlowValidationError = components["schemas"]["FlowValidationError"];

// Create a draft Flow. A Flow JSON sent with it is stored even when Meta
// finds problems in it: they come back in validation_errors, not as an
// error. The Idempotency-Key makes a retry after a timeout safe.
export async function createFlow(api: WhatsApp, wabaId: string, flow: FlowCreateRequest, reference: string) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/flows", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: flow,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Replace a Flow's Flow JSON (the object itself, at most 2 MiB; it is sent
// to Meta as it arrived). Returns Meta's validation errors: publish only
// when there are none. Another WABA's Flow is not_found, nothing uploaded.
export async function uploadFlowJson(api: WhatsApp, wabaId: string, flowId: string, flowJson: Record<string, unknown>) {
  const { data, error } = await api.PUT("/v1/wabas/{waba_id}/flows/{flow_id}/json", {
    params: { path: { waba_id: wabaId, flow_id: flowId } },
    body: flowJson,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.validation_errors;
}

// Publish a Flow once it has no validation error. A published Flow can no
// longer be deleted, only deprecated; Meta refuses while a publishing
// check fails (a number's business public key, the endpoint, the Meta app).
export async function publishFlow(api: WhatsApp, wabaId: string, flowId: string) {
  const { error } = await api.POST("/v1/wabas/{waba_id}/flows/{flow_id}/publish", {
    params: { path: { waba_id: wabaId, flow_id: flowId } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// A preview link for stakeholders: it needs no login and carries a token,
// so never log it. invalidate revokes the link given before.
export async function previewFlow(api: WhatsApp, wabaId: string, flowId: string, invalidate = false) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/flows/{flow_id}/preview", {
    params: { path: { waba_id: wabaId, flow_id: flowId } },
    body: { invalidate },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.preview_url;
}
