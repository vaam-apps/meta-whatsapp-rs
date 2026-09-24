//! `X-Hub-Signature-256` over the exact fixture bytes, and `hub.challenge`
//! verification (`webhooks/create-webhook-endpoint`).

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use wa_core::Error;
use wa_core::error::WebhookError;
use wa_core::secret::{AppSecret, VerifyToken};
use wa_webhooks::{SignatureVerifier, VerificationQuery, sign, verify_subscription};

/// Conventions review #18: the header name belongs to the framework-free
/// API. This file is built without the `axum` feature too (`just
/// features`), so moving the constant back behind it fails the build.
#[test]
fn the_signature_header_is_available_without_axum() {
    assert_eq!(wa_webhooks::SIGNATURE_HEADER, "x-hub-signature-256");
    assert_eq!(
        wa_webhooks::SIGNATURE_HEADER,
        "X-Hub-Signature-256".to_ascii_lowercase()
    );
}

fn verifier(secrets: &[&str]) -> SignatureVerifier {
    SignatureVerifier::new(secrets.iter().map(|s| AppSecret::new(*s)).collect()).unwrap()
}

#[test]
fn valid_signature_over_every_fixture_verifies_and_one_changed_byte_does_not() {
    let secret = AppSecret::new("f3a1c9e0b5d24c7e8a6b9d0c1e2f3a4b");
    let v = SignatureVerifier::new(vec![secret.clone()]).unwrap();
    for name in common::all_fixtures() {
        let body = common::fixture_bytes(&name);
        let header = sign(&secret, &body);
        assert!(v.verify(Some(&header), &body).is_ok(), "{name}");

        for index in [0, body.len() / 2, body.len() - 1] {
            let mut tampered = body.clone();
            tampered[index] ^= 0x01;
            assert!(
                matches!(
                    v.verify(Some(&header), &tampered),
                    Err(WebhookError::SignatureMismatch)
                ),
                "{name}: byte {index} flipped still verified"
            );
        }
    }
}

#[test]
fn uppercase_hex_is_accepted() {
    let v = verifier(&["secret"]);
    let body = br#"{"object":"whatsapp_business_account","entry":[]}"#;
    let header = sign(&AppSecret::new("secret"), body);
    let upper = format!("sha256={}", header["sha256=".len()..].to_uppercase());
    assert!(v.verify(Some(&upper), body).is_ok());
    // Surrounding whitespace from a sloppy proxy is tolerated.
    assert!(v.verify(Some(&format!(" {header} ")), body).is_ok());
}

#[test]
fn any_configured_secret_verifies_during_rotation() {
    let body = b"{}";
    let old = sign(&AppSecret::new("old-secret"), body);
    let new = sign(&AppSecret::new("new-secret"), body);
    let v = verifier(&["new-secret", "old-secret"]);
    assert!(v.verify(Some(&old), body).is_ok());
    assert!(v.verify(Some(&new), body).is_ok());
    let other = sign(&AppSecret::new("someone-else"), body);
    assert!(matches!(
        v.verify(Some(&other), body),
        Err(WebhookError::SignatureMismatch)
    ));
}

#[test]
fn missing_and_malformed_headers_are_distinguished() {
    let v = verifier(&["secret"]);
    let body = b"{}";
    let good = sign(&AppSecret::new("secret"), body);
    let hex = &good["sha256=".len()..];
    assert!(matches!(
        v.verify(None, body),
        Err(WebhookError::MissingSignature)
    ));
    for header in [
        String::new(),
        hex.to_owned(),                       // no prefix
        format!("sha1={hex}"),                // wrong algorithm
        format!("SHA256={hex}"),              // prefix is case-sensitive
        format!("sha256={}", &hex[..63]),     // 63 chars
        format!("sha256={hex}0"),             // 65 chars
        format!("sha256={}g", &hex[..63]),    // not hex
        format!("sha256= {hex}"),             // space inside
        format!("sha256={}", "é".repeat(32)), // 64 bytes, not hex
    ] {
        assert!(
            matches!(
                v.verify(Some(&header), body),
                Err(WebhookError::MalformedSignature)
            ),
            "{header:?}"
        );
    }
}

#[test]
fn verification_ok_mismatch_and_wrong_mode() {
    let token = VerifyToken::new("vibecoding");
    let query = |mode: &str, token: &str| -> VerificationQuery {
        serde_json::from_value(serde_json::json!({
            "hub.mode": mode, "hub.verify_token": token, "hub.challenge": "1158201444"
        }))
        .unwrap()
    };
    assert_eq!(
        verify_subscription(&query("subscribe", "vibecoding"), &token).unwrap(),
        "1158201444"
    );
    assert!(matches!(
        verify_subscription(&query("subscribe", "vibecodinG"), &token),
        Err(Error::Webhook(WebhookError::VerifyTokenMismatch))
    ));
    assert!(matches!(
        verify_subscription(&query("unsubscribe", "vibecoding"), &token),
        Err(Error::Webhook(WebhookError::InvalidVerificationRequest(_)))
    ));
}
