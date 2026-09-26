//! `/v1/admin`: tenants, keys, platform keys, attaching a WABA (verified
//! with Meta) and unbinding it (decision D4).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{AllowedTenants, NumberStatus, Scope, TenantId};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const WABA: &str = "102290129340398";
const SYSTEM_TOKEN: &str = "EAAG-system-user-token";

fn post(path: &str, admin: &str, body: &Value) -> Call {
    Call::new(Method::POST, path).key(admin).json(body)
}

#[tokio::test]
async fn tenants_are_created_listed_read_suspended_and_deleted() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    let created = h
        .call(post(
            "/v1/admin/tenants",
            &admin,
            &json!({"id": "merchant-42", "name": "Lucky Shrub"}),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let body = created.json();
    assert_eq!(body["id"], "merchant-42");
    assert_eq!(body["name"], "Lucky Shrub");
    assert_eq!(body["status"], "active");
    assert!(body["created_at"].as_str().unwrap().ends_with('Z'));

    // Malformed, unknown fields: 422 on the field.
    for (request, field) in [
        (json!({"id": "has space"}), "id"),
        (json!({"id": ""}), "id"),
        (json!({"id": "x".repeat(65)}), "id"),
        (json!({"id": "ok", "name": "x".repeat(257)}), "name"),
        (json!({"id": "ok", "surprise": true}), "body"),
    ] {
        let reply = h.call(post("/v1/admin/tenants", &admin, &request)).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{request}");
        assert_eq!(reply.code(), "invalid_request");
        assert_eq!(reply.json()["error"]["field"], field, "{request}");
    }

    for id in ["merchant-43", "merchant-44"] {
        let reply = h
            .call(post("/v1/admin/tenants", &admin, &json!({"id": id})))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED);
    }
    let page = h
        .call(Call::get("/v1/admin/tenants?limit=2").key(&admin))
        .await
        .json();
    let ids: Vec<&str> = page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["merchant-42", "merchant-43"]);
    let cursor = page["next_cursor"].as_str().unwrap();
    let next = h
        .call(Call::get(format!("/v1/admin/tenants?limit=2&cursor={cursor}")).key(&admin))
        .await
        .json();
    assert_eq!(next["data"][0]["id"], "merchant-44");
    assert_eq!(next["next_cursor"], Value::Null);
    for bad in ["limit=0", "limit=101", "limit=x", "cursor=zz"] {
        let reply = h
            .call(Call::get(format!("/v1/admin/tenants?{bad}")).key(&admin))
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }

    let one = h
        .call(Call::get("/v1/admin/tenants/merchant-42").key(&admin))
        .await;
    assert_eq!(one.json()["name"], "Lucky Shrub");
    let missing = h
        .call(Call::get("/v1/admin/tenants/nobody").key(&admin))
        .await;
    assert_eq!(
        (missing.status, missing.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );

    let suspended = h
        .call(
            Call::new(Method::PATCH, "/v1/admin/tenants/merchant-42")
                .key(&admin)
                .json(&json!({"status": "suspended"})),
        )
        .await;
    assert_eq!(suspended.json()["status"], "suspended");
    assert_eq!(suspended.json()["name"], "Lucky Shrub");
    let bad_status = h
        .call(
            Call::new(Method::PATCH, "/v1/admin/tenants/merchant-42")
                .key(&admin)
                .json(&json!({"status": "frozen"})),
        )
        .await;
    assert_eq!(bad_status.status, StatusCode::UNPROCESSABLE_ENTITY);

    let deleted = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-43").key(&admin))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let again = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-43").key(&admin))
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert!(h.graph.requests().is_empty());
}

/// A taken id is `409 tenant_exists` (coordinator's decision S4), and the
/// tenant keeps its name.
#[tokio::test]
async fn a_taken_tenant_id_is_409_tenant_exists() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    let create = |name: &str| {
        post(
            "/v1/admin/tenants",
            &admin,
            &json!({"id": "merchant-42", "name": name}),
        )
    };
    assert_eq!(
        h.call(create("Lucky Shrub")).await.status,
        StatusCode::CREATED
    );
    let taken = h.call(create("Another")).await;
    assert_eq!(
        (taken.status, taken.code().as_str()),
        (StatusCode::CONFLICT, "tenant_exists")
    );
    let tenant = h
        .call(Call::get("/v1/admin/tenants/merchant-42").key(&admin))
        .await
        .json();
    assert_eq!(tenant["name"], "Lucky Shrub");
}

