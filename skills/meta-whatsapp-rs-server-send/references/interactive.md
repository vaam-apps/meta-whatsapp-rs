# Flows, calls, carousels, products, pins and Direct Send

> **Verified against meta-whatsapp-rs 0efc244d8e35bc635847c8194efb3cdf86a6dfa8 (2026-11-02).** Source: the service's OpenAPI document (`crates/meta-whatsapp-server/openapi/v1.json`, schemas `SendMessage`, `InteractiveContent`, `InteractiveAction`), `docs/guides/server.md` section "Send messages" and [examples/interactive.ts](../examples/interactive.ts).

Every type below goes through `POST /v1/numbers/{pn}/messages` (scope
`send`) and the inbox reply (scope `inbox`), written as Meta's page for
it writes it, `card_index` and the action's `name` included.

| `type` | `interactive.type` | Meta's page |
| --- | --- | --- |
| `pin` | | groups/groups-messaging: to `group_id`, `pin` with `expiration_days` (1 to 30) or `unpin` |
| `interactive` | `flow` | flows/guides/sendingaflow: `flow_name`, or `flow_id` of your WABA |
| `interactive` | `voice_call`, `call_permission_request` | calling/call-button-messages-deep-links, calling/user-call-permissions (body only) |
| `interactive` | `carousel` | 2 to 10 media cards (an https image or video `link` each) or product cards |
| `interactive` | `product`, `product_list`, `catalog_message` | catalogs/single-product-messages, multi-product-messages, catalog-messages |
| `interactive` | `location_request_message`, `address_message`, `request_contact_info` | the location and address request pages (address: India), business-scoped-user-ids |

```ts
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
```

- **Nothing is dropped**: a key the service would not pass on to Meta (a
  `footer` on a carousel, a misspelt Flow parameter, a card's media by
  `id`, an unpin's `expiration_days`) is `invalid_request` on its
  `field`, nothing sent; so is a `card_index` out of place or a wrong
  `name`.
- **Your WABA's objects only**: a `flow_id` or a `catalog_id` is checked
  first with your number's token; another WABA's, or one Meta refuses, is
  `not_found`, nothing sent (a check Meta failed has
  `may_have_been_sent` false: repeat under the same key). A `flow_name`
  is found by Meta in your number's WABA.
- **The `flow_token`** is never logged; it comes back in the Flow's
  answer (a `message_received` event).
- **The 24-hour window**: every type here is free-form. Outside the
  window, send the template with that button (a Flow, call permission,
  call or catalog template) instead; an inbox reply refuses it first,
  `customer_service_window_closed`, asking Meta nothing.

## Direct Send

For accounts Meta made eligible: `category` (`utility`, `authentication`
or `service`), `ttl_seconds` and `direct_send_config` ride on any type.
`utility` and `authentication` go as templates Meta generates, outside
the window too (an inbox reply as well); `service` is a normal service
message.

```ts
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
```

Checked first (`invalid_request` with `field`): `ttl_seconds` only with
`utility` (30 to 43,200) or `authentication` (30 to 900);
`template_name` with `utility` only, lowercase letters, digits and `_`;
no `authentication` message to a `user_id` alone (`to`); 1,024
characters of text. Another category is `invalid_request` on `body`.
Whether your account is eligible, and which formats Direct Send takes,
is Meta's answer (`invalid_parameter`).
