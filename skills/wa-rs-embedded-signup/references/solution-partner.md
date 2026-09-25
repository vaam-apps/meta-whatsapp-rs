# Solution Partner deployments

> **Verified against wa-rs e4e98e5259e3552aa293cbb7c0d3ec93d20e7991 (2026-09-25).** Also checked against Meta's
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

## Approve before anything is shared (required)

A Solution Partner deployment refuses plain `onboard` before the code is
exchanged (`CreditError::ApprovalRequired`): onboard with
`onboard_with_approval`, and reserve the WABA for the tenant in **one
atomic write**. A lookup, then a write, lets two tenants onboarding the
same WABA at once both pass.

```rust
es.onboard_with_approval(request, vault, |verified| async move {
    reserve(reservations, &verified.waba_id, tenant).await
})
.await
```

```rust
let key = StoreKey::new("tenant.waba", waba_id.as_str());
let mine = tenant.as_bytes().to_vec();
if reservations
    .put_if_absent(&key, mine.clone(), Expiry::Never)
    .await?
    .is_some()
{
    return Ok(()); // reserved for this tenant just now
}
match reservations.get(&key).await? {
    Some(bound) if bound.value == mine => Ok(()), // this tenant again: a retry, a reconnect
    _ => Err(ValidationError::new("waba_id", "connected to another merchant").into()),
}
```

