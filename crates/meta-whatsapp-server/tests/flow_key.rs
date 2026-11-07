//! Roadmap M5d2 (design D370 to D374): a Flow key per tenant. A tenant
//! sets the private key its WABA's Flows are decrypted with
//! (`PUT /v1/wabas/{waba_id}/flow-key`); the data endpoint
//! (`POST /webhooks/flows/{waba_id}`) reads the tenant of the request's
//! WABA first and that tenant's key only.
//!
//! The decisive tests are the cross-tenant ones: requests encrypted for
//! the library's TEST-ONLY key and for a second one
//! (`tests/flows_data/kat_other_key.mjs`, made with Meta's own sample
//! code) are posted to the WABAs of two tenants holding one key each.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;

use common::flows::{self, KEY_PEM, OTHER_KEY_PEM, Stub, kat, kat_body, signed};
use common::{Call, Harness, send};
use meta_whatsapp_rs::client::flows::endpoint::FlowEndpointKey;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::idempotency::Fingerprint;
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const WABA_A: &str = "102290129340398";
const WABA_B: &str = "202290129340399";
const PN_A: &str = "106540352242922";
const PN_B: &str = "206540352242923";

/// What a service is built with: the deployment's key, and whether a
/// tenant without a key of its own may use it.
struct Rig {
    h: Harness,
    stub: Arc<Stub>,
    key_a: String,
    key_b: String,
}

async fn rig(deployment_key: Option<&str>, fallback: bool) -> Rig {
    let stub = Stub::summary();
    let h = Harness::with_settings(Settings {
        flows: Some(flows::deployment_with(
            deployment_key,
            fallback,
            stub.clone(),
        )),
        flows_deployment_key_fallback: fallback,
        ..common::test_settings()
    });
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-A").await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-B").await;
    let scopes = [Scope::Flows, Scope::Numbers];
    let key_a = h.tenant_key(A, &scopes).await;
    let key_b = h.tenant_key(B, &scopes).await;
    Rig {
        h,
        stub,
        key_a,
        key_b,
    }
}

fn put(waba: &str, key: &str, pem: &str) -> Call {
    Call::new(Method::PUT, format!("/v1/wabas/{waba}/flow-key"))
        .key(key)
        .json(&json!({"private_key": pem}))
}

fn get(waba: &str, key: &str) -> Call {
    Call::get(format!("/v1/wabas/{waba}/flow-key")).key(key)
}

fn delete(waba: &str, key: &str) -> Call {
    Call::new(Method::DELETE, format!("/v1/wabas/{waba}/flow-key")).key(key)
}

impl Rig {
    async fn set(&self, tenant: &str, pem: &str) -> Value {
        let (waba, key) = if tenant == A {
            (WABA_A, &self.key_a)
        } else {
            (WABA_B, &self.key_b)
        };
        let reply = self.h.call(put(waba, key, pem)).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        reply.json()
    }

    /// Meta's request `body` for `waba`, signed, answered.
    async fn meta(&self, waba: &str, body: &[u8]) -> common::Reply {
        send(&self.h.public, signed(waba, body).build()).await
    }
}

fn public_of(pem: &str) -> String {
    FlowEndpointKey::from_pem(pem)
        .unwrap()
        .public_key_pem()
        .unwrap()
}

fn body_lines(pem: &str) -> Vec<&str> {
    pem.lines().filter(|l| !l.starts_with("-----")).collect()
}

