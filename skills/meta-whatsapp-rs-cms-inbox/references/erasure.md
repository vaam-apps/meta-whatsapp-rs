# Erasing a customer from the inbox

> **Verified against meta-whatsapp-rs 8a157bb10a1677660c84555537cf35246da9562e (2026-09-27).** Source: the rustdoc of `ConversationStore::erase_all` and `ConversationStore::identities`, `docs/guides/cms-inbox.md` section 8 and `docs/guides/production.md` section 8.

A customer is stored under several keys on one number: a history thread
under their phone number, live messages under their BSUID, an earlier
BSUID after a number change. `erase` reaches one key; erase the person.

## The procedure, per request

1. **Collect every identity**: `Inbox::identities(&key)` on each of the
   merchant's numbers (the closure over the synced address book, where a
   contact's key, BSUID, parent BSUID and phone number are one person,
   and the identity links), plus the ones you hold yourself (the phone
   number the customer gave you, a BSUID in your CRM).
2. **`Inbox::erase_all(&ids)` on each of those numbers**, behind your
   ownership check of the number: an `Inbox` is bound to one number, and
   `Inbox::erase` and `Inbox::identities` refuse another number's key
   before the store is called. A raw `ConversationStore::erase_all`
   trusts the number it is given.
3. **Delete your own copies**: media you downloaded, exports, the
   service's outbox rows (roadmap M2f) and your dead letters (L21a).
4. **Delete the customer from Meta's contact book**:
   `PhoneNumber::delete_contact_book_entry(&bsuid)` for each of their
   BSUIDs, with the merchant's client (`with_token`) on any number of the
   portfolio the BSUID belongs to. The book is the portfolio's, so the
   entry goes for every number of it. The identities of step 1 also hold
   phone numbers, contact keys and parent BSUIDs, which the call refuses
   (`Error::Validation`, before any request): keep the BSUIDs with
   `UserId::is_bsuid`, skip the rest, and never abort the procedure on
   one (the `meta-whatsapp-rs-production` skill's example). It cannot be
   undone, and the library never calls it for you; a repeat answers
   `false`. A number that exchanged a message or call with the customer
   in the last 30 days still gets their phone number in its webhooks.
5. **Journal the erasure**, outside the database you back up: the time
   and an HMAC (a key of your own) of `phone_number_id|contact` for each
   identity. After any restore, HMAC the restored keys and erase the
   ones the journal lists.
6. **Erase again after 7 days**: Meta redelivers for up to 7 days, and
   what arrives after the erasure is recorded as any new event.

Keep the webhook dedup markers (`wa.webhook.dedup`): while one exists,
Meta's redelivery of an event already delivered is dropped instead of
recording the customer again. Never purge them to complete an erasure.

## What `erase_all` does

In one step: deletes every record keyed by the ids (messages of every
origin and revoke tombstones, summaries, window events, thread
ownership), the synced contacts naming them on any of their four ids,
the contact removals kept under them and the identity links naming
them. Their messages in a group (matched by sender: the BSUID, else the
phone number, of an inbound message) are redacted in place by default
(kind `erased`, no text, `{}` as payload, no sender), or deleted with
`with_erasure_mode(ErasureMode::Delete)` on the store; the group's
preview never keeps their text. It never crosses numbers: a `wa_id` is
the same on every number.

## What no erasure reaches

- The customer quoted or shared in someone else's message (a reply's
  `context`, a contact card); a number-change `system` message under the
  old key names the new one (erase both).
- Identities nothing connects, and a recycled phone number connecting
  two people: check what `identities` returns.
- Under `ErasureMode::Redact`, the ids of their group messages (a
  `wamid` encodes the sender's phone number); in either mode, the
  tombstone of a revoke of theirs that reached a group before its
  message (its id alone).
- On Postgres: dead rows until VACUUM, the WAL, replicas, CDC consumers,
  backups, statement logs (`log_parameter_max_length = 0` for the role).
- The webhook dedup markers and OTP challenges (hashed, expiring), the
  service's outbox and idempotency answers, SSE clients, your copies,
  logs, and Meta's side (the contact book unless step 4 deletes the
  entry, the WhatsApp Business app under coexistence).
- Records created after the erasure: a new message, an echo, a history
  chunk or address book sync not delivered yet, a late revoke (its
  tombstone holds the BSUID). An erased tombstone frees its message id,
  so the revoked message, arriving later in a history chunk, is stored
  with its content: step 6 deletes it.
