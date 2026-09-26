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
        ["templates_deleted", "template_deleted"],
        "the template deletions' audit events"
    );
    for fields in deletions {
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
    }
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
