//! The admin operations' audit events, one operation at a time: each
//! change an operator makes logs exactly its own event (target `audit`,
//! message `admin change`) with the admin key's public id and the ids it
//! touched, once the change took effect, and a refused or failed one logs
//! none. Since roadmap S5a the core's services emit them
//! (`meta_whatsapp_server_core::admin`); this pins what they emit
//! through the axum adapter: the action of each path (a rename, a
//! suspension and a reactivation each their own), its fields, the
//! disconnection of each WABA before a tenant's deletion, and the attach
//! audited once its token is stored, even when subscribing the app then
//! fails. Alone in its binary, as `logs.rs` and `forged_logs.rs` are: the
//! capture is a thread's default subscriber, and `tracing` caches whether
//! a call site is enabled across threads.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use common::capture::{Captured, subscriber};
use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::core::transport::{HttpRequest, HttpResponse, HttpTransport};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{BindOutcome, TenantId};
use meta_whatsapp_server::store::Store;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const WABA: &str = "102290129340398";
const OTHER_WABA: &str = "102290129340399";
const SYSTEM_TOKEN: &str = "EAAG-system-user-token-for-the-audit-test";
/// A WABA another tenant binds while Meta lists its numbers for an
/// attach, and its number.
const RACED_WABA: &str = "102290129340401";
const RACED_NUMBER: &str = "1972385232742149";

/// The scripted transport, except that the first listing of
/// [`RACED_WABA`]'s numbers first binds it to `merchant-b`, as that
/// tenant's attach landing then would: the attach found it unbound, and
/// only its binding's own check (D4) sees the holder.
struct BindsWhileListing {
    graph: ScriptedTransport,
    store: Arc<OnceLock<Arc<dyn Store>>>,
    armed: AtomicBool,
}

impl std::fmt::Debug for BindsWhileListing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindsWhileListing")
            .field("graph", &self.graph)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpTransport for BindsWhileListing {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let listing = request
            .url
            .path()
            .ends_with(&format!("/{RACED_WABA}/phone_numbers"));
        if listing && self.armed.swap(false, Ordering::SeqCst) {
            let bound = self
                .store
                .get()
                .unwrap()
                .bind_waba(
                    &TenantId::parse("merchant-b").unwrap(),
                    &WabaId::new(RACED_WABA),
                    &[PhoneNumberId::new(RACED_NUMBER)],
                )
                .await
                .unwrap();
            assert_eq!(bound, BindOutcome::Bound, "the holder's binding");
        }
        self.graph.send(request).await
    }
}

/// The audit events logged since `from` (a length of the capture), each
/// as its fields: `action`, `admin_key_id` and the ids it carries (a
/// field it does not carry is absent).
fn audits(captured: &Captured, from: usize) -> Vec<Value> {
    captured.text()[from..]
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["target"] == "audit")
        .map(|event| {
            assert_eq!(event["fields"]["message"], "admin change", "{event}");
            assert_eq!(event["level"], "INFO", "{event}");
            let mut fields = event["fields"].clone();
            fields.as_object_mut().unwrap().remove("message");
            fields
        })
        .collect()
}

/// An audit event's fields: `action`, the admin key's id, and `ids`.
fn event(action: &str, admin_key_id: &str, ids: &[(&str, &str)]) -> Value {
    let mut fields = json!({"action": action, "admin_key_id": admin_key_id});
    for (name, value) in ids {
        fields[*name] = json!(value);
    }
    fields
}

fn post(path: &str, admin: &str, body: &Value) -> Call {
    Call::new(Method::POST, path).key(admin).json(body)
}

fn phone_numbers(ids: &[&str]) -> Value {
    json!({"data": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()})
}

