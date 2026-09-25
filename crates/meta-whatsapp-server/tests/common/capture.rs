//! The M1.7 log capture, shared by `logs.rs` (memory) and
//! `live_postgres.rs` (Postgres).

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

use meta_whatsapp_rs::webhooks::axum::http::Method;
use serde_json::json;

use super::{Call, Harness, VERIFY_TOKEN, send};

/// Everything written, shared with the subscriber.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

const SYSTEM_TOKEN: &str = "EAAG-system-user-token-for-the-log-test";
pub const PHONE_1: &str = "+1 631-555-1111";
pub const PHONE_2: &str = "+1 631-555-2222";
pub const PHONE_3: &str = "+1 631-555-3333";

/// Runs admin and numbers calls carrying secrets and phone numbers, and
/// returns every secret they carried and what was logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
pub async fn exercise(h: &Harness) -> Vec<String> {
    let admin = h.admin_key().await;
    let mut secrets = vec![
        admin.clone(),
        SYSTEM_TOKEN.to_owned(),
        VERIFY_TOKEN.to_owned(),
    ];
    let post = |path: &str, key: &str, body: serde_json::Value| {
        Call::new(Method::POST, path).key(key).json(&body)
    };
    assert_eq!(
        h.call(post(
            "/v1/admin/tenants",
            &admin,
            json!({"id": "merchant-42", "name": "Lucky Shrub"})
        ))
        .await
        .status
        .as_u16(),
        201
    );
    let minted = h
        .call(post(
            "/v1/admin/tenants/merchant-42/keys",
            &admin,
            json!({"scopes": ["numbers"]}),
        ))
        .await
        .json();
    let tenant_key = minted["key"].as_str().unwrap().to_owned();
    let platform = h
        .call(post(
            "/v1/admin/platform-keys",
            &admin,
            json!({"tenants": "*", "scopes": ["numbers"]}),
        ))
        .await
        .json();
    let platform_key = platform["key"].as_str().unwrap().to_owned();
    secrets.push(tenant_key.clone());
    secrets.push(platform_key.clone());
    // Each key's secret part alone, too.
    for key in [&admin, &tenant_key, &platform_key] {
        secrets.push(key.rsplit('_').next().unwrap().to_owned());
    }

    // Attach: Meta lists two numbers, with their display numbers.
    h.graph.push_json(
        200,
        json!({"data": [
            {"id": "1972385232742141", "display_phone_number": PHONE_1, "verified_name": "John's Cake Shop"},
            {"id": "1972385232742142", "display_phone_number": PHONE_2, "verified_name": "John's Cake Shop"}
        ]}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let attached = h
        .call(post(
            "/v1/admin/tenants/merchant-42/wabas",
            &admin,
            json!({"waba_id": "102290129340398", "token": SYSTEM_TOKEN}),
        ))
        .await;
    assert_eq!(attached.status.as_u16(), 201, "{}", attached.text);

    // Numbers calls, by both keys, with Meta answering phone numbers.
    h.graph.push_json(
        200,
        json!({"id": "1972385232742141", "display_phone_number": PHONE_1, "quality_rating": "GREEN"}),
    );
    let details = h
        .call(Call::get("/v1/numbers/1972385232742141").key(&tenant_key))
        .await;
    assert!(
        details.text.contains(PHONE_1),
        "the answer has it; the logs must not"
    );
    h.graph.push_json(
        200,
        json!({"data": [{"about": "Hi", "email": "lucky@luckyshrub.com"}]}),
    );
    let profile = h
        .call(
            Call::get("/v1/numbers/1972385232742142/profile")
                .key(&platform_key)
                .tenant("merchant-42"),
        )
        .await;
    assert_eq!(profile.status.as_u16(), 200, "{}", profile.text);
    h.graph.push_json(200, json!({"success": true}));
    h.graph
        .push_json(200, json!({"data": [{"about": "Hello"}]}));
    let patched = h
        .call(
            Call::new(Method::PATCH, "/v1/numbers/1972385232742141/profile")
                .key(&tenant_key)
                .json(&json!({"about": "Hello"})),
        )
        .await;
    assert_eq!(patched.status.as_u16(), 200, "{}", patched.text);
    let listed = h.call(Call::get("/v1/numbers").key(&tenant_key)).await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text);
    // Failures log too: a refused key, and Meta refusing a token.
    let refused = h
        .call(Call::get("/v1/numbers").key(&format!("{tenant_key}x")))
        .await;
    assert_eq!(refused.status.as_u16(), 401, "{}", refused.text);
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let rejected = h
        .call(Call::get("/v1/numbers/1972385232742141").key(&tenant_key))
        .await;
    assert_eq!(
        (rejected.status.as_u16(), rejected.code().as_str()),
        (409, "reconnect_required"),
        "{}",
        rejected.text
    );
    // Meta's subscription check, with the verify token in the query, and a
    // made-up method (never logged as sent).
    let verified = send(
        &h.public,
        Call::get(format!(
            "/webhooks/meta?hub.mode=subscribe&hub.challenge=1&hub.verify_token={VERIFY_TOKEN}"
        ))
        .build(),
    )
    .await;
    assert_eq!(
        (verified.status.as_u16(), verified.text.as_str()),
        (200, "1")
    );
    let made_up = Method::from_bytes(MADE_UP_METHOD.as_bytes()).unwrap();
    let odd = send(&h.public, Call::new(made_up, "/livez").build()).await;
    assert_eq!(odd.status.as_u16(), 405, "{}", odd.text);

    // Every other operation of the committed document, so that `check`
    // finds each one in the logs.
    let get = |path: &str, key: &str| Call::get(path).key(key);
    let delete = |path: &str, key: &str| Call::new(Method::DELETE, path).key(key);
    for call in [
        get("/v1/admin/tenants", &admin),
        get("/v1/admin/tenants/merchant-42", &admin),
        Call::new(Method::PATCH, "/v1/admin/tenants/merchant-42")
            .key(&admin)
            .json(&json!({"name": "Lucky Shrub Ltd"})),
        get("/v1/admin/tenants/merchant-42/keys", &admin),
        get("/v1/admin/platform-keys", &admin),
        get("/v1/wabas", &tenant_key),
    ] {
        // Exercised, not merely logged: each answers 200.
        let reply = h.call(call).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.text);
    }
    // Spare keys to revoke, a spare tenant to delete.
    let spare = h
        .call(post(
            "/v1/admin/tenants/merchant-42/keys",
            &admin,
            json!({"scopes": ["numbers"]}),
        ))
        .await
        .json();
    let spare_platform = h
        .call(post(
            "/v1/admin/platform-keys",
            &admin,
            json!({"tenants": ["merchant-42"], "scopes": ["numbers"]}),
        ))
        .await
        .json();
    for minted in [&spare, &spare_platform] {
        secrets.push(minted["key"].as_str().unwrap().to_owned());
    }
    let spare_tenant = h
        .call(post(
            "/v1/admin/tenants",
            &admin,
            json!({"id": "spare-tenant"}),
        ))
        .await;
    assert_eq!(spare_tenant.status.as_u16(), 201, "{}", spare_tenant.text);
    for call in [
        delete(
            &format!(
                "/v1/admin/tenants/merchant-42/keys/{}",
                spare["api_key"]["key_id"].as_str().unwrap()
            ),
            &admin,
        ),
        delete(
            &format!(
                "/v1/admin/platform-keys/{}",
                spare_platform["api_key"]["key_id"].as_str().unwrap()
            ),
            &admin,
        ),
        delete("/v1/admin/tenants/spare-tenant", &admin),
    ] {
        assert_eq!(h.call(call).await.status.as_u16(), 204);
    }
    // A second WABA, attached then unbound; the first one disconnected by
    // its tenant.
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1972385232742143", "display_phone_number": PHONE_3}]}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let second = h
        .call(post(
            "/v1/admin/tenants/merchant-42/wabas",
            &admin,
            json!({"waba_id": "102290129340399", "token": SYSTEM_TOKEN}),
        ))
        .await;
    assert_eq!(second.status.as_u16(), 201, "{}", second.text);
    assert_eq!(
        h.call(get("/v1/admin/wabas/102290129340399", &admin))
            .await
            .status
            .as_u16(),
        200
    );
    h.graph.push_json(200, json!({"success": true}));
    let unbound = h
        .call(delete("/v1/admin/wabas/102290129340399/binding", &admin))
        .await;
    assert_eq!(unbound.status.as_u16(), 204, "{}", unbound.text);
    h.graph.push_json(200, json!({"success": true}));
    let disconnected = h
        .call(delete("/v1/wabas/102290129340398", &tenant_key))
        .await;
    assert_eq!(disconnected.status.as_u16(), 204, "{}", disconnected.text);
    let rotated = h
        .call(Call::new(Method::POST, "/v1/admin/vault/rotate").key(&admin))
        .await;
    assert_eq!(rotated.status.as_u16(), 200, "{}", rotated.text);
    // Operations, keyless.
    for path in [
        "/livez",
        "/readyz",
        "/metrics",
        "/v1/openapi.json",
        "/v1/version",
    ] {
        assert_eq!(h.call(Call::get(path)).await.status.as_u16(), 200, "{path}");
    }
    assert_eq!(h.graph.remaining(), 0);
    secrets
}

