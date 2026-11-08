//! Roadmap M5c6: a WABA's health status and a number's webhook override,
//! in the core alone (no HTTP layer, so none of the API adapter's own
//! gate): the services over a backend's records and vault and a Graph
//! client on a `ScriptedTransport`.
//!
//! 1. The WABA's health: a caller that is not the WABA's tenant reads
//!    `404 not_found` and nothing is sent; the tenant's own is filtered
//!    (another WABA's entity, a number not bound to the tenant and an
//!    unknown type dropped).
//! 2. The override: with the deployment's setting off a set is `403
//!    feature_not_available` and sends nothing, whatever the URL; with no
//!    webhooks out `409 webhooks_not_configured`; with both, a destination
//!    the endpoints' check refuses is `422` on `url` and sends nothing, a
//!    good one is the documented request; clearing is allowed in every
//!    state.

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, VerifyToken};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::json;

use crate::admin::mint;
use crate::authz::{Authorizer, Caller};
use crate::backend::Backend;
use crate::model::{BindOutcome, KeyOwner, Scope, TenantId};
use crate::number_settings::NumberSettings;
use crate::numbers::Waba;
use crate::webhooks_out::destination::{DestinationPolicy, ResolveError, Resolver};
use crate::webhooks_out::dispatch::{SendError, SendRequest, Sender, WebhooksOut};
use crate::webhooks_out::seal::{DataKey, DataKeys};

const OWNER: (&str, &str, &str, &str) = (
    "ovr-owner",
    "102290129340420",
    "106540352242950",
    "EAAG-the-override-owners-token",
);
const OTHER: (&str, &str, &str, &str) = (
    "ovr-other",
    "102290129340421",
    "106540352242951",
    "EAAG-the-override-others-token",
);
const HOOK: &str = "https://hooks.example.com/wa";

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    the_waba_health_and_the_webhook_override_services_gate_and_filter(backend).await;
}

struct Names;

#[async_trait]
impl Resolver for Names {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<IpAddr>, ResolveError> {
        match host {
            "hooks.example.com" => Ok(vec!["93.184.216.34".parse().unwrap()]),
            "inside.example.com" => Ok(vec!["10.0.0.5".parse().unwrap()]),
            _ => Err(ResolveError),
        }
    }
}

struct Nothing;

#[async_trait]
impl Sender for Nothing {
    async fn send(&self, _: SendRequest) -> Result<u16, SendError> {
        Ok(200)
    }
}

fn webhooks_out() -> WebhooksOut {
    WebhooksOut::new(
        DataKeys::new(DataKey::generate("override-suite").unwrap()),
        DestinationPolicy::parse("hooks.example.com, inside.example.com", false).unwrap(),
        Arc::new(Names),
        Arc::new(Nothing),
    )
}

async fn caller_of(backend: &dyn Backend, authz: &Authorizer, tenant: &str) -> Caller {
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(TenantId::parse(tenant).unwrap()),
        vec![Scope::Numbers],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Numbers,
        )
        .await
        .unwrap()
}