#[tokio::test]
async fn keys_are_shown_once_stored_as_digests_and_revoked_at_once() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-42").await;
    let minted = h
        .call(post(
            "/v1/admin/tenants/merchant-42/keys",
            &admin,
            &json!({"scopes": ["numbers", "send", "numbers"], "name": "medusa"}),
        ))
        .await;
    assert_eq!(minted.status, StatusCode::CREATED, "{}", minted.text);
    let body = minted.json();
    let key = body["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("wak_"));
    let key_id = body["api_key"]["key_id"].as_str().unwrap().to_owned();
    assert!(key.starts_with(&format!("wak_{key_id}_")));
    assert_eq!(body["api_key"]["kind"], "tenant");
    assert_eq!(body["api_key"]["tenant_id"], "merchant-42");
    assert_eq!(body["api_key"]["scopes"], json!(["send", "numbers"]));
    assert_eq!(body["api_key"]["revoked_at"], Value::Null);

    // The key works; the store holds its digest, never the secret.
    let used = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(used.status, StatusCode::OK, "{}", used.text);
    let record = h.store.key(&key_id).await.unwrap().unwrap();
    let secret = key.rsplit('_').next().unwrap();
    assert_eq!(
        record.secret_sha256,
        meta_whatsapp_server::keys::digest(secret)
    );
    assert!(!format!("{record:?}").contains(secret));

    // Listing never shows it again.
    let listed = h
        .call(Call::get("/v1/admin/tenants/merchant-42/keys").key(&admin))
        .await;
    assert!(!listed.text.contains(secret));
    assert!(!listed.text.contains("\"key\""));
    assert_eq!(listed.json()["data"][0]["key_id"], key_id.as_str());
    assert!(listed.json()["data"][0]["last_used_at"].is_string());

    // Revoked: effective on the next request.
    let revoked = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/admin/tenants/merchant-42/keys/{key_id}"),
            )
            .key(&admin),
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let refused = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    let listed = h
        .call(Call::get("/v1/admin/tenants/merchant-42/keys").key(&admin))
        .await
        .json();
    assert!(listed["data"][0]["revoked_at"].is_string());
    // Another tenant's path cannot revoke it (scoped), an unknown key is 404.
    h.tenant("merchant-43").await;
    let wrong = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/admin/tenants/merchant-43/keys/{key_id}"),
            )
            .key(&admin),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::NOT_FOUND);

    // Refusals before anything is stored.
    for (request, field) in [
        (json!({"scopes": []}), "scopes"),
        (
            json!({"scopes": ["numbers"], "expires_at": "2020-01-01T00:00:00Z"}),
            "expires_at",
        ),
        (
            json!({"scopes": ["numbers"], "expires_at": "tomorrow"}),
            "expires_at",
        ),
        (json!({"scopes": ["everything"]}), "body"),
    ] {
        let reply = h
            .call(post("/v1/admin/tenants/merchant-42/keys", &admin, &request))
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{request}");
        assert_eq!(reply.json()["error"]["field"], field, "{request}");
    }
    let no_tenant = h
        .call(post(
            "/v1/admin/tenants/nobody/keys",
            &admin,
            &json!({"scopes": ["numbers"]}),
        ))
        .await;
    assert_eq!(no_tenant.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn platform_keys_name_their_tenants() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-42").await;
    h.tenant("merchant-43").await;
    let minted = h
        .call(post(
            "/v1/admin/platform-keys",
            &admin,
            &json!({"tenants": ["merchant-42"], "scopes": ["numbers"]}),
        ))
        .await;
    assert_eq!(minted.status, StatusCode::CREATED, "{}", minted.text);
    let key = minted.json()["key"].as_str().unwrap().to_owned();
    assert_eq!(minted.json()["api_key"]["tenants"], json!(["merchant-42"]));
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&key).tenant("merchant-42"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&key).tenant("merchant-43"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    let any = h
        .call(post(
            "/v1/admin/platform-keys",
            &admin,
            &json!({"tenants": "*", "scopes": ["numbers"]}),
        ))
        .await;
    assert_eq!(any.json()["api_key"]["tenants"], "*");
    let listed = h
        .call(Call::get("/v1/admin/platform-keys").key(&admin))
        .await
        .json();
    assert_eq!(listed["data"].as_array().unwrap().len(), 2);

    for tenants in [json!("all"), json!([]), json!(["bad id"])] {
        let reply = h
            .call(post(
                "/v1/admin/platform-keys",
                &admin,
                &json!({"tenants": tenants, "scopes": ["numbers"]}),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{tenants}");
        assert_eq!(reply.json()["error"]["field"], "tenants");
    }
    let key_id = minted.json()["api_key"]["key_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let revoked = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/platform-keys/{key_id}")).key(&admin))
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&key).tenant("merchant-42"))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

/// solution-providers/manage-phone-numbers, "Filter phone numbers by
/// account mode" (the WABA's numbers, as Meta lists them).
fn phone_numbers_page() -> Value {
    json!({
      "data": [
        {"id": "1972385232742141", "display_phone_number": "+1 631-555-1111",
         "verified_name": "John's Cake Shop", "quality_rating": "UNKNOWN"},
        {"id": "1972385232742142", "display_phone_number": "+1 631-555-2222",
         "verified_name": "John's Cake Shop", "quality_rating": "GREEN"}
      ],
      "paging": {"cursors": {"before": "abcdefghij", "after": "klmnopqr"}}
    })
}

