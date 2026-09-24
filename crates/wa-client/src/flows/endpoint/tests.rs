//! Tests for the Flow data endpoint crypto. Fixtures: `testdata/README.md`.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
use aws_lc_rs::rsa::{
    KeySize, OAEP_SHA256_MGF1SHA256, OaepPublicEncryptingKey, PrivateDecryptingKey,
    PublicEncryptingKey,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wa_core::error::{CryptoError, WebhookError};
use wa_core::secret::AppSecret;

use super::*;

const PKCS8: &str = include_str!("testdata/test_rsa2048_pkcs8.pem");
const PKCS1: &str = include_str!("testdata/test_rsa2048_pkcs1.pem");
const PUBLIC: &str = include_str!("testdata/test_rsa2048_public.pem");
const PKCS8_ENCRYPTED: &str = include_str!("testdata/test_rsa2048_pkcs8_encrypted.pem");
const PKCS1_ENCRYPTED: &str = include_str!("testdata/test_rsa2048_pkcs1_encrypted.pem");
const RSA1024: &str = include_str!("testdata/test_rsa1024_pkcs8.pem");
const EC_P256: &str = include_str!("testdata/test_ec_p256_pkcs8.pem");
const KAT: &str = include_str!("testdata/kat_node.json");
const KAT_MEDIA: &str = include_str!("testdata/kat_media.json");

fn kat() -> Value {
    serde_json::from_str(KAT).unwrap()
}

fn kat_request() -> EncryptedFlowRequest {
    serde_json::from_value(kat()["request"].clone()).unwrap()
}

fn kat_str(field: &str) -> String {
    kat()[field].as_str().unwrap().to_owned()
}

// ── Known answers from Meta's own sample code ─────────────────────────────

#[test]
fn decrypts_the_node_vector_and_seals_byte_identical_responses() {
    let key = FlowEndpointKey::from_pem(PKCS8).unwrap();

    let (request, sealer) = key.decrypt_request(&kat_request()).unwrap();
    let expected: FlowRequest = serde_json::from_str(&kat_str("request_plaintext")).unwrap();
    assert_eq!(request, expected);
    assert_eq!(request.action, FlowAction::DataExchange);
    assert_eq!(request.screen.as_deref(), Some("APPOINTMENT"));
    assert_eq!(request.flow_token.as_deref(), Some("flowtoken-1234"));
    assert_eq!(
        request.data,
        Some(json!({"department": "shopping", "is_new_customer": true, "guests": 2}))
    );

    // AES-GCM is deterministic for a given key, IV and plaintext, so the
    // sealed text must equal what Meta's `encryptResponse` produced.
    assert_eq!(
        serde_json::to_string(&FlowResponse::health_check()).unwrap(),
        kat_str("health_plaintext")
    );
    assert_eq!(
        sealer.seal(&FlowResponse::health_check()).unwrap(),
        kat_str("sealed_health_check")
    );

    let (_, sealer) = key.decrypt_request(&kat_request()).unwrap();
    let complete = FlowResponse::complete("flowtoken-1234");
    assert_eq!(
        serde_json::to_string(&complete).unwrap(),
        kat_str("success_plaintext")
    );
    assert_eq!(sealer.seal(&complete).unwrap(), kat_str("sealed_success"));
}

#[test]
fn pkcs1_pem_is_the_same_key() {
    let key = FlowEndpointKey::from_pem(PKCS1).unwrap();
    let (_, sealer) = key.decrypt_request(&kat_request()).unwrap();
    assert_eq!(
        sealer.seal(&json!({"data": {"status": "active"}})).unwrap(),
        kat_str("sealed_health_check")
    );
}

#[test]
fn public_key_pem_matches_openssl_output() {
    let key = FlowEndpointKey::from_pem(PKCS8).unwrap();
    assert_eq!(key.public_key_pem().unwrap(), PUBLIC);
}

// ── Round trip with a key generated in-test ───────────────────────────────

struct SimulatedClient {
    aes_key: [u8; 16],
    iv: [u8; 16],
}

impl SimulatedClient {
    fn new() -> Self {
        let mut aes_key = [0u8; 16];
        let mut iv = [0u8; 16];
        getrandom::fill(&mut aes_key).unwrap();
        getrandom::fill(&mut iv).unwrap();
        Self { aes_key, iv }
    }

    /// What the WhatsApp client does: wrap the AES key with RSA-OAEP-SHA256,
    /// AES-128-GCM the payload under a 16-byte IV, append the tag, base64.
    fn encrypt(&self, public: &PublicEncryptingKey, payload: &[u8]) -> EncryptedFlowRequest {
        self.encrypt_with_key_bytes(public, &self.aes_key, payload)
    }

    fn encrypt_with_key_bytes(
        &self,
        public: &PublicEncryptingKey,
        wrapped: &[u8],
        payload: &[u8],
    ) -> EncryptedFlowRequest {
        let oaep = OaepPublicEncryptingKey::new(public.clone()).unwrap();
        let mut ct = vec![0u8; oaep.ciphertext_size()];
        let wrapped = oaep
            .encrypt(&OAEP_SHA256_MGF1SHA256, wrapped, &mut ct, None)
            .unwrap()
            .to_vec();
        let mut data = payload.to_vec();
        let tag = FlowCipher::new(&self.aes_key.into())
            .encrypt_in_place_detached(&self.iv.into(), b"", &mut data)
            .unwrap();
        data.extend_from_slice(&tag);
        EncryptedFlowRequest {
            encrypted_flow_data: STANDARD.encode(data),
            encrypted_aes_key: STANDARD.encode(wrapped),
            initial_vector: STANDARD.encode(self.iv),
        }
    }

    /// What the WhatsApp client does with the response: same key, flipped IV.
    fn open_response(&self, sealed: &str, flip_iv: bool) -> Option<Vec<u8>> {
        let mut data = STANDARD.decode(sealed).ok()?;
        let body_len = data.len().checked_sub(16)?;
        let tag: [u8; 16] = data[body_len..].try_into().ok()?;
        let iv = if flip_iv {
            self.iv.map(|b| !b)
        } else {
            self.iv
        };
        FlowCipher::new(&self.aes_key.into())
            .decrypt_in_place_detached(&iv.into(), b"", &mut data[..body_len], &tag.into())
            .ok()?;
        data.truncate(body_len);
        Some(data)
    }
}

fn generated_key() -> (FlowEndpointKey, PublicEncryptingKey) {
    let private = PrivateDecryptingKey::generate(KeySize::Rsa2048).unwrap();
    let public = private.public_key();
    let der = AsDer::<Pkcs8V1Der>::as_der(&private).unwrap();
    (
        FlowEndpointKey::from_pkcs8_der(der.as_ref()).unwrap(),
        public,
    )
}

const PING: &[u8] = br#"{"version":"3.0","action":"ping"}"#;

#[test]
fn round_trip_with_a_generated_key() {
    let (key, public) = generated_key();
    let client = SimulatedClient::new();

    let (request, sealer) = key.decrypt_request(&client.encrypt(&public, PING)).unwrap();
    assert_eq!(request.action, FlowAction::Ping);
    assert_eq!(request.version, "3.0");
    assert_eq!(request.screen, None);

    let response_b64 = sealer.seal(&FlowResponse::health_check()).unwrap();
    let plaintext = client
        .open_response(&response_b64, true)
        .expect("client can open it");
    assert_eq!(
        serde_json::from_slice::<Value>(&plaintext).unwrap(),
        json!({"data": {"status": "active"}})
    );
    assert!(
        client.open_response(&response_b64, false).is_none(),
        "the response must use the flipped IV, not the request IV"
    );
}

// ── One error for every failure ───────────────────────────────────────────

fn assert_decrypt_error(key: &FlowEndpointKey, request: &EncryptedFlowRequest, case: &str) {
    let err = key.decrypt_request(request).unwrap_err();
    assert_eq!(err, CryptoError::Decrypt, "{case}");
}

fn tweak(b64: &str, f: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut bytes = STANDARD.decode(b64).unwrap();
    f(&mut bytes);
    STANDARD.encode(bytes)
}

#[test]
fn every_failure_is_the_same_decrypt_error() {
    let (key, public) = generated_key();
    let client = SimulatedClient::new();
    let good = client.encrypt(&public, PING);
    assert!(key.decrypt_request(&good).is_ok());

    let mut cases: Vec<(&str, EncryptedFlowRequest)> = Vec::new();
    let mut with = |case, f: &dyn Fn(&mut EncryptedFlowRequest)| {
        let mut r = good.clone();
        f(&mut r);
        cases.push((case, r));
    };
    with("tampered tag", &|r| {
        r.encrypted_flow_data = tweak(&r.encrypted_flow_data, |b| {
            let last = b.len() - 1;
            b[last] ^= 1;
        });
    });
    with("tampered ciphertext", &|r| {
        r.encrypted_flow_data = tweak(&r.encrypted_flow_data, |b| b[0] ^= 1);
    });
    // GCM is CTR underneath: flipping ciphertext byte 12 flips plaintext byte
    // 12, turning `"3.0"` into `"2.0"` — still a valid request. Only tag
    // verification can reject this one.
    assert_eq!(PING[12], b'3');
    with("bit flip that keeps the JSON valid", &|r| {
        r.encrypted_flow_data = tweak(&r.encrypted_flow_data, |b| b[12] ^= 1);
    });
    with("tag stripped", &|r| {
        r.encrypted_flow_data = tweak(&r.encrypted_flow_data, |b| b.truncate(b.len() - 16));
    });
    with("shorter than a tag", &|r| {
        r.encrypted_flow_data = tweak(&r.encrypted_flow_data, |b| b.truncate(15));
    });
    with("empty flow data", &|r| r.encrypted_flow_data.clear());
    with("tampered wrapped key", &|r| {
        r.encrypted_aes_key = tweak(&r.encrypted_aes_key, |b| b[10] ^= 0x40);
    });
    with("truncated wrapped key", &|r| {
        r.encrypted_aes_key = tweak(&r.encrypted_aes_key, |b| b.truncate(255));
    });
    with("tampered IV", &|r| {
        r.initial_vector = tweak(&r.initial_vector, |b| b[3] ^= 1);
    });
    with("12-byte IV", &|r| {
        r.initial_vector = tweak(&r.initial_vector, |b| b.truncate(12));
    });
    with("bad base64", &|r| r.encrypted_aes_key.push('!'));
    with("request IV flipped", &|r| {
        r.initial_vector = tweak(&r.initial_vector, |b| b.iter_mut().for_each(|x| *x = !*x));
    });
    for (case, request) in &cases {
        assert_decrypt_error(&key, request, case);
    }

    let (other_key, _) = generated_key();
    assert_decrypt_error(&other_key, &good, "wrong private key");

    // Valid RSA and valid GCM, but the unwrapped key is not 128 bits.
    let long_key = client.encrypt_with_key_bytes(&public, &[7u8; 32], PING);
    assert_decrypt_error(&key, &long_key, "256-bit AES key");

    // Authenticates, but is not a Flow request.
    assert_decrypt_error(
        &key,
        &client.encrypt(&public, b"not json"),
        "non-JSON payload",
    );
    assert_decrypt_error(
        &key,
        &client.encrypt(&public, br#"{"action":"ping"}"#),
        "no version",
    );
}

#[test]
fn unpadded_base64_is_accepted() {
    let (key, public) = generated_key();
    let client = SimulatedClient::new();
    let mut r = client.encrypt(&public, PING);
    r.initial_vector = r.initial_vector.trim_end_matches('=').to_owned();
    assert!(key.decrypt_request(&r).is_ok());
}

// ── Key loading ───────────────────────────────────────────────────────────

fn invalid_key_reason(result: Result<FlowEndpointKey, CryptoError>) -> &'static str {
    match result {
        Err(CryptoError::InvalidKey(reason)) => reason,
        other => panic!("expected InvalidKey, got {other:?}"),
    }
}

#[test]
fn encrypted_pems_are_rejected_with_the_decrypt_command() {
    for pem in [PKCS8_ENCRYPTED, PKCS1_ENCRYPTED] {
        let reason = invalid_key_reason(FlowEndpointKey::from_pem(pem));
        assert!(reason.contains("openssl pkcs8 -topk8 -nocrypt"), "{reason}");
    }
}

#[test]
fn only_2048_bit_rsa_keys_are_accepted() {
    // aws-lc-rs accepts 3072-bit keys, so this exercises our own size check.
    let private = PrivateDecryptingKey::generate(KeySize::Rsa3072).unwrap();
    let der = AsDer::<Pkcs8V1Der>::as_der(&private).unwrap();
    assert!(invalid_key_reason(FlowEndpointKey::from_pkcs8_der(der.as_ref())).contains("2048"));
    // aws-lc-rs itself refuses anything under 2048 bits; the message is ours.
    assert!(invalid_key_reason(FlowEndpointKey::from_pem(RSA1024)).contains("2048"));
}

#[test]
fn unusable_keys_are_rejected() {
    invalid_key_reason(FlowEndpointKey::from_pem(EC_P256));
    assert!(invalid_key_reason(FlowEndpointKey::from_pem(PUBLIC)).contains("public key"));
    invalid_key_reason(FlowEndpointKey::from_pem("not a key"));
    invalid_key_reason(FlowEndpointKey::from_pem(
        "-----BEGIN PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----",
    ));
    invalid_key_reason(FlowEndpointKey::from_pem(
        "-----BEGIN PRIVATE KEY-----\n!!!!\n-----END PRIVATE KEY-----",
    ));
    invalid_key_reason(FlowEndpointKey::from_pkcs8_der(b"\x30\x03\x02\x01\x00"));
    invalid_key_reason(FlowEndpointKey::from_pkcs1_der(b""));
}

#[test]
fn pem_with_crlf_and_surrounding_text_loads() {
    let crlf = format!("key for staging:\r\n{}\r\n", PKCS8.replace('\n', "\r\n"));
    assert!(FlowEndpointKey::from_pem(&crlf).is_ok());
}

// ── Secrets stay out of Debug ─────────────────────────────────────────────

#[test]
fn debug_output_carries_no_key_material_or_tokens() {
    let key = FlowEndpointKey::from_pem(PKCS8).unwrap();
    assert_eq!(format!("{key:?}"), "FlowEndpointKey { bits: 2048, .. }");

    let (request, sealer) = key.decrypt_request(&kat_request()).unwrap();
    assert_eq!(format!("{sealer:?}"), "ResponseSealer { .. }");
    let shown = format!("{request:?}");
    assert!(!shown.contains("flowtoken-1234"), "{shown}");
    assert!(shown.contains("[REDACTED]"), "{shown}");
    assert!(shown.contains("APPOINTMENT"), "{shown}");

    let complete = FlowResponse::complete_with_params("flowtoken-1234", [("order", json!("A-1"))]);
    let shown = format!("{complete:?}");
    assert!(!shown.contains("flowtoken-1234"), "{shown}");
    assert!(
        shown.contains("A-1") && shown.contains("SUCCESS"),
        "{shown}"
    );
    assert_eq!(
        complete.data["extension_message_response"]["params"]["flow_token"],
        json!("flowtoken-1234"),
        "only Debug is redacted, never the payload"
    );

    for err in [
        FlowEndpointKey::from_pem(PKCS8_ENCRYPTED).unwrap_err(),
        key.decrypt_request(&EncryptedFlowRequest {
            encrypted_flow_data: String::new(),
            encrypted_aes_key: String::new(),
            initial_vector: String::new(),
        })
        .unwrap_err(),
    ] {
        let text = format!("{err} {err:?}");
        assert!(!text.contains("MII"), "{text}");
    }
}

// ── Payload shapes from the docs ──────────────────────────────────────────

#[test]
fn parses_the_documented_request_payloads() {
    // Data exchange sample (placeholders filled in).
    let data_exchange: FlowRequest = serde_json::from_value(json!({
        "version": "3.0",
        "action": "data_exchange",
        "screen": "SCREEN_NAME",
        "data": {"prop_1": "value_1", "prop_n": "value_n"},
        "flow_token": "FLOW-TOKEN"
    }))
    .unwrap();
    assert_eq!(data_exchange.action, FlowAction::DataExchange);
    assert_eq!(data_exchange.error_notification(), None);

    // INIT and BACK may omit screen and data.
    for (wire, action) in [("INIT", FlowAction::Init), ("BACK", FlowAction::Back)] {
        let r: FlowRequest =
            serde_json::from_value(json!({"version": "3.0", "action": wire, "flow_token": "t"}))
                .unwrap();
        assert_eq!(r.action, action);
        assert_eq!((r.screen, r.data), (None, None));
    }

    // Error notification sample.
    let notification: FlowRequest = serde_json::from_value(json!({
        "version": "3.0",
        "flow_token": "FLOW-TOKEN",
        "action": "INIT",
        "data": {"error": "ERROR-KEY", "error_message": "ERROR-MESSAGE"}
    }))
    .unwrap();
    assert_eq!(
        notification.error_notification(),
        Some(FlowErrorNotification {
            error: "ERROR-KEY".into(),
            error_message: Some("ERROR-MESSAGE".into()),
        })
    );

    // Health check sample.
    let ping: FlowRequest =
        serde_json::from_value(json!({"version": "3.0", "action": "ping"})).unwrap();
    assert_eq!(ping.action, FlowAction::Ping);

    // An action Meta adds later still parses.
    let future: FlowRequest =
        serde_json::from_value(json!({"version": "4.0", "action": "navigate"})).unwrap();
    assert_eq!(future.action, FlowAction::Unknown("navigate".into()));
}

#[test]
fn response_payloads_match_the_docs() {
    assert_eq!(
        serde_json::to_value(FlowResponse::health_check()).unwrap(),
        json!({"data": {"status": "active"}})
    );
    assert_eq!(
        serde_json::to_value(FlowResponse::acknowledge_error()).unwrap(),
        json!({"data": {"acknowledged": true}})
    );
    assert_eq!(
        serde_json::to_value(
            FlowResponse::next_screen("SCREEN_NAME", json!({"property_1": "value_1"}))
                .with_error_message("ERROR-MESSAGE")
        )
        .unwrap(),
        json!({"screen": "SCREEN_NAME", "data": {"property_1": "value_1", "error_message": "ERROR-MESSAGE"}})
    );
    assert_eq!(
        serde_json::to_value(FlowResponse::next_screen("S", Value::Null).with_error_message("e"))
            .unwrap(),
        json!({"screen": "S", "data": {"error_message": "e"}})
    );
    assert_eq!(
        serde_json::to_value(FlowResponse::complete_with_params(
            "FLOW_TOKEN",
            [
                ("optional_param1", json!("value1")),
                ("flow_token", json!("ignored")),
            ]
        ))
        .unwrap(),
        json!({"screen": "SUCCESS", "data": {"extension_message_response": {"params": {
            "flow_token": "FLOW_TOKEN", "optional_param1": "value1"
        }}}})
    );
}

#[test]
fn documented_status_codes() {
    assert_eq!(EndpointStatus::Ok.code(), 200);
    assert_eq!(EndpointStatus::DecryptionFailed.code(), 421);
    assert_eq!(EndpointStatus::SignatureMismatch.code(), 432);
    assert_eq!(RESPONSE_CONTENT_TYPE, "text/plain");
}

// ── X-Hub-Signature-256 ───────────────────────────────────────────────────

/// RFC 4231 test case 2: HMAC-SHA256(key = "Jefe", "what do ya want for nothing?").
const RFC4231_BODY: &[u8] = b"what do ya want for nothing?";
const RFC4231_HEADER: &str =
    "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";

#[test]
fn verifies_signatures_against_any_configured_secret() {
    let jefe = AppSecret::new("Jefe");
    let other = AppSecret::new("rotated-out");
    assert!(
        verify_request_signature(
            RFC4231_BODY,
            Some(RFC4231_HEADER),
            std::slice::from_ref(&jefe)
        )
        .is_ok()
    );
    assert!(
        verify_request_signature(RFC4231_BODY, Some(RFC4231_HEADER), &[other.clone(), jefe])
            .is_ok(),
        "rotation: the second secret matches"
    );
    assert!(matches!(
        verify_request_signature(RFC4231_BODY, Some(RFC4231_HEADER), &[other]),
        Err(WebhookError::SignatureMismatch)
    ));
}

#[test]
fn signature_failures_are_classified() {
    let jefe = [AppSecret::new("Jefe")];
    let check = |body: &[u8], header: Option<&str>, secrets: &[AppSecret]| {
        verify_request_signature(body, header, secrets).map_err(|e| e.to_string())
    };
    let mismatch = Err(WebhookError::SignatureMismatch.to_string());
    let malformed = Err(WebhookError::MalformedSignature.to_string());
    assert_eq!(
        check(RFC4231_BODY, None, &jefe),
        Err(WebhookError::MissingSignature.to_string())
    );
    assert_eq!(
        check(b"what do ya want for nothing!", Some(RFC4231_HEADER), &jefe),
        mismatch
    );
    assert_eq!(check(RFC4231_BODY, Some(RFC4231_HEADER), &[]), mismatch);
    let no_prefix = RFC4231_HEADER.trim_start_matches("sha256=");
    assert_eq!(check(RFC4231_BODY, Some(no_prefix), &jefe), malformed);
    assert_eq!(check(RFC4231_BODY, Some("sha256=zz"), &jefe), malformed);
    assert_eq!(
        check(
            RFC4231_BODY,
            Some(&RFC4231_HEADER[..RFC4231_HEADER.len() - 2]),
            &jefe
        ),
        malformed
    );
    assert_eq!(check(RFC4231_BODY, Some("sha1=abcd"), &jefe), malformed);
}

// ── Uploaded media (PhotoPicker / DocumentPicker) ─────────────────────────

fn media_kat() -> (Vec<u8>, Vec<u8>, FlowMedia) {
    let v: Value = serde_json::from_str(KAT_MEDIA).unwrap();
    let b64 = |f: &str| STANDARD.decode(v[f].as_str().unwrap()).unwrap();
    (
        b64("cdn_file"),
        b64("plaintext"),
        serde_json::from_value(v["media"].clone()).unwrap(),
    )
}

#[test]
fn decrypts_the_python_media_vector() {
    let (cdn_file, plaintext, media) = media_kat();
    assert_eq!(media.decrypt(&cdn_file).unwrap(), plaintext);
    assert_eq!(
        decrypt_media(&cdn_file, &media.encryption_metadata).unwrap(),
        plaintext
    );
}

#[test]
fn parses_the_documented_media_payload() {
    // The docs' example, with its missing comma and bracket fixed.
    let items: Vec<FlowMedia> = serde_json::from_value(json!([{
        "media_id": "790aba14-5f4a-4dbd-aa9e-0d75401da14b",
        "cdn_url": "https://mmg.whatsapp.net/v/redacted",
        "file_name": "IMG_5237.jpg",
        "encryption_metadata": {
            "encrypted_hash": "/QvkBvpBED2q2AHPIFuhXfLpkn22zj2kO6ggzjvhHv0=",
            "iv": "5SHjLrrsfPXTSJTcbrVSkg==",
            "encryption_key": "lPa4SXcWbk3sy2so3OxjyXmpV4aE6CcIKd+4byr5hBw=",
            "hmac_key": "15l+E9Z5gcL15WH9OQ8GgK7VVCKkfbVigoSiM9djvGU=",
            "plaintext_hash": "AOF2dHXVEpm9efk9udNy3R1cUJWnpjFwQKGBEdALqXI="
        }
    }]))
    .unwrap();
    assert_eq!(items[0].file_name.as_deref(), Some("IMG_5237.jpg"));
    let shown = format!("{:?}", items[0]);
    assert!(!shown.contains("lPa4SXcW"), "{shown}");
    assert!(!shown.contains("15l+E9Z5"), "{shown}");
}

fn reencoded(b64: &str, f: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut bytes = STANDARD.decode(b64).unwrap();
    f(&mut bytes);
    STANDARD.encode(bytes)
}

fn sha256_b64(bytes: &[u8]) -> String {
    use sha2::Digest;
    STANDARD.encode(sha2::Sha256::digest(bytes))
}

#[test]
fn every_media_check_is_enforced_with_one_error() {
    let (cdn_file, _, media) = media_kat();
    let meta = media.encryption_metadata;
    let mut cases: Vec<(&str, Vec<u8>, MediaEncryptionMetadata)> = Vec::new();

    let mut file = cdn_file.clone();
    file[0] ^= 1;
    cases.push(("file changed, hash stale", file, meta.clone()));

    // Changed ciphertext with a matching file hash: only the HMAC catches it.
    let mut file = cdn_file.clone();
    file[0] ^= 1;
    let mut m = meta.clone();
    m.encrypted_hash = sha256_b64(&file);
    cases.push(("ciphertext changed, hash recomputed", file, m));

    // Wrong HMAC key, everything else intact: only the HMAC catches it.
    let mut m = meta.clone();
    m.hmac_key = reencoded(&meta.hmac_key, |k| k[0] ^= 1);
    cases.push(("wrong hmac key", cdn_file.clone(), m));

    // Wrong plaintext hash: only the final check catches it.
    let mut m = meta.clone();
    m.plaintext_hash = reencoded(&meta.plaintext_hash, |h| h[0] ^= 1);
    cases.push(("wrong plaintext hash", cdn_file.clone(), m));

    // Wrong encrypted hash with an intact file: only the first check.
    let mut m = meta.clone();
    m.encrypted_hash = reencoded(&meta.encrypted_hash, |h| h[0] ^= 1);
    cases.push(("wrong encrypted hash", cdn_file.clone(), m));

    let mut m = meta.clone();
    m.encryption_key = reencoded(&meta.encryption_key, |k| k[0] ^= 1);
    cases.push(("wrong encryption key", cdn_file.clone(), m));

    let mut m = meta.clone();
    m.encryption_key = reencoded(&meta.encryption_key, |k| k.truncate(16));
    cases.push(("AES-128 key", cdn_file.clone(), m));

    let mut m = meta.clone();
    m.iv = reencoded(&meta.iv, |iv| iv.truncate(12));
    cases.push(("short IV", cdn_file.clone(), m));

    let short = cdn_file[..5].to_vec();
    let mut m = meta.clone();
    m.encrypted_hash = sha256_b64(&short);
    cases.push(("shorter than the HMAC", short, m));

    let mut m = meta.clone();
    m.hmac_key = "%%%".into();
    cases.push(("bad base64", cdn_file.clone(), m));

    for (case, file, m) in &cases {
        assert_eq!(decrypt_media(file, m), Err(CryptoError::Decrypt), "{case}");
    }
    assert!(decrypt_media(&cdn_file, &meta).is_ok());
}
