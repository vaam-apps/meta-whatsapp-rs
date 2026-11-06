//! Roadmap M5c5 (design D37): a send from a bound number names its
//! binding's WABA id as `messaging_account_id`, behind
//! `WA_SERVER_SEND_MESSAGING_ACCOUNT_ID` (on by default). Every route of
//! the committed document is walked and every request it sends to
//! `/{pn}/messages` is read; after a same-tenant attach of a second WABA
//! holding the number the sends name the second; another tenant's attach
//! of the number stays `409 waba_owned_by_another_tenant`; a body naming
//! the account itself is refused.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{ALL_SCOPES, Call, Harness, Sample};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{OtpSettings, Scope};
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const WABA: &str = "102290129340398";
const SECOND_WABA: &str = "102290129340399";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const TO: &str = "+16505551234";

/// The routes whose request reaches Meta's `/{pn}/messages`. The walk
/// below finds them in the committed document; this list is what it must
/// find, so a route that starts to send fails there until it is named
/// here (and made to name the account).
const SENDING: [&str; 4] = [
    "POST /v1/numbers/{pn}/messages",
    "POST /v1/numbers/{pn}/conversations/{contact}/messages",
    "POST /v1/otp/issue",
    "POST /v1/numbers/{pn}/marketing-messages",
];

/// The sending routes that name no account: the marketing send. Meta's
/// pages for `/{pn}/marketing_messages` (`marketing-messages/send-marketing-messages`
/// and the endpoint's reference) document no `messaging_account_id` for
/// it (the account model page shows it on `/{pn}/messages` and the
/// calling actions only), so none is sent: a field name is never guessed.
/// When Meta documents it, the library's `Marketing::send` gains it, the
/// route names it, and this list is emptied (the walk fails until then).
const NAMING_NONE: [&str; 1] = ["POST /v1/numbers/{pn}/marketing-messages"];

fn accepted() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": TO, "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

/// messages/text-messages, "Example request", as the service's body.
fn text() -> Value {
    json!({"to": {"phone": TO}, "type": "text", "text": {"body": "Your order has shipped."}})
}

/// A reply that needs no open window.
fn template_reply() -> Value {
    json!({"type": "template",
           "template": {"name": "order_confirmation", "language": {"code": "en_US"}}})
}

fn settings(send_messaging_account_id: bool) -> Settings {
    Settings {
        send_messaging_account_id,
        ..common::test_settings()
    }
}

