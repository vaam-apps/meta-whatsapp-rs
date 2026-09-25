//! `/v1/admin`: tenants, keys, platform keys, attaching a WABA (verified
//! with Meta) and unbinding it (decision D4).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{Scope, TenantId};
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

    // Taken, malformed, unknown fields: 422 on the field.
    for (request, field) in [
        (json!({"id": "merchant-42"}), "id"),
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

    // The admin unbinds; B may attach it.
    let unbound = h
        .call(Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA}/binding")).key(&admin))
        .await;
    assert_eq!(unbound.status, StatusCode::NO_CONTENT);
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
