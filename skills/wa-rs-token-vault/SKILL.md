---
name: wa-rs-token-vault
description: "Keeping merchants' WhatsApp business tokens with wa-rs's TokenVault - AES-256-GCM encryption at rest on any KvStore, the vault key (VaultKey, VaultKeys, key ids, custody), rotating keys (with_previous, rotate), finding the token for a webhook's phone number id (get_by_phone_number), turning it into a per-merchant client with with_token, expiry, offboarding, and why store() must only get ids Meta verified. Load when code needs to act as a merchant, route a webhook to a merchant's token, rotate or configure the vault key, or disconnect a merchant."
---

# wa-rs-token-vault

> **Verified against wa-rs e4e98e5259e3552aa293cbb7c0d3ec93d20e7991 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/vault.rs](examples/vault.rs), compiled and
tested by wa-rs's own gate (routing, rotation, wrong key, offboarding).

## When to use

After Embedded Signup put a merchant's token in the vault
(`wa-rs-embedded-signup`): every time code acts as that merchant — a
reply from the inbox, a template send, a profile change — and when
operating the vault key. Type: `wa_rs::client::embedded_signup::TokenVault`.

## Open it

```rust
let key = VaultKey::from_base64("2026-09", key_base64)?; // 32 random bytes; the id is stored with each record
TokenVault::new(kv, VaultKeys::new(key))
```

The key is 32 random bytes (e.g. `openssl rand -base64 32`) from your
secret manager, **not** from the database behind the `KvStore`. Key ids
are 1–64 characters of `A-Z a-z 0-9 - _ . :`. `VaultKey::generate` cannot
export its bytes: tests only. Lose the key and every merchant onboards
again. The store must be shared and persistent (Postgres, or Redis with
persistence and `noeviction`): it holds every merchant's token.

## Act as the merchant who owns a number

```rust
let Some(stored) = vault.get_by_phone_number(phone_number_id).await? else {
    return Ok(Err(NoMerchant::NotConnected));
};
if stored.is_expired(OffsetDateTime::now_utc()) {
    return Ok(Err(NoMerchant::Reconnect));
}
Ok(Ok(platform.with_token(stored.token))) // shares the transport and pool
```

Webhooks carry the phone number id, so `get_by_phone_number` is the usual
lookup; `vault.get(&waba_id)` works by WABA. A stale or forged index
entry yields `None`: a token is returned only if the WABA's authenticated
record lists the number.

## Rotate the key

```rust
let vault = TokenVault::new(kv, VaultKeys::new(new_key).with_previous(old_key))?;
for waba_id in my_wabas {
    vault.rotate(waba_id).await?; // re-encrypt under the new key; false if already done
}
Ok(vault) // once every WABA is rotated, drop the old key from the config
```

Reads also re-encrypt old records under the active key (`rotate_on_read`,
default `true`; turn it off on read-only replicas). The vault cannot list
its records: iterate **your** merchant table.

## Offboard

`vault.delete(&waba_id)` removes the token and unlinks its numbers.
Re-onboarding a WABA replaces its record. A Solution Partner offboards
with `EmbeddedSignup::offboard` instead, which revokes the credit line
before it deletes; `delete` leaves the credit ledger (`vault.credit`,
`vault.revoked_business`) that revocation needs once the token is gone,
and `rotate` re-seals it with the token.

## Pitfalls

- **Ownership before the vault.** The vault holds every merchant's token
  and knows no tenants: check that the calling tenant owns the phone
  number (your table) before `get_by_phone_number` (`wa-rs-cms-inbox`
  shows the order).
- **`store` trusts its input.** Every phone number id in a
  `StoredBusinessToken` is routed to that WABA. Let `onboard` write
  records (it stores only numbers Meta lists on the WABA); if you call
  `store` yourself, do the same, or one merchant's customers reach another
  merchant.
- Records are bound to their WABA id and key id (associated data): a
  record copied under another WABA, or read with other key bytes under
  the same key id, fails with `Error::Crypto` (`CryptoError::Decrypt`); one
  whose key id is not configured, with `CryptoError::InvalidKey`. Neither
  leaks a token.
- `Debug` of `StoredBusinessToken` never shows the token; never log
  `token.expose_secret()`.
- One `Client` per process, `with_token` per request: never build a
  client per merchant.

## What wa-rs does not do

- No token refresh: `expires_at` records Meta's expiry when there is one;
  an `ErrorKind::Authentication` (190) on a merchant's calls means running
  Embedded Signup again
  ([open question 8](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants)).
- No key custody or rotation schedule (open question 9), and no tenant
  mapping: one WABA shared by two of your tenants is your product's call
  (open question 6).
- It does not protect against someone holding both the key and the store.

## Related skills

`wa-rs-embedded-signup` (how records get there), `wa-rs-setup`
(`with_token`), `wa-rs-cms-inbox`, `wa-rs-storage`, `wa-rs-production`
(secrets).
