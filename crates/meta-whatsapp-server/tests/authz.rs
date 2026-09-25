//! The authorization order (docs/design/server.md, section 3.3) and
//! acceptance test M1.3: every `{pn}` and `{waba_id}` route of the
//! committed OpenAPI document answers tenant B's key on tenant A's number
//! or WABA with `404`, and the vault is never read.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use common::{ALL_SCOPES, Call, Harness};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{
    AllowedTenants, KeyScope, NumberStatus, Scope, TenantId, TenantStatus,
};
use serde_json::{Value, json};

const A: &str = "tenant-a";
const B: &str = "tenant-b";
const WABA_A: &str = "102290129340398";
const PN_A: &str = "106540352242922";
const WABA_B: &str = "102290129340399";
const PN_B: &str = "106540352242923";

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// Two tenants, each with a WABA, a number and a token in the vault.
async fn two_tenants() -> Harness {
    let h = Harness::new();
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-OF-A").await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-OF-B").await;
    h
}

/// Every operation in the committed document whose path names a number
/// or a WABA.
fn owned_routes() -> Vec<common::Operation> {
    common::spec_operations()
        .into_iter()
        .filter(|o| o.template.contains("{pn}") || o.template.contains("{waba_id}"))
        .collect()
}

/// The values of A's number and WABA.
fn on_a() -> common::Sample {
    common::Sample {
        tenant: A.to_owned(),
        waba: WABA_A.to_owned(),
        pn: PN_A.to_owned(),
        key_id: "placeholder".to_owned(),
    }
}

/// A call of `operation` on A's number or WABA, with a body and query it
/// would accept (so a refusal cannot be their fault).
fn call(operation: &common::Operation, key: &str, tenant: Option<&str>) -> Call {
    let mut call = common::sample_call(operation, &on_a(), Some(key));
    if let Some(tenant) = tenant {
        call = call.tenant(tenant);
    }
    call
}

/// M1.3. Decisive: step 4 (ownership) of the authorization order. Without
/// it, B's call reads A's token from the vault.
#[tokio::test]
async fn another_tenants_number_or_waba_is_not_found_and_the_vault_is_never_read() {
    let h = two_tenants().await;
    let b_key = h.tenant_key(B, &ALL_SCOPES).await;
    let platform = h
        .platform_key(AllowedTenants::Only(vec![tenant(B)]), &ALL_SCOPES)
        .await;
    let routes = owned_routes();
    // The table is the document: every such route is covered, and these
    // must be among them.
    for known in [
        (Method::GET, "/v1/numbers/{pn}"),
        (Method::GET, "/v1/numbers/{pn}/profile"),
        (Method::PATCH, "/v1/numbers/{pn}/profile"),
        (Method::DELETE, "/v1/wabas/{waba_id}"),
        (Method::DELETE, "/v1/admin/wabas/{waba_id}/binding"),
        (Method::POST, "/v1/numbers/{pn}/messages"),
        (Method::POST, "/v1/numbers/{pn}/messages/{message_id}/read"),
        (Method::POST, "/v1/numbers/{pn}/media"),
        (Method::GET, "/v1/numbers/{pn}/media/{media_id}"),
        (Method::DELETE, "/v1/numbers/{pn}/media/{media_id}"),
        (Method::GET, "/v1/wabas/{waba_id}/templates"),
        (Method::POST, "/v1/wabas/{waba_id}/templates"),
        (Method::DELETE, "/v1/wabas/{waba_id}/templates"),
        (Method::GET, "/v1/wabas/{waba_id}/templates/{id}"),
    ] {
        assert!(
            routes
                .iter()
                .any(|o| o.method == known.0 && o.template == known.1),
            "{known:?} is not in the document"
        );
    }

    let mut checked = 0;
    for operation in &routes {
        // Admin routes refuse any non-admin key before anything else.
        let expected = if operation.admin() {
            (StatusCode::FORBIDDEN, "forbidden")
        } else {
            (StatusCode::NOT_FOUND, "not_found")
        };
        for (key, named) in [(&b_key, None), (&platform, Some(B))] {
            let before = h.kv.vault_reads();
            let reply = h.call(call(operation, key, named)).await;
            let label = operation.label();
            assert_eq!(
                (reply.status, reply.code().as_str()),
                expected,
                "{label} with B's key: {}",
                reply.text
            );
            assert_eq!(
                h.kv.vault_reads(),
                before,
                "{label} read the vault for tenant B"
            );
            checked += 1;
        }
    }
    assert!(checked >= 28, "only {checked} calls");
    assert!(h.graph.requests().is_empty(), "no call reached Meta");
    assert_eq!(h.graph.remaining(), 0);
    // Nothing of A's changed.
    let waba = h.store.waba(&WabaId::new(WABA_A)).await.unwrap().unwrap();
    assert_eq!(waba.tenant_id, tenant(A));
    let number = h
        .store
        .number(&PhoneNumberId::new(PN_A))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number.status, NumberStatus::Connected);
    assert!(h.vault.get(&WabaId::new(WABA_A)).await.unwrap().is_some());
}