#[tokio::test]
async fn attaching_a_waba_binds_what_meta_lists_and_stores_the_token() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("platform-store").await;
    h.graph.push_json(200, phone_numbers_page());
    // subscribed-apps-api, POST: `{"success": true}`.
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(post(
            "/v1/admin/tenants/platform-store/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": SYSTEM_TOKEN}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"waba_id": WABA, "tenant_id": "platform-store",
               "phone_number_ids": ["1972385232742141", "1972385232742142"]})
    );
    assert!(!reply.text.contains(SYSTEM_TOKEN));

    // Meta was asked with the given token, for this WABA's numbers, then
    // the app was subscribed to the WABA's webhooks (docs/design/server.md,
    // section 3.4: disconnecting unsubscribes it), with no callback
    // override.
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(requests[0].path(), format!("/v25.0/{WABA}/phone_numbers"));
    assert_eq!(requests[0].bearer(), Some(SYSTEM_TOKEN));
    assert_eq!(requests[0].query("fields"), None);
    assert_eq!(requests[1].method, Method::POST);
    assert_eq!(requests[1].path(), format!("/v25.0/{WABA}/subscribed_apps"));
    assert_eq!(requests[1].bearer(), Some(SYSTEM_TOKEN));
    assert_eq!(requests[1].json(), None, "no override_callback_uri");
    assert_eq!(h.graph.remaining(), 0);

    // Bound, and the vault routes both numbers to the token.
    let tenant = TenantId::parse("platform-store").unwrap();
    assert_eq!(
        h.store
            .waba(&WabaId::new(WABA))
            .await
            .unwrap()
            .unwrap()
            .tenant_id,
        tenant
    );
    for pn in ["1972385232742141", "1972385232742142"] {
        let token = h
            .vault
            .get_by_phone_number(&PhoneNumberId::new(pn))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(token.token.expose_secret(), SYSTEM_TOKEN);
        assert_eq!(
            h.store
                .number(&PhoneNumberId::new(pn))
                .await
                .unwrap()
                .unwrap()
                .tenant_id,
            tenant
        );
    }
    // The tenant sees them.
    let key = h.tenant_key("platform-store", &[Scope::Numbers]).await;
    let wabas = h.call(Call::get("/v1/wabas").key(&key)).await.json();
    assert_eq!(wabas["data"][0]["waba_id"], WABA);
    assert!(wabas["data"][0]["attached_at"].is_string());
}

/// D4: a WABA bound to one tenant is refused to another, before Meta is
/// asked anything; the admin unbind frees it.
#[tokio::test]
async fn a_waba_of_another_tenant_is_refused_until_the_admin_unbinds_it() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.tenant("merchant-b").await;
    h.connect("merchant-a", WABA, &["1972385232742141"], "TOKEN-OF-A")
        .await;
    let reply = h
        .call(post(
            "/v1/admin/tenants/merchant-b/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": SYSTEM_TOKEN}),
        ))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "waba_owned_by_another_tenant")
    );
    assert!(h.graph.requests().is_empty(), "refused before asking Meta");
    let kept = h.vault.get(&WabaId::new(WABA)).await.unwrap().unwrap();
    assert_eq!(
        kept.token.expose_secret(),
        "TOKEN-OF-A",
        "A's token is untouched"
    );
    assert_eq!(
        h.store
            .waba(&WabaId::new(WABA))
            .await
            .unwrap()
            .unwrap()
            .tenant_id
            .as_str(),
        "merchant-a"
    );

    // The operator sees who holds it.
    let held = h
        .call(Call::get(format!("/v1/admin/wabas/{WABA}")).key(&admin))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.text);
    let held = held.json();
    assert_eq!(held["tenant_id"], "merchant-a");
    assert_eq!(held["numbers"][0]["phone_number_id"], "1972385232742141");
    assert_eq!(held["numbers"][0]["status"], "connected");
    // The admin unbinds (unsubscribing with A's token, then deleting it);
    // B may attach it.
    h.graph.push_json(200, json!({"success": true}));
    let unbound = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA}/binding")).key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT);
    let unsubscribe = h.graph.last_request().unwrap();
    assert_eq!(unsubscribe.method, Method::DELETE);
    assert_eq!(unsubscribe.path(), format!("/v25.0/{WABA}/subscribed_apps"));
    assert_eq!(unsubscribe.bearer(), Some("TOKEN-OF-A"));
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(
        h.store
            .number(&PhoneNumberId::new("1972385232742141"))
            .await
            .unwrap()
            .is_none()
    );
    let again = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA}/binding")).key(&admin))
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    let gone = h
        .call(Call::get(format!("/v1/admin/wabas/{WABA}")).key(&admin))
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    h.graph.push_json(200, phone_numbers_page());
    h.graph.push_json(200, json!({"success": true}));
    let attached = h
        .call(post(
            "/v1/admin/tenants/merchant-b/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": SYSTEM_TOKEN}),
        ))
        .await;
    assert_eq!(attached.status, StatusCode::CREATED, "{}", attached.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta refusing the token binds nothing and stores nothing.
#[tokio::test]
async fn a_token_meta_refuses_binds_nothing() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    // reference/whatsapp-business-phone-number/…-phone-number-api, 403 example.
    h.graph.push_json(
        403,
        json!({"error": {"message": "Your app doesn't have permission to access this phone number",
                         "type": "OAuthException", "code": 200, "error_subcode": 1349174,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let reply = h
        .call(post(
            "/v1/admin/tenants/merchant-a/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": SYSTEM_TOKEN}),
        ))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "permission")
    );
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_none());
    assert_eq!(h.graph.remaining(), 0);
    for (body, field) in [
        (json!({"waba_id": "", "token": SYSTEM_TOKEN}), "waba_id"),
        (json!({"waba_id": "1 2", "token": SYSTEM_TOKEN}), "waba_id"),
        (
            json!({"waba_id": "0102290129340398", "token": SYSTEM_TOKEN}),
            "waba_id",
        ),
        (
            json!({"waba_id": "10229012934039a", "token": SYSTEM_TOKEN}),
            "waba_id",
        ),
        (
            json!({"waba_id": "1".repeat(65), "token": SYSTEM_TOKEN}),
            "waba_id",
        ),
        (json!({"waba_id": WABA, "token": " "}), "token"),
        (json!({"waba_id": WABA}), "body"),
    ] {
        let reply = h
            .call(post("/v1/admin/tenants/merchant-a/wabas", &admin, &body))
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
        assert!(!reply.text.contains(SYSTEM_TOKEN));
    }
    assert_eq!(h.graph.requests().len(), 1);
}