/// A tenant sets its WABA's key, reads its public half, replaces it and
/// deletes it; Meta is never called; the private key is in no answer.
/// Decisive: the route's three handlers and the vault record behind them.
#[tokio::test]
async fn a_tenant_sets_reads_replaces_and_deletes_its_key() {
    let r = rig(None, false).await;
    let none = r.h.call(get(WABA_A, &r.key_a)).await;
    assert_eq!(none.status, StatusCode::OK, "{}", none.text);
    assert_eq!(
        none.json(),
        json!({"configured": false, "public_key": null, "public_key_sha256": null})
    );

    let set = r.set(A, KEY_PEM).await;
    let public = public_of(KEY_PEM);
    assert_eq!(set["configured"], true);
    assert_eq!(set["public_key"], public.as_str());
    assert_eq!(
        set["public_key_sha256"],
        hex::encode(Sha256::digest(public.as_bytes())).as_str()
    );
    let read = r.h.call(get(WABA_A, &r.key_a)).await;
    assert_eq!(read.json(), set);
    for reply in [&read.text, &set.to_string()] {
        for line in body_lines(KEY_PEM) {
            assert!(!reply.contains(line), "the private key was answered");
        }
        assert!(!reply.contains("PRIVATE KEY"));
    }

    // A rotation: the new key replaces the old.
    let rotated = r.set(A, OTHER_KEY_PEM).await;
    assert_eq!(rotated["public_key"], public_of(OTHER_KEY_PEM).as_str());
    assert_ne!(rotated["public_key_sha256"], set["public_key_sha256"]);

    // B sees nothing of A's.
    let b = r.h.call(get(WABA_B, &r.key_b)).await;
    assert_eq!(b.json()["configured"], false);

    let deleted = r.h.call(delete(WABA_A, &r.key_a)).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let again = r.h.call(delete(WABA_A, &r.key_a)).await;
    assert_eq!(again.status, StatusCode::NO_CONTENT, "{}", again.text);
    let gone = r.h.call(get(WABA_A, &r.key_a)).await;
    assert_eq!(gone.json()["configured"], false);
    assert!(r.h.graph.requests().is_empty(), "Meta is never called");
}

/// **The decisive cross-tenant test.** Each tenant holds its own key and
/// there is no deployment key: a request encrypted for one tenant's key
/// is answered for that tenant's WABA and is `421` (undecryptable, one
/// answer for every cause) for the other's, whichever way round, and the
/// integrating app is never asked on the refused one. Delete the tenant
/// lookup in `key_for` and the first assertion of the second half fails.
#[tokio::test]
async fn one_tenants_key_never_decrypts_another_tenants_request() {
    let r = rig(None, false).await;
    r.set(A, KEY_PEM).await;
    r.set(B, OTHER_KEY_PEM).await;

    // A's request, for A's WABA: answered with what Meta's code seals.
    let ping = kat("ping");
    let ok = r.meta(WABA_A, &kat_body("ping")).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    assert_eq!(ok.text, ping["sealed_response"].as_str().unwrap());
    // B's, for B's.
    let other = flows::kat_other("ping");
    let ok = r.meta(WABA_B, &flows::kat_other_body("ping")).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    assert_eq!(ok.text, other["sealed_response"].as_str().unwrap());

    // Crossed: not decrypted, and the app not asked.
    for (waba, body, name) in [
        (WABA_B, kat_body("data_exchange"), "A's request, B's WABA"),
        (
            WABA_A,
            flows::kat_other_body("data_exchange"),
            "B's request, A's WABA",
        ),
    ] {
        let refused = r.meta(waba, &body).await;
        assert_eq!(refused.status.as_u16(), 421, "{name}: {}", refused.text);
    }
    assert_eq!(r.stub.calls(), 0, "the app was asked a crossed request");

    // And each tenant's own data exchange reaches the app with its own
    // tenant.
    let a = r.meta(WABA_A, &kat_body("data_exchange")).await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    let b = r
        .meta(WABA_B, &flows::kat_other_body("data_exchange"))
        .await;
    assert_eq!(b.status, StatusCode::OK, "{}", b.text);
    let asked: Vec<(String, String)> = r
        .stub
        .exchanges()
        .into_iter()
        .map(|e| (e.tenant, e.waba_id))
        .collect();
    assert_eq!(
        asked,
        [
            (A.to_owned(), WABA_A.to_owned()),
            (B.to_owned(), WABA_B.to_owned())
        ]
    );
}

