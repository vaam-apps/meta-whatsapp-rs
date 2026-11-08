// The bot through meta-whatsapp-server (scope `bot`), typed from its OpenAPI
// document. Generate the types next to this file first (once per service
// upgrade):
//   npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts
// meta-whatsapp-rs's gate type-checks this file against the committed
// document (`just skills-ts`).

import createClient from "openapi-fetch";
import type { components, paths } from "./meta-whatsapp-server";

export type BotRule = components["schemas"]["BotRule"];
export type BotRuleRequest = components["schemas"]["BotRuleRequest"];
export type BotSettings = components["schemas"]["BotSettings"];
export type ErrorObject = components["schemas"]["ErrorObject"];
export type KnownErrorCode = components["schemas"]["KnownErrorCode"];

// One client per key (scope `bot`). A platform key also names the tenant it
// acts for.
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

// A number's bot is off until you turn it on. `owners` pass the rules marked
// `owners_only`; `banned` get no answer at all. Each is a phone number (with
// or without its `+`) or a business-scoped user id (BSUID); both lists are
// replaced by the call.
export async function turnOn(
  api: WhatsApp,
  pn: string,
  owners: string[],
  banned: string[],
): Promise<BotSettings> {
  const { data, error } = await api.PUT("/v1/numbers/{pn}/bot", {
    params: { path: { pn } },
    body: { enabled: true, owners, banned },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Switch it off: the rules are kept, nothing is answered.
export async function turnOff(api: WhatsApp, pn: string): Promise<BotSettings> {
  const { data, error } = await api.PUT("/v1/numbers/{pn}/bot", {
    params: { path: { pn } },
    body: { enabled: false },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Add a rule. Keep one `Idempotency-Key` per rule you mean to add: a repeat of
// the same call answers the first rule again and adds no second one. A refusal
// is `422 invalid_request` whose `field` names the part at fault
// (`trigger.names`, `action.text`, ...), or `rules` at the number's limit.
export async function add(
  api: WhatsApp,
  pn: string,
  reference: string,
  rule: BotRuleRequest,
): Promise<BotRule> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/bot/rules", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: rule,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// `/hours` (or `/open`) answers with a text, to a person once a minute. Every
// word is literal: a name is matched whole, ignoring case, after one of the
// prefixes (`/` when you name none). Nothing you write here is a pattern.
export function hoursCommand(): BotRuleRequest {
  return {
    name: "Opening hours",
    description: "Opening hours",
    trigger: { type: "command", names: ["hours", "open"] },
    action: { type: "reply", text: "We are open 9 to 5, Monday to Friday." },
    cooldown_seconds: 60,
  };
}

// A message that contains a word: the text is sent inside the 24-hour window;
// outside it the template is, because a free-form message cannot be. Without
// `outside_window_template`, nothing is sent outside the window.
export function priceKeyword(): BotRuleRequest {
  return {
    trigger: { type: "keyword", words: ["price", "cost"], mode: "contains" },
    action: {
      type: "reply",
      text: "Our price list is at https://example.com/prices.",
      outside_window_template: { name: "price_list", language: { code: "en_US" } },
    },
    priority: 200,
  };
}

// A tapped reply button or list row, by the id you sent it with.
export function menuButton(): BotRuleRequest {
  return {
    trigger: { type: "payload", ids: ["MENU"] },
    action: { type: "reply", text: "Today: soup, pasta, tiramisu." },
  };
}

// The generated help: one line per command rule that has a description. Only
// for a command or keyword trigger.
export function helpCommand(): BotRuleRequest {
  return {
    trigger: { type: "command", names: ["help"] },
    action: { type: "help" },
    priority: 1,
  };
}

// The rules in the order they are tried (priority, then age). The first rule
// that answers decides; `continue_after` lets the next ones run too.
export async function rules(api: WhatsApp, pn: string): Promise<BotRule[]> {
  const { data, error } = await api.GET("/v1/numbers/{pn}/bot/rules", {
    params: { path: { pn } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.data;
}

// Replace a rule: its id and date stay.
export async function replace(
  api: WhatsApp,
  pn: string,
  id: string,
  rule: BotRuleRequest,
): Promise<BotRule> {
  const { data, error } = await api.PUT("/v1/numbers/{pn}/bot/rules/{rule_id}", {
    params: { path: { pn, rule_id: id } },
    body: rule,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Remove a rule.
export async function remove(api: WhatsApp, pn: string, id: string): Promise<void> {
  const { error } = await api.DELETE("/v1/numbers/{pn}/bot/rules/{rule_id}", {
    params: { path: { pn, rule_id: id } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
}

// Another tenant's number, another tenant's rule and one never made are the
// same `404 not_found`.
export function missing(error: unknown): boolean {
  return error instanceof WhatsAppError && error.error.code === "not_found";
}
