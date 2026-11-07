//! The Flow key records of the vault (roadmap M5d2).

use std::sync::Arc;

use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_core::store::KvStore;

use super::*;
use crate::embedded_signup::{VaultKey, VaultKeys};

const PKCS8: &str = include_str!("../../flows/endpoint/testdata/test_rsa2048_pkcs8.pem");
const PKCS1: &str = include_str!("../../flows/endpoint/testdata/test_rsa2048_pkcs1.pem");
const SMALL: &str = include_str!("../../flows/endpoint/testdata/test_rsa1024_pkcs8.pem");
const ENCRYPTED: &str =
    include_str!("../../flows/endpoint/testdata/test_rsa2048_pkcs8_encrypted.pem");

fn kv() -> Arc<dyn KvStore> {
    Arc::new(MemoryKvStore::new())
}

fn vault_key(id: &str, byte: u8) -> VaultKey {
    VaultKey::new(id, SecretBytes::new([byte; 32])).unwrap()
}

fn vault(kv: &Arc<dyn KvStore>) -> TokenVault {
    TokenVault::new(Arc::clone(kv), VaultKeys::new(vault_key("k1", 1))).unwrap()
}

fn waba(id: &str) -> WabaId {
    WabaId::new(id).unwrap()
}

fn pem(text: &str) -> SecretBytes {
    SecretBytes::new(text.as_bytes().to_vec())
}

async fn raw(kv: &Arc<dyn KvStore>, waba_id: &WabaId) -> Option<Vec<u8>> {
    kv.get(&flow_key_of(waba_id))
        .await
        .unwrap()
        .map(|v| v.value)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The key's public half: what tells two keys apart.
fn public_of(key: &FlowEndpointKey) -> String {
    key.public_key_pem().unwrap()
}

fn expected(text: &str) -> String {
    public_of(&FlowEndpointKey::from_pem(text).unwrap())
}

/// The first 40 base64 characters of a PEM's body.
fn body_of(text: &str) -> String {
    let body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
    body[..40].to_owned()
}

#[tokio::test]
async fn a_stored_key_comes_back_for_its_owner_and_is_encrypted_at_rest() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    assert!(vault.flow_key(&w, "tenant-a").await.unwrap().is_none());
    vault
        .store_flow_key(&w, "tenant-a", &pem(PKCS8))
        .await
        .unwrap();
    let back = vault.flow_key(&w, "tenant-a").await.unwrap().unwrap();
    assert_eq!(public_of(&back), expected(PKCS8));
    let stored = raw(&kv, &w).await.unwrap();
    for needle in [
        b"tenant-a".as_slice(),
        b"PRIVATE KEY",
        b"private_key_pem",
        body_of(PKCS8).as_bytes(),
    ] {
        assert!(!contains(&stored, needle), "{needle:?}");
    }
}

#[tokio::test]
async fn the_pkcs1_form_is_stored_too() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    vault.store_flow_key(&w, "t", &pem(PKCS1)).await.unwrap();
    let back = vault.flow_key(&w, "t").await.unwrap().unwrap();
    assert_eq!(public_of(&back), expected(PKCS1));
}

#[tokio::test]
async fn another_owner_gets_no_key() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    vault
        .store_flow_key(&w, "tenant-a", &pem(PKCS8))
        .await
        .unwrap();
    assert!(vault.flow_key(&w, "tenant-b").await.unwrap().is_none());
    assert!(vault.holds_flow_key(&w).await.unwrap());
    // The new holder's key replaces it; the old owner then has none.
    vault
        .store_flow_key(&w, "tenant-b", &pem(PKCS1))
        .await
        .unwrap();
    assert!(vault.flow_key(&w, "tenant-a").await.unwrap().is_none());
    assert!(vault.flow_key(&w, "tenant-b").await.unwrap().is_some());
}

#[tokio::test]
async fn a_replaced_key_is_the_new_one_and_a_delete_removes_it() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    vault.store_flow_key(&w, "t", &pem(PKCS8)).await.unwrap();
    vault.store_flow_key(&w, "t", &pem(PKCS1)).await.unwrap();
    let now = vault.flow_key(&w, "t").await.unwrap().unwrap();
    assert_eq!(public_of(&now), expected(PKCS1));
    assert!(vault.delete_flow_key(&w).await.unwrap());
    assert!(!vault.delete_flow_key(&w).await.unwrap());
    assert!(vault.flow_key(&w, "t").await.unwrap().is_none());
    assert!(!vault.holds_flow_key(&w).await.unwrap());
}

#[tokio::test]
async fn a_key_that_cannot_decrypt_a_flow_request_is_refused_before_a_write() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    for bad in [SMALL, ENCRYPTED, "not a key", ""] {
        let error = vault.store_flow_key(&w, "t", &pem(bad)).await.unwrap_err();
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains("MII"), "{shown}");
        assert!(raw(&kv, &w).await.is_none());
    }
    let not_text = SecretBytes::new(vec![0xff, 0xfe]);
    assert!(vault.store_flow_key(&w, "t", &not_text).await.is_err());
    for owner in [String::new(), "x".repeat(MAX_FLOW_KEY_OWNER_CHARS + 1)] {
        assert!(vault.store_flow_key(&w, &owner, &pem(PKCS8)).await.is_err());
    }
    assert!(raw(&kv, &w).await.is_none());
}