/// A tenant that set no key is answered as the endpoint answers a WABA
/// with no key (`404`), whoever else has one: even when another tenant's
/// key is the very key its request was encrypted for. Decisive: the key
/// is found by the request's tenant, not by what decrypts.
#[tokio::test]
async fn a_tenant_with_no_key_is_not_found_whatever_another_tenant_set() {
    let r = rig(None, false).await;
    // B holds the key A's requests are encrypted for.
    r.set(B, KEY_PEM).await;
    let refused = r.meta(WABA_A, &kat_body("data_exchange")).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
    assert_eq!(r.stub.calls(), 0);
    // The same request for B's WABA is B's, and answered.
    let b = r.meta(WABA_B, &kat_body("data_exchange")).await;
    assert_eq!(b.status, StatusCode::OK, "{}", b.text);
    assert_eq!(r.stub.exchanges()[0].tenant, B);
}

/// The deployment's key is a fallback for a tenant with no key of its
/// own, only where the deployment allows it; a tenant with its own key is
/// never decrypted with the deployment's. Decisive: the order in
/// `TenantFirst::key_for` and the setting.
#[tokio::test]
async fn the_deployment_key_is_a_fallback_only_if_the_deployment_allows_it() {
    // Allowed (the default): A has none, the deployment's key decrypts.
    let r = rig(Some(KEY_PEM), true).await;
    let via_deployment = r.meta(WABA_A, &kat_body("ping")).await;
    assert_eq!(via_deployment.status, StatusCode::OK);
    // B sets its own: the deployment's key (what the request was
    // encrypted for) is no longer tried for B.
    r.set(B, OTHER_KEY_PEM).await;
    let own_only = r.meta(WABA_B, &kat_body("data_exchange")).await;
    assert_eq!(own_only.status.as_u16(), 421, "{}", own_only.text);
    assert_eq!(r.stub.calls(), 0);
    // B deletes it: the deployment's key serves B again.
    let deleted = r.h.call(delete(WABA_B, &r.key_b)).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let back = r.meta(WABA_B, &kat_body("ping")).await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.text);

    // Denied: the same deployment key serves nobody without a key of
    // their own.
    let r = rig(Some(KEY_PEM), false).await;
    let refused = r.meta(WABA_A, &kat_body("ping")).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
    r.set(A, KEY_PEM).await;
    let own = r.meta(WABA_A, &kat_body("ping")).await;
    assert_eq!(own.status, StatusCode::OK, "{}", own.text);
}

/// A rotation takes effect on the next request: the old public key's
/// requests are `421`, the new one's answered.
#[tokio::test]
async fn a_rotated_key_decrypts_the_next_request() {
    let r = rig(None, false).await;
    r.set(A, KEY_PEM).await;
    assert_eq!(
        r.meta(WABA_A, &kat_body("ping")).await.status,
        StatusCode::OK
    );
    r.set(A, OTHER_KEY_PEM).await;
    assert_eq!(r.meta(WABA_A, &kat_body("ping")).await.status.as_u16(), 421);
    assert_eq!(
        r.meta(WABA_A, &flows::kat_other_body("ping")).await.status,
        StatusCode::OK
    );
}

/// A record the service cannot open is never "no key": a request is `503`
/// (the records are unavailable), not `404` and not decrypted with the
/// deployment's key, which would be another key than the tenant's.
#[tokio::test]
async fn an_unreadable_record_is_unavailable_never_the_deployments_key() {
    let r = rig(Some(KEY_PEM), true).await;
    let record = StoreKey::new("wa.token", format!("flow-key/{WABA_A}"));
    r.h.kv
        .put(&record, b"{\"damaged\":true}".to_vec(), Expiry::Never)
        .await
        .unwrap();
    let refused = r.meta(WABA_A, &kat_body("ping")).await;
    assert_eq!(
        refused.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        refused.text
    );
    assert_eq!(r.stub.calls(), 0);
    // B is unaffected.
    assert_eq!(
        r.meta(WABA_B, &kat_body("ping")).await.status,
        StatusCode::OK
    );
}