/// Deleting a tenant disconnects its WABAs first; a failed unsubscribe
/// keeps everything.
#[tokio::test]
async fn deleting_a_tenant_disconnects_its_wabas_first() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.connect("merchant-a", WABA, &["1972385232742141"], "TOKEN-OF-A")
        .await;
    let key = h.tenant_key("merchant-a", &[Scope::Numbers]).await;
    // Meta fails: nothing deleted.
    h.graph.push_json(
        500,
        json!({"error": {"message": "An unexpected error occurred. Please retry your request",
                         "type": "GraphMethodException", "code": 2, "is_transient": true,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let failed = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a").key(&admin))
        .await;
    assert_eq!(
        (failed.status, failed.code().as_str()),
        (StatusCode::BAD_GATEWAY, "service_unavailable")
    );
    assert!(
        h.store
            .tenant(&TenantId::parse("merchant-a").unwrap())
            .await
            .unwrap()
            .is_some()
    );
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_some());
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&key)).await.status,
        StatusCode::OK
    );

    // Meta unsubscribes: token, bindings, keys and the tenant go.
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a").key(&admin))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let unsubscribe = h.graph.last_request().unwrap();
    assert_eq!(unsubscribe.method, Method::DELETE);
    assert_eq!(unsubscribe.path(), format!("/v25.0/{WABA}/subscribed_apps"));
    assert_eq!(unsubscribe.bearer(), Some("TOKEN-OF-A"));
    assert_eq!(h.graph.remaining(), 0);
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_none());
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&key)).await.status,
        StatusCode::UNAUTHORIZED
    );
}

/// A WABA without a usable token blocks the tenant's deletion until the
/// admin unbinds it.
#[tokio::test]
async fn a_waba_without_a_token_blocks_deletion_until_unbound() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.store
        .bind_waba(
            &TenantId::parse("merchant-a").unwrap(),
            &WabaId::new(WABA),
            &[],
        )
        .await
        .unwrap();
    let blocked = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a").key(&admin))
        .await;
    assert_eq!(
        (blocked.status, blocked.code().as_str()),
        (StatusCode::CONFLICT, "number_not_connected")
    );
    let unbound = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA}/binding")).key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT);
    let deleted = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a").key(&admin))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert!(h.graph.requests().is_empty());
}