/// The control of M1.3: the counter counts, and A's own key does reach the
/// vault on the same routes.
#[tokio::test]
async fn the_owner_reads_the_vault_on_every_owned_route() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &ALL_SCOPES).await;
    for operation in owned_routes() {
        if operation.admin() {
            continue;
        }
        let label = operation.label();
        let before = h.kv.vault_reads();
        let asked = h.graph.requests().len();
        // Graph answers with an error: the point is the vault read before it.
        h.graph.push_json(
            500,
            json!({"error": {"message": "x", "type": "OAuthException", "code": 2}}),
        );
        let _ = h.call(call(&operation, &a_key, None)).await;
        assert!(
            h.kv.vault_reads() > before,
            "{label}: A's own call did not read the vault"
        );
        assert_eq!(
            h.graph.requests().len(),
            asked + 1,
            "{label}: Meta was not asked"
        );
        let request = h.graph.last_request().unwrap();
        assert_eq!(request.bearer(), Some("TOKEN-OF-A"), "{label}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A number of another tenant and a number of nobody answer alike: no
/// probing.
#[tokio::test]
async fn a_foreign_number_looks_like_a_missing_one() {
    let h = two_tenants().await;
    let b_key = h.tenant_key(B, &[Scope::Numbers]).await;
    let foreign = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&b_key))
        .await;
    let missing = h.call(Call::get("/v1/numbers/999999999").key(&b_key)).await;
    let mut foreign = foreign.json();
    let mut missing = missing.json();
    foreign["error"]["request_id"] = Value::Null;
    missing["error"]["request_id"] = Value::Null;
    assert_eq!(foreign, missing);
}

/// Step 1 on every keyed operation of the committed document: without a
/// key it is `401` and the body is never polled; with a key of the wrong
/// kind (a tenant key on `/v1/admin`, an admin key elsewhere) `403
/// forbidden`, before the body is read and before the vault is.
/// Decisive: the guards, and step 1 before any extractor reads the body.
#[tokio::test]
async fn every_keyed_operation_needs_a_key_of_its_kind_before_the_body() {
    let h = two_tenants().await;
    let admin = h.admin_key().await;
    let tenant_key = h.tenant_key(A, &ALL_SCOPES).await;
    let sample = common::Sample {
        tenant: A.to_owned(),
        waba: WABA_A.to_owned(),
        pn: PN_A.to_owned(),
        key_id: "placeholder".to_owned(),
    };
    let reads = h.kv.vault_reads();
    let mut keyed = 0;
    for operation in common::spec_operations() {
        if !operation.keyed {
            continue;
        }
        keyed += 1;
        let wrong_kind = if operation.admin() {
            &tenant_key
        } else {
            &admin
        };
        for (key, expected) in [
            (None, (StatusCode::UNAUTHORIZED, "unauthenticated")),
            (Some(wrong_kind), (StatusCode::FORBIDDEN, "forbidden")),
        ] {
            let polled = Arc::new(AtomicBool::new(false));
            let flag = polled.clone();
            let body = Body::from_stream(futures::stream::poll_fn(move |_| {
                flag.store(true, Ordering::SeqCst);
                Poll::Ready(Some(Ok::<_, std::io::Error>(Bytes::from_static(b"{}"))))
            }));
            let mut call = Call::new(operation.method.clone(), sample.fill(&operation.template))
                .header("content-type", "application/json")
                .body(body);
            if let Some(key) = key {
                call = call.key(key);
            }
            let reply = h.call(call).await;
            let label = format!("{} with {key:?}", operation.label());
            assert_eq!(
                (reply.status, reply.code().as_str()),
                expected,
                "{label}: {}",
                reply.text
            );
            if expected.0 == StatusCode::UNAUTHORIZED {
                assert_eq!(reply.headers["www-authenticate"], "Bearer", "{label}");
            }
            assert!(!polled.load(Ordering::SeqCst), "{label}: the body was read");
        }
    }
    assert!(keyed >= 19, "{keyed} keyed operations");
    assert_eq!(h.kv.vault_reads(), reads, "no refused call read the vault");
    assert!(h.graph.requests().is_empty());
}