/// A WABA that changes hands never serves its previous holder's key to the
/// new one: B reads none and its requests are `404`, whether or not the
/// record was deleted. Decisive: the owner sealed in the vault record.
#[tokio::test]
async fn a_waba_that_changed_hands_does_not_serve_the_previous_holders_key() {
    let r = rig(None, false).await;
    r.set(A, KEY_PEM).await;
    let waba = WabaId::new(WABA_A).unwrap();
    // Unbound and bound to B (an operator's move), the record untouched.
    r.h.store.unbind_waba(&waba).await.unwrap();
    r.h.connect(B, WABA_A, &[PN_A], "TOKEN-B2").await;
    assert!(r.h.vault.holds_flow_key(&waba).await.unwrap());
    let read = r.h.call(get(WABA_A, &r.key_b)).await;
    assert_eq!(read.json()["configured"], false, "{}", read.text);
    let refused = r.meta(WABA_A, &kat_body("data_exchange")).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
    assert_eq!(r.stub.calls(), 0);
}

/// Disconnecting a WABA deletes the key its tenant set (best effort), as
/// it deletes its token.
#[tokio::test]
async fn disconnecting_a_waba_deletes_its_flow_key() {
    let r = rig(None, false).await;
    r.set(A, KEY_PEM).await;
    let waba = WabaId::new(WABA_A).unwrap();
    assert!(r.h.vault.holds_flow_key(&waba).await.unwrap());
    r.h.graph.push_json(200, json!({"success": true}));
    let done =
        r.h.call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA_A}")).key(&r.key_a))
            .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.text);
    assert!(!r.h.vault.holds_flow_key(&waba).await.unwrap());
    // B's is untouched.
    r.set(B, OTHER_KEY_PEM).await;
    assert!(
        r.h.vault
            .holds_flow_key(&WabaId::new(WABA_B).unwrap())
            .await
            .unwrap()
    );
}

/// An operator's unbinding of a WABA deletes the key its tenant set too,
/// and leaves another WABA's alone. Decisive: `Tokens::delete`, the path
/// of `forget_for_admin` (the tenant's own disconnection takes
/// `delete_if_unchanged`); the token is deleted first, as that path is
/// the one for a WABA whose token cannot be used.
#[tokio::test]
async fn an_operators_unbind_deletes_the_flow_key() {
    let r = rig(None, false).await;
    r.set(A, KEY_PEM).await;
    r.set(B, OTHER_KEY_PEM).await;
    let admin = r.h.admin_key().await;
    // The WABA's token is gone (the library's delete leaves the Flow
    // key), so the operator's unbind takes the path without a usable
    // token: `forget_for_admin`.
    let waba_a = WabaId::new(WABA_A).unwrap();
    assert!(r.h.vault.delete(&waba_a).await.unwrap());
    assert!(r.h.vault.holds_flow_key(&waba_a).await.unwrap());
    let unbound = r
        .h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA_A}/binding")).key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
    let waba_b = WabaId::new(WABA_B).unwrap();
    assert!(!r.h.vault.holds_flow_key(&waba_a).await.unwrap());
    assert!(r.h.vault.holds_flow_key(&waba_b).await.unwrap());
}