/// merchant-a holds `WABA` with `PN`, with a key of every scope.
async fn held(settings: Settings) -> (Harness, String) {
    let h = Harness::with_settings(settings);
    h.tenant(A).await;
    h.connect(A, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(A, &ALL_SCOPES).await;
    (h, key)
}

fn send(key: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{PN}/messages"))
        .key(key)
        .json(body)
}

fn reply(key: &str, body: &Value) -> Call {
    Call::new(
        Method::POST,
        format!("/v1/numbers/{PN}/conversations/16505551234/messages"),
    )
    .key(key)
    .json(body)
}

/// The bodies of the requests Meta got on `/{pn}/messages`.
fn sent(h: &Harness) -> Vec<Value> {
    h.graph
        .requests()
        .into_iter()
        .filter(|r| {
            r.method == Method::POST
                && (r.path().ends_with("/messages") || r.path().ends_with("/marketing_messages"))
        })
        .map(|r| r.json().expect("a JSON body"))
        // A read receipt (`status: read`) posts to the same edge and is
        // not a message.
        .filter(|body| body.get("status").is_none())
        .collect()
}

/// Every route of the committed document, called with a sample, on a
/// service of its own; the requests it sent to a number's messages, by
/// route. Decisive for "every send route": it follows the document, so a
/// new route that sends is found without anyone listing it.
async fn walk(setting: bool) -> Vec<(String, Vec<Value>)> {
    let sample = Sample {
        tenant: A.to_owned(),
        waba: WABA.to_owned(),
        pn: PN.to_owned(),
        key_id: "placeholder".to_owned(),
    };
    let mut found = Vec::new();
    for operation in common::spec_operations() {
        if operation.admin() || !operation.keyed {
            continue;
        }
        let (h, key) = held(settings(setting)).await;
        // The tenant's own OTP number, so that an OTP issue reaches Meta.
        assert!(
            h.store
                .set_otp_settings(
                    &meta_whatsapp_server::model::TenantId::parse(A).unwrap(),
                    Some(&OtpSettings::new(
                        PhoneNumberId::new(PN).unwrap(),
                        "login_code",
                        "en_US",
                    )),
                )
                .await
                .unwrap()
        );
        h.graph.push_json(200, accepted());
        let _ = h
            .call(common::sample_call(&operation, &sample, Some(&key)))
            .await;
        let bodies = sent(&h);
        if !bodies.is_empty() {
            found.push((operation.label(), bodies));
        }
    }
    found
}

/// Every route that reaches a number's messages names the binding's WABA
/// id, on by default; the walk finds them in the document and they are
/// the ones listed. Decisive: dropping the field on one route, flipping
/// the default, or a new sending route.
#[tokio::test]
async fn every_send_route_names_the_bindings_account() {
    let found = walk(true).await;
    let routes: BTreeSet<&str> = found.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(routes, BTreeSet::from(SENDING));
    for (label, bodies) in &found {
        for body in bodies {
            if NAMING_NONE.contains(&label.as_str()) {
                assert_eq!(body.get("messaging_account_id"), None, "{label}: {body}");
            } else {
                assert_eq!(body["messaging_account_id"], WABA, "{label}: {body}");
            }
        }
    }
}

/// With the setting off no route names it, and nothing else about the
/// request differs.
#[tokio::test]
async fn with_the_setting_off_no_send_route_names_an_account() {
    let found = walk(false).await;
    let routes: BTreeSet<&str> = found.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(routes, BTreeSet::from(SENDING));
    for (label, bodies) in &found {
        for body in bodies {
            assert_eq!(body.get("messaging_account_id"), None, "{label}: {body}");
        }
    }
    let default = Settings::default();
    assert!(default.send_messaging_account_id, "on by default");
}

/// The service's crates reach the library's send in the files named here
/// and nowhere else, so a new route that sends cannot skip the walk's
/// attention: it fails here until its file is named (and it names the
/// account through [`Messages`](meta_whatsapp_server_core::messages) or
/// the inbox).
#[test]
fn only_the_known_files_reach_the_librarys_send() {
    const SENDING_CALLS: [&str; 4] = [
        ".messages(",
        "inbox.send(",
        "OtpService::new(",
        ".marketing(",
    ];
    // The files that reach the library's send, and why.
    let known: BTreeSet<&str> = BTreeSet::from([
        // `Messages::send`, the one place that names the account
        // (`billed_to_binding`), and the mark-read call.
        "meta-whatsapp-server-core/src/messages.rs",
        // The inbox reply, which names it the same way.
        "meta-whatsapp-server-core/src/inbox.rs",
        // The OTP issue, which names it through the library's
        // `OtpService::messaging_account_id`.
        "meta-whatsapp-server-core/src/otp.rs",
        // The marketing send (`NAMING_NONE`).
        "meta-whatsapp-server-core/src/marketing.rs",
    ]);
    let root = std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap();
    let prefix = format!("{}/", root.display());
    let mut found = BTreeSet::new();
    for krate in [
        "meta-whatsapp-server-core",
        "meta-whatsapp-server-http",
        "meta-whatsapp-server-api-axum",
        "meta-whatsapp-server",
    ] {
        visit(&root.join(krate).join("src"), &mut |path, source| {
            // Test modules, conformance suites and comments do not count.
            if path.contains("/conformance/") {
                return;
            }
            let code: String = source
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            let code = code.split("#[cfg(test)]").next().unwrap_or_default();
            if SENDING_CALLS.iter().any(|call| code.contains(call)) {
                found.insert(path.strip_prefix(&prefix).unwrap().to_owned());
            }
        });
    }
    let unknown: Vec<&String> = found
        .iter()
        .filter(|p| !known.contains(p.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "a file reaches the library's send and is not named here: {unknown:?}"
    );
    // The scan sees something, or it proves nothing.
    assert!(found.len() >= 3, "{found:?}");
}

fn visit(dir: &Path, each: &mut impl FnMut(&str, &str)) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit(&path, each);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let source = std::fs::read_to_string(&path).unwrap();
            each(&path.to_string_lossy().replace('\\', "/"), &source);
        }
    }
}

