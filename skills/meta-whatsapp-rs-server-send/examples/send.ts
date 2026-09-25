// Sending messages, media and templates through meta-whatsapp-server,
// typed from its OpenAPI document. Generate the types next to this file
// first (once per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type SendMessage = components["schemas"]["SendMessage"];
export type MessageAccepted = components["schemas"]["MessageAccepted"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];
export type MediaUploaded = components["schemas"]["MediaUploaded"];
export type TemplateDefinition = components["schemas"]["TemplateDefinition"];
export type TemplatesQuery = NonNullable<
  paths["/v1/wabas/{waba_id}/templates"]["get"]["parameters"]["query"]
>;

// One client per key. A platform key also names the tenant it acts for.
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

// What a send's outcome means for your job queue.
export type Outcome =
  | { kind: "sent"; messageId: string }
  | { kind: "reconcile" } // may have gone out: wait for its status event
  | { kind: "retry_later"; afterSeconds: number | undefined }
  | { kind: "send_a_template" } // the 24-hour window is closed
  | { kind: "fix_request"; field: string | null }
  | { kind: "give_up"; code: string };

// Never resend when may_have_been_sent is true: repeat with the SAME
// Idempotency-Key (the service replays the first answer) or reconcile.
export function outcome(error: ErrorObject, retryAfter: string | null): Outcome {
  if (error.may_have_been_sent) {
    return { kind: "reconcile" };
  }
  switch (error.code as KnownErrorCode) {
    case "customer_service_window_closed":
      return { kind: "send_a_template" };
    case "invalid_request":
    case "unsupported_message_type":
    case "template_parameter_mismatch":
      return { kind: "fix_request", field: error.field };
    case "too_many_requests":
    case "rate_limited":
    case "pair_rate_limited":
      return { kind: "retry_later", afterSeconds: retryAfter === null ? undefined : Number(retryAfter) };
    default:
      return error.retryable ? { kind: "retry_later", afterSeconds: undefined } : { kind: "give_up", code: error.code };
  }
}

// Send a message. `reference` is yours (an order event, say): it is the
// Idempotency-Key, so a repeat never sends twice, and the callback_data
// its status events carry.
export async function send(api: WhatsApp, pn: string, message: SendMessage, reference: string): Promise<Outcome> {
  const { data, error, response } = await api.POST("/v1/numbers/{pn}/messages", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { ...message, callback_data: reference },
  });
  if (error) {
    return outcome(error.error, response.headers.get("Retry-After"));
  }
  return { kind: "sent", messageId: data.message_id };
}

// A free-form text: E.164 with its "+", or the BSUID a webhook gave you.
export function orderShipped(to: string, order: string): SendMessage {
  const recipient = to.startsWith("+") ? { phone: to } : { user_id: to };
  return { to: recipient, type: "text", text: { body: `Your order ${order} has shipped.` } };
}

// An approved template, as Meta's send-template pages write it: works
// outside the 24-hour window.
export function orderConfirmation(phone: string, customer: string): SendMessage {
  return {
    to: { phone },
    type: "template",
    template: {
      name: "order_confirmation",
      language: { code: "en_US" },
      components: [{ type: "body", parameters: [{ type: "text", text: customer }] }],
    },
  };
}

// Blue ticks on a received message; "typing…" only when a reply follows.
export async function markRead(api: WhatsApp, pn: string, messageId: string, typing: boolean) {
  const { error } = await api.POST("/v1/numbers/{pn}/messages/{message_id}/read", {
    params: { path: { pn, message_id: messageId } },
    body: { typing_indicator: typing },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// Upload a file (multipart: type, file). Plain fetch: the browser or
// Node builds the form's boundary.
export async function upload(baseUrl: string, key: string, pn: string, file: Blob, type: string, filename: string, reference: string): Promise<MediaUploaded> {
  const form = new FormData();
  form.append("type", type);
  form.append("file", file, filename);
  const response = await fetch(`${baseUrl}/v1/numbers/${encodeURIComponent(pn)}/media`, {
    method: "POST",
    headers: { Authorization: `Bearer ${key}`, "Idempotency-Key": reference },
    body: form,
  });
  if (!response.ok) {
    const failed: { error: ErrorObject } = await response.json();
    throw new WhatsAppError(failed.error);
  }
  const uploaded: MediaUploaded = await response.json();
  return uploaded;
}

// Download a received file, verified by the service before the first
// byte (at most 16 MiB; larger ones need stream=true).
export async function download(api: WhatsApp, pn: string, mediaId: string) {
  const { data, error, response } = await api.GET("/v1/numbers/{pn}/media/{media_id}", {
    params: { path: { pn, media_id: mediaId } },
    parseAs: "arrayBuffer",
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return { bytes: data, type: response.headers.get("Content-Type"), sha256: response.headers.get("X-WA-SHA256") };
}

// Approved templates of a WABA (the service caches a page 60 seconds).
export async function approvedTemplates(api: WhatsApp, wabaId: string) {
  const query: TemplatesQuery = { status: "APPROVED", limit: 100 };
  const { data, error } = await api.GET("/v1/wabas/{waba_id}/templates", {
    params: { path: { waba_id: wabaId }, query },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.data;
}

// Submit a template for review, in Meta's JSON; the result arrives as a
// template_status_updated event.
export async function createTemplate(api: WhatsApp, wabaId: string, definition: TemplateDefinition, reference: string) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/templates", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: definition,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}
