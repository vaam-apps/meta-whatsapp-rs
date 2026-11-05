// Commerce, QR codes, analytics and the block list through
// meta-whatsapp-server (scopes commerce, qr_codes, analytics and
// contacts), typed from its OpenAPI document; the client is send.ts's.
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import type { components } from "./meta-whatsapp-server";
import { WhatsAppError } from "./send";
import type { WhatsApp } from "./send";

export type BlockUsersRequest = components["schemas"]["BlockUsersRequest"];
export type BlockUsersResult = components["schemas"]["BlockUsersResult"];

// Block people by phone number (with its +) or BSUID. Meta only blocks
// someone who wrote to the number in the last 24 hours; the answer says
// who was and who was not, by Meta's error code, never by its text. The
// same key and body never block twice.
export async function block(api: WhatsApp, pn: string, request: BlockUsersRequest, reference: string): Promise<BlockUsersResult> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/blocked-users", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: request,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Unblock them: a POST, because a body on a DELETE is dropped by proxies.
export async function unblock(api: WhatsApp, pn: string, request: BlockUsersRequest, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/blocked-users/unblock", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: request,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.refused.map((person) => person.reasons.map((reason) => reason.code));
}

// A QR code and its short link that open a chat with a prefilled message
// (1 to 140 characters). The same key never creates two.
export async function createQrCode(api: WhatsApp, pn: string, message: string, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/qr-codes", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { prefilled_message: message, generate_qr_image: "PNG" },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return { link: data.deep_link_url, image: data.qr_image_url };
}

// Delete a QR code of this number: another number's is not_found, after
// one lookup, and nothing is deleted.
export async function deleteQrCode(api: WhatsApp, pn: string, code: string) {
  const { error } = await api.DELETE("/v1/numbers/{pn}/qr-codes/{code}", {
    params: { path: { pn, code } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// A day-by-day count of the messages sent and delivered over the last
// week. A range is bounded: a year at most (HALF_HOUR: 31 days).
export async function lastWeek(api: WhatsApp, wabaId: string, now: Date) {
  const start = new Date(now.getTime() - 7 * 24 * 3600 * 1000);
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/analytics/messaging", {
    params: {
      path: { waba_id: wabaId },
      query: { start: start.toISOString(), end: now.toISOString(), granularity: "DAY" },
    },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.data_points;
}

// The products of the WABA's catalog, a page at a time. The catalog must
// be one of the WABA's own: another's is not_found.
export async function products(api: WhatsApp, wabaId: string, catalogId: string, cursor?: string) {
  const query = cursor === undefined ? { limit: 100 } : { limit: 100, cursor };
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/catalogs/{catalog_id}/products", {
    params: { path: { waba_id: wabaId, catalog_id: catalogId }, query },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}