/// A caller cannot choose the account: a body naming it (either spelling)
/// is `422 invalid_request` on `body`, on both routes, and Meta is asked
/// nothing. Decisive: accepting a caller-supplied value.
#[tokio::test]
async fn a_body_naming_the_account_is_refused() {
    let (h, key) = held(settings(true)).await;
    for field in ["messaging_account_id", "paid_messaging_account_id"] {
        let mut body = text();
        body[field] = json!("999999999999");
        let refused = h.call(send(&key, &body)).await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{field}: {}",
            refused.text
        );
        let mut body = template_reply();
        body[field] = json!("999999999999");
        let refused = h.call(reply(&key, &body)).await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{field}: {}",
            refused.text
        );
    }
    assert!(h.graph.requests().is_empty(), "nothing was sent");
}

/// Meta's listing of `numbers` under a WABA.
fn phone_numbers(numbers: &[&str]) -> Value {
    json!({"data": numbers.iter().map(|n| json!({
        "id": n, "display_phone_number": "+1 650-555-2222",
        "verified_name": "John's Cake Shop", "quality_rating": "GREEN"
    })).collect::<Vec<_>>()})
}

/// After a same-tenant attach of a second WABA that holds the number, the
/// sends name the second (the latest attach is the one billed), the
/// second's token is the one sent with; another tenant's attach of the
/// number is `409 waba_owned_by_another_tenant`, and the sends keep
/// naming the second. Decisive: the account is the binding's as the store
/// holds it now, not one read once.
#[tokio::test]
async fn the_latest_same_tenant_attach_is_the_one_named() {
    let (h, key) = held(settings(true)).await;
    let admin = h.admin_key().await;
    h.tenant(B).await;

    h.graph.push_json(200, accepted());
    let first = h.call(send(&key, &text())).await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.text);
    assert_eq!(sent(&h)[0]["messaging_account_id"], WABA);

    // The same tenant attaches a second WABA holding the number.
    h.graph.push_json(200, phone_numbers(&[PN]));
    h.graph.push_json(200, json!({"success": true}));
    let attach = h
        .call(
            Call::new(Method::POST, format!("/v1/admin/tenants/{A}/wabas"))
                .key(&admin)
                .json(&json!({"waba_id": SECOND_WABA, "token": "EAAG-second-token"})),
        )
        .await;
    assert_eq!(attach.status, StatusCode::CREATED, "{}", attach.text);

    h.graph.push_json(200, accepted());
    let second = h.call(send(&key, &text())).await;
    assert_eq!(second.status, StatusCode::ACCEPTED, "{}", second.text);
    h.graph.push_json(200, accepted());
    let replied = h.call(reply(&key, &template_reply())).await;
    assert_eq!(replied.status, StatusCode::ACCEPTED, "{}", replied.text);
    let bodies = sent(&h);
    assert_eq!(bodies.len(), 3);
    for body in &bodies[1..] {
        assert_eq!(body["messaging_account_id"], SECOND_WABA, "{body}");
    }

    // Another tenant's attach of the number is refused, and changes nothing.
    h.graph.push_json(200, phone_numbers(&[PN]));
    let before = h.graph.requests().len();
    let refused = h
        .call(
            Call::new(Method::POST, format!("/v1/admin/tenants/{B}/wabas"))
                .key(&admin)
                .json(&json!({"waba_id": "102290129340400", "token": "EAAG-b-token"})),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::CONFLICT, "waba_owned_by_another_tenant"),
        "{}",
        refused.text
    );
    assert!(h.graph.requests().len() <= before + 1);
    h.graph.push_json(200, accepted());
    let again = h.call(send(&key, &text())).await;
    assert_eq!(again.status, StatusCode::ACCEPTED, "{}", again.text);
    assert_eq!(
        sent(&h).last().unwrap()["messaging_account_id"],
        SECOND_WABA
    );
}

/// The scope a sample key needs, for the walk's own sake: the walk mints
/// every scope, and this keeps the constant used when scopes change.
#[test]
fn the_walk_holds_every_scope() {
    assert!(ALL_SCOPES.contains(&Scope::Send));
    assert!(ALL_SCOPES.contains(&Scope::Inbox));
    assert!(ALL_SCOPES.contains(&Scope::Otp));
}
