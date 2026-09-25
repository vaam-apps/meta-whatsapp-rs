//! Meta's documented webhook payloads, as the library's tests keep them
//! (`crates/meta-whatsapp-webhooks/tests/fixtures`, copied from the
//! `webhooks/reference/*` pages), with the ids a test needs.

use serde_json::{Value, json};

/// The WABA of Meta's examples.
pub const EXAMPLE_WABA: &str = "102290129340398";
/// The business phone number id of Meta's examples.
pub const EXAMPLE_PN: &str = "106540352242922";
/// The message id of Meta's text example.
pub const EXAMPLE_WAMID: &str = "wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQTRBNjU5OUFFRTAzODEwMTQ0RgA=";
/// The customer's text in Meta's text example.
pub const EXAMPLE_TEXT: &str = "Does it come in another color?";
/// The customer's name in Meta's text example.
pub const EXAMPLE_NAME: &str = "Sheena Nelson";
/// The customer's `wa_id` (a phone number) in Meta's text example.
pub const EXAMPLE_WA_ID: &str = "16505551234";
/// The customer's BSUID in Meta's BSUID text example.
pub const EXAMPLE_BSUID: &str = "US.13491208655302741918";
/// The business's display number in Meta's examples.
pub const EXAMPLE_DISPLAY_NUMBER: &str = "15550783881";

/// A fixture of the library's tests, parsed.
pub fn fixture(name: &str) -> Value {
    let text = match name {
        "messages/text.json" => {
            include_str!("../../../meta-whatsapp-webhooks/tests/fixtures/messages/text.json")
        }
        "messages/status_sent.json" => {
            include_str!("../../../meta-whatsapp-webhooks/tests/fixtures/messages/status_sent.json")
        }
        "messages/errors.json" => {
            include_str!("../../../meta-whatsapp-webhooks/tests/fixtures/messages/errors.json")
        }
        "bsuid/text_username_no_wa_id.json" => include_str!(
            "../../../meta-whatsapp-webhooks/tests/fixtures/bsuid/text_username_no_wa_id.json"
        ),
        "fields/message_template_status_update_approved.json" => include_str!(
            "../../../meta-whatsapp-webhooks/tests/fixtures/fields/message_template_status_update_approved.json"
        ),
        "fields/partner_solutions.json" => include_str!(
            "../../../meta-whatsapp-webhooks/tests/fixtures/fields/partner_solutions.json"
        ),
        "fields/history_threads.json" => include_str!(
            "../../../meta-whatsapp-webhooks/tests/fixtures/fields/history_threads.json"
        ),
        other => panic!("no fixture {other} here (tests/common/meta.rs)"),
    };
    serde_json::from_str(text).unwrap()
}

/// Now, in Unix seconds.
pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// `value` with every `timestamp` (Meta's strings of Unix seconds) and
/// every entry's `time` set to `at`: Meta's examples are dated 2025, and
/// the service routes an event to a tenant only if it is not older than
/// the tenant's binding of its WABA.
pub fn dated(mut value: Value, at: i64) -> Value {
    fn walk(value: &mut Value, at: i64) {
        match value {
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    match (key.as_str(), &*item) {
                        ("timestamp", Value::String(_)) => *item = json!(at.to_string()),
                        ("timestamp" | "time", Value::Number(_)) => *item = json!(at),
                        _ => walk(item, at),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| walk(item, at)),
            _ => {}
        }
    }
    walk(&mut value, at);
    value
}

/// `payload` with every entry's id set to `waba` and every change's
/// `metadata.phone_number_id` to `pn`, dated now ([`dated`]).
pub fn with_ids(payload: Value, waba: &str, pn: &str) -> Value {
    let mut payload = dated(payload, now());
    for entry in payload["entry"].as_array_mut().unwrap() {
        entry["id"] = json!(waba);
        for change in entry["changes"].as_array_mut().unwrap() {
            if let Some(metadata) = change["value"].get_mut("metadata") {
                metadata["phone_number_id"] = json!(pn);
            }
        }
    }
    payload
}

/// Meta's text example, received by `pn` of `waba`, message id `wamid`.
pub fn text(waba: &str, pn: &str, wamid: &str) -> Value {
    let mut payload = with_ids(fixture("messages/text.json"), waba, pn);
    payload["entry"][0]["changes"][0]["value"]["messages"][0]["id"] = json!(wamid);
    payload
}

/// Meta's text example with its ids, dated now, as bytes.
pub fn example_text() -> Vec<u8> {
    serde_json::to_vec(&dated(fixture("messages/text.json"), now())).unwrap()
}

/// Meta's `sent` status example, for `pn` of `waba`.
pub fn status(waba: &str, pn: &str) -> Value {
    with_ids(fixture("messages/status_sent.json"), waba, pn)
}

/// Meta's template approval example (a WABA-level field), for `waba`.
pub fn template_approved(waba: &str) -> Value {
    with_ids(
        fixture("fields/message_template_status_update_approved.json"),
        waba,
        "",
    )
}

/// A field no library types yet, on `waba`: an `unknown` event.
pub fn unknown_field(waba: &str) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "time": now(),
            "changes": [{
                "field": "a_field_meta_adds_later",
                "value": {"metadata": {"phone_number_id": EXAMPLE_PN}, "note": "anything"}
            }]
        }]
    })
}

/// `value` as bytes.
pub fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