The approval sees what Meta verified (`VerifiedOnboarding`: WABA, owner
business, numbers) and runs before `store_token`. A refusal is
`Error::Step` `approve`, and nothing is stored, subscribed or shared.
Check tenants here, not after `onboard`: by then the line is attached.
Which tenant may have a WABA is your policy; wa-rs decides none
(`OPEN_QUESTIONS.md` #6).

The approval is recorded in the vault's credit ledger
(`StoredCredit::approved_at`), and `resume` shares only for an approved
WABA: a token stored without one (onboarded in Tech Provider mode before
the deployment switched, or by an older wa-rs) fails `resume` at step
`approve` until you call `resume_with_approval` once.

## What `onboard_with_approval` adds

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
`register_phone`. `Onboarded::allocation_config_id` and
`TokenVault::credit` carry the result. Every refusal is typed:
`err.credit()` is a `CreditError`, with its own `is_retryable()` and
`may_have_been_sent()`:

| `CreditError` | Means | Then |
| --- | --- | --- |
| `Busy` | another onboarding of the WABA holds the step (or this one's lease expired) | retryable: `resume` later |
| `Revoked` | the business's line was revoked (`posted`: a revocation raced this share, which was revoked at once) | `reshare_after_revocation`, your decision |
| `StatusUnknown` | a record's `request_status` is a value Meta does not document | wait, or opt in |
| `OwnerUnknown` | Meta reported no owner business: nothing can be checked or revoked | not shared; ask Meta |
| `Reconcile` | a share posted earlier has no recorded outcome and the WABA is funded by something | check Meta Business Suite before anything else |
| `ApprovalRequired` | plain `onboard`, or `resume` of an unapproved WABA | `onboard_with_approval` / `resume_with_approval` |

**A revoked business stays revoked.** After `revoke_credit_line`, or when
Meta reports only `DELETED` records for the business, `onboard_with_approval`
and `resume` refuse (`EmbeddedSignup::is_credit_line_revoked`). Funding the
merchant again is your decision, per onboarding:

```rust
request.reshare_after_revocation()
```

## When a merchant leaves

Route `account_update` (signature-checked deliveries only) to one
function; what it asks of you comes back as a `PartnerAction`:

```rust
match (&update.event, waba_id) {
    // Only YOUR app: under a Multi-Partner Solution another partner
    // uninstalling its own app is not a reason to revoke your line.
    (AccountUpdateEvent::PartnerAppUninstalled, Some(waba_id)) => {
        let ours = info.and_then(|i| i.partner_app_id.as_ref()) == Some(&es.app().app_id);
        if !ours {
            return Ok(PartnerAction::Ignored);
        }
        Ok(PartnerAction::Offboarded(
            es.offboard(waba_id, owner, vault).await?,
        ))
    }
    // A coexistence number disconnected: hand it to your policy.
    (AccountUpdateEvent::PartnerRemoved, Some(waba_id))
        if update.disconnection_info.is_some() =>
    {
        Ok(PartnerAction::CoexistenceDisconnected {
            waba_id: waba_id.clone(),
            owner: owner.cloned(),
        })
    }
    // Unshared: messaging on the WABA is blocked and Meta recommends
    // revoking at once. Revocation is per business: its other WABAs
    // lose the line too, and funding it again needs an explicit opt-in.
    (AccountUpdateEvent::PartnerRemoved, Some(waba_id)) => Ok(PartnerAction::Revoked(
        es.revoke_credit_line(waba_id, owner, vault).await?,
    )),
    // No WABA named, only its owner: revoke by business.
    (AccountUpdateEvent::PartnerRemoved, None) => match owner {
        Some(owner) => Ok(PartnerAction::Revoked(
            es.revoke_business_credit_line(owner, vault).await?,
        )),
        None => Ok(PartnerAction::Ignored),
    },
    _ => Ok(PartnerAction::Ignored),
}
```

- `waba_id` is `event.waba_id()`: for Meta's PARTNER_* events wa-rs takes
  it from `waba_info.waba_id` (Meta's entry id there is a business
  portfolio).
- `owner` is `waba_info.owner_business_id`. Pass it only from a
  signature-checked delivery. It is used when the vault no longer knows
  the merchant, and only if your line has records naming it; if it
  contradicts the owner recorded at onboarding, nothing is revoked.
- **`PartnerAppUninstalled` is yours only when `partner_app_id` is your
  app** (`es.app().app_id`): under a Multi-Partner Solution the other
  partners' uninstalls reach you too.
- **A coexistence `PartnerRemoved`** (with `disconnection_info`: the
  number changed device or was deleted, and may reconnect) is a policy
  point: revoke at once, or keep funding for a grace period and revoke
  later if it does not reconnect. wa-rs does not decide it, and neither
  does the example: it returns `PartnerAction::CoexistenceDisconnected`,
  and `on_coexistence_disconnect` shows both options.

```rust
match policy {
    CoexistencePolicy::RevokeNow => {
        Ok(Some(es.revoke_credit_line(waba_id, owner, vault).await?))
    }
    CoexistencePolicy::GracePeriod => Ok(None), // your scheduler calls revoke_credit_line later
}
```

- `revoke_credit_line` marks the business revoked first (a share posted
  meanwhile then revokes itself), then revokes every active record naming
  it plus the recorded allocation, confirming each. It reports `revoked`
  and `already_revoked`, and is safe to repeat. One that stops part-way
  is `CreditError::RevocationIncomplete`, carrying the same report plus
  what failed, what Meta has not confirmed yet and what names no
  business: repeat it when `is_retryable()`, otherwise check those
  records in Meta Business Suite.
- `offboard` revokes first and deletes the token second; if revocation
  fails nothing is deleted. The credit ledger outlives the token, so
  `PartnerAppUninstalled` and `PartnerRemoved` end revoked in either
  order. When the ledger shows a share that revocation cannot find,
  `offboard` keeps the token (`CreditError::Reconcile`); when nothing was
  ever shared, it just deletes.
- A merchant who disconnects in your CMS: unsubscribe with their token
  while it works, then `offboard` (`disconnect` in the example).

## Key rotation covers the ledger

The credit ledger is sealed with the vault keys and re-sealed on read.
Before dropping an old key, call `vault.rotate(&waba_id)` for **every WABA
you ever onboarded**, offboarded ones included (their ledger outlives the
token), and `vault.rotate_business(&business_id)` for each business you
revoked by business id alone. A record still under a dropped key fails
with `CryptoError::InvalidKey`, and onboarding, `resume` and revocation of
that merchant with it.

## Not settled by Meta's pages

- Whether the two-call method also needs the system user on the WABA.
- Whether re-adding the system user is harmless (wa-rs repeats it on
  `resume`, as `Waba::assign_user` treats it).
- Whether the lookup of a business's shared records lists revoked ones:
  wa-rs reads each record's status rather than assume.
- Which `request_status` values exist besides `DELETED`: any other is
  treated as unknown (`StatusUnknown`), never as active.
- Whether a business can attach a line shared with it to other WABAs
  itself: reconcile your credit line invoice against the WABAs you
  onboarded.
