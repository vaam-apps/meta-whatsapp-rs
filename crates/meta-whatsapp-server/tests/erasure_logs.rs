//! Roadmap M2f's log half: the erasure journal and the audit event hold
//! no identity in clear, and no log line holds the customer. Alone in its
//! binary, as `logs.rs` is: the capture is a thread's default subscriber,
//! and `tracing` caches whether a call site is enabled across threads, so
//! a test running beside it could leave the capture's call sites
//! disabled.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::capture::{Captured, subscriber};
use common::erasure::{erase, erased, merchant, needles, the_customer_writes};
use common::meta::{EXAMPLE_BSUID, EXAMPLE_PN, EXAMPLE_WA_ID};
use common::{ERASURE_KEY, Harness};
use pretty_assertions::assert_eq;
use serde_json::Value;

/// HMAC-SHA256 (RFC 2104) of `message` under `key`, with SHA-256 alone:
/// computed apart from the service's code.
fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    assert!(key.len() <= 64);
    let mut block = [0u8; 64];
    block[..key.len()].copy_from_slice(key);
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    let outer = Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize();
    hex::encode(outer)
}

/// The captured log lines, as JSON.
fn lines(captured: &Captured) -> Vec<Value> {
    captured
        .text()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Decisive: the journal and the audit event name no identity in clear.
/// Over the whole scenario, at `TRACE` for every target, no log line holds
/// the customer; the erasure writes one journal entry (target
/// `erasure_journal`), whose identities are the HMAC-SHA256 of
/// `<phone_number_id>|<contact>` under the deployment's key of each
/// identity the inbox connects (computed here apart from the code), and
/// one audit event (`customer_erased`) with the same id, ids and counts
/// only. Malformed bodies naming the customer (a number where a string
/// goes, an unknown field named after them, a cut body, the customer's
/// number as `confirm`) are `422` quoting nothing, and log nothing of
/// them. Decisive: an identity in either, the HMAC's input, a body
/// parser's error text reaching the answer or a log.
#[tokio::test]
async fn the_journal_and_the_audit_event_hold_no_identity_in_clear() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let (_, eraser) = merchant(&h).await;
    the_customer_writes(&h).await;
    // Malformed bodies naming the customer, whose parser errors would
    // quote them (serde's `invalid type: integer `…``, `unknown field
    // `…``): `422` on `body` or `confirm`, quoting nothing, logging
    // nothing of them (the sweep below).
    let path = format!("/v1/numbers/{EXAMPLE_PN}/erasures");
    let wa_id: u64 = EXAMPLE_WA_ID.parse().unwrap();
    for (body, field) in [
        (
            serde_json::json!({"confirm": EXAMPLE_PN, "contacts": [wa_id]}).to_string(),
            "body",
        ),
        (
            serde_json::json!({"confirm": EXAMPLE_PN, "contacts": [EXAMPLE_BSUID], EXAMPLE_BSUID: 1})
                .to_string(),
            "body",
        ),
        (
            format!("{{\"confirm\": \"{EXAMPLE_PN}\", \"contacts\": [\"{EXAMPLE_WA_ID}\""),
            "body",
        ),
        (
            serde_json::json!({"confirm": EXAMPLE_WA_ID, "contacts": [EXAMPLE_WA_ID]}).to_string(),
            "confirm",
        ),
    ] {
        let refused = h
            .call(
                common::Call::new(meta_whatsapp_rs::webhooks::axum::http::Method::POST, &path)
                    .key(&eraser)
                    .header("content-type", "application/json")
                    .body(body.clone().into()),
            )
            .await;
        assert_eq!(refused.status.as_u16(), 422, "{body}: {}", refused.text);
        assert_eq!(refused.json()["error"]["field"], field, "{body}");
        for needle in needles() {
            assert!(!refused.text.contains(needle), "{body}: {}", refused.text);
        }
    }
    let reply = h
        .call(erase(&eraser, EXAMPLE_PN, EXAMPLE_PN, &[EXAMPLE_BSUID]))
        .await;
    let id = erased(&reply, EXAMPLE_PN);
    let logs = captured.text();
    for needle in needles() {
        assert!(!logs.contains(needle), "{needle} was logged:\n{logs}");
    }
    let events = lines(&captured);
    let journal: Vec<&Value> = events
        .iter()
        .filter(|e| e["target"] == "erasure_journal")
        .map(|e| &e["fields"])
        .collect();
    assert_eq!(journal.len(), 1, "{journal:?}");
    let entry = journal[0];
    let mut expected: Vec<String> = [EXAMPLE_BSUID, EXAMPLE_WA_ID]
        .iter()
        .map(|contact| {
            hmac_sha256(
                ERASURE_KEY.as_bytes(),
                format!("{EXAMPLE_PN}|{contact}").as_bytes(),
            )
        })
        .collect();
    expected.sort();
    assert_eq!(entry["identities"], expected.join(","), "{entry}");
    assert_eq!(entry["erasure_id"], id, "{entry}");
    assert_eq!(entry["tenant"], "merchant-42", "{entry}");
    assert_eq!(entry["phone_number_id"], EXAMPLE_PN, "{entry}");
    assert_eq!(entry["v"], 1, "{entry}");
    assert!(entry["at"].is_string(), "{entry}");
    let audit: Vec<&Value> = events
        .iter()
        .filter(|e| e["target"] == "audit" && e["fields"]["action"] == "customer_erased")
        .map(|e| &e["fields"])
        .collect();
    assert_eq!(audit.len(), 1, "{audit:?}");
    let audit = audit[0];
    assert_eq!(audit["erasure_id"], id, "{audit}");
    assert_eq!(audit["tenant"], "merchant-42", "{audit}");
    assert_eq!(audit["phone_number_id"], EXAMPLE_PN, "{audit}");
    assert_eq!(audit["identities"], 2, "{audit}");
    assert_eq!(audit["events"], 3, "{audit}");
    assert_eq!(audit["conversations"], 2, "{audit}");
    assert!(audit["key_id"].is_string(), "{audit}");
}
