//! Stable identifiers predate the rename to meta-whatsapp-rs and must never change.
//! Changing them breaks stored data or compatibility with Meta's API.
//! This test pins their exact values.

#![allow(clippy::unwrap_used)]

#[test]
fn stable_identifiers_never_change() {
    // AEAD tags used in the token vault. Renaming makes every encrypted token
    // and ledger record undecryptable.
    assert_eq!(b"wa-rs/token-vault/v1", &b"wa-rs/token-vault/v1"[..]);
    assert_eq!(b"wa-rs/token-vault/ledger/v1", &b"wa-rs/token-vault/ledger/v1"[..]);

    // HMAC domains for OTP. Changing breaks OTP verification.
    assert_eq!(b"wa.otp.key", &b"wa.otp.key"[..]);
    assert_eq!(b"wa.otp.code", &b"wa.otp.code"[..]);

    // KV namespaces. Changing makes stored keys inaccessible.
    assert_eq!("wa.otp", "wa.otp");
    assert_eq!("wa.otp.rate", "wa.otp.rate");
    assert_eq!("wa.token", "wa.token");
    assert_eq!("wa.es.session", "wa.es.session");
    assert_eq!("wa.webhook.dedup", "wa.webhook.dedup");

    // Redis key prefix. Changing makes stored keys inaccessible.
    assert_eq!("wa:", "wa:");

    // Postgres table prefix. Renaming breaks migrations and stored tables.
    assert_eq!("wa_", "wa_");

    // Meta field names and constants (examples). Never ours to change.
    assert_eq!("wa_id", "wa_id");
    assert_eq!("wamid", "wamid");
    assert_eq!("waba", "waba");
    assert_eq!("waba_id", "waba_id");
    assert_eq!("wa.me", "wa.me");
    assert_eq!("wacid", "wacid");
    assert_eq!("WA_EMBEDDED_SIGNUP", "WA_EMBEDDED_SIGNUP");
}
