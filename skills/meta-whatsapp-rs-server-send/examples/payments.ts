// Payments through meta-whatsapp-server (India's WhatsApp Payments,
// Brazil's payment messages), typed from its OpenAPI document; the
// client and the outcome are send.ts's. meta-whatsapp-rs's gate
// type-checks this file against the committed document (`just skills-ts`).

import type { components } from "./meta-whatsapp-server";
import { WhatsAppError, send } from "./send";
import type { Outcome, SendMessage, WhatsApp } from "./send";

export type OrderParameters = components["schemas"]["OrderParameters"];
export type CreatePaymentConfiguration = components["schemas"]["CreatePaymentConfiguration"];
export type PaymentRefundRequest = components["schemas"]["PaymentRefundRequest"];

// An order to review and pay (scope send): `parameters` exactly as Meta's
// payments page for your integration writes it. The service checks it by
// the order's currency's rules before anything is sent.
export function orderDetails(phone: string, body: string, parameters: OrderParameters): SendMessage {
  return {
    to: { phone },
    type: "interactive",
    interactive: {
      type: "order_details",
      body: { text: body },
      action: { name: "review_and_pay", parameters },
    },
  };
}

// Tell the buyer their order moved on (send one after every payment update).
export function orderStatus(phone: string, referenceId: string, status: string): SendMessage {
  return {
    to: { phone },
    type: "interactive",
    interactive: {
      type: "order_status",
      body: { text: `Your order is ${status}` },
      action: { name: "review_order", parameters: { reference_id: referenceId, order: { status } } },
    },
  };
}

// Send either, under your own reference as the Idempotency-Key.
export async function sendOrder(api: WhatsApp, pn: string, message: SendMessage, reference: string): Promise<Outcome> {
  return send(api, pn, message, reference);
}

// Create a payment configuration (scope payments). The OAuth link links
// whatever gateway account opens it: hand it to the merchant only. A
// repeat under the same key answers without it (oauth_url null).
export async function createConfiguration(api: WhatsApp, wabaId: string, configuration: CreatePaymentConfiguration, reference: string) {
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/payment-configurations", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": reference } },
    body: configuration,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}

// Read an order's payment rather than trust the webhook alone.
export async function payment(api: WhatsApp, pn: string, configuration: string, referenceId: string) {
  const { data, error } = await api.GET("/v1/numbers/{pn}/payments/{configuration_name}/{reference_id}", {
    params: { path: { pn, configuration_name: configuration, reference_id: referenceId } },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data.data;
}

// Refund under a key derived from your own record: a timeout may have
// refunded (may_have_been_sent): repeat with the SAME key, or look the
// payment up; never refund again under a new key.
export async function refund(api: WhatsApp, pn: string, request: PaymentRefundRequest, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/payments/refunds", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: request,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}
