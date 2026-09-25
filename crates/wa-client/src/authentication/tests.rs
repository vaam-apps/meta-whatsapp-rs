//! Authentication template tests against the examples of
//! `templates/authentication-templates/*`.

use http::Method;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wa_core::testing::ScriptedTransport;

use super::*;
use crate::RetryPolicy;

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// The creation pages print enums in lower case (`"authentication"`,
/// `"otp"`, `"copy_code"`); we send upper case like the reference schema
/// and `GET`.
fn upper(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    match (k.as_str(), &mut *val) {
                        ("type" | "category" | "otp_type", Value::String(s)) => {
                            *s = s.to_uppercase();
                        }
                        _ => walk(val),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    v
}

fn assert_template(t: &AuthenticationTemplate, doc: Value) {
    t.validate().unwrap();
    let def = t.to_definition();
    assert_eq!(serde_json::to_value(&def).unwrap(), upper(doc.clone()));
    let parsed: TemplateDefinition = serde_json::from_value(doc).unwrap();
    assert_eq!(parsed, def);
}

#[test]
fn copy_code_template_matches_the_docs() {
    // copy-code-button-authentication-templates, example request.
    let doc = json!({
      "name": "authentication_code_copy_code_button",
      "language": "en_US",
      "category": "authentication",
      "message_send_ttl_seconds": 60,
      "components": [
        {"type": "body", "add_security_recommendation": true},
        {"type": "footer", "code_expiration_minutes": 5},
        {"type": "buttons", "buttons": [{"type": "otp", "otp_type": "copy_code", "text": "Copy Code"}]}
      ]
    });
    let t = AuthenticationTemplate::copy_code("authentication_code_copy_code_button", "en_US")
        .message_send_ttl_seconds(60)
        .security_recommendation(true)
        .code_expiration_minutes(5)
        .button_text("Copy Code");
    assert_template(&t, doc);
}

#[test]
fn one_tap_template_matches_the_docs() {
    // autofill-button-authentication-templates. Its example request puts
    // `package_name`/`signature_hash` on the button itself, a form the same
    // page says is unsupported from v21.0; this is its request syntax
    // (`supported_apps`) with the example's values.
    let doc = json!({
      "name": "authentication_code_autofill_button",
      "language": "en_US",
      "category": "authentication",
      "message_send_ttl_seconds": 60,
      "components": [
        {"type": "body", "add_security_recommendation": true},
        {"type": "footer", "code_expiration_minutes": 10},
        {"type": "buttons", "buttons": [{
          "type": "otp",
          "otp_type": "one_tap",
          "text": "Copy Code",
          "autofill_text": "Autofill",
          "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}]
        }]}
      ]
    });
    let t = AuthenticationTemplate::one_tap(
        "authentication_code_autofill_button",
        "en_US",
        [SupportedApp::new("com.example.luckyshrub", "K8a/AINcGX7")],
    )
    .message_send_ttl_seconds(60)
    .security_recommendation(true)
    .code_expiration_minutes(10)
    .button_text("Copy Code")
    .autofill_text("Autofill");
    assert_template(&t, doc);
}

#[test]
fn zero_tap_template_matches_the_docs() {
    // zero-tap-authentication-templates, example request.
    let doc = json!({
      "name": "zero_tap_auth_template",
      "language": "en_US",
      "category": "authentication",
      "message_send_ttl_seconds": 60,
      "components": [
        {"type": "body", "add_security_recommendation": true},
        {"type": "footer", "code_expiration_minutes": 5},
        {"type": "buttons", "buttons": [{
          "type": "otp",
          "otp_type": "zero_tap",
          "text": "Copy Code",
          "autofill_text": "Autofill",
          "zero_tap_terms_accepted": true,
          "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}]
        }]}
      ]
    });
    let t = AuthenticationTemplate::zero_tap(
        "zero_tap_auth_template",
        "en_US",
        [SupportedApp::new("com.example.luckyshrub", "K8a/AINcGX7")],
        true,
    )
    .message_send_ttl_seconds(60)
    .security_recommendation(true)
    .code_expiration_minutes(5)
    .button_text("Copy Code")
    .autofill_text("Autofill");
    assert_template(&t, doc);
}

#[test]
fn authentication_limits() {
    let app = || SupportedApp::new("com.example.luckyshrub", "K8a/AINcGX7");
    assert!(
        AuthenticationTemplate::zero_tap("z", "en_US", [app()], false)
            .validate()
            .is_err()
    );
    assert!(
        AuthenticationTemplate::one_tap("o", "en_US", [])
            .validate()
            .is_err()
    );
    assert!(
        AuthenticationTemplate::copy_code("c", "en_US")
            .code_expiration_minutes(0)
            .validate()
            .is_err()
    );
    assert!(
        AuthenticationTemplate::copy_code("c", "en_US")
            .code_expiration_minutes(90)
            .validate()
            .is_ok()
    );
    // time-to-live: authentication 30–900 or -1.
    assert!(
        AuthenticationTemplate::copy_code("c", "en_US")
            .message_send_ttl_seconds(901)
            .validate()
            .is_err()
    );
    assert!(
        AuthenticationTemplate::copy_code("c", "en_US")
            .message_send_ttl_seconds(-1)
            .validate()
            .is_ok()
    );
    assert!(
        AuthenticationTemplate::copy_code("c", "en_US")
            .button_text("x".repeat(26))
            .validate()
            .is_err()
    );
}

#[tokio::test]
async fn create_posts_an_authentication_template() {
    let t = ScriptedTransport::new();
    // Example response of the copy-code page.
    t.push_json(
        200,
        json!({"id": "594425479261596", "status": "PENDING", "category": "AUTHENTICATION"}),
    );
    let template =
        AuthenticationTemplate::copy_code("authentication_code_copy_code_button", "en_US")
            .security_recommendation(true)
            .code_expiration_minutes(5);
    let created = client(&t)
        .authentication("102290129340398")
        .create(&template)
        .await
        .unwrap();
    assert_eq!(created.category, Some(TemplateCategory::Authentication));
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/102290129340398/message_templates");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "name": "authentication_code_copy_code_button",
            "language": "en_US",
            "category": "AUTHENTICATION",
            "components": [
                {"type": "BODY", "add_security_recommendation": true},
                {"type": "FOOTER", "code_expiration_minutes": 5},
                {"type": "BUTTONS", "buttons": [{"type": "OTP", "otp_type": "COPY_CODE"}]}
            ]
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn previews_match_the_docs() {
    let t = ScriptedTransport::new();
    // template-preview example response.
    t.push_json(
        200,
        json!({"data": [
          {"body": "*{{1}}* is your verification code. For your security, do not share this code.",
           "buttons": [{"autofill_text": "Autofill", "text": "Copy code"}],
           "footer": "This code expires in 10 minutes.", "language": "en_US"},
          {"body": "Tu código de verificación es *{{1}}*. Por tu seguridad, no lo compartas.",
           "buttons": [{"autofill_text": "Autocompletar", "text": "Copiar código"}],
           "footer": "Este código caduca en 10 minutos.", "language": "es_ES"}
        ]}),
    );
    let previews = client(&t)
        .authentication("102290129340398")
        .previews(&PreviewQuery {
            languages: vec!["en_US".into(), "es_ES".into()],
            add_security_recommendation: Some(true),
            code_expiration_minutes: Some(10),
        })
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        "/v25.0/102290129340398/message_template_previews"
    );
    // The example request's query string.
    assert_eq!(
        req.url.query(),
        Some(
            "category=AUTHENTICATION&languages=en_US%2Ces_ES&add_security_recommendation=true&code_expiration_minutes=10&button_types=OTP"
        )
    );
    assert_eq!(previews.len(), 2);
    assert_eq!(previews[1].language, "es_ES");
    assert_eq!(
        previews[0].buttons[0].autofill_text.as_deref(),
        Some("Autofill")
    );
    assert_eq!(
        previews[0].footer.as_deref(),
        Some("This code expires in 10 minutes.")
    );
    assert_eq!(t.remaining(), 0);

    let bad = PreviewQuery {
        code_expiration_minutes: Some(91),
        ..PreviewQuery::default()
    };
    assert!(client(&t).authentication("1").previews(&bad).await.is_err());
    assert_eq!(t.requests().len(), 1);
}

#[tokio::test]
async fn upsert_matches_the_docs() {
    let t = ScriptedTransport::new();
    // bulk-management example response.
    let response = json!({"data": [
      {"id": "954638012257287", "status": "APPROVED", "language": "en_US"},
      {"id": "969725527415202", "status": "APPROVED", "language": "es_ES"},
      {"id": "969725530748535", "status": "APPROVED", "language": "fr"}
    ]});
    t.push_json(200, response.clone());
    t.push_json(200, response);
    let auth = client(&t).authentication("102290129340398");

    // bulk-management, copy code example.
    let copy = AuthenticationUpsert::from_template(
        &AuthenticationTemplate::copy_code("authentication_code_copy_code_button", "en_US")
            .security_recommendation(true)
            .code_expiration_minutes(10),
        ["en_US", "es_ES", "fr"],
    );
    let done = auth.upsert(&copy).await.unwrap();
    assert_eq!(done.len(), 3);
    assert_eq!(done[2].language.as_deref(), Some("fr"));
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/102290129340398/upsert_message_templates"
    );
    assert_eq!(
        req.json(),
        Some(json!({
          "name": "authentication_code_copy_code_button",
          "languages": ["en_US","es_ES","fr"],
          "category": "AUTHENTICATION",
          "components": [
            {"type": "BODY", "add_security_recommendation": true},
            {"type": "FOOTER", "code_expiration_minutes": 10},
            {"type": "BUTTONS", "buttons": [{"type": "OTP", "otp_type": "COPY_CODE"}]}
          ]
        }))
    );

    // bulk-management, one-tap example.
    let one_tap = AuthenticationUpsert::from_template(
        &AuthenticationTemplate::one_tap(
            "authentication_code_autofill_button",
            "en_US",
            [SupportedApp::new("com.example.luckyshrub", "K8a/AINcGX7")],
        )
        .security_recommendation(true)
        .code_expiration_minutes(15),
        ["en_US", "es_ES", "fr"],
    );
    auth.upsert(&one_tap).await.unwrap();
    assert_eq!(
        t.requests()[1].json(),
        Some(json!({
          "name": "authentication_code_autofill_button",
          "languages": ["en_US","es_ES","fr"],
          "category": "AUTHENTICATION",
          "components": [
            {"type": "BODY", "add_security_recommendation": true},
            {"type": "FOOTER", "code_expiration_minutes": 15},
            {"type": "BUTTONS", "buttons": [{
              "type": "OTP",
              "otp_type": "ONE_TAP",
              "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}]
            }]}
          ]
        }))
    );
    assert_eq!(t.remaining(), 0);

    // Labels are not supported by upsert; no languages is meaningless.
    let labelled = AuthenticationUpsert::from_template(
        &AuthenticationTemplate::copy_code("c", "en_US").button_text("Copy"),
        ["en_US"],
    );
    assert!(auth.upsert(&labelled).await.is_err());
    let none = AuthenticationUpsert::from_template(
        &AuthenticationTemplate::copy_code("c", "en_US"),
        Vec::<String>::new(),
    );
    assert!(auth.upsert(&none).await.is_err());
    assert_eq!(t.requests().len(), 2);
}

