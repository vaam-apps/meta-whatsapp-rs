//! Reference code for the `meta-whatsapp-rs-token-vault` skill: merchants' business
//! tokens encrypted at rest, routed by phone number id, rotated under a new
//! key, and turned into per-merchant clients.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use std::sync::Arc;

use time::OffsetDateTime;
use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, TokenVault, VaultKey, VaultKeys};
use meta_whatsapp_rs::core::ids::BusinessId;
use meta_whatsapp_rs::prelude::*;

/// At startup: the key comes from your secret manager, never from the
/// database that holds the `KvStore`.
pub fn open_vault(kv: Arc<dyn KvStore>, key_base64: &str) -> meta_whatsapp_rs::Result<TokenVault> {
    let key = VaultKey::from_base64("2026-09", key_base64)?; // 32 random bytes; the id is stored with each record
    TokenVault::new(kv, VaultKeys::new(key))
}

/// Rotation: the new key encrypts, the old one still decrypts. A record
/// that cannot be read does not stop the walk: it is collected, and the
/// old key stays configured until nothing is left in the list.
pub async fn rotate_all(
    kv: Arc<dyn KvStore>,
    new_key: VaultKey,
    old_key: VaultKey,
    every_waba_ever: &[WabaId], // from YOUR merchant table, offboarded WABAs included
    revoked_by_business: &[BusinessId], // Solution Partner: businesses revoked by id alone
) -> meta_whatsapp_rs::Result<(TokenVault, Vec<(String, Error)>)> {
    let vault = TokenVault::new(kv, VaultKeys::new(new_key).with_previous(old_key))?;
    let mut failed = Vec::new(); // fix or delete these records, then walk again
    for waba_id in every_waba_ever {
        if let Err(e) = vault.rotate(waba_id).await {
            failed.push((waba_id.to_string(), e)); // the rest of this WABA was still rotated
        }
    }
    for business_id in revoked_by_business {
        if let Err(e) = vault.rotate_business(business_id).await {
            failed.push((business_id.to_string(), e)); // a revocation marker no WABA names
        }
    }
    Ok((vault, failed)) // drop the old key from the config only once `failed` is empty
}

/// Why no merchant client could be made.
#[derive(Debug, PartialEq, Eq)]
pub enum NoMerchant {
    NotConnected, // nobody onboarded this number (or the index is stale)
    Reconnect,    // the token expired: run Embedded Signup again
}

/// The client that acts as the merchant owning `phone_number_id`. Check
/// that the calling tenant owns the number BEFORE this: the vault holds
/// every merchant's token and knows no tenants.
pub async fn merchant_client(
    platform: &Client, // built once, without a token
    vault: &TokenVault,
    phone_number_id: &PhoneNumberId, // e.g. `event.phone_number_id()` of a webhook
) -> meta_whatsapp_rs::Result<Result<Client, NoMerchant>> {
    let Some(stored) = vault.get_by_phone_number(phone_number_id).await? else {
        return Ok(Err(NoMerchant::NotConnected));
    };
    if stored.is_expired(OffsetDateTime::now_utc()) {
        return Ok(Err(NoMerchant::Reconnect));
    }
    Ok(Ok(platform.with_token(stored.token))) // shares the transport and pool
}

/// Offboarding: removes the token and unlinks its numbers.
pub async fn disconnect(vault: &TokenVault, waba_id: &WabaId) -> meta_whatsapp_rs::Result<bool> {
    vault.delete(waba_id).await
}

/// Writing a record yourself trusts every phone number id in it: only do it
/// with ids Meta listed on that WABA (onboarding does this for you).
pub async fn store_verified(
    vault: &TokenVault,
    waba_id: WabaId,
    token: AccessToken,
    numbers_meta_listed: Vec<PhoneNumberId>,
) -> meta_whatsapp_rs::Result<()> {
    let record = StoredBusinessToken::new(waba_id, token).phone_number_ids(numbers_meta_listed);
    vault.store(&record).await
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::core::testing::ScriptedTransport;

    use super::*;

    const KEY_2026: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // 32 bytes, test only

    #[tokio::test]
    async fn routes_by_phone_number_and_rotates() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let vault = open_vault(kv.clone(), KEY_2026).unwrap();
        store_verified(
            &vault,
            "102290129340398".into(),
            AccessToken::new("MERCHANT"),
            vec!["106540352242922".into()],
        )
        .await
        .unwrap();

        let platform = Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap();
        let merchant = merchant_client(&platform, &vault, &"106540352242922".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            merchant.token().map(AccessToken::expose_secret),
            Some("MERCHANT")
        );
        let unknown = merchant_client(&platform, &vault, &"999".into())
            .await
            .unwrap();
        assert_eq!(unknown.unwrap_err(), NoMerchant::NotConnected);

        // A record under a key already dropped: it cannot be rotated, and
        // does not stop the walk.
        let lost = TokenVault::new(
            kv.clone(),
            VaultKeys::new(VaultKey::generate("2025-01").unwrap()),
        )
        .unwrap();
        store_verified(&lost, "LOST".into(), AccessToken::new("OLD"), vec![])
            .await
            .unwrap();

        // A new key: old records still open, and rotate re-encrypts them.
        let old = VaultKey::from_base64("2026-09", KEY_2026).unwrap();
        let new = VaultKey::generate("2027-01").unwrap(); // test only: its bytes cannot be exported
        let (rotated, failed) = rotate_all(
            kv,
            new,
            old,
            &["LOST".into(), "102290129340398".into()],
            &["UNKNOWN_BUSINESS".into()],
        )
        .await
        .unwrap();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!(failed[0].0, "LOST");
        assert!(matches!(failed[0].1, Error::Crypto(_)), "{:?}", failed[0].1);
        let again = rotated
            .get(&"102290129340398".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.token.expose_secret(), "MERCHANT");
        assert!(!rotated.rotate(&"102290129340398".into()).await.unwrap()); // already rotated

        assert!(
            disconnect(&rotated, &"102290129340398".into())
                .await
                .unwrap()
        );
        assert!(
            rotated
                .get_by_phone_number(&"106540352242922".into())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_wrong_key_does_not_decrypt() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
        let vault = open_vault(kv.clone(), KEY_2026).unwrap();
        store_verified(&vault, "1".into(), AccessToken::new("T"), vec![])
            .await
            .unwrap();
        let other =
            TokenVault::new(kv, VaultKeys::new(VaultKey::generate("2026-09").unwrap())).unwrap();
        assert!(matches!(
            other.get(&"1".into()).await,
            Err(Error::Crypto(_))
        ));
    }
}
