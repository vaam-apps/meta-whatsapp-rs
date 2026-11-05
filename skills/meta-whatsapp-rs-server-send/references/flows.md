# Managing Flows

> **Verified against meta-whatsapp-rs 473e5534e9497aa0829aca5877d2dd48b5d8c90d (2026-11-05).** Source: the service's OpenAPI document (`crates/meta-whatsapp-server/openapi/v1.json`, tag flows), `docs/guides/server.md` section "Flows" and [examples/flows.ts](../examples/flows.ts).

Scope `templates`, on a WABA of your tenant, with its token. Every Flow a
request names must be that WABA's: another WABA's answers `not_found`,
like one that does not exist, and nothing is done.

| Route | Does |
| --- | --- |
| `POST /v1/wabas/{waba_id}/flows` | create (`FlowCreateRequest`: `name`, `categories`, and optionally `clone_flow_id`, `endpoint_uri`, `flow_json` as the object or Meta's text, `publish`); `201` with `id` and `validation_errors`; takes an `Idempotency-Key` |
| `GET /v1/wabas/{waba_id}/flows` | a page of the WABA's Flows (`FlowList`; `cursor` from `next_cursor`, no page size) |
| `GET`, `PATCH` and `DELETE /v1/wabas/{waba_id}/flows/{flow_id}` | read (`Flow`), change `name`, `categories` or `endpoint_uri` (`FlowUpdateRequest`), delete a draft; `204` for the last two |
| `PUT /v1/wabas/{waba_id}/flows/{flow_id}/json` | replace the Flow JSON (the body is the object, at most 2 MiB): `FlowJsonUpload` with Meta's `validation_errors` |
| `POST /v1/wabas/{waba_id}/flows/{flow_id}/preview` | `FlowPreviewRequest` (`invalidate`): `FlowPreview`, a link that needs no login |
| `POST /v1/wabas/{waba_id}/flows/{flow_id}/publish`, `POST …/deprecate` | publish; deprecate; `204` |

Create a draft, then fill it:

```ts
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
```

- **Meta validates the Flow JSON, and says so in the answer.** A
  creation or an upload with problems is stored all the same;
  `validation_errors` (`error`, `error_type`, `message`, lines and columns
  of your text, `pointers`) lists them, and publishing is refused (`422`
  `invalid_parameter`, with Meta's `details`) while any is left or a
  publishing check fails: a number's business public key, the endpoint
  set and answering the health check, a connected Meta app.
- **The upload keeps your text**: the body goes to Meta byte for byte, so
  the error lines are yours. A body that is not a JSON object is
  `invalid_request` on `body`; over 2 MiB is `payload_too_large`.
- **A published Flow can no longer be deleted**, only deprecated
  (terminal). Nothing here undoes either.
- **A preview link is a secret**: its token opens the Flow for 30 days or
  until `invalidate`; keep it out of logs.
- **Not offered**: connecting a Meta app to a Flow with an endpoint (do it
  in the Flow Builder), a Flow's health status and its assets.
- **The data endpoint** (`POST /webhooks/flows/{waba_id}` on the public
  listener) is Meta's, not yours: with `WA_SERVER_FLOWS_PRIVATE_KEY` the
  service decrypts and answers the health check and error notifications;
  the binary answers no data exchange (`503`), so a Flow that needs
  screens from your backend keeps its own `endpoint_uri`.

Upload the Flow JSON, then publish:

```ts
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
```
