//! The M1.7 log capture, shared by `logs.rs` (memory) and
//! `live_logs.rs` (Postgres).

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

use meta_whatsapp_rs::webhooks::axum::http::Method;
use serde_json::json;

use super::meta::{
    EXAMPLE_BSUID, EXAMPLE_DISPLAY_NUMBER, EXAMPLE_NAME, EXAMPLE_TEXT, EXAMPLE_WA_ID, bytes,
    fixture, text, unknown_field, with_ids,
};
use super::{APP_SECRET, Call, Harness, PREVIOUS_APP_SECRET, VERIFY_TOKEN, send, signed};

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
        APP_SECRET.to_owned(),
        PREVIOUS_APP_SECRET.to_owned(),
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
            json!({"scopes": ["numbers", "events"]}),
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

    secrets.extend(webhooks(h, &tenant_key).await);
    secrets.extend(inbox(h, &admin).await);

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

    // Sends, media and templates (M1b), with what M1.7 keeps out of the
    // logs: message text, recipients, contacts.
    secrets.extend(send_media_and_templates(h, &admin).await);
    // OTP and authentication templates (roadmap M3c): the recipient's
    // number is not logged (the code's own case is `tests/otp_logs.rs`).
    secrets.extend(otp(h, &admin).await);
    // Payments (roadmap P2): the OAuth links are credentials.
    secrets.extend(payments(h, &admin).await);
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

const ECHO_TEXT: &str = "An echo the logs must not hold";
const HISTORY_TEXT: &str = "use code THANKS30";
const UNPARSED_TEXT: &str = "a body that is not a webhook, which the logs must not hold";
const OVERSIZED_TEXT: &str = "an oversized body the logs must not hold";
const FAILED_TEXT: &str = "a message whose recording failed once";

