//! Roadmap M3d's log half (decision D6): captured at `TRACE` for every
//! target (the library's, the service's), a registration's whole flow logs
//! neither the PIN, nor a wrong PIN Meta quoted back in every text of its
//! refusal, nor the verification code; nor any of Meta's texts on these
//! routes (every other route logs them at `debug`). The PIN change and the
//! deregistration are audited, with the tenant, the key, the WABA and the
//! number's id. Alone in its binary, as `logs.rs` is: the capture is a
//! thread's default subscriber, and `tracing` caches whether a call site
//! is enabled across threads.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::Harness;
use common::capture::{Captured, subscriber};
use common::registration::{self as flow, PN, TENANT, WABA, connected};

/// Decisive: `OwnedNumber::failed_withholding_texts` on the registration
/// services (with `failed`, Meta's texts, which quote the wrong PIN, are
/// logged at `debug`), and every log line's fields.
#[tokio::test]
async fn a_registration_logs_no_pin_no_code_and_none_of_metas_texts() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let key = connected(&h).await;
    flow::flow(&h, &key).await;
    let logs = captured.text();
    assert!(
        logs.contains("/v1/numbers/{pn}/register") && logs.contains("graph request"),
        "the capture saw the requests and the library's events:\n{logs}"
    );
    assert!(
        logs.contains("AXsgnV2Cm3ZMGF3dF_cfYIn"),
        "Meta's trace id is logged"
    );
    flow::logs_hold_no_secret(&logs);
    for text in ["is not this number's", "pin=", "upstream failed"] {
        assert!(
            !logs.contains(text),
            "Meta's text was logged: {text}\n{logs}"
        );
    }
    let audited: Vec<serde_json::Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    let actions: Vec<&str> = audited
        .iter()
        .filter_map(|f| f["action"].as_str())
        .collect();
    assert_eq!(actions, ["two_step_pin_set", "number_deregistered"]);
    let key_id = key.split('_').nth(1).unwrap();
    for fields in &audited {
        assert_eq!(fields["tenant"], TENANT, "{fields}");
        assert_eq!(fields["waba_id"], WABA, "{fields}");
        assert_eq!(fields["phone_number_id"], PN, "{fields}");
        assert_eq!(fields["key_id"], key_id, "{fields}");
    }
}

/// The digit-run check finds a PIN anywhere but in a fraction of a
/// second.
#[test]
fn the_check_finds_a_pin_outside_a_times_fraction() {
    assert_eq!(
        flow::digit_runs("at 10:58:10.581063Z pin=581063 x581063y"),
        ["10", "58", "10", "581063", "581063"]
    );
}