/// Meta refusing to subscribe the app is answered with its error; the
/// WABA stays attached (the token is stored after the binding, as
/// onboarding does), and repeating the attach subscribes it. Decisive: the
/// subscription itself (its refusal must surface).
#[tokio::test]
async fn a_refused_subscription_is_reported_and_a_repeat_finishes_it() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.graph.push_json(200, phone_numbers_page());
    // subscribed-apps-api, POST, an error of the documented shape.
    h.graph.push_json(
        403,
        json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException",
                         "code": 200, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let attach = || {
        post(
            "/v1/admin/tenants/merchant-a/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": SYSTEM_TOKEN}),
        )
    };
    let reply = h.call(attach()).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "permission")
    );
    // Where it stopped, and that repeating the call finishes it.
    let body = reply.json();
    assert_eq!(body["error"]["step"], "subscribe_app");
    assert_eq!(body["error"]["resumable"], true);
    assert_eq!(body["error"]["graph"]["code"], 200);
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, Method::POST);
    assert_eq!(requests[1].path(), format!("/v25.0/{WABA}/subscribed_apps"));
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_some());
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_some());

    h.graph.push_json(200, phone_numbers_page());
    h.graph.push_json(200, json!({"success": true}));
    let again = h.call(attach()).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.text);
    assert_eq!(h.graph.requests().len(), 4);
    assert_eq!(h.graph.remaining(), 0);
}

/// A token Meta rejects (`190`) once it is stored, when subscribing the
/// app, is a stored token's `190`: `409 reconnect_required`, not the `422
/// invalid_request` of a token refused before anything was bound (which
/// would say nothing changed). The answer says where it stopped and that
/// it can be finished; the WABA stays attached, its numbers
/// `reconnect_required`, and repeating the call with a valid token
/// finishes it. Decisive: the subscribe step's own error mapping.
#[tokio::test]
async fn a_token_meta_rejects_once_stored_is_reconnect_required_and_resumable() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.graph.push_json(200, phone_numbers_page());
    h.graph.push_json(
        401,
        json!({"error": {"message": "Error validating access token: Session has expired",
                         "type": "OAuthException", "code": 190, "error_subcode": 463,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let attach = |token: &str| {
        post(
            "/v1/admin/tenants/merchant-a/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": token}),
        )
    };
    let reply = h.call(attach(SYSTEM_TOKEN)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "reconnect_required"),
        "{}",
        reply.text
    );
    let body = reply.json();
    assert_eq!(body["error"]["step"], "subscribe_app");
    assert_eq!(body["error"]["resumable"], true);
    assert_eq!(body["error"]["field"], Value::Null);
    assert_eq!(body["error"]["graph"]["code"], 190);
    assert!(!reply.text.contains(SYSTEM_TOKEN));
    // Attached, the token stored, its numbers marked.
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_some());
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_some());
    let numbers = h.store.waba_numbers(&WabaId::new(WABA)).await.unwrap();
    assert_eq!(numbers.len(), 2);
    assert!(
        numbers
            .iter()
            .all(|n| n.status == NumberStatus::ReconnectRequired),
        "{numbers:?}"
    );

    // A valid token finishes it.
    h.graph.push_json(200, phone_numbers_page());
    h.graph.push_json(200, json!({"success": true}));
    let again = h.call(attach("EAAG-a-new-system-user-token")).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.text);
    let numbers = h.store.waba_numbers(&WabaId::new(WABA)).await.unwrap();
    assert!(
        numbers.iter().all(|n| n.status == NumberStatus::Connected),
        "{numbers:?}"
    );
    let stored = h.vault.get(&WabaId::new(WABA)).await.unwrap().unwrap();
    assert_eq!(stored.token.expose_secret(), "EAAG-a-new-system-user-token");
    assert_eq!(h.graph.remaining(), 0);
}

/// A token Meta rejects (`190`) is the request's input: `422
/// invalid_request` on `token`, with Meta's code, and nothing bound
/// (conventions review S5; a stored token's `190` is
/// `reconnect_required`).
#[tokio::test]
async fn a_token_meta_rejects_is_invalid_input() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let reply = h
        .call(post(
            "/v1/admin/tenants/merchant-a/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": SYSTEM_TOKEN}),
        ))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request")
    );
    let body = reply.json();
    assert_eq!(body["error"]["field"], "token");
    assert_eq!(body["error"]["graph"]["code"], 190);
    assert!(!reply.text.contains(SYSTEM_TOKEN));
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_none());
    assert_eq!(h.graph.remaining(), 0);
}

