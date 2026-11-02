// The send union's other types through meta-whatsapp-server (roadmap
// M5a): Flows, call buttons and call permission requests, carousels,
// product messages, location and address requests, pins and Direct Send,
// typed from its OpenAPI document; the client and the outcome are
// send.ts's. meta-whatsapp-rs's gate type-checks this file against the
// committed document (`just skills-ts`).

import type { components } from "./meta-whatsapp-server";
import { send } from "./send";
import type { Outcome, SendMessage, WhatsApp } from "./send";

export type FlowParameters = components["schemas"]["FlowParameters"];
export type CarouselCard = components["schemas"]["CarouselCard"];

// Open a published Flow by name; your flow_token comes back in its
// answer (never logged by the service). By flow_id instead, the Flow must
// be your WABA's: another's is not_found, nothing sent.
export function flow(phone: string, body: string, flowName: string, flowToken: string): SendMessage {
  const parameters: FlowParameters = {
    flow_message_version: "3",
    flow_name: flowName,
    flow_cta: "Book!",
    flow_token: flowToken,
  };
  return {
    to: { phone },
    type: "interactive",
    interactive: { type: "flow", body: { text: body }, action: { name: "flow", parameters } },
  };
}

// A WhatsApp call button; payload returns in the call's events.
export function callButton(phone: string, body: string, payload: string): SendMessage {
  return {
    to: { phone },
    type: "interactive",
    interactive: {
      type: "voice_call",
      body: { text: body },
      action: { name: "voice_call", parameters: { display_text: "Call on WhatsApp", ttl_minutes: 100, payload } },
    },
  };
}

// Ask before calling: only the body can be set (no header, no footer).
export function callPermission(phone: string, body: string): SendMessage {
  return {
    to: { phone },
    type: "interactive",
    interactive: { type: "call_permission_request", body: { text: body }, action: { name: "call_permission_request" } },
  };
}

// A media card carousel: 2 to 10 cards, card_index their position, each
// with an https image and one URL button.
export function carousel(phone: string, body: string, products: { image: string; text: string; url: string }[]): SendMessage {
  const cards = products.map((p, i): CarouselCard => ({
    card_index: i,
    type: "cta_url",
    header: { type: "image", image: { link: p.image } },
    body: { text: p.text },
    action: { name: "cta_url", parameters: { display_text: "Buy now", url: p.url } },
  }));
  return { to: { phone }, type: "interactive", interactive: { type: "carousel", body: { text: body }, action: { cards } } };
}

// Several products of your WABA's catalog (another catalog: not_found).
export function productList(phone: string, catalogId: string, skus: string[]): SendMessage {
  return {
    to: { phone },
    type: "interactive",
    interactive: {
      type: "product_list",
      header: { type: "text", text: "Our picks" },
      body: { text: "Have a look" },
      action: {
        catalog_id: catalogId,
        sections: [{ title: "Picks", product_items: skus.map((sku) => ({ product_retailer_id: sku })) }],
      },
    },
  };
}

// Pin a message in a group for a week (unpin: no expiration_days).
export function pin(groupId: string, messageId: string): SendMessage {
  return { to: { group_id: groupId }, type: "pin", pin: { type: "pin", message_id: messageId, expiration_days: 7 } };
}

// Direct Send (eligible accounts only): Meta sends a utility message as a
// template it generates, outside the 24-hour window too.
export function utility(phone: string, text: string, templateName: string): SendMessage {
  return {
    to: { phone },
    type: "text",
    text: { body: text },
    category: "utility",
    ttl_seconds: 3600,
    direct_send_config: { template_name: templateName },
  };
}

// Send any of them under your own reference as the Idempotency-Key.
export async function sendInteractive(api: WhatsApp, pn: string, message: SendMessage, reference: string): Promise<Outcome> {
  return send(api, pn, message, reference);
}
