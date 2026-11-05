// A template's life after its creation through meta-whatsapp-server
// (scope templates): edits, unpausing, archiving, the Template Library,
// migrations and template groups, typed from its OpenAPI document; the
// client is send.ts's. meta-whatsapp-rs's gate type-checks this file
// against the committed document (`just skills-ts`).

import type { components } from "./meta-whatsapp-server";
import { WhatsAppError } from "./send";
import type { WhatsApp } from "./send";

export type TemplateEdit = components["schemas"]["TemplateEdit"];
export type LibraryTemplateRequest = components["schemas"]["LibraryTemplateRequest"];
export type TemplateMigration = components["schemas"]["TemplateMigration"];
export type TemplateGroupCreate = components["schemas"]["TemplateGroupCreate"];

// Replace a template's components (all of them): Meta reviews it again.
// An approved template takes 10 edits in 30 days, so never retry one
// blindly after a timeout.
export async function editTemplate(api: WhatsApp, wabaId: string, id: string, edit: TemplateEdit) {
  const { error } = await api.PATCH("/v1/wabas/{waba_id}/templates/{id}", {
    params: { path: { waba_id: wabaId, id } },
    body: edit,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// Archive up to 100 templates: Meta's answer says which ones it did not
// archive, and why. Every id must be this WABA's, else not_found and
// nothing archived.
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

// Browse the Template Library, then create from an entry under your own
// reference: the button inputs are an array of objects.
export async function fromLibrary(api: WhatsApp, wabaId: string, request: LibraryTemplateRequest, reference: string) {
  const entries = await api.GET("/v1/wabas/{waba_id}/templates/library", {
    params: { path: { waba_id: wabaId }, query: { name: request.library_template_name } },
  });
  if (entries.error) {
    throw new WhatsAppError(entries.error.error);
  }
  if (entries.data.data.length === 0) {
    return undefined;
  }
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/templates/library", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: request,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Recreate another of your WABAs' templates in this one (the same
// business's; the source must be your tenant's too).
export async function migrate(api: WhatsApp, wabaId: string, migration: TemplateMigration, reference: string) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/templates/migrations", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: migration,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Group templates for their analytics; ids as strings.
export async function createGroup(api: WhatsApp, wabaId: string, group: TemplateGroupCreate, reference: string) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/template-groups", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: group,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.id;
}