/// A listing that never ends (a fresh cursor on every page) stops past
/// 1,000 numbers: `502 upstream`, nothing bound, stored or subscribed
/// (security review L3). 1,000 numbers exactly are attached.
#[tokio::test]
async fn a_listing_past_a_thousand_numbers_is_refused() {
    let page = |n: usize, last: bool| {
        let data: Vec<Value> = (0..100)
            .map(|i| json!({"id": format!("{}", 1_000_000 + n * 100 + i)}))
            .collect();
        if last {
            json!({"data": data})
        } else {
            json!({"data": data, "paging": {"cursors": {"after": format!("c{n}")},
                   "next": format!("https://graph.facebook.com/v25.0/{WABA}/phone_numbers?after=c{n}")}})
        }
    };
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    for n in 0..11 {
        h.graph.push_json(200, page(n, false));
    }
    let attach = || {
        post(
            "/v1/admin/tenants/merchant-a/wabas",
            &admin,
            &json!({"waba_id": WABA, "token": SYSTEM_TOKEN}),
        )
    };
    let reply = h.call(attach()).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::BAD_GATEWAY, "upstream")
    );
    assert_eq!(h.graph.remaining(), 0, "11 pages read, no more");
    assert!(
        h.graph
            .requests()
            .iter()
            .all(|r| r.path() == format!("/v25.0/{WABA}/phone_numbers")),
        "nothing subscribed"
    );
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_none());

    for n in 0..10 {
        h.graph.push_json(200, page(n, n == 9));
    }
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(attach()).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json()["phone_number_ids"].as_array().unwrap().len(),
        1000
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// The admin unbind frees a WABA whatever Meta says: the app is
/// unsubscribed with the stored token at best, and the token and bindings
/// always go (security review L5, coordinator's decision). Decisive:
/// deleting the vault entry.
#[tokio::test]
async fn unbinding_deletes_the_token_even_when_meta_refuses() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.connect("merchant-a", WABA, &["1972385232742141"], "TOKEN-OF-A")
        .await;
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let unbound = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA}/binding")).key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(
        h.vault
            .get_by_phone_number(&PhoneNumberId::new("1972385232742141"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_none());
    assert_eq!(h.graph.remaining(), 0);
}

/// An unusable token, expired or sealed with a key the service no longer
/// has, goes with the bindings: the unbind asks Meta nothing (no token to
/// ask with) and deletes the vault entry and its phone index anyway, as
/// for a usable one. Decisive: deleting the vault entry when the token is
/// unusable.
#[tokio::test]
async fn unbinding_deletes_an_unusable_token_too() {
    use std::sync::Arc;

    use meta_whatsapp_rs::client::embedded_signup::{
        StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
    };
    use meta_whatsapp_rs::core::secret::AccessToken;
    use meta_whatsapp_rs::core::store::KvStore;

    let h = Harness::new();
    let admin = h.admin_key().await;
    let tenant = h.tenant("merchant-a").await;
    let expired = (WABA, "1972385232742141");
    let undecryptable = ("102290129340399", "1972385232742142");
    let lost = TokenVault::new(
        h.kv.clone() as Arc<dyn KvStore>,
        VaultKeys::new(VaultKey::generate("lost").unwrap()),
    )
    .unwrap();
    for ((waba, pn), vault, expires) in [(expired, &h.vault, true), (undecryptable, &lost, false)] {
        let (waba, pn) = (WabaId::new(waba), PhoneNumberId::new(pn));
        h.store
            .bind_waba(&tenant, &waba, std::slice::from_ref(&pn))
            .await
            .unwrap();
        let mut token = StoredBusinessToken::new(waba, AccessToken::new("TOKEN-OF-A"))
            .phone_number_ids(vec![pn]);
        if expires {
            token = token.expires_at(time::OffsetDateTime::now_utc() - time::Duration::hours(1));
        }
        vault.store(&token).await.unwrap();
    }
    // Unusable indeed: expired, and not readable with the service's key.
    let token = h.vault.get(&WabaId::new(expired.0)).await.unwrap().unwrap();
    assert!(token.is_expired(time::OffsetDateTime::now_utc()));
    assert!(h.vault.get(&WabaId::new(undecryptable.0)).await.is_err());

    for (waba, pn) in [expired, undecryptable] {
        let unbound = h
            .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{waba}/binding")).key(&admin))
            .await;
        assert_eq!(unbound.status, StatusCode::NO_CONTENT, "{}", unbound.text);
        // Gone: no record left, readable or not, and no phone index entry.
        assert!(
            matches!(h.vault.get(&WabaId::new(waba)).await, Ok(None)),
            "{waba}: the vault entry was kept"
        );
        assert!(
            matches!(
                h.vault.get_by_phone_number(&PhoneNumberId::new(pn)).await,
                Ok(None)
            ),
            "{waba}: the phone index was kept"
        );
        assert!(h.store.waba(&WabaId::new(waba)).await.unwrap().is_none());
    }
    assert!(h.graph.requests().is_empty());
}

