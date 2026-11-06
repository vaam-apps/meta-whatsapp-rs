// A number's profile picture and display name through meta-whatsapp-server
// (scope `numbers`, and `media` for the upload), typed from its OpenAPI
// document. Generate the types next to this file first (once per service
// upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type ProfilePatch = components["schemas"]["ProfilePatch"];
export type DisplayName = components["schemas"]["DisplayName"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scopes `numbers` and `media`). A platform key also
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

// Set the picture from a completed upload of this number (a JPEG or PNG:
// `POST /v1/numbers/{pn}/media-uploads`, then its chunk, as the media
// skill shows; the handle Meta gives for the file stays in the service).
// Another tenant's upload, another number's, an expired one and an unknown
// one are all `not_found` (nothing is asked of Meta); one that is not an
// image, or not complete, is `invalid_request` on
// `profile_picture_upload_id`.
export async function setPicture(api: WhatsApp, pn: string, uploadId: string) {
  const body: ProfilePatch = { profile_picture_upload_id: uploadId };
  const { data, error } = await api.PATCH("/v1/numbers/{pn}/profile", {
    params: { path: { pn } },
    body,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.profile_picture_url;
}

// Ask Meta to review a new display name. The answer is `202`: the review is
// asynchronous, and the name changes only once it is approved *and* the
// number is registered again. Keep the `Idempotency-Key` per change you
// mean to make: Meta allows 10 changes per 30 days.
export async function requestDisplayName(api: WhatsApp, pn: string, name: string, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/display-name", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { new_display_name: name },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.new_display_name;
}

// Where the display name stands: the one WhatsApp holds, and the review of
// the one requested.
export async function displayName(api: WhatsApp, pn: string): Promise<DisplayName> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/display-name", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Once the review says APPROVED (the `phone_number_name_updated` event, or
// `new_name_status`), register the number again with its two-step PIN
// (typed for the attempt, stored nowhere) within the 14 days Meta allows:
// until then the old name stays.
export async function applyApprovedName(api: WhatsApp, pn: string, pin: string, attempt: string): Promise<boolean> {
  const status = await displayName(api, pn);
  if (status.new_name_status !== "APPROVED") {
    return false;
  }
  const { error } = await api.POST("/v1/numbers/{pn}/register", {
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:rename:${attempt}` } },
    body: { pin },
  });
  if (error) {
    if ((error.error.code as KnownErrorCode) === "two_step_verification") {
      return false;
    }
    throw new WhatsAppError(error.error);
  }
  return true;
}