/// Every way a key fails step 1 is the same `401`.
#[tokio::test]
async fn bad_keys_are_401() {
    let h = two_tenants().await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    let (id, secret) = key["wak_".len()..].split_once('_').unwrap();
    let flipped = format!(
        "wak_{id}_{}{}",
        &secret[..secret.len() - 1],
        if secret.ends_with('a') { 'b' } else { 'a' }
    );
    let unknown = format!("wak_{}_{secret}", "0".repeat(id.len()));
    for (authorization, why) in [
        (None, "no header"),
        (Some("Basic dXNlcjpwYXNz".to_owned()), "another scheme"),
        (Some("Bearer".to_owned()), "no key"),
        (Some("Bearer not-a-key".to_owned()), "not a key"),
        (Some(format!("Bearer {flipped}")), "wrong secret"),
        (Some(format!("Bearer {unknown}")), "unknown key id"),
        (Some(format!("Bearer {key}x")), "trailing character"),
    ] {
        let mut request = Call::get("/v1/numbers");
        if let Some(value) = authorization {
            request = request.header("authorization", &value);
        }
        let reply = h.call(request).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{why}");
        assert_eq!(reply.code(), "unauthenticated", "{why}");
    }
    // The genuine key works, with the scheme in any case.
    let reply = h
        .call(Call::get("/v1/numbers").header("authorization", &format!("bearer {key}")))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

/// A revoked or expired key stops at once.
#[tokio::test]
async fn revoked_and_expired_keys_are_401() {
    let h = two_tenants().await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&key)).await.status,
        StatusCode::OK
    );
    let key_id = key["wak_".len()..].split_once('_').unwrap().0;
    assert!(
        h.store
            .revoke_key(&KeyScope::Tenant(tenant(A)), key_id)
            .await
            .unwrap()
    );
    let reply = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::UNAUTHORIZED, "unauthenticated")
    );

    let (minted, _) = meta_whatsapp_server::api::admin::mint(
        h.store.as_ref(),
        meta_whatsapp_server::model::KeyOwner::Tenant(tenant(A)),
        vec![Scope::Numbers],
        String::new(),
        Some(time::OffsetDateTime::now_utc() + time::Duration::milliseconds(300)),
    )
    .await
    .unwrap();
    let expiring = minted.expose_key().to_owned();
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&expiring)).await.status,
        StatusCode::OK
    );
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let reply = h.call(Call::get("/v1/numbers").key(&expiring)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "expired");
}