/// A `KvStore` whose deletes fail while `failing` is set.
#[derive(Debug)]
struct FailingDeletes {
    inner: meta_whatsapp_rs::adapters::store::MemoryKvStore,
    failing: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl meta_whatsapp_rs::core::store::KvStore for FailingDeletes {
    async fn get(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
    ) -> Result<
        Option<meta_whatsapp_rs::core::store::Versioned>,
        meta_whatsapp_rs::core::error::StorageError,
    > {
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
        value: Vec<u8>,
        expiry: meta_whatsapp_rs::core::store::Expiry,
    ) -> Result<u64, meta_whatsapp_rs::core::error::StorageError> {
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
        value: Vec<u8>,
        expiry: meta_whatsapp_rs::core::store::Expiry,
    ) -> Result<Option<u64>, meta_whatsapp_rs::core::error::StorageError> {
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: meta_whatsapp_rs::core::store::Expiry,
    ) -> Result<Option<u64>, meta_whatsapp_rs::core::error::StorageError> {
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(
        &self,
        key: &meta_whatsapp_rs::core::store::StoreKey,
    ) -> Result<bool, meta_whatsapp_rs::core::error::StorageError> {
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(meta_whatsapp_rs::core::error::StorageError::Backend(
                anyhow::anyhow!("the store is down"),
            ));
        }
        self.inner.delete(key).await
    }
}

/// The unbind deletes the token before the bindings: when the token
/// cannot be deleted, the binding stays and the answer is `503`, so the
/// operator repeats the unbind (bindings gone first would leave a token no
/// route reaches, and nothing to repeat the unbind on). Both ways: after
/// Meta unsubscribed the app with a usable token, and without a usable
/// token. Decisive: the order in `OwnedWaba::forget` and in the
/// unusable-token branch.
#[tokio::test]
async fn a_token_that_cannot_be_deleted_keeps_the_binding_for_a_retry() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
    use meta_whatsapp_rs::core::secret::AccessToken;
    use meta_whatsapp_server::store::MemoryStore;

    let kv = Arc::new(FailingDeletes {
        inner: MemoryKvStore::new(),
        failing: AtomicBool::new(false),
    });
    let h = Harness::on(Arc::new(MemoryStore::new()), kv.clone());
    let admin = h.admin_key().await;
    let tenant = h.tenant("merchant-a").await;
    let usable = WABA;
    let expired = "102290129340399";
    h.connect("merchant-a", usable, &["1972385232742141"], "TOKEN-OF-A")
        .await;
    h.store
        .bind_waba(&tenant, &WabaId::new(expired), &[])
        .await
        .unwrap();
    h.vault
        .store(
            &StoredBusinessToken::new(expired, AccessToken::new("TOKEN-OF-A"))
                .expires_at(time::OffsetDateTime::now_utc() - time::Duration::hours(1)),
        )
        .await
        .unwrap();
    let unbind = |waba: &str| {
        Call::new(Method::DELETE, format!("/v1/admin/wabas/{waba}/binding")).key(&admin)
    };
    for waba in [usable, expired] {
        if waba == usable {
            h.graph.push_json(200, json!({"success": true}));
        }
        kv.failing.store(true, Ordering::SeqCst);
        let refused = h.call(unbind(waba)).await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
            "{waba}"
        );
        assert!(
            h.store.waba(&WabaId::new(waba)).await.unwrap().is_some(),
            "{waba}: the binding went before the token"
        );
        assert!(h.vault.get(&WabaId::new(waba)).await.unwrap().is_some());

