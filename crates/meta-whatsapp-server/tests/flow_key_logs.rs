//! Roadmap M5d2's log half (design D374): captured at `TRACE` for every
//! target, setting, reading, replacing, deleting and refusing a Flow key,
//! and the data endpoint reading it (and failing to), log no part of the
//! private key. The setting and the deletion are audited, with the tenant,
//! the key's public id and the WABA, and nothing of the key. Alone in its
//! binary, as `logs.rs` is: the capture is a thread's default subscriber.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::flows::{self, KEY_PEM, OTHER_KEY_PEM, Stub, kat_body, signed};
use common::{Call, Harness, send};
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey};
use meta_whatsapp_rs::webhooks::axum::http::Method;
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use serde_json::json;

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";

/// Decisive: every line of `flow_key`'s services, the extractors and the
/// data endpoint's key read.
#[tokio::test]
async fn a_flow_key_is_in_no_log_line_and_its_changes_are_audited() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let stub = Stub::summary();
    let h = Harness::with_settings(Settings {
        flows: Some(flows::deployment_with(Some(KEY_PEM), true, stub)),
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], "TOKEN").await;
    let key = h.tenant_key(TENANT, &[Scope::Flows]).await;
    let path = format!("/v1/wabas/{WABA}/flow-key");
    let put = |pem: &str| {
        Call::new(Method::PUT, path.clone())
            .key(&key)
            .json(&json!({"private_key": pem}))
    };

    for pem in [KEY_PEM, OTHER_KEY_PEM] {
        assert_eq!(h.call(put(pem)).await.status.as_u16(), 200);
    }
    assert_eq!(
        h.call(put("not-a-key-SENTINEL-91c4")).await.status.as_u16(),
        422
    );
    assert_eq!(
        h.call(Call::get(path.clone()).key(&key))
            .await
            .status
            .as_u16(),
        200
    );
    // The data endpoint reads the tenant's key: decrypts nothing (the
    // request is for the other key), and then an unreadable record.
    let refused = send(&h.public, signed(WABA, &kat_body("ping")).build()).await;
    assert_eq!(refused.status.as_u16(), 421, "{}", refused.text);
    let record = StoreKey::new("wa.token", format!("flow-key/{WABA}"));
    h.kv.put(&record, b"{\"damaged\":true}".to_vec(), Expiry::Never)
        .await
        .unwrap();
    let unreadable = send(&h.public, signed(WABA, &kat_body("ping")).build()).await;
    assert_eq!(unreadable.status.as_u16(), 503, "{}", unreadable.text);
    assert_eq!(
        h.call(Call::new(Method::DELETE, path.clone()).key(&key))
            .await
            .status
            .as_u16(),
        204
    );
    // A second deletion is not audited again.
    h.call(Call::new(Method::DELETE, path).key(&key)).await;

    let logs = captured.text();
    assert!(
        logs.contains("a tenant's Flow key could not be read"),
        "the capture saw the endpoint's warning:\n{logs}"
    );
    for pem in [KEY_PEM, OTHER_KEY_PEM] {
        for line in pem.lines().filter(|l| !l.starts_with("-----")) {
            assert!(
                !logs.contains(line),
                "a line of a Flow private key was logged"
            );
        }
    }
    for text in ["PRIVATE KEY", "SENTINEL-91c4", "private_key"] {
        assert!(!logs.contains(text), "logged: {text}");
    }
    let audited: Vec<serde_json::Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let actions: Vec<&str> = audited
        .iter()
        .filter_map(|f| f["action"].as_str())
        .collect();
    assert_eq!(
        actions,
        ["flow_key_set", "flow_key_set", "flow_key_deleted"],
        "{audited:?}"
    );
    let key_id = key.split('_').nth(1).unwrap();
    for fields in &audited {
        assert_eq!(fields["tenant"], TENANT, "{fields}");
        assert_eq!(fields["waba_id"], WABA, "{fields}");
        assert_eq!(fields["key_id"], key_id, "{fields}");
        assert_eq!(
            fields.as_object().unwrap().len(),
            5,
            "no other field: {fields}"
        );
    }
}
