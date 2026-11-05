// WhatsApp groups through meta-whatsapp-server (scope `groups`), typed from
// its OpenAPI document. Generate the types next to this file first (once
// per service upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type CreateGroupRequest = components["schemas"]["CreateGroupRequest"];
export type GroupParticipantRef = components["schemas"]["GroupParticipantRef"];
export type GroupSummary = components["schemas"]["GroupSummary"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scope `groups`). A platform key also names the
// tenant it acts for.
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

// Ask Meta for a group. It is made asynchronously: the answer is only a
// request id. The group's id and invite link arrive in a
// `group_lifecycle_update` event (`group_updated` in /v1/events, type
// `group_create`, with that `request_id`), and the same event makes the
// group the number's in the service. Until then the group's routes answer
// `not_found`.
export async function createGroup(api: WhatsApp, pn: string, group: CreateGroupRequest, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/groups", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: group,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.request_id;
}

// A group created before the service, or whose event was missed, is found
// by listing the number's groups: every page listed becomes the number's.
export async function findGroup(api: WhatsApp, pn: string, subject: string): Promise<string | undefined> {
  let after: string | undefined;
  for (;;) {
    const { data, error } = await api.GET("/v1/numbers/{pn}/groups", {
      params: { path: { pn }, query: after === undefined ? { limit: 100 } : { limit: 100, after } },
    });
    if (error) {
      throw new WhatsAppError(error.error);
    }
    const found = data.data.find((group) => group.subject === subject);
    if (found !== undefined) {
      return found.id;
    }
    if (data.next_after === null) {
      return undefined;
    }
    after = data.next_after;
  }
}

// The invite link is a credential: whoever holds it can join. Reading it
// does not change it; resetting it stops every link sent before.
export async function inviteLink(api: WhatsApp, pn: string, groupId: string, reset: boolean): Promise<string> {
  const params = { path: { pn, group_id: groupId } };
  const { data, error } = reset
    ? await api.POST("/v1/numbers/{pn}/groups/{group_id}/invite-link", { params })
    : await api.GET("/v1/numbers/{pn}/groups/{group_id}/invite-link", { params });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.invite_link;
}

// Approve every open join request, 100 at a time. A request Meta could not
// approve comes back with its Meta code (`reasons`), never its text.
export async function approveJoinRequests(api: WhatsApp, pn: string, groupId: string) {
  const params = { path: { pn, group_id: groupId } };
  const refused: string[] = [];
  const open = await api.GET("/v1/numbers/{pn}/groups/{group_id}/join-requests", { params });
  if (open.error) {
    throw new WhatsAppError(open.error.error);
  }
  const ids = open.data.data.map((request) => request.join_request_id);
  for (let from = 0; from < ids.length; from += 100) {
    const { data, error } = await api.POST("/v1/numbers/{pn}/groups/{group_id}/join-requests/approve", {
      params,
      body: { join_request_ids: ids.slice(from, from + 100) },
    });
    if (error) {
      throw new WhatsAppError(error.error);
    }
    for (const failed of data.failed) {
      refused.push(failed.join_request_id ?? "unknown");
    }
  }
  return refused;
}

// Remove participants (at most 8 a call), each named one way: `phone`
// (E.164 with its +), `wa_id` (digits, as the participants route lists
// it) or `user_id` (a BSUID). `true` when Meta says some were not removed:
// the `group_participants_update` event says which.
export async function removeParticipants(
  api: WhatsApp,
  pn: string,
  groupId: string,
  users: GroupParticipantRef[],
): Promise<boolean> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/groups/{group_id}/participants/remove", {
    params: { path: { pn, group_id: groupId } },
    body: { users },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.partial;
}

// A group the number does not hold is `not_found`: another tenant's, another
// number's, one the service has not seen yet, or one Meta no longer has.
export async function holdsGroup(api: WhatsApp, pn: string, groupId: string): Promise<boolean> {
  const { error } = await api.GET("/v1/numbers/{pn}/groups/{group_id}", {
    params: { path: { pn, group_id: groupId }, query: { fields: "subject" } },
  });
  if (error === undefined) {
    return true;
  }
  if ((error.error.code as KnownErrorCode) === "not_found") {
    return false;
  }
  throw new WhatsAppError(error.error);
}