/// See the module.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
pub async fn the_waba_health_and_the_webhook_override_services_gate_and_filter(
    backend: &dyn Backend,
) {
    let records = backend.records();
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("override-suite").unwrap()),
    )
    .unwrap();
    for (tenant, waba, pn, token) in [OWNER, OTHER] {
        let tenant = TenantId::parse(tenant).unwrap();
        records.create_tenant(&tenant, "").await.unwrap();
        let waba = WabaId::new(waba).unwrap();
        let pn = PhoneNumberId::new(pn).unwrap();
        let bound = records
            .bind_waba(&tenant, &waba, std::slice::from_ref(&pn))
            .await
            .unwrap();
        assert_eq!(bound, BindOutcome::Bound);
        vault
            .store(&StoredBusinessToken::new(waba, AccessToken::new(token)).phone_number_ids([pn]))
            .await
            .unwrap();
    }
    let script = ScriptedTransport::new();
    let client = Client::builder().transport(script.clone()).build().unwrap();
    let authz = Authorizer::new(records.clone(), vault.clone(), client).unwrap();
    let owner = caller_of(backend, &authz, OWNER.0).await;
    let other = caller_of(backend, &authz, OTHER.0).await;
    let owners_waba = authz
        .owned_waba(&owner, WabaId::new(OWNER.1).unwrap())
        .await
        .unwrap();
    let owners_number = authz
        .owned_number(&owner, PhoneNumberId::new(OWNER.2).unwrap())
        .await
        .unwrap();

    // 1. The WABA's health.
    let waba = Waba::new(&authz, &owners_waba).unwrap();
    let refused = waba.health_status(&other).await.unwrap_err();
    assert_eq!((refused.status(), refused.code()), (404, "not_found"));
    assert!(script.requests().is_empty(), "a request was sent");
    script.push_json(
        200,
        json!({"health_status": {"can_send_message": "LIMITED", "entities": [
            {"entity_type": "WABA", "id": OWNER.1, "can_send_message": "AVAILABLE"},
            {"entity_type": "WABA", "id": OTHER.1, "can_send_message": "BLOCKED"},
            {"entity_type": "PHONE_NUMBER", "id": OWNER.2, "can_send_message": "AVAILABLE"},
            {"entity_type": "PHONE_NUMBER", "id": OTHER.2, "can_send_message": "BLOCKED"},
            {"entity_type": "BUSINESS", "id": "2729063490586005"},
            {"entity_type": "APP", "id": "634974688087057"},
            {"entity_type": "FROM_THE_FUTURE", "id": "1"}
        ]}, "id": OWNER.1}),
    );
    let health = waba.health_status(&owner).await.unwrap();
    let kept: Vec<&str> = health.entities.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        kept,
        [OWNER.1, OWNER.2, "2729063490586005", "634974688087057"]
    );
    let request = script.last_request().unwrap();
    assert_eq!(request.bearer(), Some(OWNER.3));
    assert_eq!(request.path(), format!("/v25.0/{}", OWNER.1));
    let health_requests = script.requests().len();

    // 2. The override.
    let token = || VerifyToken::new("myvoiceismypassport?");
    let out = webhooks_out();
    let off = NumberSettings::new(&authz, &owner, &owners_number)
        .unwrap()
        .with_webhook_override(false, Some(&out));
    let unconfigured = NumberSettings::new(&authz, &owner, &owners_number)
        .unwrap()
        .with_webhook_override(true, None);
    let on = NumberSettings::new(&authz, &owner, &owners_number)
        .unwrap()
        .with_webhook_override(true, Some(&out));
    // The setting off: refused, whatever the URL, nothing sent.
    let refused = off.set_webhook_override(HOOK, token()).await.unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (403, "feature_not_available")
    );
    // No webhooks out: the destination is never trusted unchecked.
    let refused = unconfigured
        .set_webhook_override(HOOK, token())
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (409, "webhooks_not_configured")
    );
    // A destination a webhook endpoint would be refused at.
    for hook in [
        "https://10.0.0.1/wa",
        "https://inside.example.com/wa",
        "http://hooks.example.com/wa",
        "https://u:p@hooks.example.com/wa",
        "https://evil.example/wa",
        "",
    ] {
        let refused = on.set_webhook_override(hook, token()).await.unwrap_err();
        assert_eq!(
            (refused.status(), refused.code(), refused.field()),
            (422, "invalid_request", Some("url")),
            "{hook}"
        );
    }
    let refused = on
        .set_webhook_override(HOOK, VerifyToken::new(""))
        .await
        .unwrap_err();
    assert_eq!(refused.field(), Some("verify_token"));
    assert_eq!(
        script.requests().len(),
        health_requests,
        "a request was sent"
    );
    // A good one is the documented request.
    script.push_json(200, json!({"success": true}));
    on.set_webhook_override(HOOK, token()).await.unwrap();
    let request = script.last_request().unwrap();
    assert_eq!(request.bearer(), Some(OWNER.3));
    assert_eq!(
        request.json(),
        Some(json!({"webhook_configuration": {
            "override_callback_uri": HOOK, "verify_token": "myvoiceismypassport?"
        }}))
    );
    // Clearing, in every state.
    for service in [&off, &unconfigured, &on] {
        script.push_json(200, json!({"success": true}));
        service.clear_webhook_override().await.unwrap();
        assert_eq!(
            script.last_request().unwrap().json(),
            Some(json!({"webhook_configuration": {"override_callback_uri": ""}}))
        );
    }
    assert_eq!(script.requests().len(), health_requests + 4);
}
