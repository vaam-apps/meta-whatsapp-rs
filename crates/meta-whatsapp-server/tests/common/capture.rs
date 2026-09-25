//! The M1.7 log capture, shared by `logs.rs` (memory) and
//! `live_postgres.rs` (Postgres).

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
    let _ = h
        .call(
            Call::get("/v1/numbers/1972385232742142/profile")
                .key(&platform_key)
                .tenant("merchant-42"),
        )
        .await;
    h.graph.push_json(200, json!({"success": true}));
    h.graph
        .push_json(200, json!({"data": [{"about": "Hello"}]}));
    let _ = h
        .call(
            Call::new(Method::PATCH, "/v1/numbers/1972385232742141/profile")
                .key(&tenant_key)
                .json(&json!({"about": "Hello"})),
        )
        .await;
    let _ = h.call(Call::get("/v1/numbers").key(&tenant_key)).await;
    // Failures log too: a refused key, and Meta refusing a token.
    let _ = h
        .call(Call::get("/v1/numbers").key(&format!("{tenant_key}x")))
        .await;
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let _ = h
        .call(Call::get("/v1/numbers/1972385232742141").key(&tenant_key))
        .await;
    // Meta's subscription check, with the verify token in the query.
    let _ = send(
        &h.public,
        Call::get(format!(
            "/webhooks/meta?hub.mode=subscribe&hub.challenge=1&hub.verify_token={VERIFY_TOKEN}"
        ))
        .build(),
    )
    .await;
    assert_eq!(h.graph.remaining(), 0);
    secrets
}

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

/// What M1.7 asks of the captured logs.
pub fn check(logs: &str, secrets: &[String]) {
    // The capture works: requests are logged, with their route templates.
    assert!(logs.contains("\"route\":\"/v1/numbers/{pn}\""), "{logs}");
    assert!(logs.contains("\"route\":\"/v1/admin/tenants/{id}/wabas\""));
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
    for phone in [PHONE_1, PHONE_2, "16315551111", "6315551111"] {
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