/// Step 2: which tenant a key acts as.
#[tokio::test]
async fn keys_act_only_as_their_tenants() {
    let h = two_tenants().await;
    h.tenant("tenant-c").await;
    let a_key = h.tenant_key(A, &[Scope::Numbers]).await;
    let only_a = h
        .platform_key(AllowedTenants::Only(vec![tenant(A)]), &[Scope::Numbers])
        .await;
    let any = h.platform_key(AllowedTenants::All, &[Scope::Numbers]).await;
    let admin = h.admin_key().await;
    let numbers_of = |reply: common::Reply| -> Vec<String> {
        reply.json()["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["phone_number_id"].as_str().unwrap().to_owned())
            .collect()
    };

    // A tenant key: its own tenant, named or not; never another.
    let own = h.call(Call::get("/v1/numbers").key(&a_key)).await;
    assert_eq!(numbers_of(own), [PN_A]);
    let named = h.call(Call::get("/v1/numbers").key(&a_key).tenant(A)).await;
    assert_eq!(numbers_of(named), [PN_A]);
    let other = h.call(Call::get("/v1/numbers").key(&a_key).tenant(B)).await;
    assert_eq!(
        (other.status, other.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );

    // A platform key: only with WA-Tenant, only within its set.
    for (key, tenant_header, expected) in [
        (&only_a, None, Err("forbidden")),
        (&only_a, Some(A), Ok(PN_A)),
        (&only_a, Some(B), Err("forbidden")),
        (&only_a, Some("not a tenant id!"), Err("forbidden")),
        (&any, Some(B), Ok(PN_B)),
        (&any, Some("tenant-nobody"), Err("forbidden")),
        (&any, Some("tenant-c"), Ok("")),
        (&admin, None, Err("forbidden")),
        (&admin, Some(A), Err("forbidden")),
    ] {
        let mut request = Call::get("/v1/numbers").key(key);
        if let Some(t) = tenant_header {
            request = request.tenant(t);
        }
        let reply = h.call(request).await;
        match expected {
            Ok(number) => {
                assert_eq!(
                    reply.status,
                    StatusCode::OK,
                    "{tenant_header:?}: {}",
                    reply.text
                );
                let expected: Vec<&str> = if number.is_empty() {
                    vec![]
                } else {
                    vec![number]
                };
                assert_eq!(numbers_of(reply), expected);
            }
            Err(code) => {
                assert_eq!(reply.status, StatusCode::FORBIDDEN, "{tenant_header:?}");
                assert_eq!(reply.code(), code);
            }
        }
    }
    // Tenant and platform keys never reach /v1/admin.
    for key in [&a_key, &any] {
        let reply = h
            .call(Call::get("/v1/admin/tenants").key(key).tenant(A))
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden")
        );
    }
}

/// Suspension: the tenant's keys and platform keys naming it stop, with
/// `tenant_suspended`; lifting it restores them.
#[tokio::test]
async fn a_suspended_tenant_is_403_tenant_suspended() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Numbers]).await;
    let platform = h.platform_key(AllowedTenants::All, &[Scope::Numbers]).await;
    let reads = h.kv.vault_reads();
    h.store
        .update_tenant(&tenant(A), None, Some(TenantStatus::Suspended))
        .await
        .unwrap();
    for request in [
        Call::get("/v1/numbers").key(&a_key),
        Call::get(format!("/v1/numbers/{PN_A}")).key(&a_key),
        Call::get("/v1/numbers").key(&platform).tenant(A),
    ] {
        let reply = h.call(request).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "tenant_suspended")
        );
    }
    // Other tenants are unaffected.
    let reply = h
        .call(Call::get("/v1/numbers").key(&platform).tenant(B))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    h.store
        .update_tenant(&tenant(A), None, Some(TenantStatus::Active))
        .await
        .unwrap();
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&a_key)).await.status,
        StatusCode::OK
    );
    assert_eq!(h.kv.vault_reads(), reads, "no step reached the vault");
}

/// Step 3, and the order of steps 2, 3 and 4.
#[tokio::test]
async fn the_scope_is_checked_after_the_tenant_and_before_ownership() {
    let h = two_tenants().await;
    let no_numbers = h.tenant_key(B, &[Scope::Send, Scope::Templates]).await;
    let reads = h.kv.vault_reads();
    // Missing scope: 403 forbidden, on B's own number and on A's alike
    // (step 3 before step 4: the answer says nothing about ownership).
    for path in [
        "/v1/numbers".to_owned(),
        format!("/v1/numbers/{PN_B}"),
        format!("/v1/numbers/{PN_A}"),
    ] {
        let reply = h.call(Call::get(&path).key(&no_numbers)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{path}"
        );
    }
    // A suspended tenant is reported before a missing scope (step 2 first).
    h.store
        .update_tenant(&tenant(B), None, Some(TenantStatus::Suspended))
        .await
        .unwrap();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&no_numbers))
        .await;
    assert_eq!(reply.code(), "tenant_suspended");
    assert_eq!(h.kv.vault_reads(), reads);
}

/// Step 5: no token, an expired one, a number marked after a `190`.
#[tokio::test]
async fn the_vault_decides_last() {
    let h = Harness::new();
    h.tenant(A).await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    // Bound, but no token stored.
    h.store
        .bind_waba(
            &tenant(A),
            &WabaId::new(WABA_A),
            &[PhoneNumberId::new(PN_A)],
        )
        .await
        .unwrap();
    for path in [
        format!("/v1/numbers/{PN_A}"),
        format!("/v1/numbers/{PN_A}/profile"),
    ] {
        let reply = h.call(Call::get(&path).key(&key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::CONFLICT, "number_not_connected"),
            "{path}"
        );
    }
    let reply = h
        .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA_A}")).key(&key))
        .await;
    assert_eq!(reply.code(), "number_not_connected");

    // An expired token.
    h.vault
        .store(
            &meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken::new(
                WABA_A,
                meta_whatsapp_rs::core::secret::AccessToken::new("EXPIRED"),
            )
            .phone_number_ids([PN_A])
            .expires_at(time::OffsetDateTime::now_utc() - time::Duration::hours(1)),
        )
        .await
        .unwrap();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "reconnect_required")
    );

    // A number marked reconnect_required: refused before the vault is read.
    h.connect(A, WABA_A, &[PN_A], "TOKEN-OF-A").await;
    h.store
        .set_waba_status(&WabaId::new(WABA_A), NumberStatus::ReconnectRequired)
        .await
        .unwrap();
    let before = h.kv.vault_reads();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&key))
        .await;
    assert_eq!(reply.code(), "reconnect_required");
    assert_eq!(h.kv.vault_reads(), before);
    assert!(h.graph.requests().is_empty());
}

