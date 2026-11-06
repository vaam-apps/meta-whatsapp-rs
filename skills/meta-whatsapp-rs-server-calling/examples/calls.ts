// WhatsApp Calling through meta-whatsapp-server (scope `calls`), typed from
// its OpenAPI document. Generate the types next to this file first (once
// per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type CallingSettings = components["schemas"]["CallingSettings"];
export type CallAnnouncement = components["schemas"]["CallAnnouncement"];
export type CallParticipant = components["schemas"]["CallParticipant"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scope `calls`; `analytics` for the call analytics). A
// platform key also names the tenant it acts for.
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

// May the business call this user? Name them by phone number (E.164 with
// its +) and/or BSUID. `start_call` says whether a call may be placed now.
export async function mayCall(api: WhatsApp, pn: string, user: CallParticipant): Promise<boolean> {
  const query = {
    ...(user.phone != null ? { phone: user.phone } : {}),
    ...(user.user_id != null ? { user_id: user.user_id } : {}),
  };
  const { data, error } = await api.GET("/v1/numbers/{pn}/call-permissions", {
    params: { path: { pn }, query },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.actions.some((action) => action.action_name === "start_call" && action.can_perform_action);
}

// Place a call with your own SDP offer. Keep the `Idempotency-Key` per call
// you mean to place: a repeat is answered from the kept answer, never a
// second ring. On `504` the user's phone may be ringing: wait for the call's
// events before trying again. Recording is the caller's choice, per call:
// pass `recording` only when you mean to record, with the purpose Meta
// speaks to both participants.
export async function placeCall(
  api: WhatsApp,
  pn: string,
  to: CallParticipant,
  sdpOffer: string,
  reference: string,
  recording?: CallAnnouncement,
): Promise<string> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/calls", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { to, sdp_offer: sdpOffer, ...(recording !== undefined ? { recording } : {}) },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.call_id;
}

// A user's call arrives as a `call_updated` event (`connect`, with the user's
// SDP offer). Answer it within 30 to 60 seconds: pre-accept so that media
// can connect, then accept, and only then send media.
export async function answerCall(api: WhatsApp, pn: string, callId: string, sdpAnswer: string) {
  const params = { path: { pn, call_id: callId } };
  const pre = await api.POST("/v1/numbers/{pn}/calls/{call_id}/pre-accept", {
    params,
    body: { sdp_answer: sdpAnswer },
  });
  if (pre.error) {
    throw new WhatsAppError(pre.error.error);
  }
  const accepted = await api.POST("/v1/numbers/{pn}/calls/{call_id}/accept", {
    params,
    body: { sdp_answer: sdpAnswer },
  });
  if (accepted.error) {
    throw new WhatsAppError(accepted.error.error);
  }
}

// End a call in either direction, also after the media ended (Meta prices
// the call from this). A call the number does not hold is `not_found`:
// another tenant's, another number's, one the service saw no event of in
// the last 24 hours.
export async function endCall(api: WhatsApp, pn: string, callId: string): Promise<boolean> {
  const { error } = await api.POST("/v1/numbers/{pn}/calls/{call_id}/terminate", {
    params: { path: { pn, call_id: callId } },
  });
  if (error === undefined) {
    return true;
  }
  if ((error.error.code as KnownErrorCode) === "not_found") {
    return false;
  }
  throw new WhatsAppError(error.error);
}

// Take calls from nine to five on weekdays. `call_hours` is replaced as a
// whole: a holiday schedule left out clears the one Meta holds.
export async function setCallHours(api: WhatsApp, pn: string, timezoneId: string): Promise<void> {
  const days = ["MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY"];
  const settings: CallingSettings = {
    call_hours: {
      status: "ENABLED",
      timezone_id: timezoneId,
      weekly_operating_hours: days.map((day) => ({ day_of_week: day, open_time: "0900", close_time: "1700" })),
    },
  };
  const { error } = await api.PATCH("/v1/numbers/{pn}/call-settings", {
    params: { path: { pn } },
    body: settings,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}