/// A method no client sends, which the logs must not repeat.
const MADE_UP_METHOD: &str = "MADEUPMETHODFORTHELOGS";

/// A JSON subscriber at `TRACE` for every target, writing to the capture.
pub fn subscriber(captured: &Captured) -> impl tracing::Subscriber + Send + Sync + 'static {
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(move || writer.clone())
        .finish()
}

/// `(method, route)` of every request span in the captured JSON logs.
fn logged_requests(logs: &str) -> BTreeSet<(String, String)> {
    let mut requests = BTreeSet::new();
    for line in logs.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let span = &event["span"];
        if span["name"] == "request"
            && let (Some(method), Some(route)) = (span["method"].as_str(), span["route"].as_str())
        {
            requests.insert((method.to_owned(), route.to_owned()));
        }
    }
    requests
}

/// Every change an operator made (a successful non-GET on `/v1/admin`) has
/// an `audit` event in its request, and every keyed request that passed
/// step 1 names its key's public id (security review L4).
fn check_audit(logs: &str) {
    let events: Vec<serde_json::Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let audited: BTreeSet<String> = events
        .iter()
        .filter(|e| e["target"] == "audit")
        .filter_map(|e| e["span"]["request_id"].as_str().map(str::to_owned))
        .collect();
    let mut changes = 0;
    for event in &events {
        let span = &event["span"];
        if event["fields"]["message"] != "request" || span["name"] != "request" {
            continue;
        }
        let status = event["fields"]["status"].as_u64().unwrap_or_default();
        let route = span["route"].as_str().unwrap_or_default();
        let keyed =
            route.starts_with("/v1/") && !matches!(route, "/v1/openapi.json" | "/v1/version");
        if keyed && (200..300).contains(&status) {
            assert!(span["key_id"].is_string(), "no key_id on {span}");
        }
        if route.starts_with("/v1/admin/")
            && span["method"] != "GET"
            && (200..300).contains(&status)
        {
            changes += 1;
            let id = span["request_id"].as_str().unwrap();
            assert!(audited.contains(id), "no audit event for {span}");
        }
    }
    assert!(changes >= 10, "{changes} admin changes");
}

