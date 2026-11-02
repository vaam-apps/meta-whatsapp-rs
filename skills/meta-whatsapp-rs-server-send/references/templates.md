# Managing templates

> **Verified against meta-whatsapp-rs 4edbbe7ee97ff56b587e367cf10fc4f618c6ee79 (2026-11-02).** Source: the service's OpenAPI document (`crates/meta-whatsapp-server/openapi/v1.json`, tag `templates`), `docs/guides/server.md` section "Templates" and [examples/templates.ts](../examples/templates.ts).

Scope `templates`, on a WABA of your tenant, with its token. Every
template id, group id and catalog a request names must be that WABA's:
another WABA's answers `not_found`, like one that does not exist, and
nothing is done.

| Route | Does |
| --- | --- |
| `PATCH /v1/wabas/{waba_id}/templates/{id}` | edit (`TemplateEdit`: Meta's body, `components` replacing all of them); `204` |
| `POST /v1/wabas/{waba_id}/templates/{id}/unpause` | unpause a paused template; `204` |
| `GET /v1/wabas/{waba_id}/templates/{id}/compare` | compare with one other template (`template_ids`, `start`, `end`: UNIX timestamps): `TemplateComparison` |
| `POST /v1/wabas/{waba_id}/templates/archive`, `POST /v1/wabas/{waba_id}/templates/unarchive` | `TemplateIds` (`hsm_ids`, 1 to 100); Meta's answer with `failed_templates` |
| `GET /v1/wabas/{waba_id}/templates/library`, `POST /v1/wabas/{waba_id}/templates/library` | browse the Template Library; create from an entry (`LibraryTemplateRequest`); `201` |
| `POST /v1/wabas/{waba_id}/templates/migrations` | `TemplateMigration`: recreate another of your WABAs' templates here; `TemplateMigrationResult` |
| `POST /v1/wabas/{waba_id}/template-groups`, `GET`, `PATCH` and `DELETE /v1/wabas/{waba_id}/template-groups/{group_id}` | template groups: create (`201` with `id`), read with the first page of its templates, change, delete |

Edit a template's components:

```ts
export async function editTemplate(api: WhatsApp, wabaId: string, id: string, edit: TemplateEdit) {
  const { error } = await api.PATCH("/v1/wabas/{waba_id}/templates/{id}", {
    params: { path: { waba_id: wabaId, id } },
    body: edit,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}
```

- **Never retry an edit blindly.** An approved template takes 10 edits
  in 30 days; a `timeout` may have edited it. Only `APPROVED`,
  `REJECTED` and `PAUSED` templates can be edited; `name` and `language`
  cannot (`invalid_request` on them).
- **Bulk ids are found in your WABA first**, in its first 1,000
  templates: past them, or another WABA's, is `not_found` with nothing
  done. An archived template is deleted by Meta 28 days later unless
  unarchived; read `failed_templates` for the ones Meta refused.
- **The library's button inputs are an array of objects**, not the
  quoted string Meta's page prints (`invalid_request` on `body`). A
  creation from the library, a migration and a group's creation take an
  `Idempotency-Key`: the same key never creates twice.
- **A migration's source is one of your tenant's WABAs** (and the same
  business's, Meta's rule); its ids are checked in it first.
- **Zero-tap is never accepted for you**: `zero_tap_terms_accepted: true`
  anywhere is `invalid_request` on its path.

```ts
export async function archive(api: WhatsApp, wabaId: string, ids: string[]) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/templates/archive", {
    params: { path: { waba_id: wabaId } },
    body: { hsm_ids: ids },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.failed_templates;
}
```

Not offered: the list of a WABA's template groups, the pages after the
first of a group's templates, and the library's next page (no page of
Meta's documents them).