#[test]
fn otp_message_is_the_documented_send_payload() {
    // copy-code / one-tap / zero-tap pages, send example request.
    let doc = json!({
      "name": "verification_code",
      "language": {"code": "en_US"},
      "components": [
        {"type": "body", "parameters": [{"type": "text", "text": "J$FpnYnP"}]},
        {"type": "button", "sub_type": "url", "index": "0", "parameters": [{"type": "text", "text": "J$FpnYnP"}]}
      ]
    });
    let m = otp_template_message("verification_code", "en_US", "J$FpnYnP");
    assert_eq!(serde_json::to_value(&m).unwrap(), doc);
    assert!(
        !format!("{m:?}").contains("J$FpnYnP"),
        "Debug must not print the code"
    );
}

#[test]
fn module_doc_example_is_valid() {
    // Mirrors the `no_run` example in the module docs.
    let template = AuthenticationTemplate::one_tap(
        "login_code",
        "en_US",
        [SupportedApp::new("com.example.luckyshrub", "K8a/AINcGX7")],
    )
    .security_recommendation(true)
    .code_expiration_minutes(10)
    .message_send_ttl_seconds(600);
    template.validate().unwrap();
    assert_eq!(
        OtpConfig::new("tenant").ttl,
        std::time::Duration::from_secs(u64::from(template.code_expiration_minutes.unwrap()) * 60),
        "the doc pairs code_expiration_minutes with the default OTP ttl"
    );
}