/// Every success status an operation answered is one its document lists
/// (conventions review S1; errors fall under each operation's `default`).
fn check_declared_statuses(logs: &str) {
    let spec: serde_json::Value = serde_json::from_str(super::SPEC).unwrap();
    for line in logs.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let span = &event["span"];
        if event["fields"]["message"] != "request" || span["name"] != "request" {
            continue;
        }
        let status = event["fields"]["status"].as_u64().unwrap_or_default();
        let (Some(method), Some(route)) = (span["method"].as_str(), span["route"].as_str()) else {
            continue;
        };
        let operation = &spec["paths"][route][method.to_lowercase()];
        if operation.is_null() || !(200..300).contains(&status) {
            continue;
        }
        assert!(
            operation["responses"].get(status.to_string()).is_some(),
            "{method} {route} answered {status}, which its document does not list"
        );
    }
}

/// What M1.7 asks of the captured logs.
pub fn check(logs: &str, secrets: &[String]) {
    check_audit(logs);
    check_declared_statuses(logs);
    // The capture works, and covers every operation of the committed
    // document: requests are logged with their route templates.
    let logged = logged_requests(logs);
    for operation in super::spec_operations() {
        let wanted = (operation.method.to_string(), operation.template.clone());
        assert!(
            logged.contains(&wanted),
            "{} was not exercised (tests/common/capture.rs): {logged:?}",
            operation.label()
        );
    }
    assert!(logged.contains(&("GET".to_owned(), "/webhooks/meta".to_owned())));
    assert!(logged.contains(&("other".to_owned(), "/livez".to_owned())));
    assert!(
        !logs.contains(MADE_UP_METHOD),
        "a made-up method was logged"
    );
    assert!(logs.contains("\"tenant\":\"merchant-42\""));
    assert!(
        logs.contains("graph request"),
        "the library's events are captured"
    );
    for secret in secrets {
        assert!(
            !logs.contains(secret.as_str()),
            "a secret was logged:\n{logs}"
        );
    }
    for phone in [
        PHONE_1,
        PHONE_2,
        PHONE_3,
        "16315551111",
        "6315551111",
        "6315553333",
    ] {
        assert!(
            !logs.contains(phone),
            "a phone number was logged: {phone}\n{logs}"
        );
    }
    // Meta's error message goes to debug logs only.
    let with_message: Vec<&str> = logs
        .lines()
        .filter(|l| l.contains("Invalid OAuth access token"))
        .collect();
    assert!(!with_message.is_empty(), "the capture has the debug line");
    for line in with_message {
        assert!(line.contains("\"level\":\"DEBUG\""), "{line}");
    }
}
