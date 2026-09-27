# Erasure step 4: Meta's contact book

> **Verified against meta-whatsapp-rs be626b4361b1799b3b14026626d31746d04a55aa (2026-09-27).** Source: the rustdoc of `PhoneNumber::delete_contact_book_entry` and `UserId::is_bsuid`, and `docs/guides/production.md` section 8.

Meta keeps each user's phone number with their BSUID in the business
portfolio's contact book, and uses it to put the phone number in the
webhooks of users who hide it behind a username. An erasure deletes the
customer's entries (step 4 in `SKILL.md`, "Erasing a customer"):

```rust
let number = merchant.phone_number(inbox.phone_number_id().clone());
let mut failed = Vec::new();
for id in inbox.identities(&inbox.key(contact)).await? {
    let id = UserId::new(id);
    if !id.is_bsuid() {
        continue; // a contact key, a phone number or a parent BSUID
    }
    if let Err(e) = number.delete_contact_book_entry(&id).await {
        tracing::warn!(kind = ?e.kind(), "contact book entry kept"); // never log the BSUID
        failed.push(id); // the erasure goes on: steps 5 and 6 still run
    }
}
```

- **BSUIDs only.** `Inbox::identities` mixes contact keys, phone
  numbers, BSUIDs and parent BSUIDs. The call refuses anything but a
  standard BSUID with `Error::Validation` before any request, so a loop
  without `UserId::is_bsuid` stops at the first phone number and the
  journal (step 5) never runs. Skip what is not a BSUID; never abort.
- **The merchant's client.** Use `with_token` and their token, on any of
  their numbers: the book belongs to the portfolio the BSUID belongs to,
  and the entry goes for every number of that portfolio, unlike the
  inbox's erasure, which stays on each number.
- **No undo.** Nothing restores an entry. The call answers `true` when
  an entry existed and `false` when there was none, so a repeat (step 6)
  is harmless. A `DELETE` is replayed on transient errors, so `false`
  can also mean a lost first attempt deleted it.
- **What it leaves.** A business number that exchanged a message or a
  call with that phone number in the last 30 days still gets it in its
  webhooks (Meta's rule is per number), and any new interaction records
  the entry again.