#[tokio::test]
async fn a_record_copied_to_another_waba_does_not_open() {
    let kv = kv();
    let vault = vault(&kv);
    let (a, b) = (waba("100"), waba("200"));
    vault.store_flow_key(&a, "t", &pem(PKCS8)).await.unwrap();
    let stored = raw(&kv, &a).await.unwrap();
    kv.put(&flow_key_of(&b), stored, Expiry::Never)
        .await
        .unwrap();
    assert!(vault.flow_key(&b, "t").await.is_err());
    assert!(vault.flow_key(&a, "t").await.unwrap().is_some());
}

#[tokio::test]
async fn the_vault_keys_rotate_it_on_read_and_on_demand() {
    let w = waba("100");
    let rotated = |kv: &Arc<dyn KvStore>| {
        TokenVault::new(
            Arc::clone(kv),
            VaultKeys::new(vault_key("new", 2)).with_previous(vault_key("old", 1)),
        )
        .unwrap()
    };
    // On demand, for a WABA with no token.
    let kv = kv();
    let old = TokenVault::new(Arc::clone(&kv), VaultKeys::new(vault_key("old", 1))).unwrap();
    old.store_flow_key(&w, "t", &pem(PKCS8)).await.unwrap();
    let before = raw(&kv, &w).await.unwrap();
    let vault = rotated(&kv).rotate_on_read(false);
    assert!(vault.flow_key(&w, "t").await.unwrap().is_some());
    assert_eq!(raw(&kv, &w).await.unwrap(), before, "a read-only replica");
    assert!(vault.rotate(&w).await.unwrap());
    assert_ne!(raw(&kv, &w).await.unwrap(), before);
    assert!(!vault.rotate(&w).await.unwrap(), "already current");
    assert!(old.flow_key(&w, "t").await.is_err(), "the old key left");
    assert!(
        TokenVault::new(Arc::clone(&kv), VaultKeys::new(vault_key("new", 2)))
            .unwrap()
            .flow_key(&w, "t")
            .await
            .unwrap()
            .is_some()
    );
    // On read.
    let kv = self::kv();
    let old = TokenVault::new(Arc::clone(&kv), VaultKeys::new(vault_key("old", 1))).unwrap();
    old.store_flow_key(&w, "t", &pem(PKCS8)).await.unwrap();
    let before = raw(&kv, &w).await.unwrap();
    assert!(rotated(&kv).flow_key(&w, "t").await.unwrap().is_some());
    assert_ne!(raw(&kv, &w).await.unwrap(), before);
}

#[tokio::test]
async fn the_tokens_deletion_leaves_the_key() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    vault.store_flow_key(&w, "t", &pem(PKCS8)).await.unwrap();
    assert!(!vault.delete(&w).await.unwrap(), "no token to delete");
    assert!(vault.flow_key(&w, "t").await.unwrap().is_some());
}

#[tokio::test]
async fn the_key_is_in_no_debug_or_error_text() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    vault.store_flow_key(&w, "t", &pem(PKCS8)).await.unwrap();
    let key = vault.flow_key(&w, "t").await.unwrap().unwrap();
    let shown = format!("{key:?} {vault:?} {:?}", pem(PKCS8));
    assert!(!shown.contains(&body_of(PKCS8)), "{shown}");
    // A damaged record is an error without detail.
    kv.put(&flow_key_of(&w), b"{}".to_vec(), Expiry::Never)
        .await
        .unwrap();
    let error = vault.flow_key(&w, "t").await.unwrap_err();
    assert!(!format!("{error} {error:?}").contains(&body_of(PKCS8)));
}

/// The record's name and layout are stable identifiers (docs/architecture.md,
/// § Stable identifiers): a record written today must open after any
/// rename. The literals spell their `w` as `\x77` so a search-and-replace
/// cannot rewrite them with the code.
#[tokio::test]
async fn the_store_key_and_the_sealed_layout_are_pinned() {
    let kv = kv();
    let vault = vault(&kv);
    let w = waba("100");
    vault
        .store_flow_key(&w, "tenant-a", &pem(PKCS8))
        .await
        .unwrap();
    let key = flow_key_of(&w);
    assert_eq!(key.namespace(), "\x77a.token");
    assert_eq!(key.key(), "flow-key/100");
    let stored = kv.get(&key).await.unwrap().unwrap();
    let record: serde_json::Value = serde_json::from_slice(&stored.value).unwrap();
    let mut fields: Vec<&str> = record
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(fields, ["ciphertext", "kid", "nonce", "v"]);
    let blob: SealedBlob = serde_json::from_value(record).unwrap();
    let plain = vault.open_blob(&key, &blob).unwrap();
    let plain: serde_json::Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(plain["owner"], "tenant-a");
    assert_eq!(plain["private_key_pem"], PKCS8);
    assert_eq!(plain.as_object().unwrap().len(), 2);
}