/// A `190` on a merchant's call marks its numbers: the next call is `409
/// reconnect_required` without asking Meta.
#[tokio::test]
async fn a_190_marks_the_numbers_reconnect_required() {
    let h = two_tenants().await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    // reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api, 401 example.
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "error_subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "reconnect_required")
    );
    assert_eq!(reply.json()["error"]["graph"]["code"], 190);
    let number = h
        .store
        .number(&PhoneNumberId::new(PN_A))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number.status, NumberStatus::ReconnectRequired);
    let listed = h.call(Call::get("/v1/numbers").key(&key)).await.json();
    assert_eq!(listed["data"][0]["status"], "reconnect_required");
    let again = h
        .call(Call::get(format!("/v1/numbers/{PN_A}/profile")).key(&key))
        .await;
    assert_eq!(again.code(), "reconnect_required");
    assert_eq!(
        h.graph.requests().len(),
        1,
        "the second call never reached Meta"
    );
    assert_eq!(h.graph.remaining(), 0);
    // B's number is untouched.
    let b = h
        .store
        .number(&PhoneNumberId::new(PN_B))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.status, NumberStatus::Connected);
}

/// Step 5 uses a token only for the WABA the number is bound to: when the
/// vault's phone index names another WABA (another tenant's onboarding
/// listed the number: Embedded Signup stores before it binds), the number
/// is not connected, and that WABA's token never reaches Meta. Decisive:
/// the vault record's WABA compared with the binding's.
#[tokio::test]
async fn a_token_for_another_waba_is_never_used() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Numbers]).await;
    // B's WABA token now indexes A's number too; A's binding is unchanged.
    h.vault
        .store(
            &meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken::new(
                WABA_B,
                meta_whatsapp_rs::core::secret::AccessToken::new("TOKEN-OF-B"),
            )
            .phone_number_ids([PN_B, PN_A]),
        )
        .await
        .unwrap();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&a_key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "number_not_connected"),
        "{}",
        reply.text
    );
    assert!(h.graph.requests().is_empty(), "B's token reached Meta");
}

/// One token reaching two tenants' WABAs: the platform's system user
/// token, attached (`POST /v1/admin/tenants/{id}/wabas`) to a WABA of each.
async fn one_token_two_tenants() -> Harness {
    let h = Harness::new();
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "PLATFORM-SYSTEM-TOKEN").await;
    h.connect(B, WABA_B, &[PN_B], "PLATFORM-SYSTEM-TOKEN").await;
    h
}

/// `error` without its request id, to compare two answers.
fn without_request_id(reply: &common::Reply) -> Value {
    let mut body = reply.json();
    body["error"]["request_id"] = Value::Null;
    body
}