/// Coexistence: an echo of the merchant's phone app and a history sync; a
/// signed body that is not a webhook; one over 3 MiB; one whose recording
/// fails once (500), then goes through on Meta's redelivery. Returns the
/// texts they carried.
async fn more_deliveries(h: &Harness, waba: &str, pn: &str) -> Vec<String> {
    let mut echo = with_ids(fixture("fields/smb_message_echoes_text.json"), waba, pn);
    let item = &mut echo["entry"][0]["changes"][0]["value"]["message_echoes"][0];
    item["id"] = json!("wamid.CAPTURE-ECHO");
    item["text"]["body"] = json!(ECHO_TEXT);
    let history = with_ids(fixture("fields/history_threads.json"), waba, pn);
    let unparsed = format!("{{\"note\": \"{UNPARSED_TEXT}\"}}").into_bytes();
    for body in [bytes(&echo), bytes(&history), unparsed] {
        let reply = send(&h.public, signed(&body).build()).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.text);
    }
    let mut oversized = format!("{{\"note\": \"{OVERSIZED_TEXT}\"}}").into_bytes();
    oversized.resize(3 * 1024 * 1024 + 1, b' ');
    let reply = send(&h.public, signed(&oversized).build()).await;
    assert_eq!(reply.status.as_u16(), 413);
    let mut failing = text(waba, pn, "wamid.CAPTURE-FAILED");
    failing["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = json!(FAILED_TEXT);
    let failing = bytes(&failing);
    h.outbox.fail_next(1);
    assert_eq!(
        send(&h.public, signed(&failing).build())
            .await
            .status
            .as_u16(),
        500
    );
    assert_eq!(
        send(&h.public, signed(&failing).build())
            .await
            .status
            .as_u16(),
        200
    );
    [
        ECHO_TEXT,
        HISTORY_TEXT,
        UNPARSED_TEXT,
        OVERSIZED_TEXT,
        FAILED_TEXT,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Meta's deliveries for merchant-42's first number (Meta's text examples,
/// by phone number and by BSUID, a status, an error, a field no library
/// types, and refused ones), then polling them; returns what they carried
/// that the logs must not: the signatures, the message text, the customer's
/// name, BSUIDs and username.
async fn webhooks(h: &Harness, tenant_key: &str) -> Vec<String> {
    const WABA: &str = "102290129340398";
    const PN: &str = "1972385232742141";
    let by_phone = bytes(&text(WABA, PN, "wamid.CAPTURE-1"));
    let mut by_bsuid = with_ids(fixture("bsuid/text_username_no_wa_id.json"), WABA, PN);
    by_bsuid["entry"][0]["changes"][0]["value"]["messages"][0]["id"] = json!("wamid.CAPTURE-2");
    let bodies = [
        by_phone.clone(),
        bytes(&by_bsuid),
        bytes(&with_ids(fixture("messages/status_sent.json"), WABA, PN)),
        bytes(&with_ids(fixture("messages/errors.json"), WABA, PN)),
        bytes(&unknown_field(WABA)),
    ];
    let mut carried = Vec::new();
    for body in &bodies {
        let call = signed(body);
        let reply = send(&h.public, call.build()).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.text);
        let signature = meta_whatsapp_rs::webhooks::sign(
            &meta_whatsapp_rs::core::secret::AppSecret::new(APP_SECRET),
            body,
        );
        carried.push(signature.trim_start_matches("sha256=").to_owned());
    }
    carried.extend(more_deliveries(h, WABA, PN).await);
    // Refused: unsigned, and signed with another secret.
    let unsigned = Call::new(Method::POST, "/webhooks/meta").body(by_phone.clone().into());
    assert_eq!(send(&h.public, unsigned.build()).await.status.as_u16(), 401);
    let forged = meta_whatsapp_rs::webhooks::sign(
        &meta_whatsapp_rs::core::secret::AppSecret::new("a-forger-s-secret"),
        &by_phone,
    );
    let wrong = Call::new(Method::POST, "/webhooks/meta")
        .header("x-hub-signature-256", &forged)
        .body(by_phone.clone().into());
    assert_eq!(send(&h.public, wrong.build()).await.status.as_u16(), 401);
    carried.push(forged.trim_start_matches("sha256=").to_owned());
    // The tenant polls them: the answer holds what the logs must not.
    let polled = h.call(Call::get("/v1/events").key(tenant_key)).await;
    assert_eq!(polled.status.as_u16(), 200, "{}", polled.text);
    let types: Vec<String> = polled.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        types,
        [
            "message_received",
            "message_received",
            "status_updated",
            "error_reported",
            "message_echoed",
            "history_synced",
            "message_received"
        ]
    );
    for private in [
        EXAMPLE_TEXT,
        EXAMPLE_NAME,
        EXAMPLE_BSUID,
        EXAMPLE_WA_ID,
        ECHO_TEXT,
        HISTORY_TEXT,
        FAILED_TEXT,
    ] {
        assert!(polled.text.contains(private), "{private}: {}", polled.text);
    }
    // The same events by id and live (roadmap M2b): they carry what the
    // logs must not either.
    let first = polled.json()["data"][0]["id"].as_str().unwrap().to_owned();
    let one = h
        .call(Call::get(format!("/v1/events/{first}")).key(tenant_key))
        .await;
    assert_eq!(one.status.as_u16(), 200, "{}", one.text);
    assert!(one.text.contains(EXAMPLE_TEXT), "{}", one.text);
    for path in [
        "/v1/events/stream".to_owned(),
        format!("/v1/numbers/{PN}/events/stream"),
    ] {
        let live = h
            .call(
                Call::get(path.as_str())
                    .key(tenant_key)
                    .header("last-event-id", "0"),
            )
            .await;
        assert_eq!(live.status.as_u16(), 200, "{path}: {}", live.text);
        assert!(live.text.contains(EXAMPLE_TEXT), "{path}: {}", live.text);
    }
    carried.extend(
        [
            EXAMPLE_TEXT,
            EXAMPLE_NAME,
            EXAMPLE_BSUID,
            "US.ENT.11815799212886844830",
            "realsheenanelson",
            "wamid.CAPTURE-1",
        ]
        .map(str::to_owned),
    );
    carried
}

/// A reply's text, and a contact with no conversation, neither of which
/// may be logged.
pub const INBOX_REPLY_TEXT: &str = "Yes, the Echeveria comes in blue";
pub const INBOX_STRANGER: &str = "16505559876";
/// The caller's references for those replies.
pub const INBOX_KEYS: [&str; 3] = [
    "inbox-reply-order-1234",
    "inbox-reply-order-5678",
    "inbox-read-ticket-881",
];

/// The inbox routes (roadmaps M2a and M2g) on merchant-42's first number,
/// which [`webhooks`] filled: its conversations, the history of Meta's
/// example customer, a reply to them within the window, and one to a
/// contact with no conversation (refused `409`); their window, a message
/// by id, and the conversation marked read with a receipt to Meta; returns
/// the key it minted and what the logs must not hold (the reply's text,
/// the stranger, the caller's references).
async fn inbox(h: &Harness, admin: &str) -> Vec<String> {
    const PN: &str = "1972385232742141";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["inbox"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let listed = h
        .call(Call::get(format!("/v1/numbers/{PN}/conversations")).key(&key))
        .await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text);
    assert!(listed.text.contains(EXAMPLE_WA_ID), "{}", listed.text);
    let history = h
        .call(
            Call::get(format!(
                "/v1/numbers/{PN}/conversations/{EXAMPLE_WA_ID}/messages"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(history.status.as_u16(), 200, "{}", history.text);
    assert!(history.text.contains(EXAMPLE_TEXT), "{}", history.text);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp",
               "contacts": [{"input": RECIPIENT, "wa_id": RECIPIENT_DIGITS}],
               "messages": [{"id": "wamid.CAPTURE-REPLY"}]}),
    );
    let reply = |contact: &str, idempotency_key: &str| {
        Call::new(
            Method::POST,
            format!("/v1/numbers/{PN}/conversations/{contact}/messages"),
        )
        .key(&key)
        .header("idempotency-key", idempotency_key)
        .json(&json!({"type": "text", "text": {"body": INBOX_REPLY_TEXT},
                      "callback_data": CALLBACK_DATA}))
    };
    let replied = h.call(reply(EXAMPLE_WA_ID, INBOX_KEYS[0])).await;
    assert_eq!(replied.status.as_u16(), 202, "{}", replied.text);
    let refused = h.call(reply(INBOX_STRANGER, INBOX_KEYS[1])).await;
    assert_eq!(
        (refused.status.as_u16(), refused.code().as_str()),
        (409, "customer_service_window_closed"),
        "{}",
        refused.text
    );
    let window = h
        .call(
            Call::get(format!(
                "/v1/numbers/{PN}/conversations/{EXAMPLE_WA_ID}/window"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(window.status.as_u16(), 200, "{}", window.text);
    assert_eq!(window.json()["open"], true, "{}", window.text);
    let found = h
        .call(Call::get(format!("/v1/numbers/{PN}/messages/wamid.CAPTURE-1")).key(&key))
        .await;
    assert_eq!(found.status.as_u16(), 200, "{}", found.text);
    h.graph.push_json(200, json!({"success": true}));
    let asked = h.graph.requests().len();
    let read = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/numbers/{PN}/conversations/{EXAMPLE_WA_ID}/read"),
            )
            .key(&key)
            .header("idempotency-key", INBOX_KEYS[2])
            .json(&json!({"notify_customer": true})),
        )
        .await;
    assert_eq!(read.status.as_u16(), 204, "{}", read.text);
    assert_eq!(h.graph.requests().len(), asked + 1, "one read receipt");
    vec![
        key.clone(),
        key.rsplit('_').next().unwrap().to_owned(),
        INBOX_REPLY_TEXT.to_owned(),
        INBOX_STRANGER.to_owned(),
        INBOX_KEYS[0].to_owned(),
        INBOX_KEYS[1].to_owned(),
        INBOX_KEYS[2].to_owned(),
    ]
}

/// The OAuth links Meta answers in the capture: credentials the logs must
/// not hold.
pub const OAUTH_LINKS: [&str; 2] = [
    "https://www.facebook.com/payment/onboarding/init/?token=CAPTURE-OAUTH-LINK-1",
    "https://www.facebook.com/payment/onboarding/init/?token=CAPTURE-OAUTH-LINK-2",
];

/// The payment routes (roadmap P2) on merchant-42's WABA and first
/// number, and an `order_details` send: every route answered as Meta's
/// pages print it, a creation repeated under its key (answered without
/// its link); returns what the logs must not hold (the links, the key).
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn payments(h: &Harness, admin: &str) -> Vec<String> {
    const WABA: &str = "102290129340398";
    const PN: &str = "1972385232742141";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["payments", "send"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let configuration = json!({
        "configuration_name": "test-payment-configuration",
        "merchant_category_code": {"code": "0000", "description": "Test MCC Code"},
        "purpose_code": {"code": "00", "description": "Test Purpose Code"},
        "status": "Active",
        "provider_mid": "test-payment-gateway-mid",
        "provider_name": "RazorPay",
        "created_timestamp": 1_720_203_204,
        "updated_timestamp": 1_721_088_316
    });
    let base = format!("/v1/wabas/{WABA}/payment-configurations");
    h.graph.push_json(
        200,
        json!({"data": [{"payment_configurations": [configuration.clone()]}]}),
    );
    let listed = h.call(Call::get(&base).key(&key)).await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text);
    h.graph.push_json(
        200,
        json!({"oauth_url": OAUTH_LINKS[0], "expiration": 1_721_687_287, "success": true}),
    );
    let create = || {
        Call::new(Method::POST, &base)
            .key(&key)
            .header("idempotency-key", "payment-configuration-1")
            .json(&json!({"configuration_name": "test-payment-configuration",
                          "purpose_code": "00", "merchant_category_code": "0000",
                          "provider_name": "razorpay",
                          "redirect_url": "https://test-redirect-url.com"}))
    };
    let created = h.call(create()).await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text);
    assert!(created.text.contains(OAUTH_LINKS[0]), "the caller gets it");
    let again = h.call(create()).await;
    assert_eq!(again.status.as_u16(), 201, "{}", again.text);
    assert!(!again.text.contains("CAPTURE-OAUTH-LINK"), "{}", again.text);
    // A creation cut by a timeout may have gone through: audited, its
    // outcome unknown (design D58).
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let cut = h
        .call(
            Call::new(Method::POST, &base)
                .key(&key)
                .header("idempotency-key", "payment-configuration-2")
                .json(&json!({"configuration_name": "test-payment-configuration",
                              "provider_name": "payu"})),
        )
        .await;
    assert_eq!(cut.status.as_u16(), 504, "{}", cut.text);
    h.graph
        .push_json(200, json!({"data": [configuration.clone()]}));
    let one = h
        .call(Call::get(format!("{base}/test-payment-configuration")).key(&key))
        .await;
    assert_eq!(one.status.as_u16(), 200, "{}", one.text);
    h.graph.push_json(
        200,
        json!({"oauth_url": OAUTH_LINKS[1], "expiration": 1_721_687_287}),
    );
    let link = h
        .call(
            Call::new(
                Method::POST,
                format!("{base}/test-payment-configuration/oauth-link"),
            )
            .key(&key)
            .json(&json!({"redirect_url": "https://test-redirect-url.com"})),
        )
        .await;
    assert_eq!(link.status.as_u16(), 200, "{}", link.text);
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(Call::new(Method::DELETE, format!("{base}/test-payment-configuration")).key(&key))
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text);
    h.graph
        .push_json(200, json!({"data": [configuration.clone()]}));
    h.graph.push_json(
        200,
        json!({"payments": [{"reference_id": "BM345A-12", "status": "pending",
                             "currency": "INR", "amount": {"value": 21000, "offset": 100}}]}),
    );
    let payment = h
        .call(
            Call::get(format!(
                "/v1/numbers/{PN}/payments/test-payment-configuration/BM345A-12"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(payment.status.as_u16(), 200, "{}", payment.text);
    h.graph.push_json(200, json!({"data": [configuration]}));
    h.graph.push_json(
        200,
        json!({"id": "refund-id", "status": "pending", "speed_processed": "normal"}),
    );
    let refunded = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/payments/refunds"))
                .key(&key)
                .header("idempotency-key", "refund-1")
                .json(&json!({"reference_id": "BM345A-12", "speed": "normal",
                              "payment_config_id": "test-payment-configuration",
                              "amount": {"currency": "INR", "value": "100", "offset": "100"}})),
        )
        .await;
    assert_eq!(refunded.status.as_u16(), 201, "{}", refunded.text);
    // A refund cut by a timeout: it may have gone through, so it is
    // audited, its outcome unknown (design D58).
    h.graph.push_json(
        200,
        json!({"data": [{"configuration_name": "test-payment-configuration",
                         "status": "Active", "provider_name": "razorpay"}]}),
    );
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let cut = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/payments/refunds"))
                .key(&key)
                .header("idempotency-key", "refund-cut-1")
                .json(&json!({"reference_id": "BM345A-13",
                              "payment_config_id": "test-payment-configuration",
                              "amount": {"currency": "INR", "value": 250, "offset": 100}})),
        )
        .await;
    assert_eq!(cut.status.as_u16(), 504, "{}", cut.text);
    vec![
        key.clone(),
        key.rsplit('_').next().unwrap().to_owned(),
        OAUTH_LINKS[0].to_owned(),
        OAUTH_LINKS[1].to_owned(),
        "CAPTURE-OAUTH-LINK".to_owned(),
    ]
}

/// A message's text, a recipient, a contact and the caller's references,
/// none of which may be logged.
pub const MESSAGE_TEXT: &str = "Your order 1234 of Echeveria has shipped";
pub const RECIPIENT: &str = "+16505551234";
pub const RECIPIENT_DIGITS: &str = "16505551234";
pub const BSUID: &str = "US.13491208655302741918";
pub const CONTACT_NAME: &str = "Barbara J. Johnson";
pub const CONTACT_PHONE: &str = "+1 (940) 555-1234";
pub const CALLBACK_DATA: &str = "order:1234:shipped";
pub const IDEMPOTENCY_KEY: &str = "order-1234-shipped-notice";

/// The M1b routes, each exercised, with the tokens, text, recipients and
/// contacts M1.7 forbids in the logs; returns the key it minted and what
/// must not be logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn send_media_and_templates(h: &Harness, admin: &str) -> Vec<String> {
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["send", "media", "templates"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let pn = "1972385232742141";
    let sent_response = |input: &str, id_field: &str, id: &str| {
        let mut contact = json!({"input": input});
        contact[id_field] = json!(id);
        json!({"messaging_product": "whatsapp", "contacts": [contact],
               "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]})
    };
    // A text, with an Idempotency-Key, then its replay (no second request).
    h.graph
        .push_json(200, sent_response(RECIPIENT, "wa_id", RECIPIENT_DIGITS));
    let text = || {
        Call::new(Method::POST, format!("/v1/numbers/{pn}/messages"))
            .key(&key)
            .header("idempotency-key", IDEMPOTENCY_KEY)
            .json(&json!({"to": {"phone": RECIPIENT}, "type": "text",
                          "text": {"body": MESSAGE_TEXT}, "callback_data": CALLBACK_DATA}))
    };
    let sent = h.call(text()).await;
    assert_eq!(sent.status.as_u16(), 202, "{}", sent.text);
    let replayed = h.call(text()).await;
    assert_eq!(replayed.status.as_u16(), 202, "{}", replayed.text);
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
    // A contact card to a BSUID.
    h.graph
        .push_json(200, sent_response(BSUID, "user_id", BSUID));
    let card = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{pn}/messages"))
                .key(&key)
                .json(
                    &json!({"to": {"user_id": BSUID}, "type": "contacts", "contacts": [
                    {"name": {"formatted_name": CONTACT_NAME},
                     "phones": [{"phone": CONTACT_PHONE, "type": "Mobile"}]}]}),
                ),
        )
        .await;
    assert_eq!(card.status.as_u16(), 202, "{}", card.text);
    // Refusals: a number without its `+` (before any request), and Meta's
    // closed window.
    let local = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{pn}/messages"))
                .key(&key)
                .json(&json!({"to": {"phone": RECIPIENT_DIGITS}, "type": "text",
                              "text": {"body": MESSAGE_TEXT}})),
        )
        .await;
    assert_eq!(local.status.as_u16(), 422, "{}", local.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#131047) Re-engagement message", "type": "OAuthException",
                         "code": 131047, "error_data": {"details": "Message failed to send because more than 24 hours have passed since the customer last replied to this number."},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let twice = h
        .call(text().header("idempotency-key", "another-key"))
        .await;
    assert_eq!(
        (twice.status.as_u16(), twice.code().as_str()),
        (422, "invalid_request"),
        "two keys: {}",
        twice.text
    );
    let closed = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{pn}/messages"))
                .key(&key)
                .json(&json!({"to": {"phone": RECIPIENT}, "type": "text",
                              "text": {"body": MESSAGE_TEXT}})),
        )
        .await;
    assert_eq!(closed.status.as_u16(), 409, "{}", closed.text);
    // Read receipt with a typing indicator.
    h.graph.push_json(200, json!({"success": true}));
    let read = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/numbers/{pn}/messages/wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA/read"),
            )
            .key(&key)
            .json(&json!({"typing_indicator": true})),
        )
        .await;
    assert_eq!(read.status.as_u16(), 204, "{}", read.text);
    // Media: upload, download (whole, verified), delete.
    let png = b"\x89PNG\r\n\x1a\nvoucher".to_vec();
    h.graph.push_json(200, json!({"id": "1037543291543636"}));
    let uploaded = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{pn}/media"))
                .key(&key)
                .multipart(&[
                    ("type", None, b"image/png"),
                    ("file", Some("voucher.png"), &png),
                ]),
        )
        .await;
    assert_eq!(uploaded.status.as_u16(), 201, "{}", uploaded.text);
    let digest = sha256_hex(&png);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp",
               "url": "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636&ext=1&hash=abc",
               "mime_type": "image/png", "sha256": digest, "file_size": png.len().to_string(),
               "id": "1037543291543636"}),
    );
    h.graph.push_bytes(200, "image/png", png.clone());
    let downloaded = h
        .call(Call::get(format!("/v1/numbers/{pn}/media/1037543291543636")).key(&key))
        .await;
    assert_eq!(downloaded.status.as_u16(), 200, "{}", downloaded.text);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp",
               "url": "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636&ext=1&hash=abc",
               "mime_type": "image/png", "sha256": digest, "id": "1037543291543636"}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/numbers/{pn}/media/1037543291543636"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text);
    // Templates: list, one, create, delete.
    let waba = "102290129340398";
    h.graph.push_json(
        200,
        json!({"data": [{"name": "order_confirmation", "language": "en_US", "status": "APPROVED",
                         "category": "UTILITY", "id": "1407680676729941",
                         "components": [{"type": "BODY", "text": "Thank you for your order, {{1}}!"}]}]}),
    );
    let listed = h
        .call(Call::get(format!("/v1/wabas/{waba}/templates")).key(&key))
        .await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text);
    h.graph.push_json(
        200,
        json!({"name": "order_confirmation", "id": "1407680676729941"}),
    );
    h.graph.push_json(
        200,
        json!({"data": [{"name": "order_confirmation", "status": "APPROVED", "id": "1407680676729941"}]}),
    );
    let one = h
        .call(Call::get(format!("/v1/wabas/{waba}/templates/1407680676729941")).key(&key))
        .await;
    assert_eq!(one.status.as_u16(), 200, "{}", one.text);
    h.graph.push_json(
        200,
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
    );
    let created = h
        .call(
            Call::new(Method::POST, format!("/v1/wabas/{waba}/templates"))
                .key(&key)
                .json(&super::sample_template_definition()),
        )
        .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text);
    h.graph.push_json(200, json!({"success": true}));
    let removed = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/wabas/{waba}/templates?name=order_confirmation"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(removed.status.as_u16(), 204, "{}", removed.text);
    // One language by id (looked up among the WABA's templates of that
    // name), then an id that is not the WABA's: refused, not audited.
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1407680676729941", "name": "order_confirmation"}]}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let removed = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/wabas/{waba}/templates?name=order_confirmation&id=1407680676729941"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(removed.status.as_u16(), 204, "{}", removed.text);
    h.graph.push_json(200, json!({"data": []}));
    let refused = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/wabas/{waba}/templates?name=order_confirmation&id=1407680676729942"),
            )
            .key(&key),
        )
        .await;
    assert_eq!(refused.status.as_u16(), 404, "{}", refused.text);
    vec![
        key.clone(),
        key.rsplit('_').next().unwrap().to_owned(),
        MESSAGE_TEXT.to_owned(),
        RECIPIENT.to_owned(),
        RECIPIENT_DIGITS.to_owned(),
        BSUID.to_owned(),
        CONTACT_NAME.to_owned(),
        CONTACT_PHONE.to_owned(),
        CALLBACK_DATA.to_owned(),
        IDEMPOTENCY_KEY.to_owned(),
    ]
}

