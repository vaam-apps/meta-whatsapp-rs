# Commerce, QR codes, analytics and the block list

> **Verified against meta-whatsapp-rs e6cebefa7240233b0411f806092759c506d581fd (2026-11-05).** Source: the service's OpenAPI document (`crates/meta-whatsapp-server/openapi/v1.json`, tags `commerce`, `qr_codes`, `analytics` and `contacts`), `docs/guides/server.md` section "Commerce, QR codes, analytics and the block list" and [examples/commerce.ts](../examples/commerce.ts).

Four scopes, one per family: `commerce`, `qr_codes`, `analytics` and
`contacts`. A key minted before them has none, and `send` grants none:
mint a key for these routes on purpose.

| Route | Does |
| --- | --- |
| `GET /v1/numbers/{pn}/commerce-settings`, `PATCH /v1/numbers/{pn}/commerce-settings` | the cart and the catalog's visibility; `CommerceSettingsUpdate`, `204` |
| `GET /v1/wabas/{waba_id}/catalogs`, `GET /v1/wabas/{waba_id}/catalogs/{catalog_id}`, `GET /v1/wabas/{waba_id}/catalogs/{catalog_id}/products` | the catalog the WABA connects; its products, 1 to 100 a page (`limit`, `cursor`) |
| `POST /v1/numbers/{pn}/qr-codes`, `GET /v1/numbers/{pn}/qr-codes` | create (`CreateQrCode`, `201`); list, 1 to 25 a page |
| `GET /v1/numbers/{pn}/qr-codes/{code}`, `PATCH /v1/numbers/{pn}/qr-codes/{code}`, `DELETE /v1/numbers/{pn}/qr-codes/{code}` | read, change the message, delete |
| `GET /v1/wabas/{waba_id}/analytics/messaging`, `GET /v1/wabas/{waba_id}/analytics/conversations`, `GET /v1/wabas/{waba_id}/analytics/pricing`, `GET /v1/wabas/{waba_id}/analytics/templates` | analytics, `start` and `end` as RFC 3339 times |
| `GET /v1/numbers/{pn}/blocked-users`, `POST /v1/numbers/{pn}/blocked-users`, `POST /v1/numbers/{pn}/blocked-users/unblock` | the block list; block and unblock `BlockUsersRequest` |

Block people, under a reference of your own:

```ts
export async function block(api: WhatsApp, pn: string, request: BlockUsersRequest, reference: string): Promise<BlockUsersResult> {
  const { data, error } = await api.POST("/v1/numbers/{pn}/blocked-users", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: request,
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return data;
}
```

- **A block answers `200` even when Meta refused some people**: read
  `refused`, each with Meta's error code and the service's kind for it
  (`reasons`), never Meta's text, which may quote the person. Neither a
  failed block nor a failed QR code creation carries Meta's text.
- **People are phone numbers with their `+`, or BSUIDs.** A parent BSUID
  is `invalid_request`. Meta blocks only someone who wrote to the number
  in the last 24 hours. Unblocking is a `POST`, as a body on a `DELETE`
  is dropped by proxies. Blocks and unblocks are audited by count, never
  by person.
- **A QR code is the number's, or it does not exist**: the service looks
  for it in the number's own list first, so another number's code is
  `not_found` after one lookup, and nothing is changed. The same for a
  catalog among the WABA's own, and for the templates of template
  analytics.
- **Ranges are bounded**, before Meta is asked: `invalid_request` with
  the `field` at fault. A year at most (90 days for templates), and
  `HALF_HOUR` 31 days. Filters hold 50 values at most, each one of
  Meta's own; `phone_numbers` are digits.

```ts
export async function createQrCode(api: WhatsApp, pn: string, message: string, reference: string) {
  const { data, error } = await api.POST("/v1/numbers/{pn}/qr-codes", {
    params: { path: { pn }, header: { "Idempotency-Key": reference } },
    body: { prefilled_message: message, generate_qr_image: "PNG" },
  });
  if (error) {
    throw new WhatsAppError(error.error);
  }
  return { link: data.deep_link_url, image: data.qr_image_url };
}
```

Not offered yet (roadmap M5g2): a catalog's product sets, one product by
its id, template group analytics, and the two analytics switches.
