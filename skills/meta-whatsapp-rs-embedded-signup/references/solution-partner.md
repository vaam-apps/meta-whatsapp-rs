# Solution Partner deployments

> **Verified against wa-rs 0e63aba8378556b4e34cf4cd5b5392f18f2a5e00 (2026-09-25).** Also checked against Meta's
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

~~Plain `onboard` shared the line, and `resume` shared for any stored
token~~ (until 1a7b5bf): on an older pin, gate with `onboard_with_approval`
yourself.

The approval is recorded in the vault's credit ledger
(`StoredCredit::approved_at`) for the token record onboarding stores
(`StoredCredit::approved_token_created_at`), and `resume` shares only for
a WABA whose stored token record was approved: a token stored without
one (onboarded in Tech Provider mode before the deployment switched, by
an older wa-rs, or stored again since) fails `resume` at step `approve`
until you call `resume_with_approval` once. ~~An approval held for any
later token of the WABA~~ (until e0f7e58).

## What `onboard_with_approval` adds

After `subscribe_app` and before `register_phone` (Meta's order):

| Step | Does | Token |
| --- | --- | --- |
| `assign_system_user` | `POST /{waba}/assigned_users` (share-and-attach only) | your system user's |
| `share_credit_line` | checks, then shares; records the allocation in the vault's credit ledger | yours; the attach of the two-call method uses the merchant's |

`share_credit_line` **checks before it posts**: a share whose answer was
lost may have gone through, and Meta refuses a second attach. It reads
your line's records for the owner business and the allocation it
recorded, each with its `request_status`, and posts nothing when an
active one already funds the WABA. A lost answer (a timeout, a 5xx) is
`CreditError::Reconcile`, **not retryable**: do not retry at once; a later
`resume` checks first, and posts again only when nothing funds the WABA
(Meta does not document that its records show an applied share at once).
~~A timed-out share is returned as the retryable transport error~~
(until e0f7e58). `Onboarded::allocation_config_id` and
`TokenVault::credit` carry the result. Every refusal is typed:
`err.credit()` is a `CreditError`, with its own `is_retryable()` and
`may_have_been_sent()`:

| `CreditError` | Means | Then |
| --- | --- | --- |
| `Busy` | another onboarding of the WABA holds the step (or this one's lease expired); `posted`: the two-call method had shared (and recorded) the line before it lost the lease | retryable: `resume` later (it attaches without sharing again) |
| `Revoked` | the business's line was revoked (`posted`: a revocation raced this share, which was revoked at once or is recorded for the next revocation) | `reshare_after_revocation`, your decision |
| `StatusUnknown` | a record's `request_status` is a value Meta does not document | wait, or opt in |
| `OwnerUnknown` | Meta reported no owner business: nothing can be checked or revoked | not shared; ask Meta |
| `Reconcile` | a share may be live: its answer was lost, a racing revocation could not find it, or a share with no recorded outcome and the WABA funded by something | not retryable; check Meta Business Suite before anything else |
| `AttachFailed` | the two-call method shared (recorded) and the attach was refused | fix the request, then `resume` |
| `ApprovalRequired` | plain `onboard`, or `resume` of an unapproved WABA or token | `onboard_with_approval` / `resume_with_approval` |

`Reconcile`, and a revocation left with records naming no business, are
`ErrorKind::Unknown`: a person has to look.

**A revoked business stays revoked.** After `revoke_credit_line`, or when
Meta reports only `DELETED` records for the business, `onboard_with_approval`
and `resume` refuse (`EmbeddedSignup::is_credit_line_revoked`). Funding the
merchant again is your decision, made on one onboarding but **business-wide
in effect**: a successful re-share clears the business's revocation marker,
so its other WABAs are no longer refused either. Never set it on every
onboarding; gate it (the example's `reconnect` spends a grant):

```rust
request.reshare_after_revocation()
```

## When a merchant leaves

Route `account_update` (signature-checked deliveries only) to one
function; what it asks of you comes back as a `PartnerAction`. First, a
removal from a Multi-Partner Solution you are not in is not yours:

```rust
let partners = info.map(|i| i.solution_partner_business_ids.as_slice());
if partners.is_some_and(|ids| !ids.is_empty() && !ids.contains(our_business)) {
    return Ok(PartnerAction::Ignored);
}
```

Meta sends `solution_partner_business_ids` only under a Multi-Partner
Solution and does not say whose business the entry id is, so nothing else
identifies you. Then:

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
    // Unshared, or a coexistence number disconnected (it may
    // reconnect): revoke at once either way, as Meta recommends.
    // Revocation is per business: its other WABAs lose the line too,
    // and funding it again needs an explicit opt-in (`reconnect`).
    (AccountUpdateEvent::PartnerRemoved, Some(waba_id)) => {
        let revoked = es.revoke_credit_line(waba_id, owner, vault).await?;
        let Some(info) = &update.disconnection_info else {
            return Ok(PartnerAction::Revoked(revoked)); // no reconnect grant
        };
        // Only a disconnection the merchant made earns a reconnect.
        let by_merchant = info.initiated_by == Some(DisconnectionInitiator::User);
        let why = "coexistence disconnection initiated by the merchant";
        let reconnect_granted =
            by_merchant && grant_reconnect(reservations, waba_id, why).await?;
        Ok(PartnerAction::Disconnected {
            revoked,
            reconnect_granted,
        })
    }
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
- **Every `PartnerRemoved` of your solution revokes at once**, a
  coexistence one too (with `disconnection_info`: the number changed
  device, was re-registered or went inactive, and may reconnect). That is
  the owner's decision for wa-rs (2026-09-25), and what Meta recommends
  for any removal. wa-rs itself stays passive: nothing revokes unless
  your handler calls `revoke_credit_line`. The example returns
  `PartnerAction::Disconnected` for a coexistence removal, so you can ask
  the merchant to reconnect.
- **Copied an earlier version of this example?** Its policy point for a
  coexistence disconnection (revoke now or after a grace period) is gone:
  revoke at once on every `PartnerRemoved`, and never pass
  `reshare_after_revocation` on a reconnect without a grant. wa-rs's
  CHANGELOG names the removed items.
- **A merchant who reconnects onboards again**, and the revoked business
  is not funded again on its own (`EmbeddedSignup::is_credit_line_revoked`).
  Because the opt-in clears the business-wide marker, the example never
  hands it out on the merchant's say-so: `grant_reconnect` writes one
  grant for the WABA and the tenant bound to it, only for a disconnection
  the merchant made (`initiated_by: USER`: a new device, a new number),
  and `reconnect`'s approval consumes it atomically before anything is
  stored. An unshared WABA, an offboarding, a `SYSTEM` disconnection
  (inactivity, enforcement) or unpaid invoices get none: funding them
  again is your staff's decision, a `grant_reconnect` by hand. A
  reconnect that fails after its approval has spent the grant; `resume`
  it with the same opt-in.

```rust
let request = fund_again(request);
es.onboard_with_approval(&request, vault, |verified| async move {
    reserve(reservations, &verified.waba_id, tenant).await?;
    if !reservations
        .delete(&reconnect_key(&verified.waba_id, tenant))
        .await?
    {
        let why = "no reconnect grant: funding this business again is your staff's decision";
        return Err(ValidationError::new("waba_id", why).into());
    }
    Ok(())
})
```

- `revoke_credit_line` marks the business revoked first (a share posted
  meanwhile then revokes what it may have made, or reports it:
  `Reconcile`), then revokes every active record naming it plus the
  recorded allocation, confirming each. It reports `revoked` and
  `already_revoked`, and is safe to repeat. One that stops part-way is
  `CreditError::RevocationIncomplete`, carrying the same report plus what
  failed, what Meta has not confirmed yet, what names no business,
  `share_pending` (the WABA's ledger shows a share with no recorded
  outcome and this call revoked no record: it may be live and not listed
  yet) and `ledger` (a ledger write that failed): repeat it when
  `is_retryable()`, otherwise check those records in Meta Business Suite.
- **A pending share Meta never lists** (a post whose answer was lost and
  that never reached Meta) keeps every revocation of the WABA at
  `share_pending`. Once someone has checked the WABA's funding in Meta
  Business Suite, clear it from your admin tool (`clear_lost_share` in the
  example). It posts nothing and holds the WABA's credit lease; it checks
  your line's records for the owner business and the WABA's
  `primary_funding_id` again, and clears nothing while a record may be
  live (`PendingShareClearance::NotCleared`, recording a record that
  funds the WABA as its allocation). A `primary_funding_id` that no
  record explains stops it too: it may be the lost share itself, applied
  while Meta's lookup does not list it yet, and wa-rs cannot tell it from
  the merchant's own card. `SharesFound::unexplained_funding` returns it;
  pass it back as `acknowledged_funding` only once someone has seen in
  Meta Business Suite that what pays for the WABA is not your credit
  line. Otherwise it seals who cleared it, when, and the acknowledged
  funding in the ledger (`StoredCredit::cleared_shares`); revocation and
  `offboard` then behave as if nothing had been posted. It refuses when
  nothing is pending, and needs a merchant token that still works (Meta
  serves `primary_funding_id` to it; after `PartnerRemoved` it may not,
  and a merchant who connects again stores a new one). **Operator-only**:
  never reachable from a merchant's route; `cleared_by` is the operator id
  of your authenticated staff session, not a name or an email (kept,
  sealed, as long as the WABA's credit record; `Debug` redacts it; at
  most `MAX_CLEARED_BY_CHARS` characters, nothing invisible).

```rust
let outcome = es.clear_pending_share(waba_id, admin, confirmed, vault);
Ok(match outcome.await? {
    PendingShareClearance::Cleared(_) => Clearance::Cleared, // vault.credit(waba_id) → cleared_shares
    PendingShareClearance::NotCleared(found) => match found.unexplained_funding() {
        Some(funding) => Clearance::ConfirmFunding(funding.clone()),
        None => Clearance::MayBeLive,
    },
    _ => Clearance::MayBeLive, // nothing cleared
})
```

- `offboard` revokes first and deletes the token second; if revocation
  fails nothing is deleted. The credit ledger outlives the token, so
  `PartnerAppUninstalled` and `PartnerRemoved` end revoked in either
  order. When the ledger shows a share that revocation cannot find,
  `offboard` keeps the token (`CreditError::Reconcile`), and a pending
  share it revoked nothing for keeps it too (`RevocationIncomplete`,
  `share_pending`: call again, or clear it as above); when nothing was
  ever shared, it just deletes. It marks a recorded business revoked
  even then (the owner's decision, 2026-09-25: the marker comes before any
  lookup, so no racing share survives), so funding that business later
  takes `reshare_after_revocation`.
- A merchant who disconnects in your CMS: unsubscribe with their token
  while it works, then `offboard` (`disconnect` in the example).

## Key rotation covers the ledger

The credit ledger is sealed with the vault keys and re-sealed on read.
Before dropping an old key, call `vault.rotate(&waba_id)` for **every WABA
you ever onboarded**, offboarded ones included (their ledger outlives the
token), and `vault.rotate_business(&business_id)` for each business you
revoked by business id alone, collecting failures rather than stopping
at the first (`wa-rs-token-vault`). A record still under a dropped key
fails with `CryptoError::InvalidKey`, and onboarding and `resume` of that
merchant with it. Revocation goes on with what it can read (an
unreadable token or credit record is skipped, an unreadable marker
replaced), but misses the allocation and any pending share recorded in
an unreadable credit record, and returns the `InvalidKey` error when
nothing readable names the business.

## Not settled by Meta's pages

- Whether the two-call method also needs the system user on the WABA.
- Whether re-adding the system user is harmless (wa-rs repeats it on
  `resume`, as `Waba::assign_user` treats it).
- Whether the lookup of a business's shared records lists revoked ones:
  wa-rs reads each record's status rather than assume.
- Which `request_status` values exist besides `DELETED`: any other is
  treated as unknown (`StatusUnknown`), never as active.
- Whether the lookup lists a record, and the WABA's `primary_funding_id`
  shows it, as soon as its share returns. `resume` after a lost answer,
  and a share that raced a revocation and lost its answer, rely on it:
  that is why a lost answer is `Reconcile`, and why a revocation keeps a
  pending share it revoked nothing for incomplete.
- Whether a `PartnerRemoved` about another partner can reach your app
  outside a Multi-Partner Solution (the example filters only on
  `solution_partner_business_ids`).
- Whether a business can attach a line shared with it to other WABAs
  itself: reconcile your credit line invoice against the WABAs you
  onboarded.
