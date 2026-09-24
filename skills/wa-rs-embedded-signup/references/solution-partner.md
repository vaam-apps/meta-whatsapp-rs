# Solution Partner deployments

> **Verified against wa-rs 8bc676747a09a3c9225a53954030ed7d4eb44adf (2026-09-24).** Also checked against Meta's
> `solution-providers/share-and-revoke-credit-lines` and
> `solution-providers/manage-system-users` pages as fetched on 2026-09-24.
> Code: [examples/solution_partner.rs](../examples/solution_partner.rs).

A **Solution Partner** pays Meta for its merchants through its own credit
line and invoices them; a **Tech Provider**'s merchants add their own
payment method. wa-rs supports both, **one per deployment**: configure
`SolutionPartner` once at startup, or leave it out.

## On Meta's side

- Solution Partner status and a credit line; its id from
  `CreditLines::list` (your system user token, once).
- A system user with the business_management permission, and Admin or
  Financial Editor on your portfolio; its token and its id.
- Under a Multi-Partner Solution without `WabaTask::Messaging`,
  `WabaTask::Manage` is refused on the merchant's WABA: pass granular
  tasks including `WabaTask::ManageBilling` with
  `SolutionPartner::system_user_tasks`.

## Configure once

```rust
let Some(p) = partner else { return Ok(es) }; // Tech Provider: merchants add a payment method
let mut sp = SolutionPartner::new(p.system_token, p.system_user_id, p.credit_line_id)
    .method(CreditSharing::ShareAndAttach); // Meta's current method (the default)
if let Some(code) = p.default_currency {
    sp = sp.default_currency(code.parse::<WabaCurrency>()?); // only the six Meta lists
}
Ok(es.solution_partner(sp))
```

`CreditSharing::ShareThenAttach` is Meta's newer two-call method (share
with the verified owner business, then attach with the merchant's token).

## Per merchant: the currency

```rust
Ok(match merchant_currency {
    Some(code) => request.currency(code.parse::<WabaCurrency>()?),
    None => request, // SolutionPartner::default_currency, or a validation error
})
```

Without either, `onboard` fails **before** the code is exchanged. It must
match the merchant's billing: a credit line cannot change once attached
(a new WABA is the only way). A Tech Provider request with a currency is
refused.

## What `onboard` adds

After `subscribe_app` and before `register_phone` (Meta's order):

| Step | Does | Token |
| --- | --- | --- |
| `assign_system_user` | `POST /{waba}/assigned_users` (share-and-attach only) | your system user's |
| `share_credit_line` | checks, then shares; stores the allocation id with the token | yours; the attach of the two-call method uses the merchant's |

`share_credit_line` **checks before it posts**: a share that timed out may
have gone through, and Meta refuses a second attach. So a failed credit
step is fixed with `resume`, like `register_phone`; it posts nothing when
the line already funds the WABA. `Onboarded::allocation_config_id` and
`StoredBusinessToken::allocation_config_id` carry the result.

## When a merchant removes you

```rust
if update.event != AccountUpdateEvent::PartnerRemoved {
    return Ok(Vec::new());
}
// The merchant's WABA is in waba_info: Meta's PARTNER_* examples carry
// a business id, not the WABA, as the entry id (`event.waba_id()`).
let Some(waba_id) = update.waba_info.as_ref().and_then(|i| i.waba_id.as_ref()) else {
    return Ok(Vec::new());
};
es.revoke_credit_line(waba_id, vault).await // every WABA of that business
```

Messaging on that WABA is blocked and its owner can no longer be read;
Meta recommends revoking at once. `revoke_credit_line` uses the owner
business stored at onboarding, and revokes for **every** WABA of that
business. It leaves the vault entry: `TokenVault::delete` it if the
merchant is gone.

## Not settled by Meta's pages

- Whether the two-call method also needs the system user on the WABA.
- Whether re-adding the system user is harmless (wa-rs repeats it on
  `resume`, as `Waba::assign_user` treats it).
- Whether the lookup of a business's shared records lists revoked ones.