/// An OTP recipient: in no log line.
pub const OTP_PHONE: &str = "+16315554444";

/// The OTP routes and authentication templates (roadmap M3c), each
/// exercised: the tenant's settings set on its attached number, read, a
/// code sent and checked, an authentication template created, the
/// settings cleared; returns what must not be logged.
async fn otp(h: &Harness, admin: &str) -> Vec<String> {
    let saved = h
        .call(
            Call::new(Method::PUT, "/v1/admin/tenants/merchant-42/otp")
                .key(admin)
                .json(&json!({
                    "phone_number_id": "1972385232742141",
                    "template": {"name": "login_code", "language": "en_US"}
                })),
        )
        .await;
    assert_eq!(saved.status.as_u16(), 200, "{}", saved.text);
    let read = h
        .call(Call::get("/v1/admin/tenants/merchant-42/otp").key(admin))
        .await;
    assert_eq!(read.status.as_u16(), 200, "{}", read.text);
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["otp", "templates"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    h.graph.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": OTP_PHONE, "wa_id": "16315554444"}],
            "messages": [{"id": "wamid.HBgLMTYzMTU1NTQ0NDQVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
        }),
    );
    let sent = h
        .call(
            Call::new(Method::POST, "/v1/otp/issue")
                .key(&key)
                .json(&json!({"phone": OTP_PHONE})),
        )
        .await;
    assert_eq!(sent.json()["status"], "sent", "{}", sent.text);
    let checked = h
        .call(
            Call::new(Method::POST, "/v1/otp/verify")
                .key(&key)
                .json(&json!({"phone": OTP_PHONE, "code": "not-the-code"})),
        )
        .await;
    assert_eq!(checked.json()["result"], "invalid", "{}", checked.text);
    h.graph.push_json(
        200,
        json!({"data": [{"id": "954638012257287", "status": "APPROVED", "language": "en_US"}]}),
    );
    let created = h
        .call(
            Call::new(
                Method::POST,
                "/v1/wabas/102290129340398/templates/authentication",
            )
            .key(&key)
            .json(&json!({"name": "login_code", "languages": ["en_US"], "otp_type": "copy_code"})),
        )
        .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text);
    let cleared = h
        .call(Call::new(Method::DELETE, "/v1/admin/tenants/merchant-42/otp").key(admin))
        .await;
    assert_eq!(cleared.status.as_u16(), 204, "{}", cleared.text);
    vec![
        key,
        OTP_PHONE.to_owned(),
        "16315554444".to_owned(),
        "6315554444".to_owned(),
    ]
}