/// B's media id on A's number, with a token that reaches both: the lookup
/// and the deletion carry A's `phone_number_id`, so Meta refuses B's
/// media; the refusal is `404 not_found`, the answer for a media id that
/// does not exist, whatever Meta's code, and nothing is downloaded or
/// deleted. Decisive: `phone_number_id` on the lookup and the deletion,
/// and the refusal answered as a missing media id.
#[tokio::test]
async fn another_tenants_media_id_is_not_found() {
    const B_MEDIA: &str = "1037543291543637";
    let h = one_token_two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Media]).await;
    // How Meta may refuse a media id that is not the number's: an invalid
    // parameter (100, subcode 33: "does not exist, cannot be loaded due
    // to missing permissions"), a permission error, a 404 of its own.
    let refusals = [
        (
            400,
            json!({"error": {"message": "Unsupported get request. B-MEDIA-SENTINEL", "type": "GraphMethodException",
                             "code": 100, "error_subcode": 33, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        ),
        (
            403,
            json!({"error": {"message": "B-MEDIA-SENTINEL", "type": "OAuthException", "code": 200,
                             "error_data": {"details": "B-MEDIA-SENTINEL belongs to another number"}}}),
        ),
        (
            404,
            json!({"error": {"message": "B-MEDIA-SENTINEL", "type": "GraphMethodException", "code": 803}}),
        ),
    ];
    let mut answers = Vec::new();
    for (status, refusal) in &refusals {
        for method in [Method::GET, Method::DELETE] {
            let asked = h.graph.requests().len();
            h.graph.push_json(*status, refusal.clone());
            let reply = h
                .call(
                    Call::new(
                        method.clone(),
                        format!("/v1/numbers/{PN_A}/media/{B_MEDIA}"),
                    )
                    .key(&a_key),
                )
                .await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{method} <- {status}: {}",
                reply.text
            );
            assert!(!reply.text.contains("B-MEDIA-SENTINEL"), "{}", reply.text);
            let requests = h.graph.requests();
            assert_eq!(requests.len(), asked + 1, "{method}: one call, no download");
            let request = requests.last().unwrap();
            assert_eq!(
                (request.method.clone(), request.path().to_owned()),
                (method.clone(), format!("/v25.0/{B_MEDIA}"))
            );
            assert_eq!(
                request.query("phone_number_id").as_deref(),
                Some(PN_A),
                "{method}: Meta is asked for A's number's media only"
            );
            answers.push(without_request_id(&reply));
        }
    }
    // Every refusal reads like a number that does not exist.
    let missing = h
        .call(Call::get("/v1/numbers/999999999/media/1").key(&a_key))
        .await;
    for answer in answers {
        assert_eq!(answer, without_request_id(&missing));
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// B's template id on A's WABA, with a token that reaches both: Meta's
/// template object does not name its WABA, so the id is looked for among
/// A's WABA's templates of its name; not there, it is `404 not_found`,
/// nothing of it is answered, and a deletion by that id deletes nothing.
/// Decisive: the lookup through the WABA's own edge.
#[tokio::test]
async fn another_tenants_template_id_is_not_found() {
    const B_TEMPLATE: &str = "1407680676729942";
    let h = one_token_two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Templates]).await;
    // The token reaches B's template: Meta answers its name.
    h.graph
        .push_json(200, json!({"name": "b_private_offer", "id": B_TEMPLATE}));
    // A's WABA has no template of that name.
    h.graph.push_json(200, json!({"data": []}));
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA_A}/templates/{B_TEMPLATE}")).key(&a_key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("b_private_offer"), "{}", reply.text);
    let requests = h.graph.requests();
    let [named, listed] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(
        named.query("fields").as_deref(),
        Some("name"),
        "its name only"
    );
    assert_eq!(
        listed.path(),
        format!("/v25.0/{WABA_A}/message_templates"),
        "then A's own templates"
    );
    assert_eq!(listed.query("name").as_deref(), Some("b_private_offer"));
    // B's template shares a name with one of A's: A's is not B's.
    h.graph
        .push_json(200, json!({"name": "order_confirmation", "id": B_TEMPLATE}));
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1407680676729941", "name": "order_confirmation", "language": "en_US"}]}),
    );
    let same_name = h
        .call(Call::get(format!("/v1/wabas/{WABA_A}/templates/{B_TEMPLATE}")).key(&a_key))
        .await;
    assert_eq!(without_request_id(&same_name), without_request_id(&reply));
    // A template id of nobody (Meta: 803) reads the same.
    h.graph.push_json(
        404,
        json!({"error": {"message": "Template not found", "type": "GraphMethodException", "code": 803}}),
    );
    let missing = h
        .call(Call::get(format!("/v1/wabas/{WABA_A}/templates/1407680676729943")).key(&a_key))
        .await;
    assert_eq!(without_request_id(&missing), without_request_id(&reply));
    // A has a template of the same name, with another id: still not B's.
    let before = h.graph.requests().len();
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1407680676729941", "name": "order_confirmation"}]}),
    );
    let deleted = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/wabas/{WABA_A}/templates?name=order_confirmation&id={B_TEMPLATE}"),
            )
            .key(&a_key),
        )
        .await;
    assert_eq!(
        (deleted.status, deleted.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        deleted.text
    );
    let requests = h.graph.requests();
    assert_eq!(requests.len(), before + 1, "no DELETE reached Meta");
    assert_eq!(requests.last().unwrap().method, Method::GET);
    assert_eq!(
        requests.last().unwrap().path(),
        format!("/v25.0/{WABA_A}/message_templates")
    );
    assert_eq!(h.graph.remaining(), 0);
}