        kv.failing.store(false, Ordering::SeqCst);
        if waba == usable {
            h.graph.push_json(200, json!({"success": true}));
        }
        let unbound = h.call(unbind(waba)).await;
        assert_eq!(
            unbound.status,
            StatusCode::NO_CONTENT,
            "{waba}: {}",
            unbound.text
        );
        assert!(h.store.waba(&WabaId::new(waba)).await.unwrap().is_none());
        assert!(h.vault.get(&WabaId::new(waba)).await.unwrap().is_none());
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// The rotation walks every page of bindings, not only the first 100
/// (`MAX_PAGE_SIZE`). Decisive: following `next_after`.
#[tokio::test]
async fn rotating_the_vault_key_walks_every_page_of_bindings() {
    use std::sync::Arc;

    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::client::embedded_signup::{
        StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
    };
    use meta_whatsapp_rs::core::secret::AccessToken;
    use meta_whatsapp_rs::core::store::KvStore;
    use meta_whatsapp_server::auth::rotate_vault;
    use meta_whatsapp_server::model::MAX_PAGE_SIZE;
    use meta_whatsapp_server::store::{MemoryStore, RecordStore};

    let old =
        || VaultKey::from_base64("k1", "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=").unwrap();
    let new =
        || VaultKey::from_base64("k2", "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=").unwrap();
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let store = MemoryStore::new();
    let tenant = TenantId::parse("merchant-a").unwrap();
    store.create_tenant(&tenant, "").await.unwrap();
    let sealing = TokenVault::new(kv.clone(), VaultKeys::new(old())).unwrap();
    let wabas = 2 * MAX_PAGE_SIZE + 1;
    for i in 0..wabas {
        let waba = format!("{}", 300_000 + i);
        store
            .bind_waba(&tenant, &WabaId::new(waba.as_str()), &[])
            .await
            .unwrap();
        sealing
            .store(&StoredBusinessToken::new(
                waba.as_str(),
                AccessToken::new(format!("TOKEN-{waba}")),
            ))
            .await
            .unwrap();
    }
    let rolling = TokenVault::new(kv.clone(), VaultKeys::new(new()).with_previous(old()))
        .unwrap()
        .rotate_on_read(false);
    let report = rotate_vault(&store, &rolling).await.unwrap();
    assert_eq!(report.wabas, wabas);
    assert_eq!(report.rotated, wabas);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    // The last one too opens with the new key alone.
    let new_only = TokenVault::new(kv, VaultKeys::new(new())).unwrap();
    let last = format!("{}", 300_000 + wabas - 1);
    let token = new_only
        .get(&WabaId::new(last.as_str()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token.token.expose_secret(), format!("TOKEN-{last}"));
}

/// Vault key rotation walks every bound WABA (the vault cannot list its
/// records): tokens sealed with the previous key are re-encrypted under the
/// active one, so the previous key can go; a record under a key no longer
/// configured is reported, and the walk goes on (design section 4.2,
/// conventions review S12). Decisive: `TokenVault::rotate` on each binding.
#[tokio::test]
async fn rotating_the_vault_key_rewrites_every_bound_token() {
    use std::sync::Arc;

    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::client::embedded_signup::{
        StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
    };
    use meta_whatsapp_rs::core::secret::AccessToken;
    use meta_whatsapp_rs::core::store::KvStore;
    use meta_whatsapp_server::auth::rotate_vault;
    use meta_whatsapp_server::store::{MemoryStore, RecordStore};

    // 32 bytes of one value each, in base64.
    let key = |id: &str, byte: u8| {
        let b64 = match byte {
            1 => "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
            2 => "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=",
            _ => "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
        };
        VaultKey::from_base64(id, b64).unwrap()
    };
    let kv: Arc<dyn KvStore> = Arc::new(MemoryKvStore::new());
    let store = MemoryStore::new();
    let tenant = TenantId::parse("merchant-a").unwrap();
    store.create_tenant(&tenant, "").await.unwrap();
    let old = TokenVault::new(kv.clone(), VaultKeys::new(key("k1", 1))).unwrap();
    let lost = TokenVault::new(kv.clone(), VaultKeys::new(key("k0", 9))).unwrap();
    for (waba, vault) in [("201", &old), ("202", &old), ("203", &lost)] {
        store
            .bind_waba(&tenant, &WabaId::new(waba), &[])
            .await
            .unwrap();
        vault
            .store(&StoredBusinessToken::new(
                waba,
                AccessToken::new(format!("TOKEN-{waba}")),
            ))
            .await
            .unwrap();
    }
    let rolling = TokenVault::new(
        kv.clone(),
        VaultKeys::new(key("k2", 2)).with_previous(key("k1", 1)),
    )
    .unwrap()
    .rotate_on_read(false);
    let report = rotate_vault(&store, &rolling).await.unwrap();
    assert_eq!(report.wabas, 3);
    assert_eq!(report.rotated, 2);
    assert_eq!(report.failed, ["203"]);
    // The previous key can go: both records open with the new key alone.
    let new_only = TokenVault::new(kv.clone(), VaultKeys::new(key("k2", 2))).unwrap();
    for waba in ["201", "202"] {
        let token = new_only.get(&WabaId::new(waba)).await.unwrap().unwrap();
        assert_eq!(token.token.expose_secret(), format!("TOKEN-{waba}"));
    }
    // Again: nothing left to rewrite.
    let again = rotate_vault(&store, &rolling).await.unwrap();
    assert_eq!((again.rotated, again.failed.len()), (0, 1));
}

/// `POST /v1/admin/vault/rotate`, admin key only, answers the report.
#[tokio::test]
async fn the_rotation_route_reports_the_walk() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.connect("merchant-a", WABA, &["1972385232742141"], "TOKEN-OF-A")
        .await;
    let reply = h
        .call(Call::new(Method::POST, "/v1/admin/vault/rotate").key(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"wabas": 1, "rotated": 0, "failed": []})
    );
    assert!(h.graph.requests().is_empty());
}

/// A platform key allowed a tenant loses it when the tenant is deleted: a
/// tenant created again with the same id is another tenant, and the key
/// naming it is `403 forbidden` (security review M4). Decisive: deleting
/// the tenant from the platform keys' allowed tenants.
#[tokio::test]
async fn a_platform_key_does_not_follow_a_recreated_tenant_id() {
    let h = Harness::new();
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    let platform = h
        .platform_key(
            AllowedTenants::Only(vec![TenantId::parse("merchant-a").unwrap()]),
            &[Scope::Numbers],
        )
        .await;
    let as_a = || Call::get("/v1/numbers").key(&platform).tenant("merchant-a");
    assert_eq!(h.call(as_a()).await.status, StatusCode::OK);
    let deleted = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a").key(&admin))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let created = h
        .call(post(
            "/v1/admin/tenants",
            &admin,
            &json!({"id": "merchant-a", "name": "someone else"}),
        ))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let reply = h.call(as_a()).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
}
