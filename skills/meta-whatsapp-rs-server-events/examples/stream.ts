// Live WhatsApp events from meta-whatsapp-server: GET /v1/events/stream
// (Server-Sent Events), resumed with Last-Event-ID, and one event by its
// id, typed from its OpenAPI document (generate meta-whatsapp-server.d.ts
// next to this file as events.ts says). meta-whatsapp-rs's gate
// type-checks this file against the committed document (`just skills-ts`).

import type { paths } from "./meta-whatsapp-server";
import { WhatsAppError, type Cursor, type Event, type WhatsApp } from "./events";

// Annotate the query with the generated type: TypeScript then rejects a
// misspelled parameter.
export type StreamQuery = NonNullable<paths["/v1/events/stream"]["get"]["parameters"]["query"]>;

// One frame as the service sends it: `event: whatsapp` with the event's
// sequence as `id` and its envelope as `data`, or the last frame,
// `event: lagged` or `event: shutdown`, whose data is {"after": n}.
type Frame = { event: string; id: string | undefined; data: string };

// The complete frames of `buffer` (each ends with a blank line), and what
// is left of it. Comments (`: keepalive`) carry nothing.
function split(buffer: string): { frames: Frame[]; rest: string } {
  const frames: Frame[] = [];
  let rest = buffer;
  let end = rest.indexOf("\n\n");
  while (end >= 0) {
    const raw = rest.slice(0, end);
    rest = rest.slice(end + 2);
    end = rest.indexOf("\n\n");
    let event = "message";
    let id: string | undefined;
    const data: string[] = [];
    for (const line of raw.split("\n")) {
      if (line.startsWith(":")) continue;
      const colon = line.indexOf(":");
      const field = colon < 0 ? line : line.slice(0, colon);
      const value = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
      if (field === "event") event = value;
      if (field === "id") id = value;
      if (field === "data") data.push(value);
    }
    if (data.length > 0) frames.push({ event, id, data: data.join("\n") });
  }
  return { frames, rest };
}

// Follow the tenant's events until `signal` aborts. Handle each event,
// then save its sequence: a reconnection sends it as Last-Event-ID and the
// stream resumes exactly after it, on any replica. The cursor is the one
// polling keeps (events.ts): catch up with GET /v1/events first, then
// follow from its next_after. Handlers must be idempotent (skip an event
// id already handled), as for polling.
export async function follow(
  api: WhatsApp,
  cursor: Cursor,
  handle: (event: Event) => Promise<void>,
  resync: () => Promise<void>,
  signal: AbortSignal,
): Promise<void> {
  while (!signal.aborted) {
    const after = await cursor.load();
    const header = after === undefined ? {} : { "Last-Event-ID": String(after) };
    const query: StreamQuery = { types: "message_received,status_updated" };
    const { data, error, response } = await api.GET("/v1/events/stream", {
      params: { header, query },
      parseAs: "stream",
      signal,
    });
    if (error) {
      // Too many streams open for the tenant on this replica (429), or
      // the replica is stopping (503): wait, then open it again.
      const code = error.error.code;
      if (code === "too_many_streams" || code === "too_many_requests" || code === "shutting_down") {
        const seconds = Number(response.headers.get("Retry-After"));
        await new Promise((resolve) => setTimeout(resolve, (seconds > 0 ? seconds : 1) * 1000));
        continue;
      }
      // Events after the cursor were purged, or the cursor is past the
      // tenant's newest event (a restored database): rebuild, start over.
      const expired = code === "cursor_expired";
      const unknown = code === "invalid_request" && error.error.field === "Last-Event-ID";
      if (expired || unknown) {
        await resync();
        await cursor.clear();
        continue;
      }
      throw new WhatsAppError(error.error);
    }
    if (data === null) continue;
    const reader = data.pipeThrough(new TextDecoderStream()).getReader();
    let buffer = "";
    let ended = false;
    while (!ended) {
      const { value, done } = await reader.read();
      if (done) break;
      const { frames, rest } = split(buffer + value);
      buffer = rest;
      for (const frame of frames) {
        if (frame.event === "whatsapp") {
          const event = JSON.parse(frame.data) as Event;
          await handle(event);
          await cursor.save(event.sequence);
        } else if (frame.event === "lagged" || frame.event === "shutdown") {
          // Dropped as too slow, events purged before they came, or the
          // replica stopping: reconnect with the cursor saved above.
          ended = true;
          break;
        }
      }
    }
    await reader.cancel();
  }
}

// One event, whole, by the id an envelope carries (a webhook delivery
// that left a large `data` out refers to it): undefined when it is not
// this tenant's (another tenant's, past retention, or no event's id).
export async function eventById(api: WhatsApp, id: string): Promise<Event | undefined> {
  const { data, error } = await api.GET("/v1/events/{id}", { params: { path: { id } } });
  if (error) {
    if (error.error.code === "not_found") return undefined;
    throw new WhatsAppError(error.error);
  }
  return data;
}
