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
        other => panic!("no fixture {other} here (tests/common/meta.rs)"),
    };
    serde_json::from_str(text).unwrap()
}

/// `payload` with every entry's id set to `waba` and every change's
/// `metadata.phone_number_id` to `pn`.
pub fn with_ids(mut payload: Value, waba: &str, pn: &str) -> Value {
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

/// Meta's text example with its ids, as bytes.
pub fn example_text() -> Vec<u8> {
    serde_json::to_vec(&fixture("messages/text.json")).unwrap()
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
            "time": 1_751_247_548,
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