/// SHA-256 of `data`, hex.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(data))
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
    // A tenant's template deletions are audited too, each under its own
    // action (every language of a name, then one id; the refused one is
    // not), with the tenant, the WABA and its key's id.
    let deletions: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["target"] == "audit" && e["fields"]["message"] == "tenant change")
        .map(|e| &e["fields"])
        .collect();
    let actions: Vec<&str> = deletions
        .iter()
        .filter_map(|fields| fields["action"].as_str())
        .collect();
    assert_eq!(
        actions,
        [
            "templates_deleted",
            "template_deleted",
            "payment_configuration_created",
            "payment_configuration_created",
            "payment_oauth_link_regenerated",
            "payment_configuration_deleted",
            "payment_refunded",
            "payment_refunded"
        ],
        "the template deletions' and the payment changes' audit events"
    );
    for fields in &deletions {
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
    }
    check_payment_audit(&deletions);
}

/// The payment changes' audit events (of the tenant changes `changes`)
/// carry what reconciles them (design D58): the configuration, each
/// change's outcome, and for a refund the number, the order, the amount
/// and Meta's answer, or an unknown outcome when it timed out.
fn check_payment_audit(changes: &[&serde_json::Value]) {
    let payments: Vec<&serde_json::Value> = changes
        .iter()
        .copied()
        .filter(|fields| {
            fields["action"]
                .as_str()
                .is_some_and(|a| a.starts_with("payment_"))
        })
        .collect();
    for fields in &payments {
        assert_eq!(
            fields["configuration_name"], "test-payment-configuration",
            "{fields}"
        );
        assert!(fields.get("oauth_url").is_none(), "{fields}");
    }
    let outcomes: Vec<(&str, &str)> = payments
        .iter()
        .map(|fields| {
            (
                fields["action"].as_str().unwrap_or_default(),
                fields["outcome"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("payment_configuration_created", "answered"),
            ("payment_configuration_created", "unknown"),
            ("payment_oauth_link_regenerated", "answered"),
            ("payment_configuration_deleted", "answered"),
            ("payment_refunded", "answered"),
            ("payment_refunded", "unknown"),
        ],
        "each payment change's outcome"
    );
    let refunds: Vec<serde_json::Value> = payments
        .iter()
        .filter(|fields| fields["action"] == "payment_refunded")
        .map(|fields| {
            json!({
                "phone_number_id": fields["phone_number_id"],
                "reference_id": fields["reference_id"],
                "amount_value": fields["amount_value"],
                "amount_offset": fields["amount_offset"],
                "currency": fields["currency"],
                "refund_id": fields.get("refund_id"),
                "refund_status": fields.get("refund_status"),
                "outcome": fields["outcome"],
            })
        })
        .collect();
    assert_eq!(
        refunds,
        [
            json!({"phone_number_id": "1972385232742141", "reference_id": "BM345A-12",
                   "amount_value": 100, "amount_offset": 100, "currency": "INR",
                   "refund_id": "refund-id", "refund_status": "pending",
                   "outcome": "answered"}),
            json!({"phone_number_id": "1972385232742141", "reference_id": "BM345A-13",
                   "amount_value": 250, "amount_offset": 100, "currency": "INR",
                   "refund_id": null, "refund_status": null, "outcome": "unknown"}),
        ],
        "the refunds' audit events"
    );
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
    assert!(logged.contains(&("POST".to_owned(), "/webhooks/meta".to_owned())));
    assert!(
        logs.lines()
            .any(|l| l.contains("operator-only event recorded") && l.contains("data_sha256")),
        "the operator-only event is logged, by size and digest"
    );
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
        EXAMPLE_WA_ID,
        EXAMPLE_DISPLAY_NUMBER,
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
