# Payments

> **Verified against meta-whatsapp-rs 351285a9902a25316a76f8141b5681bbb24a570d (2026-11-02).** Source: the service's OpenAPI document (`crates/meta-whatsapp-server/openapi/v1.json`, tag `payments`), `docs/guides/server.md` section "Payments" and [examples/payments.ts](../examples/payments.ts).

India's WhatsApp Payments and Brazil's payment messages. The messages
are sends (scope `send`); the configurations, a payment's lookup and its
refund take the scope `payments`, which a key minted before it lacks.

| Route | Does |
| --- | --- |
| `POST /v1/numbers/{pn}/messages` | `interactive` of type `order_details` or `order_status` |
| `GET /v1/wabas/{waba_id}/payment-configurations`, `POST /v1/wabas/{waba_id}/payment-configurations` | list; create (`201` with the gateway's OAuth link) |
| `GET /v1/wabas/{waba_id}/payment-configurations/{configuration_name}`, `DELETE /v1/wabas/{waba_id}/payment-configurations/{configuration_name}` | one; remove it |
| `POST /v1/wabas/{waba_id}/payment-configurations/{configuration_name}/oauth-link` | a new OAuth link |
| `GET /v1/numbers/{pn}/payments/{configuration_name}/{reference_id}` | an order's payment, attempts and refunds |
| `POST /v1/numbers/{pn}/payments/refunds` | refund an order paid through a gateway |

An order's `parameters` are Meta's object, as the payments page of your
integration writes it (UPI intent, payment link or gateway in India; Pix,
payment link, Boleto or one-click in Brazil):

```ts
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
```

- The service checks every rule of the order's currency first (`INR`
  India's, `BRL` Brazil's: the total, the offsets, the `reference_id`)
  and refuses a key it would not pass on: `invalid_request` with `field`,
  nothing sent. An `order_status` names no currency, so India's own
  refusals (a pending status) are Meta's.
- **The OAuth link is a credential**: hand it to the merchant only (its
  answers tell every cache not to store them). A repeat of a creation under
  its `Idempotency-Key` answers `oauth_url` null: the service never
  stores the link; ask for a new one.
- **A refund requires an `Idempotency-Key`**: without one it is
  `idempotency_key_required` (428), nothing refunded. It is never made
  twice under one key. On a `504` with `may_have_been_sent` true, repeat
  with the SAME key or read the payment; never refund under a new key:

```ts
export async function refund(api: WhatsApp, pn: string, request: PaymentRefundRequest, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/payments/refunds", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: request,
  });
```

- A lookup or a refund naming a configuration your number's WABA does
  not have is `not_found` (404), nothing read or refunded.
- Until the WABA accepted Meta's WhatsApp Payments terms, Meta refuses
  (`permission`, a send `payment`, 403) and `error.graph.details`
  carries the link the merchant opens. The service accepts nothing.
- The answers come as events: `payment_configuration_updated`, a status
  of type `payment` in `status_updated`, Brazil's payment method reply
  in `message_received`.