/// What cannot decrypt a Flow request is refused `422` on `private_key`
/// before anything is stored, without quoting it.
#[tokio::test]
async fn a_key_that_cannot_decrypt_a_flow_request_is_refused_without_being_quoted() {
    const DIR: &str = "../meta-whatsapp-client/src/flows/endpoint/testdata/";
    let read = |name: &str| std::fs::read_to_string(format!("{DIR}{name}")).unwrap();
    let r = rig(None, false).await;
    for (name, bad) in [
        ("an encrypted PEM", read("test_rsa2048_pkcs8_encrypted.pem")),
        ("a 1024-bit key", read("test_rsa1024_pkcs8.pem")),
        ("an EC key", read("test_ec_p256_pkcs8.pem")),
        ("the public half", read("test_rsa2048_public.pem")),
        ("text", "not-a-key-SENTINEL-7f3a91".to_owned()),
        ("empty", String::new()),
        ("too long", format!("{KEY_PEM}{}", "A".repeat(20_000))),
    ] {
        let reply = r.h.call(put(WABA_A, &r.key_a, &bad)).await;
        assert_eq!(reply.status.as_u16(), 422, "{name}: {}", reply.text);
        assert_eq!(reply.code(), "invalid_request", "{name}");
        assert_eq!(reply.json()["error"]["field"], "private_key", "{name}");
        assert!(!reply.text.contains("SENTINEL-7f3a91"), "{name}");
        for line in body_lines(&bad).into_iter().filter(|l| l.len() > 20) {
            assert!(!reply.text.contains(line), "{name}: quoted the key");
        }
    }
    // A key under another name, an unknown field, no body.
    for body in [
        json!({"key": KEY_PEM}),
        json!({"private_key": KEY_PEM, "extra": 1}),
        json!({"private_key": 7}),
        json!({}),
    ] {
        let reply =
            r.h.call(
                Call::new(Method::PUT, format!("/v1/wabas/{WABA_A}/flow-key"))
                    .key(&r.key_a)
                    .json(&body),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 422, "{body}: {}", reply.text);
        for line in body_lines(KEY_PEM) {
            assert!(!reply.text.contains(line), "{body}: quoted the key");
        }
    }
    let waba = WabaId::new(WABA_A).unwrap();
    assert!(!r.h.vault.holds_flow_key(&waba).await.unwrap());
    assert!(r.h.graph.requests().is_empty());
}

/// A malformed body that carries the private key (cut off, of the wrong
/// shape, a duplicated or unknown field named after one of its lines,
/// not UTF-8, the wrong content type, over the body limit) is refused
/// without a byte of the key in the answer, and nothing is stored.
/// Decisive: `ApiJson` answering with the service's sentence instead of
/// serde's or axum's, which quote what they cannot parse.
#[tokio::test]
async fn a_malformed_body_carrying_the_key_never_echoes_it() {
    let r = rig(None, false).await;
    let line = body_lines(KEY_PEM)[1].to_owned();
    let quoted = serde_json::to_string(KEY_PEM).unwrap();
    let mut non_utf8 = format!("{{\"private_key\": {quoted}, \"x\": \"").into_bytes();
    non_utf8.extend_from_slice(&[0xff, 0xfe]);
    let padding = "A".repeat(70 * 1024);
    let bodies: Vec<(&str, Vec<u8>, &str, u16)> = vec![
        (
            "cut off",
            format!("{{\"private_key\": {}", &quoted[..quoted.len() / 2]).into_bytes(),
            "application/json",
            422,
        ),
        (
            "an array of two",
            format!("[{quoted}, {quoted}]").into_bytes(),
            "application/json",
            422,
        ),
        (
            "a bare string",
            quoted.clone().into_bytes(),
            "application/json",
            422,
        ),
        (
            "a nested object",
            format!("{{\"private_key\": {{\"k\": {quoted}}}}}").into_bytes(),
            "application/json",
            422,
        ),
        (
            "an array value",
            format!("{{\"private_key\": [{quoted}]}}").into_bytes(),
            "application/json",
            422,
        ),
        (
            "a field named after a line",
            format!("{{\"private_key\": {quoted}, \"{line}\": 1}}").into_bytes(),
            "application/json",
            422,
        ),
        (
            "a duplicated field",
            format!("{{\"private_key\": {quoted}, \"private_key\": {quoted}}}").into_bytes(),
            "application/json",
            422,
        ),
        ("not UTF-8", non_utf8, "application/json", 422),
        (
            "the wrong content type",
            format!("{{\"private_key\": {quoted}}}").into_bytes(),
            "text/plain",
            422,
        ),
        (
            "the key as the body, unwrapped",
            KEY_PEM.as_bytes().to_vec(),
            "application/json",
            422,
        ),
        (
            "over the body limit",
            format!("{{\"private_key\": {quoted}, \"pad\": \"{padding}\"}}").into_bytes(),
            "application/json",
            413,
        ),
    ];
    for (name, body, content_type, status) in bodies {
        let reply =
            r.h.call(
                Call::new(Method::PUT, format!("/v1/wabas/{WABA_A}/flow-key"))
                    .key(&r.key_a)
                    .header("content-type", content_type)
                    .body(meta_whatsapp_rs::webhooks::axum::body::Body::from(body)),
            )
            .await;
        assert_eq!(reply.status.as_u16(), status, "{name}: {}", reply.text);
        for line in body_lines(KEY_PEM) {
            assert!(!reply.text.contains(line), "{name}: quoted the key");
        }
        assert!(!reply.text.contains("PRIVATE KEY"), "{name}");
    }
    let waba = WabaId::new(WABA_A).unwrap();
    assert!(!r.h.vault.holds_flow_key(&waba).await.unwrap());
}