/// Every admin change, and the refusals and failures beside them.
/// Decisive: each `audit` call of `meta_whatsapp_server_core::admin`, its
/// action, its subject and its place after the change (the attach's after
/// the binding's own D4 check: a WABA bound to another tenant while Meta
/// listed it is refused unaudited).
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn each_admin_change_logs_its_own_audit_event_and_a_refused_one_none() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let store = Arc::new(OnceLock::new());
    let h = Harness::with_transport(common::test_settings(), |graph| {
        Arc::new(BindsWhileListing {
            graph,
            store: store.clone(),
            armed: AtomicBool::new(true),
        })
    });
    assert!(store.set(h.store.clone()).is_ok());
    let admin = h.admin_key().await;
    let admin_id = admin["wak_".len()..].split_once('_').unwrap().0.to_owned();
    let admin_id = admin_id.as_str();
    // Run `call`, check its status, and answer the audit events it logged.
    let run = async |call: Call, status: StatusCode| {
        let from = captured.text().len();
        let reply = h.call(call).await;
        assert_eq!(reply.status, status, "{}", reply.text);
        (reply, audits(&captured, from))
    };
    let none: Vec<Value> = Vec::new();

    // Tenants.
    let (_, logged) = run(
        post(
            "/v1/admin/tenants",
            &admin,
            &json!({"id": "merchant-a", "name": "A"}),
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        logged,
        [event(
            "tenant_created",
            admin_id,
            &[("tenant", "merchant-a")]
        )]
    );
    for (body, status) in [
        (json!({"id": "merchant-a"}), StatusCode::CONFLICT),
        (json!({"id": "has space"}), StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (_, logged) = run(post("/v1/admin/tenants", &admin, &body), status).await;
        assert_eq!(logged, none, "{body}");
    }
    for (body, action) in [
        (json!({"name": "Lucky Shrub"}), "tenant_renamed"),
        (json!({"status": "suspended"}), "tenant_suspended"),
        (json!({"status": "active"}), "tenant_activated"),
    ] {
        let (_, logged) = run(
            Call::new(Method::PATCH, "/v1/admin/tenants/merchant-a")
                .key(&admin)
                .json(&body),
            StatusCode::OK,
        )
        .await;
        assert_eq!(
            logged,
            [event(action, admin_id, &[("tenant", "merchant-a")])],
            "{body}"
        );
    }
    let (_, logged) = run(
        Call::new(Method::PATCH, "/v1/admin/tenants/nobody")
            .key(&admin)
            .json(&json!({"status": "suspended"})),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(logged, none);
    let (_, logged) = run(
        Call::get("/v1/admin/tenants/merchant-a").key(&admin),
        StatusCode::OK,
    )
    .await;
    assert_eq!(logged, none, "a read is not audited");

    // A tenant's keys.
    let (minted, logged) = run(
        post(
            "/v1/admin/tenants/merchant-a/keys",
            &admin,
            &json!({"scopes": ["numbers"]}),
        ),
        StatusCode::CREATED,
    )
    .await;
    let key_id = minted.json()["api_key"]["key_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        logged,
        [event(
            "tenant_key_minted",
            admin_id,
            &[("tenant", "merchant-a"), ("key_id", &key_id)]
        )]
    );
    for (path, body, status) in [
        (
            "/v1/admin/tenants/nobody/keys",
            json!({"scopes": ["numbers"]}),
            StatusCode::NOT_FOUND,
        ),
        (
            "/v1/admin/tenants/merchant-a/keys",
            json!({"scopes": []}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (_, logged) = run(post(path, &admin, &body), status).await;
        assert_eq!(logged, none, "{path} {body}");
    }
    let (_, logged) = run(
        Call::new(
            Method::DELETE,
            format!("/v1/admin/tenants/merchant-a/keys/{key_id}"),
        )
        .key(&admin),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(
        logged,
        [event(
            "tenant_key_revoked",
            admin_id,
            &[("tenant", "merchant-a"), ("key_id", &key_id)]
        )]
    );
    let (_, logged) = run(
        Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a/keys/nokey").key(&admin),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(logged, none);

    // Platform keys.
    let (minted, logged) = run(
        post(
            "/v1/admin/platform-keys",
            &admin,
            &json!({"tenants": "*", "scopes": ["numbers"]}),
        ),
        StatusCode::CREATED,
    )
    .await;
    let platform_id = minted.json()["api_key"]["key_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        logged,
        [event(
            "platform_key_minted",
            admin_id,
            &[("key_id", &platform_id)]
        )]
    );
    let (_, logged) = run(
        post(
            "/v1/admin/platform-keys",
            &admin,
            &json!({"tenants": [], "scopes": ["numbers"]}),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(logged, none);
    let (_, logged) = run(
        Call::new(
            Method::DELETE,
            format!("/v1/admin/platform-keys/{platform_id}"),
        )
        .key(&admin),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(
        logged,
        [event(
            "platform_key_revoked",
            admin_id,
            &[("key_id", &platform_id)]
        )]
    );
    let (_, logged) = run(
        Call::new(Method::DELETE, "/v1/admin/platform-keys/nokey").key(&admin),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(logged, none);

    // Attaching: audited once the token is stored, even when subscribing
    // the app then fails (the WABA stays attached); refused, none.
    let attach = |tenant: &str, waba: &str| {
        post(
            &format!("/v1/admin/tenants/{tenant}/wabas"),
            &admin,
            &json!({"waba_id": waba, "token": SYSTEM_TOKEN}),
        )
    };
    h.graph.push_json(200, phone_numbers(&["1972385232742141"]));
    h.graph.push_json(200, json!({"success": true}));
    let (_, logged) = run(attach("merchant-a", WABA), StatusCode::CREATED).await;
    let attached = event(
        "waba_attached",
        admin_id,
        &[("tenant", "merchant-a"), ("waba_id", WABA)],
    );
    assert_eq!(logged, [attached]);
    h.graph.push_json(200, phone_numbers(&["1972385232742142"]));
    h.graph.push_json(
        403,
        json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException",
                         "code": 200, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let (_, logged) = run(attach("merchant-a", OTHER_WABA), StatusCode::FORBIDDEN).await;
    assert_eq!(
        logged,
        [event(
            "waba_attached",
            admin_id,
            &[("tenant", "merchant-a"), ("waba_id", OTHER_WABA)]
        )],
        "stored, then the subscription refused"
    );
    h.tenant("merchant-b").await;
    let (_, logged) = run(attach("merchant-b", WABA), StatusCode::CONFLICT).await;
    assert_eq!(logged, none, "D4's refusal");
    h.graph.push_json(200, phone_numbers(&[RACED_NUMBER]));
    let (_, logged) = run(attach("merchant-a", RACED_WABA), StatusCode::CONFLICT).await;
    assert_eq!(logged, none, "the binding's own D4 refusal");
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let (_, logged) = run(
        attach("merchant-b", "102290129340400"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(logged, none, "Meta refused the token: nothing bound");
    let (_, logged) = run(
        Call::get(format!("/v1/admin/wabas/{WABA}")).key(&admin),
        StatusCode::OK,
    )
    .await;
    assert_eq!(logged, none, "a read is not audited");

    // The vault's rotation.
    let (_, logged) = run(
        Call::new(Method::POST, "/v1/admin/vault/rotate").key(&admin),
        StatusCode::OK,
    )
    .await;
    assert_eq!(logged, [event("vault_rotated", admin_id, &[])]);

    // Unbinding.
    h.graph.push_json(200, json!({"success": true}));
    let (_, logged) = run(
        Call::new(
            Method::DELETE,
            format!("/v1/admin/wabas/{OTHER_WABA}/binding"),
        )
        .key(&admin),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(
        logged,
        [event(
            "waba_unbound",
            admin_id,
            &[("tenant", "merchant-a"), ("waba_id", OTHER_WABA)]
        )]
    );
    let (_, logged) = run(
        Call::new(
            Method::DELETE,
            format!("/v1/admin/wabas/{OTHER_WABA}/binding"),
        )
        .key(&admin),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(logged, none);

    // A tenant's deletion: Meta failing to unsubscribe logs nothing; then
    // each WABA's disconnection, then the deletion.
    h.graph.push_json(
        500,
        json!({"error": {"message": "An unexpected error occurred. Please retry your request",
                         "type": "GraphMethodException", "code": 2, "is_transient": true,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let delete = || Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a").key(&admin);
    let (_, logged) = run(delete(), StatusCode::BAD_GATEWAY).await;
    assert_eq!(logged, none);
    h.graph.push_json(200, json!({"success": true}));
    let (_, logged) = run(delete(), StatusCode::NO_CONTENT).await;
    assert_eq!(
        logged,
        [
            event(
                "waba_disconnected",
                admin_id,
                &[("tenant", "merchant-a"), ("waba_id", WABA)]
            ),
            event("tenant_deleted", admin_id, &[("tenant", "merchant-a")]),
        ]
    );
    let (_, logged) = run(delete(), StatusCode::NOT_FOUND).await;
    assert_eq!(logged, none);
    assert_eq!(h.graph.remaining(), 0);
    // The token is never logged, audited or not.
    assert!(!captured.text().contains(SYSTEM_TOKEN));
}
