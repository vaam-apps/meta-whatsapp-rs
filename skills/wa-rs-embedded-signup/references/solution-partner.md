# Solution Partner deployments

> **Verified against wa-rs 8bc676747a09a3c9225a53954030ed7d4eb44adf (2026-09-24).** Also checked against Meta's
> `solution-providers/share-and-revoke-credit-lines`,
> `solution-providers/manage-system-users` and
> `webhooks/reference/account_update` pages as fetched on 2026-09-24.
> Code: [examples/solution_partner.rs](../examples/solution_partner.rs).

A **Solution Partner** pays Meta for its merchants through its own credit
line and invoices them; a **Tech Provider**'s merchants add their own
payment method. wa-rs supports both, **one per deployment**: configure
`SolutionPartner` once at startup, or leave it out. You are liable to Meta
for every message sent on a shared line, and a line cannot be changed once
attached to a WABA: the rules below exist for that.

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

Take it from your billing records for the merchant, never from the
browser: it sets what Meta charges you. Without one, `onboard` fails
**before** the code is exchanged. The first currency is sealed in the
vault before the first share is posted; a `resume` or a new onboarding of
the WABA naming another is refused. A Tech Provider request with a
currency is refused.

## Gate before anything is shared

```rust
es.onboard_with_approval(request, vault, |verified| {
    let taken = bound_to(&verified.waba_id).is_some_and(|other| other != tenant);
    async move {
        if taken {
            return Err(
                ValidationError::new("waba_id", "connected to another merchant").into(),
            );
        }
        Ok(())
    }
})
.await
```

The approval sees what Meta verified (`VerifiedOnboarding`: WABA, owner
business, numbers) and runs before `store_token`. A refusal is
`Error::Step` `approve`, and nothing is stored, subscribed or shared. Check
tenants here, not after `onboard`: by then the line is attached.

## What `onboard` adds

After `subscribe_app` and before `register_phone` (Meta's order):

| Step | Does | Token |
| --- | --- | --- |
| `assign_system_user` | `POST /{waba}/assigned_users` (share-and-attach only) | your system user's |
| `share_credit_line` | checks, then shares; records the allocation in the vault's credit ledger | yours; the attach of the two-call method uses the merchant's |

`share_credit_line` **checks before it posts**: a share that timed out may
have gone through, and Meta refuses a second attach. It reads your line's
records for the owner business and the allocation it recorded, each with
its `request_status`, and posts nothing when an active one already funds
the WABA. So a failed credit step is fixed with `resume`, like
`register_phone`. Two onboardings of one WABA at once: the second is
refused (`EmbeddedSignup::is_credit_step_busy`) and resumes later.
`Onboarded::allocation_config_id` and `TokenVault::credit` carry the
result.

**A revoked business stays revoked.** After `revoke_credit_line`, or when
Meta reports only `DELETED` records for the business, `onboard` and
`resume` refuse (`EmbeddedSignup::is_credit_line_revoked`). Funding the
merchant again is your decision, per onboarding:

```rust
request.reshare_after_revocation()
```

## When a merchant leaves

```rust
match update.event {
    // Unshared: messaging on the WABA is blocked and Meta recommends
    // revoking at once. Revocation is per business: its other WABAs
    // lose the line too, and funding it again needs an explicit opt-in.
    AccountUpdateEvent::PartnerRemoved => {
        Ok(Some(es.revoke_credit_line(waba_id, owner, vault).await?))
    }
    // The app was removed: revoke first, then forget the token.
    AccountUpdateEvent::PartnerAppUninstalled => {
        Ok(es.offboard(waba_id, owner, vault).await?.credit)
    }
    _ => Ok(None),
}
```

- `waba_id` is `event.waba_id()`: for Meta's PARTNER_* events wa-rs takes
  it from `waba_info.waba_id` (Meta's entry id there is a business
  portfolio).
- `owner` is `waba_info.owner_business_id`. Pass it only from a
  signature-checked delivery. It is used when the vault no longer knows
  the merchant; if it contradicts the owner recorded at onboarding,
  nothing is revoked.
- `revoke_credit_line` marks the business revoked first, then revokes
  every active record naming it plus the recorded allocation, confirming
  each. It reports `revoked` and `already_revoked`, and is safe to repeat.
- `offboard` revokes first and deletes the token second; if revocation
  fails nothing is deleted. The credit ledger outlives the token, so
  `PartnerAppUninstalled` and `PartnerRemoved` end revoked in either
  order.
- A merchant who disconnects in your CMS: unsubscribe with their token
  while it works, then `offboard` (`disconnect` in the example).

When to revoke after `PartnerRemoved` (at once, as Meta recommends, or
later for a coexistence number that may reconnect) is your product call;
a reconnect then needs `reshare_after_revocation`.

## Not settled by Meta's pages

- Whether the two-call method also needs the system user on the WABA.
- Whether re-adding the system user is harmless (wa-rs repeats it on
  `resume`, as `Waba::assign_user` treats it).
- Whether the lookup of a business's shared records lists revoked ones:
  wa-rs reads each record's status rather than assume.
- Whether a business can attach a line shared with it to other WABAs
  itself: reconcile your credit line invoice against the WABAs you
  onboarded.