/// Another tenant's WABA, or a key without the scope, is refused before
/// the vault is read or written: no read of `wa.token`, no write at all.
/// Decisive: the owned-WABA extractor, ahead of every vault call.
#[tokio::test]
async fn a_refused_flow_key_call_touches_no_vault_record() {
    let r = rig(None, false).await;
    r.set(B, OTHER_KEY_PEM).await;
    let without = r.h.tenant_key(A, &[Scope::Numbers]).await;
    let reads = r.h.kv.vault_reads();
    let writes = r.h.kv.written().len();
    for call in [
        put(WABA_B, &r.key_a, KEY_PEM),
        get(WABA_B, &r.key_a),
        delete(WABA_B, &r.key_a),
        put(WABA_A, &without, KEY_PEM),
        get(WABA_A, &without),
        delete(WABA_A, &without),
        // A's key, B's WABA, under an idempotency key.
        put(WABA_B, &r.key_a, KEY_PEM).header("idempotency-key", "x-1"),
    ] {
        let reply = r.h.call(call).await;
        assert!(matches!(reply.status.as_u16(), 403 | 404), "{}", reply.text);
        for line in body_lines(OTHER_KEY_PEM) {
            assert!(!reply.text.contains(line));
        }
    }
    assert_eq!(r.h.kv.vault_reads(), reads, "the vault was read");
    assert_eq!(r.h.kv.written().len(), writes, "something was written");
    // And B's own key is still B's.
    let read = r.h.call(get(WABA_B, &r.key_b)).await;
    assert_eq!(read.json()["public_key"], public_of(OTHER_KEY_PEM).as_str());
}

/// The scope and the ownership: a key without `flows` is `403`, another
/// tenant's WABA is `404`, and nothing is stored or read for either.
#[tokio::test]
async fn the_routes_need_the_flows_scope_and_the_callers_own_waba() {
    let r = rig(None, false).await;
    let without = r.h.tenant_key(A, &[Scope::Numbers, Scope::Templates]).await;
    for call in [
        put(WABA_A, &without, KEY_PEM),
        get(WABA_A, &without),
        delete(WABA_A, &without),
    ] {
        let reply = r.h.call(call).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
        assert_eq!(reply.code(), "forbidden");
    }
    for call in [
        put(WABA_B, &r.key_a, KEY_PEM),
        get(WABA_B, &r.key_a),
        delete(WABA_B, &r.key_a),
    ] {
        let reply = r.h.call(call).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    }
    let waba_b = WabaId::new(WABA_B).unwrap();
    let waba_a = WabaId::new(WABA_A).unwrap();
    assert!(!r.h.vault.holds_flow_key(&waba_b).await.unwrap());
    assert!(!r.h.vault.holds_flow_key(&waba_a).await.unwrap());
    // B's key is not deleted by A's call either.
    r.set(B, KEY_PEM).await;
    let denied = r.h.call(delete(WABA_B, &r.key_a)).await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    assert!(r.h.vault.holds_flow_key(&waba_b).await.unwrap());
}

/// An `Idempotency-Key` on the setting: the same key and the same private
/// key replay the kept answer; another key under it is
/// `idempotency_key_reused`. The record holds the WABA and the public
/// half's digest, never the private key.
#[tokio::test]
async fn the_setting_takes_an_idempotency_key_over_the_public_half() {
    let r = rig(None, false).await;
    let first =
        r.h.call(put(WABA_A, &r.key_a, KEY_PEM).header("idempotency-key", "set-1"))
            .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert!(first.headers.get("idempotent-replayed").is_none());
    let replay =
        r.h.call(put(WABA_A, &r.key_a, KEY_PEM).header("idempotency-key", "set-1"))
            .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.text, first.text);
    let reused =
        r.h.call(put(WABA_A, &r.key_a, OTHER_KEY_PEM).header("idempotency-key", "set-1"))
            .await;
    assert_eq!(reused.status.as_u16(), 422, "{}", reused.text);
    assert_eq!(reused.code(), "idempotency_key_reused");
    // The other key was not stored.
    let read = r.h.call(get(WABA_A, &r.key_a)).await;
    assert_eq!(read.json()["public_key"], public_of(KEY_PEM).as_str());
    // A rejected key never claims the key.
    let bad =
        r.h.call(put(WABA_A, &r.key_a, "nope").header("idempotency-key", "set-2"))
            .await;
    assert_eq!(bad.status.as_u16(), 422);
    let fine =
        r.h.call(put(WABA_A, &r.key_a, OTHER_KEY_PEM).header("idempotency-key", "set-2"))
            .await;
    assert_eq!(fine.status, StatusCode::OK, "{}", fine.text);
}

/// The same key in its other encoding (PKCS#1, not PKCS#8) under the same
/// `Idempotency-Key` is the same request: the fingerprint is over the
/// public half, so it is not a function of the private key's text.
/// Decisive: delete the public digest from the fingerprint (hash the
/// private text instead) and the repeat is `idempotency_key_reused`.
#[tokio::test]
async fn the_fingerprint_is_not_a_function_of_the_private_keys_text() {
    const PKCS1: &str = include_str!(
        "../../meta-whatsapp-client/src/flows/endpoint/testdata/test_rsa2048_pkcs1.pem"
    );
    assert_ne!(PKCS1, KEY_PEM);
    let r = rig(None, false).await;
    let first =
        r.h.call(put(WABA_A, &r.key_a, KEY_PEM).header("idempotency-key", "enc-1"))
            .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let again =
        r.h.call(put(WABA_A, &r.key_a, PKCS1).header("idempotency-key", "enc-1"))
            .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
}

/// The fingerprint holds the WABA and the public half's digest and
/// nothing else: it is the same for the same pair, differs by either,
/// and no function of the private key is an input.
#[test]
fn the_fingerprint_is_over_the_waba_and_the_public_digest() {
    let a = WabaId::new(WABA_A).unwrap();
    let b = WabaId::new(WABA_B).unwrap();
    let digest = |pem: &str| hex::encode(Sha256::digest(public_of(pem).as_bytes()));
    let f = |waba: &WabaId, pem: &str| *Fingerprint::set_flow_key(waba, &digest(pem)).as_bytes();
    assert_eq!(f(&a, KEY_PEM), f(&a, KEY_PEM));
    assert_ne!(f(&a, KEY_PEM), f(&b, KEY_PEM));
    assert_ne!(f(&a, KEY_PEM), f(&a, OTHER_KEY_PEM));
    // It is not the digest of the private key's text, nor of its body.
    let private_digest = Sha256::digest(KEY_PEM.as_bytes());
    assert_ne!(f(&a, KEY_PEM).as_slice(), private_digest.as_slice());
}
