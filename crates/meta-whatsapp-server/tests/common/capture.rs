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

/// What the capture planted and the logs must not hold, each under the
/// section of the scenario that planted it, so that a failure names it
/// without printing it.
///
/// Every planted value is a distinctive fixed text, or a random one of at
/// least 256 bits (the API keys: 43 base62 characters; the `whsec_`
/// secrets: 32 bytes), so none can occur in a log line by chance. The
/// exception is a PIN or a code of six digits: no format lets it be longer,
/// so a value of only digits is matched as a whole token, not inside a
/// longer run of digits or letters, nor as the fraction of a number (see
/// [`Secrets::leaks`]), and each shape a log line would print it in is
/// planted too, as a text that matches anywhere.
#[derive(Default)] // no `Debug`: the values never reach a message
pub struct Secrets {
    /// `(section, value)`, in the order planted.
    entries: Vec<(String, String)>,
    section: String,
}

impl Secrets {
    pub fn new(section: &str, values: Vec<String>) -> Self {
        let mut secrets = Self::default();
        secrets.section(section);
        secrets.extend(values);
        secrets
    }

    /// Values planted from now on belong to `name`.
    pub fn section(&mut self, name: &str) {
        name.clone_into(&mut self.section);
    }

    pub fn push(&mut self, value: String) {
        self.entries.push((self.section.clone(), value));
    }

    pub fn extend(&mut self, values: impl IntoIterator<Item = String>) {
        for value in values {
            self.push(value);
        }
    }

    /// The label of entry `index`: its section and its place in it, with its
    /// length and the first bytes of its SHA-256 (to tell two apart),
    /// never the value.
    fn label(&self, index: usize) -> String {
        let (section, value) = &self.entries[index];
        let place = self.entries[..index]
            .iter()
            .filter(|(s, _)| s == section)
            .count();
        let digest = sha256_hex(value.as_bytes());
        format!(
            "planted secret {section}#{place} ({} bytes, sha256 {})",
            value.len(),
            &digest[..8]
        )
    }

    /// The first planted secret found in `logs`, as a message naming its
    /// label, the log line's number, target and the field it is in, never
    /// the value or the line.
    ///
    /// A line's `timestamp` is not searched: its microseconds are six
    /// digits drawn from the clock, which a six-digit PIN equals once in
    /// a million lines (this failed `live_logs.rs` once in 200 runs). Nor is
    /// a value of only digits found inside a longer token, or as the
    /// fraction of a number, as in `elapsed_secs: 0.000835164`.
    pub fn leaks(&self, logs: &str) -> Option<String> {
        for (n, line) in logs.lines().enumerate() {
            let line = without_timestamp(line);
            for (index, (_, secret)) in self.entries.iter().enumerate() {
                if !occurs(&line, secret) {
                    continue;
                }
                let (target, field) = locate(&line, secret);
                return Some(format!(
                    "a secret was logged: {} on log line {} (target {target}, field {field})",
                    self.label(index),
                    n + 1
                ));
            }
        }
        None
    }
}

/// `line` without the value of its leading `timestamp`.
fn without_timestamp(line: &str) -> String {
    const HEAD: &str = "{\"timestamp\":\"";
    if let Some(rest) = line.strip_prefix(HEAD)
        && let Some(end) = rest.find('"')
    {
        return format!("{HEAD}{}", &rest[end..]);
    }
    line.to_owned()
}

/// Whether `secret` is in `text`: anywhere, unless it is only digits, then
/// as a whole token.
fn occurs(text: &str, secret: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    if !secret.bytes().all(|b| b.is_ascii_digit()) {
        return text.contains(secret);
    }
    text.match_indices(secret).any(|(at, _)| {
        !continues(text[..at].chars().rev()) && !continues(text[at + secret.len()..].chars())
    })
}

/// Whether the characters going outward from a match go on with the same
/// token: a letter or digit, or a point followed by a digit.
fn continues(mut outward: impl Iterator<Item = char>) -> bool {
    match outward.next() {
        Some(c) if c.is_ascii_alphanumeric() => true,
        Some('.') => outward.next().is_some_and(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// The target and the field of a JSON log line that `secret` is in, as
/// `(target, field)`; `"?"` where the line has no such place.
fn locate(line: &str, secret: &str) -> (String, String) {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
        return ("?".to_owned(), "(a line that is not JSON)".to_owned());
    };
    let target = event["target"].as_str().unwrap_or("?").to_owned();
    let mut places = Vec::new();
    for group in ["fields", "span"] {
        if let Some(object) = event[group].as_object() {
            places.extend(object.iter().map(|(k, v)| (format!("{group}.{k}"), v)));
        }
    }
    if let Some(spans) = event["spans"].as_array() {
        for span in spans {
            let name = span["name"].as_str().unwrap_or("?");
            if let Some(object) = span.as_object() {
                places.extend(
                    object
                        .iter()
                        .map(|(k, v)| (format!("spans[{name}].{k}"), v)),
                );
            }
        }
    }
    places.push(("target".to_owned(), &event["target"]));
    for (field, value) in places {
        let text = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        // Plain, and as it is escaped in the line (the quoted shapes).
        if occurs(&text, secret) || occurs(&value.to_string(), secret) {
            return (target, field);
        }
    }
    (target, "(none of fields, span, spans)".to_owned())
}

const SYSTEM_TOKEN: &str = "EAAG-system-user-token-for-the-log-test";
pub const PHONE_1: &str = "+1 631-555-1111";
pub const PHONE_2: &str = "+1 631-555-2222";
pub const PHONE_3: &str = "+1 631-555-3333";

/// Runs admin and numbers calls carrying secrets and phone numbers, and
/// returns every secret they carried and what was logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
pub async fn exercise(h: &Harness) -> Secrets {
    let admin = h.admin_key().await;
    let mut secrets = Secrets::new(
        "harness",
        vec![
            admin.clone(),
            SYSTEM_TOKEN.to_owned(),
            VERIFY_TOKEN.to_owned(),
            APP_SECRET.to_owned(),
            PREVIOUS_APP_SECRET.to_owned(),
        ],
    );
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
    secrets.section("keys");
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

    secrets.section("webhooks");
    secrets.extend(webhooks(h, &tenant_key).await);
    secrets.section("inbox");
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
    secrets.section("send_media_and_templates");
    secrets.extend(send_media_and_templates(h, &admin).await);
    // OTP and authentication templates (roadmap M3c): the recipient's
    // number is not logged (the code's own case is `tests/otp_logs.rs`).
    secrets.section("otp");
    secrets.extend(otp(h, &admin).await);
    secrets.section("registration");
    secrets.extend(registration(h, &tenant_key).await);
    // The profile picture's upload and the display name (roadmap M5c2).
    secrets.section("profile");
    secrets.extend(picture_and_display_name(h, &admin).await);
    // A number's settings (roadmap M5c3): keys, the username, the
    // declarations and the contact book's id are secrets.
    secrets.section("number_settings");
    secrets.extend(number_settings(h, &admin).await);
    // Payments (roadmap P2): the OAuth links are credentials.
    secrets.section("payments");
    secrets.extend(payments(h, &admin).await);
    // Groups (roadmap M5f): neither a participant, nor a join request's
    // id, nor an invite link, nor a group's id is logged.
    secrets.section("groups");
    secrets.extend(groups(h, &admin).await);
    // Calling (roadmap M5e): an SDP (addresses, ICE credentials), a SIP
    // password and host, the callee and the call's id are not logged.
    secrets.section("calling");
    secrets.extend(calling(h, &admin).await);
    // Flows (roadmap M5d): the Flow key, a user's answers, tokens and the
    // preview link are secrets.
    secrets.section("flows");
    secrets.extend(flows(h, &admin).await);
    // Commerce, QR codes, analytics and the block list (roadmap M5g): the
    // people blocked, a code's message and a number filter are not logged.
    secrets.section("commerce");
    secrets.extend(commerce_qr_analytics(h, &admin).await);
    // The Marketing Messages API, max price, welcome message sequences and
    // the max price agreement (roadmap M5h): a recipient, a parameter, a
    // welcome message and the signer are not logged.
    secrets.section("marketing");
    secrets.extend(marketing(h, &admin).await);
    // Conversation routing (roadmap M5l): the user whose thread it is and
    // the note forwarded to the new owner are not logged.
    secrets.section("routing");
    secrets.extend(routing(h, &admin).await);
    secrets.section("webhook_endpoints");
    secrets.extend(webhook_endpoints(h, &admin).await);
    // Embedded Signup (roadmap M3a): neither the merchant's token, nor the
    // PIN, the code or the state is logged.
    secrets.section("signup");
    secrets.extend(signup(h, &admin).await);
    // Hosted signup, several WABAs and pre-verified numbers (roadmap M3f).
    secrets.section("signup_m3f");
    secrets.extend(signup_m3f(h, &admin).await);
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
    secrets.section("spare-keys");
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
    // The token-expiry task (roadmap M3e): a round that tells a tenant of a
    // token near its expiry, and logs one it could not read. No token, no
    // part of one, no tenant's record reaches a log line.
    secrets.section("token_expiry");
    secrets.extend(token_expiry(h).await);
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

/// The token-expiry task (roadmap M3e) over two WABAs of a tenant of its
/// own: one whose token expires in two days (told: an event recorded, a line
/// logged) and one whose record is garbage (not readable: a warning logged,
/// counted `failed`). Returns what must not be logged: both tokens and the
/// garbage.
async fn token_expiry(h: &Harness) -> Vec<String> {
    use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
    use meta_whatsapp_rs::core::clock::Clock;
    use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
    use meta_whatsapp_rs::core::secret::AccessToken;
    use meta_whatsapp_rs::core::store::{Expiry, KvStore as _, StoreKey};

    const TOKEN: &str = "EAAG-token-near-its-expiry-SENTINEL-for-the-log-test";
    const OTHER: &str = "EAAG-token-of-the-unreadable-record-SENTINEL-for-the-log-test";
    const GARBAGE: &str = "garbage-stored-as-a-token-record-SENTINEL-for-the-log-test";
    const WABA: &str = "102290129349101";
    const BAD_WABA: &str = "102290129349102";
    h.tenant("merchant-expiry").await;
    for (waba, pn, token) in [
        (WABA, "106540352249101", TOKEN),
        (BAD_WABA, "106540352249102", OTHER),
    ] {
        h.connect("merchant-expiry", waba, &[pn], token).await;
    }
    h.vault
        .store(
            &StoredBusinessToken::new(WabaId::new(WABA).unwrap(), AccessToken::new(TOKEN))
                .phone_number_ids([PhoneNumberId::new("106540352249101").unwrap()])
                .expires_at(h.clock.now() + time::Duration::days(2)),
        )
        .await
        .unwrap();
    h.backend
        .kv()
        .put(
            &StoreKey::new("wa.token", format!("waba/{BAD_WABA}")),
            GARBAGE.as_bytes().to_vec(),
            Expiry::Never,
        )
        .await
        .unwrap();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let task = {
        let (state, backend, stop) = (h.state.clone(), h.backend.clone(), stop.clone());
        tokio::spawn(async move {
            state
                .run_token_expiry(
                    backend,
                    std::time::Duration::from_hours(7 * 24),
                    std::time::Duration::from_millis(20),
                    async move { stop.notified().await },
                )
                .await;
        })
    };
    let start = std::time::Instant::now();
    loop {
        let metrics = h.call(Call::get("/metrics")).await.text;
        if metrics.contains("wa_server_token_expiry_events_total{outcome=\"recorded\"}")
            && metrics.contains("wa_server_token_expiry_events_total{outcome=\"failed\"}")
        {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "no round ran"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    stop.notify_one();
    task.await.unwrap();
    vec![TOKEN.to_owned(), OTHER.to_owned(), GARBAGE.to_owned()]
}

const ECHO_TEXT: &str = "An echo the logs must not hold";
const HISTORY_TEXT: &str = "use code THANKS30";
const UNPARSED_TEXT: &str = "a body that is not a webhook, which the logs must not hold";
const OVERSIZED_TEXT: &str = "an oversized body the logs must not hold";
const FAILED_TEXT: &str = "a message whose recording failed once";
const STANDBY_TEXT: &str = "a standby copy the logs must not hold";
const HANDOVER_TEXT: &str = "a handover note the logs must not hold";
const CLICK_ID: &str = "click-id-the-logs-must-not-hold";
const CLICK_TOKEN: &str = "tracking-token-the-logs-must-not-hold";
const CLICK_PRODUCT: &str = "product-id-the-logs-must-not-hold";

/// Meta's examples of the three types roadmap M2d made tenant-visible, on
/// `pn` of `waba`, carrying what the logs must not hold: a standby copy of
/// the customer's message, a handover's note, a click's ids (Meta names
/// neither the user nor the message, but the click id and token are
/// Meta's tracking).
fn reviewed_deliveries(waba: &str, pn: &str) -> [Vec<u8>; 3] {
    use meta_whatsapp_server_core::conformance::webhooks::{REVIEWED_EXAMPLES, example_on};
    // Another customer than the inbox exercise's (`EXAMPLE_WA_ID`): a
    // standby copy and a handover mark the thread another app's, which
    // would refuse that exercise's reply.
    let of = |kind: &str, wanted: &str| {
        REVIEWED_EXAMPLES
            .iter()
            .find(|(k, example)| *k == kind && example.contains(wanted))
            .unwrap()
            .1
            .replace(EXAMPLE_WA_ID, "16505550199")
    };
    let at = super::meta::sent_now();
    let mut standby = example_on(
        &of("standby_observed", "Test standby message"),
        waba,
        pn,
        "capture",
        at,
    );
    standby["entry"][0]["changes"][0]["value"]["standby"]["messages"][0]["text"]["body"] =
        json!(STANDBY_TEXT);
    let mut handover = example_on(
        &of("thread_control_changed", "control_taken"),
        waba,
        pn,
        "capture",
        at,
    );
    handover["entry"][0]["changes"][0]["value"]["control_taken"]["metadata"] = json!(HANDOVER_TEXT);
    let mut click = example_on(
        &of("user_action_reported", "user_actions"),
        waba,
        pn,
        "capture",
        at,
    );
    let data = &mut click["entry"][0]["changes"][0]["value"]["user_actions"][0]["marketing_messages_link_click_data"];
    data["click_id"] = json!(CLICK_ID);
    data["tracking_token"] = json!(CLICK_TOKEN);
    data["product_id"] = json!(CLICK_PRODUCT);
    [bytes(&standby), bytes(&handover), bytes(&click)]
}

/// Coexistence: an echo of the merchant's phone app and a history sync;
/// Meta's examples of the three types roadmap M2d made tenant-visible; a
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
    let [standby, handover, click] = reviewed_deliveries(waba, pn);
    for body in [
        bytes(&echo),
        bytes(&history),
        standby,
        handover,
        click,
        unparsed,
    ] {
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
        STANDBY_TEXT,
        HANDOVER_TEXT,
        CLICK_ID,
        CLICK_TOKEN,
        CLICK_PRODUCT,
        // The standby copy's and the handover's customer.
        "16505550199",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Meta's deliveries for merchant-42's first number (Meta's text examples,
/// by phone number and by BSUID, a status, an error, a field no library
/// types, and refused ones), then polling them; returns what they carried
/// that the logs must not: the signatures, the message text, the customer's
/// name, BSUIDs and username.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
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
            // The attach's (roadmap M2d), one per number.
            "number_connected",
            "number_connected",
            "message_received",
            "message_received",
            "status_updated",
            "error_reported",
            "message_echoed",
            "history_synced",
            // Tenant-visible since roadmap M2d.
            "standby_observed",
            "thread_control_changed",
            "user_action_reported",
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
        STANDBY_TEXT,
        HANDOVER_TEXT,
        CLICK_ID,
        CLICK_TOKEN,
        CLICK_PRODUCT,
    ] {
        assert!(polled.text.contains(private), "{private}: {}", polled.text);
    }
    // The same events by id and live (roadmap M2b): they carry what the
    // logs must not either.
    // The first message (the attach's `number_connected` events come
    // before it: roadmap M2d).
    let first = polled.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "message_received")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
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

/// The webhook endpoint routes (roadmap M2c) for merchant-42: an endpoint
/// made (its secret, shown once), listed, read, changed, its secret
/// rotated (the new one), a ping, its deliveries, a retry of the ping, and
/// its deletion. The secrets are what the logs must not hold. On a harness
/// without webhooks out, every route answers `409 webhooks_not_configured`,
/// and is exercised all the same.
/// A converter that answers a fixed file in the format asked for, for the
/// log test's converted upload (roadmap M5c1).
#[derive(Debug)]
struct StubConverter;

#[async_trait::async_trait]
impl meta_whatsapp_rs::core::media::MediaConverter for StubConverter {
    async fn convert(
        &self,
        _input: meta_whatsapp_rs::core::media::MediaInput,
        target: meta_whatsapp_rs::core::media::TargetFormat,
    ) -> meta_whatsapp_rs::Result<meta_whatsapp_rs::core::media::ConvertedMedia> {
        Ok(meta_whatsapp_rs::core::media::ConvertedMedia::new(
            &b"OggS-converted"[..],
            target,
        ))
    }
}

/// `settings` with what the resumable upload and conversion routes need to
/// run through (roadmap M5c1): the Meta app, and a converter.
pub fn with_uploads(
    settings: meta_whatsapp_server::state::Settings,
) -> meta_whatsapp_server::state::Settings {
    meta_whatsapp_server::state::Settings {
        app_id: Some(meta_whatsapp_rs::core::ids::AppId::new("634974688087057").unwrap()),
        media_converter: Some(Arc::new(StubConverter)),
        // Roadmap M5c3: the legal acts are taken, so that their routes
        // run through (they are off by default).
        legal_acts: true,
        ..settings
    }
}

/// Webhooks out for a log test: a throwaway data key, loopback listed,
/// plain `http`; no dispatcher runs, so nothing is sent.
pub fn webhooks_out() -> meta_whatsapp_server_core::webhooks_out::dispatch::WebhooksOut {
    use meta_whatsapp_server_core::webhooks_out::destination::DestinationPolicy;
    use meta_whatsapp_server_core::webhooks_out::seal::{DataKey, DataKeys};
    meta_whatsapp_server_core::webhooks_out::dispatch::WebhooksOut::new(
        DataKeys::new(DataKey::generate("k1").unwrap()),
        DestinationPolicy::parse("127.0.0.0/8", true).unwrap(),
        std::sync::Arc::new(meta_whatsapp_server_http::webhooks_out::SystemResolver),
        std::sync::Arc::new(meta_whatsapp_server_http::webhooks_out::HyperSender::new().unwrap()),
    )
}

/// The dispatcher's round on a harness with webhooks out: the ping signed
/// with both secrets and attempted (a closed loopback port), its logs
/// captured with the routes'.
async fn dispatch_round(h: &Harness) {
    if let Some(out) = h.state.settings().webhooks_out.clone() {
        let dispatcher = meta_whatsapp_server_core::webhooks_out::dispatch::Dispatcher::new(
            h.backend.as_ref(),
            std::sync::Arc::new(h.clock.clone()),
            out,
        );
        dispatcher.fan_out().await.unwrap();
        assert_eq!(dispatcher.dispatch_due().await, 1, "the ping, attempted");
    }
}

/// `secrets`, and each `whsec_` secret's key alone (after the prefix).
fn with_bare_keys(mut secrets: Vec<String>) -> Vec<String> {
    let keys: Vec<String> = secrets
        .iter()
        .filter_map(|s| s.strip_prefix("whsec_").map(str::to_owned))
        .collect();
    secrets.extend(keys);
    secrets
}

async fn webhook_endpoints(h: &Harness, admin: &str) -> Vec<String> {
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["webhooks"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let configured = h.state.settings().webhooks_out.is_some();
    let mut secrets = Vec::new();
    let created = h
        .call(
            Call::new(Method::POST, "/v1/webhook-endpoints")
                .key(&key)
                .json(&json!({"url": "http://127.0.0.1:9/hooks?token=receivers-own-token", "types": ["message_received"]})),
        )
        .await;
    let id = if configured {
        assert_eq!(created.status.as_u16(), 201, "{}", created.text);
        secrets.push(created.json()["secret"].as_str().unwrap().to_owned());
        created.json()["id"].as_str().unwrap().to_owned()
    } else {
        assert_eq!(created.code(), "webhooks_not_configured");
        format!("whe_{}", "0".repeat(32))
    };
    secrets.push("receivers-own-token".to_owned());
    let listed = h.call(Call::get("/v1/webhook-endpoints").key(&key)).await;
    let read = h
        .call(Call::get(format!("/v1/webhook-endpoints/{id}")).key(&key))
        .await;
    let changed = h
        .call(
            Call::new(Method::PATCH, format!("/v1/webhook-endpoints/{id}"))
                .key(&key)
                .json(&json!({"types": ["message_received", "status_updated"]})),
        )
        .await;
    let rotated = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/webhook-endpoints/{id}/rotate-secret"),
            )
            .key(&key),
        )
        .await;
    if configured {
        secrets.push(rotated.json()["secret"].as_str().unwrap().to_owned());
    }
    let pinged = h
        .call(Call::new(Method::POST, format!("/v1/webhook-endpoints/{id}/test")).key(&key))
        .await;
    dispatch_round(h).await;
    let deliveries = h
        .call(Call::get(format!("/v1/webhook-endpoints/{id}/deliveries")).key(&key))
        .await;
    if configured {
        assert_eq!(
            deliveries.json()["data"][0]["status"],
            "failed",
            "{}",
            deliveries.text
        );
    }
    let ping = if configured {
        pinged.json()["id"].as_str().unwrap().to_owned()
    } else {
        format!("whd_{}", "0".repeat(32))
    };
    let retried = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/webhook-endpoints/{id}/deliveries/{ping}/retry"),
            )
            .key(&key),
        )
        .await;
    let deleted = h
        .call(Call::new(Method::DELETE, format!("/v1/webhook-endpoints/{id}")).key(&key))
        .await;
    if configured {
        for (reply, status) in [
            (&listed, 200),
            (&read, 200),
            (&changed, 200),
            (&rotated, 200),
            (&pinged, 202),
            (&deliveries, 200),
            (&retried, 202),
            (&deleted, 204),
        ] {
            assert_eq!(reply.status.as_u16(), status, "{}", reply.text);
        }
    }
    with_bare_keys(secrets)
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
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
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
    // Roadmap M2f: the customer erased on the number, by a key of the
    // scope `erasure`; the logs keep the journal's HMACs and the audit
    // event's counts, never the identity (`check`).
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["erasure"]})),
        )
        .await
        .json();
    let erasure_key = minted["key"].as_str().unwrap().to_owned();
    let erasure = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/erasures"))
                .key(&erasure_key)
                .json(&json!({"confirm": PN, "contacts": [EXAMPLE_WA_ID, INBOX_STRANGER]})),
        )
        .await;
    assert_eq!(erasure.status.as_u16(), 200, "{}", erasure.text);
    assert_eq!(
        h.graph.requests().len(),
        asked + 1,
        "an erasure asks Meta nothing"
    );
    vec![
        erasure_key.clone(),
        erasure_key.rsplit('_').next().unwrap().to_owned(),
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
/// A Flow's token (roadmap M5a, design D103): in no log line.
pub const FLOW_TOKEN: &str = "CAPTURE-FLOW-TOKEN-AQAAAAACS5FpgQ";
/// A template Flow button's token and a template's body text (roadmap
/// M5b, design D121): a one-time passcode in a parameter, a customer's
/// words in an edit; in no log line either.
pub const TEMPLATE_FLOW_TOKEN: &str = "CAPTURE-TEMPLATE-FLOW-TOKEN-ZQ81";
pub const TEMPLATE_PASSCODE: &str = "CAPTURE-PASSCODE-482913";
pub const TEMPLATE_BODY: &str = "CAPTURE-TEMPLATE-BODY-Echeveria-sale";

/// A Flow's preview token (roadmap M5d): the link opens the Flow for
/// whoever holds it; in no log line.
pub const FLOW_PREVIEW_TOKEN: &str = "CAPTURE-FLOW-PREVIEW-TOKEN-b9d6c7";
/// What a user typed into a Flow screen (the KAT's `data_exchange`
/// request, `tests/flows_data/kat_endpoint.mjs`) and the Flow token the
/// request carries: in no log line.
const FLOW_ANSWER: &str = "shopping";
const FLOW_ENDPOINT_TOKEN: &str = "flowtoken-1234";
/// A Flow JSON's text: the caller's, not logged either.
const FLOW_JSON_TEXT: &str = "CAPTURE-FLOW-JSON-TEXT-Heading-Hello-World";

/// The Flows routes (roadmap M5d): each management route once with Meta's
/// documented answer (a creation with the Flow JSON, the preview link
/// whose token is a secret, the changes audited), and Meta's data endpoint
/// requests: a health check, an error notification, a data exchange
/// (a user's answer and the Flow token in it), a forged signature and an
/// unsigned request. Neither the Flow private key, a user's answer, a
/// token, the Flow JSON nor a sealed body is logged.
#[allow(clippy::too_many_lines)] // eleven routes and the endpoint, each with its scripted answer
async fn flows(h: &Harness, admin: &str) -> Vec<String> {
    const WABA: &str = "102290129340398";
    const FLOW: &str = "1234567890123456";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["flows"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let call = |method: Method, rest: &str| {
        Call::new(method, format!("/v1/wabas/{WABA}/flows{rest}")).key(&key)
    };
    let read = || {
        h.graph.push_json(
            200,
            json!({"id": FLOW, "name": "My first flow", "status": "DRAFT",
                   "categories": ["OTHER"], "validation_errors": [],
                   "whatsapp_business_account": {"id": WABA}}),
        );
    };
    h.graph.push_json(200, json!({"id": FLOW, "success": true}));
    let created = h
        .call(
            call(Method::POST, "").json(&json!({"name": "My first flow", "categories": ["OTHER"],
            "flow_json": format!("{{\"version\":\"5.0\",\"text\":\"{FLOW_JSON_TEXT}\"}}")})),
        )
        .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text);
    h.graph.push_json(
        200,
        json!({"data": [{"id": FLOW, "name": "My first flow", "status": "DRAFT",
                         "categories": ["OTHER"], "validation_errors": []}],
               "paging": {"cursors": {"before": "a", "after": "b"}}}),
    );
    let listed = h.call(call(Method::GET, "")).await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text);
    read();
    let got = h.call(call(Method::GET, &format!("/{FLOW}"))).await;
    assert_eq!(got.status.as_u16(), 200, "{}", got.text);
    read();
    h.graph.push_json(200, json!({"success": true}));
    let updated = h
        .call(call(Method::PATCH, &format!("/{FLOW}")).json(&json!({"name": "New flow name"})))
        .await;
    assert_eq!(updated.status.as_u16(), 204, "{}", updated.text);
    read();
    h.graph
        .push_json(200, json!({"success": true, "validation_errors": []}));
    let uploaded = h
        .call(
            call(Method::PUT, &format!("/{FLOW}/json"))
                .header("content-type", "application/json")
                .body(meta_whatsapp_rs::webhooks::axum::body::Body::from(format!(
                    "{{\"version\":\"5.0\",\"text\":\"{FLOW_JSON_TEXT}\"}}"
                ))),
        )
        .await;
    assert_eq!(uploaded.status.as_u16(), 200, "{}", uploaded.text);
    read();
    h.graph.push_json(
        200,
        json!({"preview": {"preview_url": format!(
            "https://business.facebook.com/wa/manage/flows/{FLOW}/preview/?token={FLOW_PREVIEW_TOKEN}"),
            "expires_at": "2023-05-21T11:18:09+0000"}, "id": FLOW}),
    );
    let previewed = h
        .call(call(Method::POST, &format!("/{FLOW}/preview")).json(&json!({})))
        .await;
    assert_eq!(previewed.status.as_u16(), 200, "{}", previewed.text);
    assert!(
        previewed.text.contains(FLOW_PREVIEW_TOKEN),
        "the answer has the link"
    );
    for rest in ["/publish", "/deprecate"] {
        read();
        h.graph.push_json(200, json!({"success": true}));
        let done = h.call(call(Method::POST, &format!("/{FLOW}{rest}"))).await;
        assert_eq!(done.status.as_u16(), 204, "{rest}: {}", done.text);
    }
    read();
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h.call(call(Method::DELETE, &format!("/{FLOW}"))).await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text);

    // Meta's data endpoint (the public listener): the three payloads, a
    // forged signature, and no signature at all.
    for name in ["ping", "error_notification", "data_exchange"] {
        let reply = send(
            &h.public,
            super::flows::signed("102290129340398", &super::flows::kat_body(name)).build(),
        )
        .await;
        assert_eq!(reply.status.as_u16(), 200, "{name}: {}", reply.text);
    }
    let forged = send(
        &h.public,
        super::flows::signed_with(
            "102290129340398",
            &super::flows::kat_body("ping"),
            "not-the-apps-secret",
        )
        .build(),
    )
    .await;
    assert_eq!(forged.status.as_u16(), 432, "{}", forged.text);
    let unsigned = send(
        &h.public,
        Call::new(Method::POST, "/webhooks/flows/102290129340398")
            .body(meta_whatsapp_rs::webhooks::axum::body::Body::from("{}"))
            .build(),
    )
    .await;
    assert_eq!(unsigned.status.as_u16(), 432, "{}", unsigned.text);

    let mut secrets = vec![
        key.clone(),
        key.rsplit('_').next().unwrap().to_owned(),
        FLOW_PREVIEW_TOKEN.to_owned(),
        FLOW_ANSWER.to_owned(),
        FLOW_ENDPOINT_TOKEN.to_owned(),
        FLOW_JSON_TEXT.to_owned(),
    ];
    // The Flow private key: every line of its body, and the sealed texts.
    secrets.extend(
        super::flows::KEY_PEM
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .map(str::to_owned),
    );
    for name in ["ping", "error_notification", "data_exchange"] {
        let scenario = super::flows::kat(name);
        secrets.push(scenario["sealed_response"].as_str().unwrap().to_owned());
        secrets.push(
            scenario["request"]["encrypted_flow_data"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        secrets.push(
            scenario["request"]["encrypted_aes_key"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    secrets
}

/// The registration routes (roadmap M3d), each with a PIN or a code in
/// its body, and a refusal whose texts quote the PIN: the PIN and the code
/// are secrets, looked for as a leaked string, JSON value or Meta text
/// would print them (`tests/registration_logs.rs` looks for them as any
/// run of digits). The PIN change and the deregistration are audited.
async fn registration(h: &Harness, tenant_key: &str) -> Vec<String> {
    const PN: &str = "1972385232742141";
    const PIN: &str = "581063";
    const CODE: &str = "774216";
    let post = |path: &str, body: serde_json::Value| {
        Call::new(Method::POST, format!("/v1/numbers/{PN}/{path}"))
            .key(tenant_key)
            .json(&body)
    };
    let ok = || json!({"success": true});
    h.graph.push_json(200, ok());
    let requested = h
        .call(
            post(
                "request-code",
                json!({"code_method": "SMS", "language": "en_US"}),
            )
            .header("idempotency-key", "register:1972385232742141:code"),
        )
        .await;
    assert_eq!(requested.status.as_u16(), 200, "{}", requested.text);
    h.graph.push_json(200, ok());
    let verified = h.call(post("verify-code", json!({"code": CODE}))).await;
    assert_eq!(verified.status.as_u16(), 200, "{}", verified.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#133005) PIN {PIN} is not this number's"),
            "type": "OAuthException", "code": 133005, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
            "error_data": {"messaging_product": "whatsapp", "details": format!("pin {PIN}")}}}),
    );
    let refused = h.call(post("register", json!({"pin": PIN}))).await;
    assert_eq!(refused.code(), "two_step_verification", "{}", refused.text);
    h.graph.push_json(200, ok());
    let registered = h
        .call(post("register", json!({"pin": PIN})).header("idempotency-key", "register:1"))
        .await;
    assert_eq!(registered.status.as_u16(), 200, "{}", registered.text);
    h.graph.push_json(200, ok());
    let set = h.call(post("two-step-pin", json!({"pin": PIN}))).await;
    assert_eq!(set.status.as_u16(), 200, "{}", set.text);
    h.graph.push_json(200, ok());
    let deregistered = h.call(post("deregister", json!({"confirm": PN}))).await;
    assert_eq!(deregistered.status.as_u16(), 200, "{}", deregistered.text);
    let mut secrets = Vec::new();
    for secret in [PIN, CODE] {
        // Quoted, JSON-escaped inside a log line, and as Meta's texts
        // quote it.
        secrets.push(format!("\"{secret}\""));
        secrets.push(format!("{secret}\\\""));
        secrets.push(format!("PIN {secret}"));
        secrets.push(format!("pin {secret}"));
    }
    secrets
}

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
    // A Flow by id with its token (roadmap M5a): its check and send, a
    // local refusal (a misspelt parameter), Meta's refusal of the send.
    let flow = |parameters: serde_json::Value| {
        Call::new(Method::POST, format!("/v1/numbers/{pn}/messages"))
            .key(&key)
            .json(&json!({"to": {"phone": RECIPIENT}, "type": "interactive",
                          "interactive": {"type": "flow", "body": {"text": MESSAGE_TEXT},
                                          "action": {"name": "flow", "parameters": parameters}}}))
    };
    let parameters = json!({"flow_message_version": "3", "flow_token": FLOW_TOKEN,
                            "flow_id": "1234567890123456", "flow_cta": "Book!"});
    let flow_of_the_waba = json!({"id": "1234567890123456",
                                  "whatsapp_business_account": {"id": "102290129340398"}});
    h.graph.push_json(200, flow_of_the_waba.clone());
    h.graph
        .push_json(200, sent_response(RECIPIENT, "wa_id", RECIPIENT_DIGITS));
    let flowed = h.call(flow(parameters.clone())).await;
    assert_eq!(flowed.status.as_u16(), 202, "{}", flowed.text);
    let mut misspelt = parameters.clone();
    misspelt["flow_tokn"] = json!(FLOW_TOKEN);
    let misspelt = h.call(flow(misspelt)).await;
    assert_eq!(misspelt.status.as_u16(), 422, "{}", misspelt.text);
    h.graph.push_json(200, flow_of_the_waba);
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#131009) Parameter value is not valid", "type": "OAuthException",
                         "code": 131009, "error_data": {"details": "Flow token is invalid"},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let failed = h.call(flow(parameters)).await;
    assert_eq!(failed.status.as_u16(), 422, "{}", failed.text);
    // A template with a Flow button (its token) and a passcode parameter:
    // sent, refused locally (a misspelt key), refused by Meta.
    let template_flow = |action: serde_json::Value| {
        Call::new(Method::POST, format!("/v1/numbers/{pn}/messages"))
            .key(&key)
            .json(
                &json!({"to": {"phone": RECIPIENT}, "type": "template", "template": {
                "name": "book_table", "language": {"code": "en_US"},
                "components": [
                    {"type": "body", "parameters": [{"type": "text", "text": TEMPLATE_PASSCODE}]},
                    {"type": "button", "sub_type": "flow", "index": "0",
                     "parameters": [{"type": "action", "action": action}]}]}}),
            )
    };
    h.graph
        .push_json(200, sent_response(RECIPIENT, "wa_id", RECIPIENT_DIGITS));
    let sent = h
        .call(template_flow(json!({"flow_token": TEMPLATE_FLOW_TOKEN})))
        .await;
    assert_eq!(sent.status.as_u16(), 202, "{}", sent.text);
    let misspelt = h
        .call(template_flow(json!({"flow_tokn": TEMPLATE_FLOW_TOKEN})))
        .await;
    assert_eq!(misspelt.status.as_u16(), 422, "{}", misspelt.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#131009) Parameter value is not valid", "type": "OAuthException",
                         "code": 131009, "error_data": {"details": "Flow token is invalid"},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let failed = h
        .call(template_flow(json!({"flow_token": TEMPLATE_FLOW_TOKEN})))
        .await;
    assert_eq!(failed.status.as_u16(), 422, "{}", failed.text);
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
    let mut uploads = resumable_uploads(h, &key, pn).await;
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
    template_lifecycle(h, &key, waba).await;
    uploads.extend([
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
        FLOW_TOKEN.to_owned(),
        TEMPLATE_FLOW_TOKEN.to_owned(),
        TEMPLATE_PASSCODE.to_owned(),
        TEMPLATE_BODY.to_owned(),
    ]);
    uploads
}

/// Roadmap M5b's template routes, each once with Meta's documented answer:
/// an edit, an unpause, a comparison, an archive and an unarchive, the
/// library browsed and a template created from it, a migration, and a
/// group created, read, changed and deleted (the changes audited).
#[allow(clippy::too_many_lines)] // twelve routes, each with its scripted answer
async fn template_lifecycle(h: &Harness, key: &str, waba: &str) {
    const ID: &str = "1407680676729941";
    const OTHER: &str = "1533406637136032";
    let named = |id: &str| {
        h.graph
            .push_json(200, json!({"name": "order_confirmation", "id": id}));
        h.graph.push_json(
            200,
            json!({"data": [{"id": id, "name": "order_confirmation"}]}),
        );
    };
    let call = |method: Method, path: String| Call::new(method, path).key(key);
    named(ID);
    h.graph.push_json(200, json!({"success": true}));
    let edited = h
        .call(
            call(Method::PATCH, format!("/v1/wabas/{waba}/templates/{ID}"))
                .json(&json!({"category": "MARKETING", "components": [
                    {"type": "BODY", "text": TEMPLATE_BODY}]})),
        )
        .await;
    assert_eq!(edited.status.as_u16(), 204, "{}", edited.text);
    named(ID);
    h.graph.push_json(200, json!({"success": true}));
    let unpaused = h
        .call(call(
            Method::POST,
            format!("/v1/wabas/{waba}/templates/{ID}/unpause"),
        ))
        .await;
    assert_eq!(unpaused.status.as_u16(), 204, "{}", unpaused.text);
    named(ID);
    named(OTHER);
    h.graph.push_json(
        200,
        json!({"data": [{"metric": "MESSAGE_SENDS", "type": "NUMBER_VALUES",
                         "number_values": [{"key": ID, "value": 1273}, {"key": OTHER, "value": 1042}]}]}),
    );
    let compared = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{waba}/templates/{ID}/compare?template_ids={OTHER}&start=1674844791182&end=1674845395982"),
        ))
        .await;
    assert_eq!(compared.status.as_u16(), 200, "{}", compared.text);
    for (route, answer) in [
        (
            "archive",
            json!({"archived_templates": [ID], "failed_templates": {}}),
        ),
        (
            "unarchive",
            json!({"unarchived_templates": [ID], "failed_templates": {}}),
        ),
    ] {
        h.graph.push_json(200, json!({"data": [{"id": ID}]}));
        h.graph.push_json(200, answer);
        let done = h
            .call(
                call(Method::POST, format!("/v1/wabas/{waba}/templates/{route}"))
                    .json(&json!({"hsm_ids": [ID]})),
            )
            .await;
        assert_eq!(done.status.as_u16(), 200, "{route}: {}", done.text);
    }
    h.graph.push_json(
        200,
        json!({"data": [{"name": "delivery_update_1", "language": "en_US", "category": "UTILITY",
                         "body": "Good news! Your order #{{1}} is on its way.", "id": "7147013345418927"}]}),
    );
    let browsed = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{waba}/templates/library?topic=order_management"),
        ))
        .await;
    assert_eq!(browsed.status.as_u16(), 200, "{}", browsed.text);
    h.graph.push_json(
        200,
        json!({"id": "1627019861106479", "status": "APPROVED", "category": "UTILITY"}),
    );
    let created = h
        .call(
            call(Method::POST, format!("/v1/wabas/{waba}/templates/library"))
                .json(&super::sample_library_template()),
        )
        .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text);
    h.graph.push_json(
        200,
        json!({"migrated_templates": ["1473688840035974"], "failed_templates": {}}),
    );
    let migrated = h
        .call(
            call(
                Method::POST,
                format!("/v1/wabas/{waba}/templates/migrations"),
            )
            .json(&json!({"source_waba_id": waba, "page_number": 0})),
        )
        .await;
    assert_eq!(migrated.status.as_u16(), 200, "{}", migrated.text);
    let group = "9020555671393375";
    let group_answer = json!({"name": "Black Friday 2024", "id": group,
        "whatsapp_business_account": {"id": waba, "name": "Lucky Shrub"}});
    h.graph.push_json(200, json!({"data": [{"id": ID}]}));
    h.graph.push_json(200, json!({"id": group}));
    let grouped = h
        .call(
            call(Method::POST, format!("/v1/wabas/{waba}/template-groups"))
                .json(&json!({"name": "Black Friday 2024", "whatsapp_business_templates": [ID]})),
        )
        .await;
    assert_eq!(grouped.status.as_u16(), 201, "{}", grouped.text);
    h.graph.push_json(200, group_answer.clone());
    let read = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{waba}/template-groups/{group}"),
        ))
        .await;
    assert_eq!(read.status.as_u16(), 200, "{}", read.text);
    h.graph.push_json(200, group_answer.clone());
    h.graph.push_json(200, json!({"success": true}));
    let renamed = h
        .call(
            call(
                Method::PATCH,
                format!("/v1/wabas/{waba}/template-groups/{group}"),
            )
            .json(&json!({"name": "Black Friday 2025"})),
        )
        .await;
    assert_eq!(renamed.status.as_u16(), 204, "{}", renamed.text);
    h.graph.push_json(200, group_answer);
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(call(
            Method::DELETE,
            format!("/v1/wabas/{waba}/template-groups/{group}"),
        ))
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text);
}

/// The person blocked (a phone number, its digits, a BSUID), a QR code's
/// message and an analytics phone filter (roadmap M5g): in no log line.
pub const BLOCKED_PHONE: &str = "+16505559876";
pub const BLOCKED_DIGITS: &str = "16505559876";
pub const BLOCKED_BSUID: &str = "US.99887766554433221100";
pub const QR_MESSAGE: &str = "CAPTURE-QR-MESSAGE-Echeveria-monday";
pub const ANALYTICS_NUMBER: &str = "16505550999";

/// Roadmap M5g's routes on merchant-42's WABA and first number, each once
/// with Meta's documented answer: the commerce settings and a catalog with
/// its products, the QR codes (created, listed, read, changed, deleted),
/// the four analytics, and the block list (a block answered in part, an
/// unblock, a block cut by a timeout, one refused by Meta, whose texts
/// quote the person); returns what the logs must not hold. The audit
/// events are `check_audit`'s.
#[allow(clippy::too_many_lines)] // eighteen routes, each with its scripted answer
async fn commerce_qr_analytics(h: &Harness, admin: &str) -> Vec<String> {
    use meta_whatsapp_rs::core::clock::Clock as _;
    use time::format_description::well_known::Rfc3339;
    const WABA: &str = "102290129340398";
    const PN: &str = "1972385232742141";
    const CATALOG: &str = "194836987003835";
    const CODE: &str = "4O4YGZEG3RIVE1";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["commerce", "qr_codes", "analytics", "contacts"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let call = |method: Method, path: String| Call::new(method, path).key(&key);
    let ok = |reply: &super::Reply, status: u16| {
        assert_eq!(reply.status.as_u16(), status, "{}", reply.text);
    };
    // Commerce settings.
    h.graph.push_json(
        200,
        json!({"data": [{"is_cart_enabled": true, "is_catalog_visible": true, "id": "727705352028726"}]}),
    );
    let settings = h
        .call(call(
            Method::GET,
            format!("/v1/numbers/{PN}/commerce-settings"),
        ))
        .await;
    ok(&settings, 200);
    h.graph.push_json(200, json!({"success": true}));
    let changed = h
        .call(
            call(Method::PATCH, format!("/v1/numbers/{PN}/commerce-settings"))
                .json(&json!({"is_cart_enabled": false})),
        )
        .await;
    ok(&changed, 204);
    // The catalog, then its products.
    let catalogs = json!({"data": [{"id": CATALOG, "name": "Lucky Shrub", "product_count": 1, "vertical": "commerce"}]});
    h.graph.push_json(200, catalogs.clone());
    let listed = h
        .call(call(Method::GET, format!("/v1/wabas/{WABA}/catalogs")))
        .await;
    ok(&listed, 200);
    h.graph.push_json(200, catalogs.clone());
    let one = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}"),
        ))
        .await;
    ok(&one, 200);
    h.graph.push_json(200, catalogs);
    h.graph.push_json(
        200,
        json!({"data": [{"id": "5000000000000001", "retailer_id": "sku-1", "name": "Mug"}],
               "paging": {"cursors": {"before": "QVFI", "after": "MjQZD"},
                          "next": "https://graph.facebook.com/v25.0/x?after=MjQZD&access_token=CAPTURE-PAGING-TOKEN"}}),
    );
    let products = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products?limit=1"),
        ))
        .await;
    ok(&products, 200);
    // QR codes.
    let code = json!({"code": CODE, "prefilled_message": QR_MESSAGE,
                      "deep_link_url": format!("https://wa.me/message/{CODE}")});
    h.graph.push_json(200, code.clone());
    let created = h
        .call(
            call(Method::POST, format!("/v1/numbers/{PN}/qr-codes"))
                .json(&json!({"prefilled_message": QR_MESSAGE})),
        )
        .await;
    ok(&created, 201);
    h.graph.push_json(200, json!({"data": [code.clone()]}));
    let codes = h
        .call(call(Method::GET, format!("/v1/numbers/{PN}/qr-codes")))
        .await;
    ok(&codes, 200);
    h.graph.push_json(200, json!({"data": [code.clone()]}));
    let read = h
        .call(call(
            Method::GET,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
        ))
        .await;
    ok(&read, 200);
    h.graph.push_json(200, json!({"data": [code.clone()]}));
    h.graph.push_json(200, code.clone());
    let updated = h
        .call(
            call(Method::PATCH, format!("/v1/numbers/{PN}/qr-codes/{CODE}"))
                .json(&json!({"prefilled_message": QR_MESSAGE})),
        )
        .await;
    ok(&updated, 200);
    // A creation and a change Meta refuses with texts quoting the message:
    // in no answer and no log (the message is a person's text).
    let quoting = || {
        json!({"error": {"message": format!("(#100) Invalid parameter {QR_MESSAGE}"),
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": QR_MESSAGE},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}})
    };
    h.graph.push_json(400, quoting());
    let refused_creation = h
        .call(
            call(Method::POST, format!("/v1/numbers/{PN}/qr-codes"))
                .json(&json!({"prefilled_message": QR_MESSAGE})),
        )
        .await;
    assert!(
        refused_creation.status.is_client_error(),
        "{}",
        refused_creation.text
    );
    h.graph.push_json(200, json!({"data": [code.clone()]}));
    h.graph.push_json(400, quoting());
    let refused_change = h
        .call(
            call(Method::PATCH, format!("/v1/numbers/{PN}/qr-codes/{CODE}"))
                .json(&json!({"prefilled_message": QR_MESSAGE})),
        )
        .await;
    assert!(
        refused_change.status.is_client_error(),
        "{}",
        refused_change.text
    );
    // Another number's code: refused after the lookup, not audited.
    h.graph.push_json(200, json!({"data": []}));
    let other = h
        .call(call(
            Method::DELETE,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
        ))
        .await;
    ok(&other, 404);
    h.graph.push_json(200, json!({"data": [code]}));
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(call(
            Method::DELETE,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
        ))
        .await;
    ok(&deleted, 204);
    // Analytics.
    let end = h.clock.now().replace_nanosecond(0).unwrap() - time::Duration::days(1);
    let start = end - time::Duration::days(3);
    let (s, e) = (start.unix_timestamp(), end.unix_timestamp());
    let times = format!(
        "start={}&end={}",
        start.format(&Rfc3339).unwrap(),
        end.format(&Rfc3339).unwrap()
    );
    h.graph.push_json(
        200,
        json!({"analytics": {"phone_numbers": [ANALYTICS_NUMBER], "country_codes": ["US"], "granularity": "DAY",
               "data_points": [{"start": s, "end": e, "sent": 3, "delivered": 2}]}, "id": WABA}),
    );
    let messaging = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/analytics/messaging?{times}&granularity=DAY&phone_numbers={ANALYTICS_NUMBER}"),
        ))
        .await;
    ok(&messaging, 200);
    h.graph.push_json(
        200,
        json!({"conversation_analytics": {"data": [{"data_points": [
            {"start": s, "end": e, "conversation": 3, "phone_number": ANALYTICS_NUMBER, "cost": 1.5}]}]}}),
    );
    let conversations = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/analytics/conversations?{times}&granularity=DAILY&dimensions=PHONE"),
        ))
        .await;
    ok(&conversations, 200);
    h.graph.push_json(
        200,
        json!({"pricing_analytics": {"data": [{"data_points": [
            {"start": s, "end": e, "country": "IN", "pricing_type": "REGULAR", "volume": 2, "cost": 4.6}]}]}}),
    );
    let pricing = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/analytics/pricing?{times}&granularity=DAILY"),
        ))
        .await;
    ok(&pricing, 200);
    h.graph
        .push_json(200, json!({"data": [{"id": "1421988012088524"}]}));
    h.graph.push_json(
        200,
        json!({"data": [{"granularity": "DAILY", "product_type": "cloud_api", "data_points": [
            {"template_id": "1421988012088524", "start": s, "end": e, "sent": 1, "delivered": 1, "read": 1}]}]}),
    );
    let templates = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/analytics/templates?{times}&template_ids=1421988012088524"),
        ))
        .await;
    ok(&templates, 200);
    // The block list: a page, a block answered in part (Meta's texts quote
    // the person), an unblock, a block cut by a timeout, a block Meta
    // refuses with a text quoting the person.
    h.graph.push_json(
        200,
        json!({"data": [{"messaging_product": "whatsapp", "wa_id": BLOCKED_DIGITS}],
               "paging": {"cursors": {"after": "eyJ2", "before": "eyJ2"}}}),
    );
    let page = h
        .call(call(Method::GET, format!("/v1/numbers/{PN}/blocked-users")))
        .await;
    ok(&page, 200);
    let people = json!({"users": [{"phone": BLOCKED_PHONE}, {"user_id": BLOCKED_BSUID}]});
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "block_users": {
            "added_users": [{"input": BLOCKED_PHONE, "wa_id": BLOCKED_DIGITS}],
            "failed_users": [{"input": BLOCKED_BSUID, "user_id": BLOCKED_BSUID,
                "errors": [{"message": format!("Re-engagement required for {BLOCKED_PHONE}"),
                            "code": 131_047,
                            "error_data": {"details": format!("{BLOCKED_BSUID} has not messaged")}}]}]}}),
    );
    let blocked = h
        .call(call(Method::POST, format!("/v1/numbers/{PN}/blocked-users")).json(&people))
        .await;
    ok(&blocked, 200);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "block_users": {
            "removed_users": [{"input": BLOCKED_PHONE, "wa_id": BLOCKED_DIGITS}]}}),
    );
    let unblocked = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/blocked-users/unblock"),
            )
            .json(&json!({"users": [{"phone": BLOCKED_PHONE}]})),
        )
        .await;
    ok(&unblocked, 200);
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let cut = h
        .call(call(Method::POST, format!("/v1/numbers/{PN}/blocked-users")).json(&people))
        .await;
    ok(&cut, 504);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) Invalid parameter {BLOCKED_PHONE}"),
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": format!("{BLOCKED_DIGITS} {BLOCKED_BSUID}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h
        .call(call(Method::POST, format!("/v1/numbers/{PN}/blocked-users")).json(&people))
        .await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    vec![
        key.clone(),
        key.rsplit('_').next().unwrap().to_owned(),
        BLOCKED_PHONE.to_owned(),
        BLOCKED_DIGITS.to_owned(),
        BLOCKED_BSUID.to_owned(),
        QR_MESSAGE.to_owned(),
        ANALYTICS_NUMBER.to_owned(),
        "CAPTURE-PAGING-TOKEN".to_owned(),
    ]
}

/// The user of a thread action (a phone number, its digits, a BSUID) and
/// the `metadata` forwarded to the new owner (roadmap M5l): in no log line.
pub const ROUTING_PHONE: &str = "+16505557321";
pub const ROUTING_DIGITS: &str = "16505557321";
pub const ROUTING_BSUID: &str = "US.55443322110099887766";
pub const ROUTING_NOTE: &str = "CAPTURE-ROUTING-NOTE-Aloe-wants-a-human";

/// Roadmap M5l's routes on merchant-42's first number, each once with
/// Meta's documented answer: the roles, a pass (targeted and not), a take,
/// a release, a pass cut by a timeout and a take Meta refuses with a text
/// quoting the user; returns what the logs must not hold. The audit
/// events are `check_audit`'s.
async fn routing(h: &Harness, admin: &str) -> Vec<String> {
    const PN: &str = "1972385232742141";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["routing"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let ok = |reply: &super::Reply, status: u16| {
        assert_eq!(reply.status.as_u16(), status, "{}", reply.text);
    };
    let act = |action: &str, body: serde_json::Value| {
        Call::new(
            Method::POST,
            format!("/v1/numbers/{PN}/thread-control/{action}"),
        )
        .key(&key)
        .json(&body)
    };
    h.graph
        .push_json(200, json!({"roles": ["escalation"], "is_standby": false}));
    let roles = h
        .call(Call::get(format!("/v1/numbers/{PN}/routing-roles")).key(&key))
        .await;
    ok(&roles, 200);
    let accepted = || json!({"messaging_product": "whatsapp", "request_id": "REQ-LOG"});
    h.graph.push_json(200, accepted());
    let passed = h
        .call(act(
            "pass",
            json!({"user": {"user_id": ROUTING_BSUID}, "target_role": "ai_agent",
                   "metadata": ROUTING_NOTE}),
        ))
        .await;
    ok(&passed, 200);
    h.graph.push_json(200, accepted());
    let untargeted = h
        .call(act("pass", json!({"user": {"phone": ROUTING_PHONE}})))
        .await;
    ok(&untargeted, 200);
    h.graph.push_json(200, accepted());
    let taken = h
        .call(act(
            "take",
            json!({"user": {"wa_id": ROUTING_DIGITS}, "metadata": ROUTING_NOTE}),
        ))
        .await;
    ok(&taken, 200);
    h.graph.push_json(200, accepted());
    let released = h
        .call(act("release", json!({"user": {"phone": ROUTING_PHONE}})))
        .await;
    ok(&released, 200);
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let cut = h
        .call(act(
            "pass",
            json!({"user": {"phone": ROUTING_PHONE}, "target_role": "ctwa",
                   "metadata": ROUTING_NOTE}),
        ))
        .await;
    ok(&cut, 504);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) recipient is invalid {ROUTING_BSUID} {ROUTING_PHONE}"),
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": format!("{ROUTING_DIGITS} {ROUTING_NOTE}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h
        .call(act(
            "take",
            json!({"user": {"user_id": ROUTING_BSUID}, "metadata": ROUTING_NOTE}),
        ))
        .await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    vec![
        key.clone(),
        key.rsplit('_').next().unwrap().to_owned(),
        ROUTING_PHONE.to_owned(),
        ROUTING_DIGITS.to_owned(),
        ROUTING_BSUID.to_owned(),
        ROUTING_NOTE.to_owned(),
    ]
}

/// A group's id (`groups/webhooks`: it embeds the business number) is in no
/// line of the service's own, and in the library's `graph request` debug
/// line only (it names the path it calls: [`check`] says so); a group's
/// invite links (credentials), a join request's id (it embeds a number) and
/// the participants are in no log line.
const GROUP_ID: &str = "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjAdOTY0MTUZD";
const GROUP_LINK: &str = "https://chat.whatsapp.com/BjCg8D2zYrE7kS1uFp0xQa";
const GROUP_LINK_CODE: &str = "BjCg8D2zYrE7kS1uFp0xQa";
const GROUP_NEW_LINK_CODE: &str = "ResetLinkCodeForTheLogs9xQ";
const GROUP_JOIN_ID: &str = "MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw";
const GROUP_JOIN_ID_2: &str = "MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIx";
const GROUP_PHONE: &str = "+16315557777";
const GROUP_WA_ID: &str = "16315558888";
const GROUP_BSUID: &str = "US.4021998877665544";
const GROUP_USERNAME: &str = "@group-member-for-the-logs";
const GROUP_PICTURE_MARKER: &str = "GroupPictureBytesForTheLogs-4Tq8";

/// The group routes (roadmap M5f), each exercised on the tenant's
/// attached number: a creation asked and reported (Meta's event makes the
/// group the number's), the listing, the read, a change, the invite link
/// read and reset (once cut by a timeout), the participants listed and
/// removed (the answer Meta's `206`, a refusal quoting a number), the join
/// requests listed, approved (one refused) and rejected, the analytics and
/// the deletion, then Meta's deletion event. Returns what must not be
/// logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn groups(h: &Harness, admin: &str) -> Vec<String> {
    const WABA: &str = "102290129340398";
    const PN: &str = "1972385232742141";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["groups"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let base = format!("/v1/numbers/{PN}/groups");
    let one = format!("{base}/{GROUP_ID}");
    let call = |method: Method, path: &str| Call::new(method, path).key(&key);
    // A creation, asked under a key and repeated.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "request_id": "6f1c2d3e-4a5b-4c6d-8e7f-9a0b1c2d3e4f"}),
    );
    let create = || {
        call(Method::POST, &base)
            .header("idempotency-key", "group-log-1")
            .json(&json!({"subject": "New Purchase Inquiry", "join_approval_mode": "approval_required"}))
    };
    let created = h.call(create()).await;
    assert_eq!(created.status.as_u16(), 202, "{}", created.text);
    let replayed = h.call(create()).await;
    assert_eq!(replayed.status.as_u16(), 202, "{}", replayed.text);
    // Meta's event (the invite link in it) makes it the number's.
    let reported = h
        .webhook(&super::meta::group_created(WABA, PN, GROUP_ID))
        .await;
    assert_eq!(reported.status.as_u16(), 200, "{}", reported.text);
    h.graph.push_json(
        200,
        json!({"data": {"groups": [{"id": GROUP_ID, "subject": "New Purchase Inquiry", "created_at": "1683731200"}]},
               "paging": {"cursors": {"after": "AFTER", "before": "BEFORE"}}}),
    );
    let listed = h.call(Call::get(&base).key(&key)).await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": GROUP_ID, "subject": "New Purchase Inquiry",
               "suspended": false, "total_participant_count": 2}),
    );
    let read = h.call(Call::get(&one).key(&key)).await;
    assert_eq!(read.status.as_u16(), 200, "{}", read.text);
    h.graph.push_json(200, json!({"success": true}));
    let changed = h
        .call(call(Method::PATCH, &one).json(&json!({"description": "Jim's purchase options."})))
        .await;
    assert_eq!(changed.status.as_u16(), 204, "{}", changed.text);
    // The picture (roadmap M5f2): a marker rides in the image's bytes.
    h.graph.push_json(200, json!({"success": true}));
    let picture = super::jpeg(192, 192, GROUP_PICTURE_MARKER.as_bytes());
    let pictured = h
        .call(call(Method::PUT, &format!("{one}/picture")).multipart(&[(
            "file",
            Some("group.jpg"),
            &picture,
        )]))
        .await;
    assert_eq!(pictured.status.as_u16(), 204, "{}", pictured.text);
    // The invite link: read, reset, reset again with the call cut.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "invite_link": GROUP_LINK}),
    );
    let link = h
        .call(Call::get(format!("{one}/invite-link")).key(&key))
        .await;
    assert_eq!(link.status.as_u16(), 200, "{}", link.text);
    assert!(link.text.contains(GROUP_LINK_CODE), "the caller gets it");
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp",
               "invite_link": format!("https://chat.whatsapp.com/{GROUP_NEW_LINK_CODE}")}),
    );
    let reset = h
        .call(call(Method::POST, &format!("{one}/invite-link")))
        .await;
    assert_eq!(reset.status.as_u16(), 200, "{}", reset.text);
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let cut = h
        .call(call(Method::POST, &format!("{one}/invite-link")))
        .await;
    assert_eq!(cut.status.as_u16(), 504, "{}", cut.text);
    // The participants, listed and removed.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": GROUP_ID, "total_participant_count": 3,
               "participants": [{"wa_id": GROUP_WA_ID}, {"user_id": GROUP_BSUID, "username": GROUP_USERNAME}]}),
    );
    let participants = h
        .call(Call::get(format!("{one}/participants")).key(&key))
        .await;
    assert_eq!(participants.status.as_u16(), 200, "{}", participants.text);
    assert!(
        participants.text.contains(GROUP_WA_ID),
        "the caller gets it"
    );
    let remove = |users: serde_json::Value| {
        call(Method::POST, &format!("{one}/participants/remove")).json(&json!({"users": users}))
    };
    h.graph.push_json(200, json!({"success": true}));
    let removed = h
        .call(remove(
            json!([{"phone": GROUP_PHONE}, {"wa_id": GROUP_WA_ID}, {"user_id": GROUP_BSUID}]),
        ))
        .await;
    assert_eq!(removed.status.as_u16(), 200, "{}", removed.text);
    h.graph.push_json(206, json!({"success": true}));
    let partial = h.call(remove(json!([{"phone": GROUP_PHONE}]))).await;
    assert_eq!(partial.status.as_u16(), 200, "{}", partial.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#131203) Recipient {GROUP_PHONE} has not accepted"),
                         "type": "OAuthException", "code": 131_203,
                         "error_data": {"messaging_product": "whatsapp",
                                        "details": format!("{GROUP_PHONE} {GROUP_USERNAME}")}}}),
    );
    let refused = h.call(remove(json!([{"phone": GROUP_PHONE}]))).await;
    // `131203` is `409 recipient_terms_not_accepted` since L27 (it was
    // `502 unknown`); the logs must hold none of Meta's text either way.
    assert_eq!(refused.status.as_u16(), 409, "{}", refused.text);
    // The join requests.
    h.graph.push_json(
        200,
        json!({"data": [{"join_request_id": GROUP_JOIN_ID, "wa_id": GROUP_WA_ID, "creation_timestamp": 1_755_548_877}],
               "paging": {"cursors": {"before": "B", "after": "A"}}}),
    );
    let requests = h
        .call(Call::get(format!("{one}/join-requests")).key(&key))
        .await;
    assert_eq!(requests.status.as_u16(), 200, "{}", requests.text);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "approved_join_requests": [GROUP_JOIN_ID],
               "failed_join_requests": [{"join_request_id": GROUP_JOIN_ID_2, "errors": [{
                   "code": 131_203, "message": format!("{GROUP_PHONE} has not accepted"),
                   "title": "Unable to add participant to group",
                   "error_data": {"details": GROUP_USERNAME}}]}]}),
    );
    let approved = h
        .call(
            call(Method::POST, &format!("{one}/join-requests/approve"))
                .json(&json!({"join_request_ids": [GROUP_JOIN_ID, GROUP_JOIN_ID_2]})),
        )
        .await;
    assert_eq!(approved.status.as_u16(), 200, "{}", approved.text);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "rejected_join_requests": [GROUP_JOIN_ID_2]}),
    );
    let rejected = h
        .call(
            call(Method::POST, &format!("{one}/join-requests/reject"))
                .json(&json!({"join_request_ids": [GROUP_JOIN_ID_2]})),
        )
        .await;
    assert_eq!(rejected.status.as_u16(), 200, "{}", rejected.text);
    // The analytics.
    let now = time::OffsetDateTime::now_utc();
    let at = |days: i64| {
        (now - time::Duration::days(days))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    };
    h.graph.push_json(
        200,
        json!({"data": [{"granularity": "DAILY", "data_points": [
            {"group_id": GROUP_ID, "start": 1_685_548_801, "end": 1_685_635_200, "sent": 100}]}]}),
    );
    let analytics = h
        .call(
            Call::get(format!(
                "{one}/analytics?start={}&end={}&metrics=SENT",
                at(3),
                at(1)
            ))
            .key(&key),
        )
        .await;
    assert_eq!(analytics.status.as_u16(), 200, "{}", analytics.text);
    // The deletion, and Meta's event that says it is done.
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h.call(call(Method::DELETE, &one)).await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text);
    let mut gone = super::meta::with_ids(
        serde_json::from_str(include_str!(
            "../../../meta-whatsapp-webhooks/tests/fixtures/pages/groups.webhooks__delete_group_succeed.json"
        ))
        .unwrap(),
        WABA,
        PN,
    );
    gone["entry"][0]["changes"][0]["value"]["groups"][0]["group_id"] = json!(GROUP_ID);
    let gone = h.webhook(&bytes(&gone)).await;
    assert_eq!(gone.status.as_u16(), 200, "{}", gone.text);
    vec![
        GROUP_LINK_CODE.to_owned(),
        GROUP_NEW_LINK_CODE.to_owned(),
        GROUP_JOIN_ID.to_owned(),
        GROUP_JOIN_ID_2.to_owned(),
        GROUP_PHONE.to_owned(),
        "16315557777".to_owned(),
        GROUP_WA_ID.to_owned(),
        GROUP_BSUID.to_owned(),
        GROUP_USERNAME.to_owned(),
        GROUP_PICTURE_MARKER.to_owned(),
    ]
}

/// What the calling scenario plants: an SDP offer and answer with their
/// ICE credentials and addresses, a SIP server's host and password, the
/// callee and the call's id, and a text Meta quotes in a refusal.
const CALL_ICE_PWD_OFFER: &str = "IcePwdOfTheOfferForTheLogs9Q";
const CALL_ICE_PWD_ANSWER: &str = "IcePwdOfTheAnswerForTheLogs9Q";
const CALL_ADDRESS: &str = "203.0.113.77";
const CALL_SIP_PASSWORD: &str = "SipPasswordForTheLogs-7Zk2";
const CALL_SIP_HOST: &str = "sip.calling-logs.example";
const CALL_PHONE: &str = "+16315559090";
const CALL_BSUID: &str = "US.4021998800112233";
const CALL_ID: &str = "wacid.CallIdForTheLogsABGGFjFVU2";
const CALL_ID_USER: &str = "wacid.UserCallIdForTheLogsHBgLMTIx";
const CALL_META_TEXT: &str = "MetaQuotedSdpLineForTheLogs=a-candidate-9Q";

/// The calling routes (roadmap M5e), each exercised on the tenant's
/// attached number: the settings read (Meta sends the SIP password) and
/// changed (the SIP server, a voicemail announcement looked up), the
/// permission read by phone and by BSUID, a call placed under a key (and
/// repeated) with a recording, a user's call reported by Meta's event then
/// pre-accepted, accepted with a transcription, rejected, ended (once cut
/// by a timeout, once refused with a text of Meta's quoting an SDP line)
/// and the call analytics. Returns what must not be logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn calling(h: &Harness, admin: &str) -> Vec<String> {
    const WABA: &str = "102290129340398";
    const PN: &str = "1972385232742141";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["calls", "analytics"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let call = |method: Method, path: &str| Call::new(method, path).key(&key);
    let offer = format!(
        "v=0\r\no=- 1 2 IN IP4 {CALL_ADDRESS}\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=ice-pwd:{CALL_ICE_PWD_OFFER}\r\n"
    );
    let answer = format!(
        "v=0\r\no=- 3 4 IN IP4 {CALL_ADDRESS}\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=ice-pwd:{CALL_ICE_PWD_ANSWER}\r\n"
    );
    // The settings, with the SIP password Meta sends.
    h.graph.push_json(
        200,
        json!({"calling": {"status": "ENABLED", "sip": {"status": "ENABLED", "servers": [
            {"hostname": CALL_SIP_HOST, "sip_user_password": CALL_SIP_PASSWORD}]}}}),
    );
    let read = h
        .call(Call::get(format!("/v1/numbers/{PN}/call-settings")).key(&key))
        .await;
    assert_eq!(read.status.as_u16(), 200, "{}", read.text);
    assert!(!read.text.contains(CALL_SIP_PASSWORD), "{}", read.text);
    h.graph.push_json(200, json!({"success": true}));
    let changed = h
        .call(
            call(Method::PATCH, &format!("/v1/numbers/{PN}/call-settings")).json(&json!({
                "call_hours": {"status": "ENABLED", "timezone_id": "America/Manaus",
                    "weekly_operating_hours": [
                        {"day_of_week": "MONDAY", "open_time": "0900", "close_time": "1700"}]},
                "sip": {"status": "ENABLED", "servers": [{"hostname": CALL_SIP_HOST}]}
            })),
        )
        .await;
    assert_eq!(changed.status.as_u16(), 204, "{}", changed.text);
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "url": "https://lookaside.fbsbx.com/x",
               "mime_type": "audio/ogg", "sha256": "abc", "file_size": 1, "id": "1037543291543636"}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let voicemail = h
        .call(
            call(Method::PATCH, &format!("/v1/numbers/{PN}/call-settings")).json(&json!({
                "voicemail": {"status": "ENABLED", "triggers": ["REJECT"],
                    "audio": {"default": {"announcement_media_id": "1037543291543636"}}}
            })),
        )
        .await;
    assert_eq!(voicemail.status.as_u16(), 204, "{}", voicemail.text);
    // The permission, by phone number and by BSUID.
    for query in [
        format!("phone={}", CALL_PHONE.replace('+', "%2B")),
        format!("user_id={CALL_BSUID}"),
    ] {
        h.graph.push_json(
            200,
            json!({"messaging_product": "whatsapp", "permission": {"status": "permanent"},
                   "actions": []}),
        );
        let permission = h
            .call(Call::get(format!("/v1/numbers/{PN}/call-permissions?{query}")).key(&key))
            .await;
        assert_eq!(permission.status.as_u16(), 200, "{}", permission.text);
    }
    // A call placed under a key and repeated, with a recording.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "calls": [{"id": CALL_ID}]}),
    );
    let place = || {
        call(Method::POST, &format!("/v1/numbers/{PN}/calls"))
            .header("idempotency-key", "call-log-1")
            .json(
                &json!({"to": {"phone": CALL_PHONE, "user_id": CALL_BSUID}, "sdp_offer": offer,
                "recording": {"status": "ENABLED", "purpose": "quality assurance",
                              "announcement_language": "en_US"}}),
            )
    };
    let placed = h.call(place()).await;
    assert_eq!(placed.status.as_u16(), 201, "{}", placed.text);
    let replayed = h.call(place()).await;
    assert_eq!(replayed.status.as_u16(), 201, "{}", replayed.text);
    // A refused placement logs nothing of its SDP either.
    let bad = h
        .call(
            call(Method::POST, &format!("/v1/numbers/{PN}/calls"))
                .json(&json!({"to": {"phone": "16315559090"}, "sdp_offer": offer})),
        )
        .await;
    assert_eq!(bad.status.as_u16(), 422, "{}", bad.text);
    // A user's call, reported by Meta's event (the user's offer in it),
    // then answered.
    let reported = h
        .webhook(&super::meta::call_connected(WABA, PN, CALL_ID_USER))
        .await;
    assert_eq!(reported.status.as_u16(), 200, "{}", reported.text);
    let one = format!("/v1/numbers/{PN}/calls/{CALL_ID_USER}");
    for (action, body) in [
        ("pre-accept", json!({"sdp_answer": answer})),
        (
            "accept",
            json!({"sdp_answer": answer, "biz_opaque_callback_data": "random_string",
                   "transcription": {"status": "ENABLED", "purpose": "notes",
                                     "announcement_language": "en_US"}}),
        ),
    ] {
        h.graph.push_json(200, json!({"success": true}));
        let reply = h
            .call(call(Method::POST, &format!("{one}/{action}")).json(&body))
            .await;
        assert_eq!(reply.status.as_u16(), 204, "{action}: {}", reply.text);
    }
    h.graph.push_json(200, json!({"success": true}));
    let rejected = h.call(call(Method::POST, &format!("{one}/reject"))).await;
    assert_eq!(rejected.status.as_u16(), 204, "{}", rejected.text);
    h.graph.push_json(200, json!({"success": true}));
    let ended = h
        .call(call(Method::POST, &format!("{one}/terminate")))
        .await;
    assert_eq!(ended.status.as_u16(), 204, "{}", ended.text);
    // An ending cut by a timeout (audited as unknown), and one Meta
    // refuses quoting an SDP line (audited by nothing: it did not go
    // through).
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let cut = h
        .call(call(Method::POST, &format!("{one}/terminate")))
        .await;
    assert_eq!(cut.status.as_u16(), 504, "{}", cut.text);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#138000) {CALL_META_TEXT}"),
                         "type": "OAuthException", "code": 138_000,
                         "error_data": {"messaging_product": "whatsapp",
                                        "details": CALL_META_TEXT}}}),
    );
    let refused = h
        .call(call(Method::POST, &format!("{one}/accept")).json(&json!({"sdp_answer": answer})))
        .await;
    // 138000 is `calling_not_enabled` since roadmap L28 (it was `502 unknown`).
    assert_eq!(refused.status.as_u16(), 409, "{}", refused.text);
    assert!(!refused.text.contains(CALL_META_TEXT), "{}", refused.text);
    // The call analytics.
    let now = time::OffsetDateTime::now_utc();
    let at = |days: i64| {
        (now - time::Duration::days(days))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
            .replace('+', "%2B")
    };
    h.graph.push_json(
        200,
        json!({"call_analytics": {"granularity": "DAILY", "data_points": [
            {"start": 1_765_958_400, "end": 1_766_044_800, "count": 35, "average_duration": 106}]}}),
    );
    let analytics = h
        .call(
            Call::get(format!(
                "/v1/wabas/{WABA}/analytics/calls?start={}&end={}&granularity=DAILY",
                at(3),
                at(1)
            ))
            .key(&key),
        )
        .await;
    assert_eq!(analytics.status.as_u16(), 200, "{}", analytics.text);
    vec![
        CALL_ICE_PWD_OFFER.to_owned(),
        CALL_ICE_PWD_ANSWER.to_owned(),
        CALL_ADDRESS.to_owned(),
        CALL_SIP_PASSWORD.to_owned(),
        CALL_SIP_HOST.to_owned(),
        CALL_PHONE.to_owned(),
        "16315559090".to_owned(),
        CALL_BSUID.to_owned(),
        CALL_ID.to_owned(),
        CALL_ID_USER.to_owned(),
        CALL_META_TEXT.to_owned(),
        // The user's own offer in Meta's connect event (its ICE password).
        "UApvJw3NcwFRDvIMKdM0vWCdlXah25E9".to_owned(),
    ]
}

/// The signature a Meta session id carries (`?sig=`) and the handle it
/// answers: in no log line (roadmap M5c1).
const UPLOAD_SIGNATURE: &str = "SIG-LOG-SECRET-9QZ";
const UPLOAD_HANDLE: &str = "2:HANDLE-LOG-SECRET-9QZ";

/// The resumable upload routes and a converted upload (roadmap M5c1), each
/// exercised: what they must keep out of the logs is Meta's session id
/// (its signature), the handle, and the service's own upload ids.
async fn resumable_uploads(h: &Harness, key: &str, pn: &str) -> Vec<String> {
    let base = format!("/v1/numbers/{pn}/media-uploads");
    let start = |len: u64| {
        Call::new(Method::POST, base.clone())
            .key(key)
            .header("idempotency-key", "log-upload-1")
            .json(&json!({"file_name": "offer.png", "file_length": len, "file_type": "image/png"}))
    };
    h.graph.push_json(
        200,
        json!({"id": format!("upload:LOGSESSION?sig={UPLOAD_SIGNATURE}")}),
    );
    let started = h.call(start(12)).await;
    assert_eq!(started.status.as_u16(), 201, "{}", started.text);
    let first = started.json()["upload_id"].as_str().unwrap().to_owned();
    // The kept answer of the same key.
    let replayed = h.call(start(12)).await;
    assert_eq!(replayed.status.as_u16(), 201, "{}", replayed.text);
    h.graph.push_json(200, json!({"h": UPLOAD_HANDLE}));
    let chunk = h
        .call(
            Call::new(Method::POST, format!("{base}/{first}?file_offset=0"))
                .key(key)
                .header("content-type", "application/octet-stream")
                .body(meta_whatsapp_rs::webhooks::axum::body::Body::from(
                    b"png bytes...".to_vec(),
                )),
        )
        .await;
    assert_eq!(chunk.status.as_u16(), 200, "{}", chunk.text);
    let done = h.call(Call::get(format!("{base}/{first}")).key(key)).await;
    assert_eq!(done.status.as_u16(), 200, "{}", done.text);
    // One that is asked of Meta.
    h.graph.push_json(200, json!({"id": "upload:LOGSESSION-2"}));
    let second = h
        .call(
            Call::new(Method::POST, base.clone()).key(key).json(
                &json!({"file_name": "offer.png", "file_length": 12, "file_type": "image/png"}),
            ),
        )
        .await;
    let second = second.json()["upload_id"].as_str().unwrap().to_owned();
    h.graph.push_json(
        200,
        json!({"id": "upload:LOGSESSION-2", "file_offset": "0"}),
    );
    let status = h.call(Call::get(format!("{base}/{second}")).key(key)).await;
    assert_eq!(status.status.as_u16(), 200, "{}", status.text);
    // A converted upload.
    h.graph.push_json(200, json!({"id": "1037543291543636"}));
    let converted = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{pn}/media"))
                .key(key)
                .multipart(&[
                    ("convert", None, b"ogg_opus"),
                    ("file", Some("voice.wav"), b"RIFF-recording"),
                ]),
        )
        .await;
    assert_eq!(converted.status.as_u16(), 201, "{}", converted.text);
    vec![
        UPLOAD_SIGNATURE.to_owned(),
        UPLOAD_HANDLE.to_owned(),
        first,
        second,
    ]
}

const PICTURE_HANDLE: &str = "4::PICTURE-HANDLE-LOG-SECRET-7XK";
const OTHER_PICTURE_HANDLE: &str = "4::OTHER-PICTURE-HANDLE-LOG-SECRET-7XK";
const PICTURE_SIGNATURE: &str = "SIG-PICTURE-LOG-SECRET-7XK";
const PICTURE_URL_SIGNATURE: &str = "oh=PICTURE-URL-LOG-SECRET-7XK";
/// A display name: audited by its length alone (design D164).
pub const DISPLAY_NAME: &str = "Log Test Shrub Display Name";

/// The profile picture and the display name (roadmap M5c2), each
/// exercised: a picture set from the number's own completed upload (its
/// handle, Meta's session, the picture's URL and the upload ids are
/// secrets), another number's upload refused, a display name requested
/// under a key, repeated, read, and requested with the call cut. Returns
/// what must not be logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn picture_and_display_name(h: &Harness, admin: &str) -> Vec<String> {
    const PN: &str = "1972385232742141";
    const OTHER_PN: &str = "1972385232742142";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["numbers", "media"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    // A completed PNG upload on `pn`, with Meta's session and handle.
    let upload = |pn: &'static str, session: &'static str, handle: &'static str| {
        let key = key.clone();
        async move {
            h.graph.push_json(
                200,
                json!({"id": format!("upload:{session}?sig={PICTURE_SIGNATURE}")}),
            );
            let started = h
                .call(
                    Call::new(Method::POST, format!("/v1/numbers/{pn}/media-uploads"))
                        .key(&key)
                        .json(&json!({"file_name": "logo.png", "file_length": 12, "file_type": "image/png"})),
                )
                .await;
            assert_eq!(started.status.as_u16(), 201, "{}", started.text);
            let id = started.json()["upload_id"].as_str().unwrap().to_owned();
            h.graph.push_json(200, json!({"h": handle}));
            let chunk = h
                .call(
                    Call::new(
                        Method::POST,
                        format!("/v1/numbers/{pn}/media-uploads/{id}?file_offset=0"),
                    )
                    .key(&key)
                    .header("content-type", "application/octet-stream")
                    .body(meta_whatsapp_rs::webhooks::axum::body::Body::from(
                        b"png bytes...".to_vec(),
                    )),
                )
                .await;
            assert_eq!(chunk.status.as_u16(), 200, "{}", chunk.text);
            id
        }
    };
    let mine = upload(PN, "PICSESSION", PICTURE_HANDLE).await;
    let other = upload(OTHER_PN, "OTHERPICSESSION", OTHER_PICTURE_HANDLE).await;
    let patch = |id: &str| {
        Call::new(Method::PATCH, format!("/v1/numbers/{PN}/profile"))
            .key(&key)
            .json(&json!({"profile_picture_upload_id": id}))
    };
    h.graph.push_json(200, json!({"success": true}));
    h.graph.push_json(
        200,
        json!({"data": [{"about": "Hi",
                "profile_picture_url": format!("https://pps.whatsapp.net/v/t61.24/pic.jpg?{PICTURE_URL_SIGNATURE}")}]}),
    );
    let set = h.call(patch(&mine)).await;
    assert_eq!(set.status.as_u16(), 200, "{}", set.text);
    // Another number's upload: not found, nothing asked of Meta.
    let refused = h.call(patch(&other)).await;
    assert_eq!(refused.status.as_u16(), 404, "{}", refused.text);
    // The display name: asked under a key and repeated, read, then asked
    // with the call cut.
    let request = |key_header: &str| {
        Call::new(Method::POST, format!("/v1/numbers/{PN}/display-name"))
            .key(&key)
            .header("idempotency-key", key_header)
            .json(&json!({"new_display_name": DISPLAY_NAME}))
    };
    h.graph.push_json(200, json!({"success": true}));
    let asked = h.call(request("display-name-log-1")).await;
    assert_eq!(asked.status.as_u16(), 202, "{}", asked.text);
    let replayed = h.call(request("display-name-log-1")).await;
    assert_eq!(replayed.status.as_u16(), 202, "{}", replayed.text);
    h.graph.push_json(
        200,
        json!({"verified_name": "Lucky Shrub", "name_status": "APPROVED",
               "new_display_name": DISPLAY_NAME, "new_name_status": "PENDING_REVIEW", "id": PN}),
    );
    let status = h
        .call(Call::get(format!("/v1/numbers/{PN}/display-name")).key(&key))
        .await;
    assert_eq!(status.status.as_u16(), 200, "{}", status.text);
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let cut = h.call(request("display-name-log-2")).await;
    assert_eq!(cut.status.as_u16(), 504, "{}", cut.text);
    vec![
        PICTURE_HANDLE.to_owned(),
        OTHER_PICTURE_HANDLE.to_owned(),
        PICTURE_SIGNATURE.to_owned(),
        PICTURE_URL_SIGNATURE.to_owned(),
        DISPLAY_NAME.to_owned(),
        mine,
        other,
    ]
}

/// The business's public key of the payload encryption scenario: base64 of
/// a `SubjectPublicKeyInfo` header and a distinctive tail.
const PAYLOAD_KEY: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AUEFZTE9BRC1LRVktTE9HLVNFQ1JFVC03WEs=";
const CLOUD_KEY: &str = "CLOUD-ENCRYPTION-KEY-LOG-SECRET-7XK";
const KEY_FINGERPRINT: &str = "FINGERPRINT-LOG-SECRET-7XK";
const SETTINGS_USERNAME: &str = "log_secret_shrub_7xk";
const OBA_SITE: &str = "https://oba-log-secret-7xk.example";
const OBA_LINK: &str = "https://oba-link-log-secret-7xk.example/";
const OBA_INFO: &str = "OBA-ADDITIONAL-INFO-LOG-SECRET-7XK";
const ENTITY_NAME: &str = "Compliance Entity Log Secret 7XK";
const OFFICER_NAME: &str = "Grievance Officer Log Secret 7XK";
const OFFICER_EMAIL: &str = "grievance-log-secret-7xk@luckyshrub.example";
const CARE_EMAIL: &str = "care-log-secret-7xk@luckyshrub.example";
const CARE_MOBILE: &str = "+91 98712 34567";
const CONTACT_BSUID: &str = "US.13491208655302749999";
const PROMPT: &str = "PROMPT-LOG-SECRET-7XK";
const COMMAND: &str = "cmdlogsecret7xk";
const COMMAND_HINT: &str = "COMMAND-HINT-LOG-SECRET-7XK";

/// The audit actions of a number's settings (roadmap M5c3), in the order
/// the scenario `number_settings` takes them.
const SETTINGS_ACTIONS: [&str; 10] = [
    "number_setting_changed",
    "payload_encryption_changed",
    "payload_encryption_changed",
    "username_set",
    "username_deleted",
    "official_business_account_requested",
    "compliance_info_set",
    "conversational_components_set",
    "contact_book_entry_deleted",
    "payload_encryption_changed",
];

/// A number's settings (roadmap M5c3), each exercised with the legal acts
/// on: a setting changed, payload encryption turned on (its key, Meta's
/// cloud key and the fingerprint planted), off, and on with the call cut,
/// the username, the application, the declaration, the components and the
/// contact book entry erased. Returns what must not be logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn number_settings(h: &Harness, admin: &str) -> Vec<String> {
    const PN: &str = "1972385232742141";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["numbers", "erasure"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let url = |path: &str| format!("/v1/numbers/{PN}/{path}");
    let ok = |reply: &super::Reply, status: u16| {
        assert_eq!(reply.status.as_u16(), status, "{}", reply.text);
    };

    // The settings read: Meta's cloud key and the fingerprint are in the
    // answer's source and in no log (the first, in no answer either).
    h.graph.push_json(
        200,
        json!({
            "storage_configuration": {"status": "IN_COUNTRY_STORAGE_DISABLED"},
            "payload_encryption": {
                "status": "enabled",
                "client_encryption_key_fingerprint": KEY_FINGERPRINT,
                "cloud_encryption_key": CLOUD_KEY
            }
        }),
    );
    ok(&h.call(Call::get(url("settings")).key(&key)).await, 200);
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(
            Call::new(Method::PATCH, url("settings"))
                .key(&key)
                .json(&json!({"search_visibility": "HIDDEN"})),
        )
        .await,
        200,
    );
    // Payload encryption on, off, and on with the call cut.
    let on = || {
        Call::new(Method::PUT, url("settings/payload-encryption"))
            .key(&key)
            .json(&json!({"client_encryption_key": PAYLOAD_KEY}))
    };
    h.graph.push_json(200, json!({"success": true}));
    ok(&h.call(on()).await, 204);
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(Call::new(Method::DELETE, url("settings/payload-encryption")).key(&key))
            .await,
        204,
    );
    // A key refused before any request, and Meta's refusal quoting it.
    let refused = h
        .call(
            Call::new(Method::PUT, url("settings/payload-encryption"))
                .key(&key)
                .json(&json!({"client_encryption_key": format!("{PAYLOAD_KEY}!!")})),
        )
        .await;
    ok(&refused, 422);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("bad key {PAYLOAD_KEY}"), "type": "OAuthException",
                "code": 100, "error_data": {"details": format!("bad key {PAYLOAD_KEY}")},
                "fbtrace_id": "A1b2C3"}}),
    );
    ok(&h.call(on()).await, 422);

    // The username.
    h.graph.push_json(200, json!({"status": "approved"}));
    ok(
        &h.call(
            Call::new(Method::PUT, url("username"))
                .key(&key)
                .json(&json!({"username": SETTINGS_USERNAME})),
        )
        .await,
        200,
    );
    h.graph.push_json(
        200,
        json!({"username": SETTINGS_USERNAME, "status": "approved"}),
    );
    ok(&h.call(Call::get(url("username")).key(&key)).await, 200);
    h.graph.push_json(
        200,
        json!({"data": [{"username_suggestions": [SETTINGS_USERNAME]}]}),
    );
    ok(
        &h.call(Call::get(url("username/suggestions")).key(&key))
            .await,
        200,
    );
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(Call::new(Method::DELETE, url("username")).key(&key))
            .await,
        204,
    );

    // The application, the declaration, the components, the contact book.
    h.graph.push_json(
        200,
        json!({"success": true, "message": "Application submitted", "tracking_id": "T-1"}),
    );
    ok(
        &h.call(
            Call::new(Method::POST, url("official-business-account"))
                .key(&key)
                .header("idempotency-key", "oba-log-1")
                .json(&json!({
                    "business_website_url": OBA_SITE,
                    "primary_country_of_operation": "US",
                    "supporting_links": [OBA_LINK, "https://b.example", "https://c.example",
                                         "https://d.example", "https://e.example"],
                    "additional_supporting_information": OBA_INFO
                })),
        )
        .await,
        202,
    );
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(
            Call::new(Method::PUT, url("compliance-info"))
                .key(&key)
                .json(&json!({
                    "entity_name": ENTITY_NAME,
                    "entity_type": "PRIVATE_COMPANY",
                    "grievance_officer_details": {"name": OFFICER_NAME, "email": OFFICER_EMAIL},
                    "customer_care_details": {"email": CARE_EMAIL, "mobile_number": CARE_MOBILE}
                })),
        )
        .await,
        204,
    );
    h.graph.push_json(
        200,
        json!({"data": [{"entity_name": ENTITY_NAME,
                "grievance_officer_details": {"name": OFFICER_NAME, "email": OFFICER_EMAIL}}]}),
    );
    ok(
        &h.call(Call::get(url("compliance-info")).key(&key)).await,
        200,
    );
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(
            Call::new(Method::PUT, url("conversational-components"))
                .key(&key)
                .json(&json!({"prompts": [PROMPT],
                    "commands": [{"command_name": COMMAND, "command_description": COMMAND_HINT}]})),
        )
        .await,
        204,
    );
    // The reads: the application's status, the health, the components.
    h.graph.push_json(
        200,
        json!({"official_business_account": {"oba_status": "PENDING"}, "id": PN}),
    );
    ok(
        &h.call(Call::get(url("official-business-account")).key(&key))
            .await,
        200,
    );
    h.graph.push_json(
        200,
        json!({"health_status": {"can_send_message": "AVAILABLE", "entities": []}, "id": PN}),
    );
    ok(
        &h.call(Call::get(url("health-status")).key(&key)).await,
        200,
    );
    h.graph.push_json(
        200,
        json!({"conversational_automation": {"prompts": [PROMPT],
                "commands": [{"command_name": COMMAND, "command_description": COMMAND_HINT}]}, "id": PN}),
    );
    ok(
        &h.call(Call::get(url("conversational-components")).key(&key))
            .await,
        200,
    );
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": true, "deleted": true}),
    );
    ok(
        &h.call(
            Call::new(
                Method::DELETE,
                url(&format!("contact-book/{CONTACT_BSUID}")),
            )
            .key(&key),
        )
        .await,
        200,
    );
    // Payload encryption on again, with the call cut.
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    ok(&h.call(on()).await, 504);
    vec![
        PAYLOAD_KEY.to_owned(),
        CLOUD_KEY.to_owned(),
        KEY_FINGERPRINT.to_owned(),
        SETTINGS_USERNAME.to_owned(),
        OBA_SITE.to_owned(),
        OBA_LINK.to_owned(),
        OBA_INFO.to_owned(),
        ENTITY_NAME.to_owned(),
        OFFICER_NAME.to_owned(),
        OFFICER_EMAIL.to_owned(),
        CARE_EMAIL.to_owned(),
        CARE_MOBILE.to_owned(),
        CONTACT_BSUID.to_owned(),
        PROMPT.to_owned(),
        COMMAND.to_owned(),
        COMMAND_HINT.to_owned(),
    ]
}

/// The settings' audit events (roadmap M5c3; design D192) of the scenario
/// `number_settings`: in order, each with its outcome, the tenant, the
/// WABA, the number by id and the key, and `setting` where the action
/// covers several; never a value.
fn check_settings_audit(changes: &[&serde_json::Value]) {
    let events: Vec<&serde_json::Value> = changes
        .iter()
        .copied()
        .filter(|fields| {
            fields["action"]
                .as_str()
                .is_some_and(|action| SETTINGS_ACTIONS.contains(&action))
        })
        .collect();
    let seen: Vec<(&str, &str, Option<&str>)> = events
        .iter()
        .map(|fields| {
            (
                fields["action"].as_str().unwrap_or_default(),
                fields["outcome"].as_str().unwrap_or_default(),
                fields["setting"].as_str(),
            )
        })
        .collect();
    let mut expected: Vec<(&str, &str, Option<&str>)> = SETTINGS_ACTIONS
        .iter()
        .map(|action| (*action, "answered", None))
        .collect();
    expected[0].2 = Some("search_visibility");
    expected[1].2 = Some("enable");
    expected[2].2 = Some("disable");
    expected[9] = ("payload_encryption_changed", "unknown", Some("enable"));
    assert_eq!(seen, expected, "the settings' audit events");
    for fields in events {
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
        assert_eq!(fields["phone_number_id"], "1972385232742141", "{fields}");
        for name in fields.as_object().unwrap().keys() {
            assert!(
                matches!(
                    name.as_str(),
                    "message"
                        | "action"
                        | "key_id"
                        | "tenant"
                        | "waba_id"
                        | "phone_number_id"
                        | "setting"
                        | "outcome"
                ),
                "an unexpected field `{name}`: {fields}"
            );
        }
    }
}

/// A marketing send's recipient and parameter, a welcome message, a
/// sequence's name and the agreement's signer and token (roadmap M5h):
/// personal data and secrets, in no log line.
const MARKETING_PHONE: &str = "+16315559090";
const MARKETING_PARAMETER: &str = "MARKETING-PARAMETER-FOR-THE-LOGS";
const WELCOME_TEXT: &str = "WELCOME-TEXT-FOR-THE-LOGS";
const WELCOME_AUTOFILL: &str = "WELCOME-AUTOFILL-FOR-THE-LOGS";
const WELCOME_NAME: &str = "WELCOME-NAME-FOR-THE-LOGS";
const SIGNER_NAME: &str = "Signer Name For The Logs";
const SIGNER_EMAIL: &str = "signer.for.the.logs@signer-domain.example";
const BUSINESS_TOKEN: &str = "EAAB-business-token-for-the-logs";

/// The audit actions of the tenant's marketing changes (roadmap M5h), in
/// the order the scenario `marketing` takes them.
const MARKETING_ACTIONS: [&str; 7] = [
    "cloud_api_marketing_set",
    "marketing_template_duplicated",
    "template_max_price_set",
    "template_max_price_cleared",
    "welcome_sequence_created",
    "welcome_sequence_updated",
    "welcome_sequence_deleted",
];

/// The Marketing Messages API, max price and welcome message sequences
/// (roadmap M5h), each exercised with the legal acts on, and the
/// operator's signing of the max price agreement (answered, refused
/// quoting the signer, cut by a timeout). Returns what must not be logged.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn marketing(h: &Harness, admin: &str) -> Vec<String> {
    const WABA: &str = "102290129340398";
    const PN: &str = "1972385232742141";
    const TEMPLATE: &str = "1407680676729941";
    const SEQUENCE: &str = "186473890";
    const BUSINESS: &str = "529216684107530";
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-42/keys")
                .key(admin)
                .json(&json!({"scopes": ["marketing"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let call = |method: Method, path: String| Call::new(method, path).key(&key);
    let ok = |reply: &super::Reply, status: u16| {
        assert_eq!(reply.status.as_u16(), status, "{}", reply.text);
    };
    let owned_template = || {
        h.graph
            .push_json(200, json!({"name": "seasonal_sale_promo", "id": TEMPLATE}));
        h.graph.push_json(
            200,
            json!({"data": [{"id": TEMPLATE, "name": "seasonal_sale_promo"}]}),
        );
    };
    let owned_sequence = || {
        h.graph.push_json(
            200,
            json!([{"sequence_id": SEQUENCE, "name": WELCOME_NAME,
                    "welcome_message_sequence": {"text": WELCOME_TEXT,
                        "autofill_message": {"content": WELCOME_AUTOFILL}},
                    "is_used_in_ad": false}]),
        );
    };
    // A send, answered; then refused, Meta's text quoting the recipient and
    // the parameter.
    let promo = json!({
        "to": {"phone": MARKETING_PHONE},
        "template": {"name": "seasonal_sale_promo", "language": {"code": "en_US"},
            "components": [{"type": "body", "parameters": [{"type": "text", "text": MARKETING_PARAMETER}]}]},
        "product_policy": "STRICT",
        "message_activity_sharing": true,
        "bid_spec": {"per_message_bid_multiplier": 1.5}
    });
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp",
               "contacts": [{"input": MARKETING_PHONE, "wa_id": "16315559090"}],
               "messages": [{"id": "wamid.HBgLMTYzMTU1NTkwOTAVAgARGBI5QTNDQTVCM0Q0Q0Q2RTY3RTcA"}]}),
    );
    let sent = h
        .call(call(Method::POST, format!("/v1/numbers/{PN}/marketing-messages")).json(&promo))
        .await;
    ok(&sent, 202);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#131055) Message failed to send: {MARKETING_PARAMETER}"),
                         "type": "OAuthException", "code": 131_055,
                         "error_data": {"messaging_product": "whatsapp",
                                        "details": format!("{MARKETING_PHONE} {MARKETING_PARAMETER}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h
        .call(call(Method::POST, format!("/v1/numbers/{PN}/marketing-messages")).json(&promo))
        .await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    // Onboarding and the Cloud API switch.
    h.graph.push_json(
        200,
        json!({"marketing_messages_onboarding_status": "ELIGIBLE", "id": WABA}),
    );
    h.graph.push_json(
        200,
        json!({"owner_business_info": {"name": "Lucky Shrub", "id": BUSINESS,
               "marketing_messages_onboarding_status": {"status": "REQUEST_SENT", "time": "2025-08-13"}}}),
    );
    ok(
        &h.call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/marketing/onboarding"),
        ))
        .await,
        200,
    );
    h.graph.push_json(200, json!({"id": WABA}));
    ok(
        &h.call(
            call(
                Method::PUT,
                format!("/v1/wabas/{WABA}/marketing/cloud-api-marketing"),
            )
            .json(&json!({"disabled": true})),
        )
        .await,
        204,
    );
    h.graph.push_json(
        200,
        json!({"disable_marketing_messages_on_cloud_api": true, "id": WABA}),
    );
    ok(
        &h.call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/marketing/cloud-api-marketing"),
        ))
        .await,
        200,
    );
    // Max price: a duplicate, a price set and cleared, a reach estimate.
    owned_template();
    h.graph.push_json(
        200,
        json!({"id": "1733678867511493", "status": "APPROVED", "category": "MARKETING",
               "source_template_id": TEMPLATE}),
    );
    ok(
        &h.call(
            call(
                Method::POST,
                format!("/v1/wabas/{WABA}/marketing/template-duplicates"),
            )
            .json(
                &json!({"source_template_id": TEMPLATE, "name": "seasonal_sale_promo_cheaper",
                          "optimization_spec": {"bid_amount": 5000}}),
            ),
        )
        .await,
        201,
    );
    owned_template();
    h.graph.push_json(
        200,
        json!({"optimization_spec": {"bid_strategy": "LOWEST_COST_WITH_BID_CAP", "bid_amount": 87000,
               "bid_country_multiplier_overrides": [{"key": "IN", "value": 1.2}]}, "id": TEMPLATE}),
    );
    ok(
        &h.call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/marketing/templates/{TEMPLATE}/max-price"),
        ))
        .await,
        200,
    );
    owned_template();
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(
            call(
                Method::PUT,
                format!("/v1/wabas/{WABA}/marketing/templates/{TEMPLATE}/max-price"),
            )
            .json(&json!({"bid_amount": 5000, "bid_country_multiplier_overrides": {"IN": 1.2}})),
        )
        .await,
        204,
    );
    owned_template();
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(call(
            Method::DELETE,
            format!("/v1/wabas/{WABA}/marketing/templates/{TEMPLATE}/max-price"),
        ))
        .await,
        204,
    );
    h.graph.push_json(
        200,
        json!({"waba_currency": "USD", "estimates": [{"bid_amount": 400, "users": 1000,
               "deliveries_lower_bound": 500, "deliveries_upper_bound": 570,
               "cost_lower_bound": 389.74, "cost_upper_bound": 390.74}]}),
    );
    ok(
        &h.call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/marketing/reach-estimates?countries=IN&date_interval=L7D"),
        ))
        .await,
        200,
    );
    // Welcome message sequences: created, refused (Meta's text quoting the
    // message), read, listed, changed and deleted.
    let welcome = json!({"name": WELCOME_NAME,
        "welcome_message_sequence": {"text": WELCOME_TEXT,
            "autofill_message": {"content": WELCOME_AUTOFILL},
            "ice_breakers": [{"title": "Quick reply 1"}]}});
    h.graph.push_json(200, json!({"sequence_id": SEQUENCE}));
    ok(
        &h.call(call(Method::POST, format!("/v1/wabas/{WABA}/welcome-sequences")).json(&welcome))
            .await,
        201,
    );
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#4027001) Invalid input {WELCOME_TEXT}"),
                         "type": "OAuthException", "code": 4_027_001,
                         "error_data": {"details": format!("{WELCOME_AUTOFILL} {WELCOME_NAME}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h
        .call(call(Method::POST, format!("/v1/wabas/{WABA}/welcome-sequences")).json(&welcome))
        .await;
    assert!(refused.status.is_server_error(), "{}", refused.text);
    owned_sequence();
    ok(
        &h.call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/welcome-sequences/{SEQUENCE}"),
        ))
        .await,
        200,
    );
    owned_sequence();
    ok(
        &h.call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/welcome-sequences"),
        ))
        .await,
        200,
    );
    owned_sequence();
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(
            call(
                Method::PATCH,
                format!("/v1/wabas/{WABA}/welcome-sequences/{SEQUENCE}"),
            )
            .json(&json!({"name": WELCOME_NAME})),
        )
        .await,
        204,
    );
    owned_sequence();
    h.graph.push_json(200, json!({"success": true}));
    ok(
        &h.call(call(
            Method::DELETE,
            format!("/v1/wabas/{WABA}/welcome-sequences/{SEQUENCE}"),
        ))
        .await,
        204,
    );
    // The operator signs the max price agreement: answered, refused (Meta's
    // text quoting the signer), then cut by a timeout.
    let sign = |token: &str| {
        Call::new(
            Method::POST,
            format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement"),
        )
        .key(admin)
        .json(&json!({"signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL, "token": token}))
    };
    h.graph.push_json(
        200,
        json!({"status": "pending", "signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL,
               "message": format!("Agreement email sent to {SIGNER_EMAIL}")}),
    );
    ok(&h.call(sign(BUSINESS_TOKEN)).await, 200);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) Invalid email {SIGNER_EMAIL}"),
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": format!("{SIGNER_NAME} {SIGNER_EMAIL}")},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(sign(BUSINESS_TOKEN)).await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    ok(&h.call(sign(BUSINESS_TOKEN)).await, 504);
    // Bodies that are not the documented one, each carrying the token and
    // the signer: refused as `422 body` or on the input, never logged.
    for body in [
        json!({"signer_name": SIGNER_NAME, "token": BUSINESS_TOKEN}),
        json!({"signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL,
               "token": BUSINESS_TOKEN, "extra": SIGNER_EMAIL}),
        json!({"signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL.replace('@', "-"),
               "token": BUSINESS_TOKEN}),
        json!({"signer_name": SIGNER_NAME, "signer_email": SIGNER_EMAIL, "token": 5}),
    ] {
        let reply = h
            .call(
                Call::new(
                    Method::POST,
                    format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement"),
                )
                .key(admin)
                .json(&body),
            )
            .await;
        ok(&reply, 422);
    }
    vec![
        key.clone(),
        key.rsplit('_').next().unwrap().to_owned(),
        MARKETING_PHONE.to_owned(),
        MARKETING_PARAMETER.to_owned(),
        WELCOME_TEXT.to_owned(),
        WELCOME_AUTOFILL.to_owned(),
        WELCOME_NAME.to_owned(),
        SIGNER_NAME.to_owned(),
        SIGNER_EMAIL.to_owned(),
        BUSINESS_TOKEN.to_owned(),
    ]
}

/// The audit events of `marketing` (roadmap M5h): the tenant's changes by
/// ids and flags, in order, every one answered, none naming a text; the
/// operator's agreement under the admin key's id and the business, answered
/// then, cut by a timeout, `unknown`, with no signer.
fn check_marketing_audit(events: &[serde_json::Value], changes: &[&serde_json::Value]) {
    let ours: Vec<&serde_json::Value> = changes
        .iter()
        .copied()
        .filter(|fields| {
            fields["action"]
                .as_str()
                .is_some_and(|action| MARKETING_ACTIONS.contains(&action))
        })
        .collect();
    let seen: Vec<&str> = ours
        .iter()
        .map(|fields| fields["action"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        seen, MARKETING_ACTIONS,
        "the marketing changes' audit events"
    );
    for fields in &ours {
        assert_eq!(fields["outcome"], "answered", "{fields}");
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
        for name in fields.as_object().unwrap().keys() {
            assert!(
                matches!(
                    name.as_str(),
                    "message"
                        | "action"
                        | "key_id"
                        | "tenant"
                        | "waba_id"
                        | "template_id"
                        | "new_template_id"
                        | "sequence_id"
                        | "bid_amount"
                        | "disabled"
                        | "outcome"
                ),
                "an unexpected field `{name}`: {fields}"
            );
        }
    }
    assert_eq!(ours[0]["disabled"], true, "{}", ours[0]);
    assert_eq!(
        ours[1]["new_template_id"], "1733678867511493",
        "{}",
        ours[1]
    );
    assert_eq!(ours[1]["bid_amount"], 5000, "{}", ours[1]);
    let agreements: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["target"] == "audit" && e["fields"]["action"] == "max_price_agreement_signed")
        .map(|e| &e["fields"])
        .collect();
    let outcomes: Vec<&str> = agreements
        .iter()
        .map(|fields| fields["outcome"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(outcomes, ["answered", "unknown"], "the agreement's events");
    for fields in agreements {
        assert_eq!(fields["message"], "admin change", "{fields}");
        assert!(fields["admin_key_id"].is_string(), "{fields}");
        assert_eq!(fields["business_id"], "529216684107530", "{fields}");
        for name in fields.as_object().unwrap().keys() {
            assert!(
                matches!(
                    name.as_str(),
                    "message" | "action" | "admin_key_id" | "business_id" | "outcome"
                ),
                "an unexpected field `{name}`: {fields}"
            );
        }
    }
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

/// Embedded Signup's routes (roadmap M3a), each exercised as a Tech
/// Provider (the capture harnesses' deployment, `logs.rs` and
/// `live_logs.rs`): an attempt started, completed up to a registration
/// Meta refuses (`133005`: the attempt kept), listed, resumed with a
/// corrected PIN; the operator's re-funding, which a Tech Provider has
/// none of (`409`). Returns what must not be logged: the merchant's
/// token, both PINs, the code and the state.
async fn signup(h: &Harness, admin: &str) -> Vec<String> {
    use super::signup::{PHONE, TOKEN, WABA, script_until_verified, success};
    const CODE: &str = "AQB-signup-code-SENTINEL-for-the-log-test";
    const PIN: &str = "835164";
    const NEW_PIN: &str = "472905";
    h.tenant("merchant-77").await;
    let minted = h
        .call(
            Call::new(Method::POST, "/v1/admin/tenants/merchant-77/keys")
                .key(admin)
                .json(&json!({"scopes": ["signup"]})),
        )
        .await
        .json();
    let key = minted["key"].as_str().unwrap().to_owned();
    let started = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(&key)
                .json(&json!({})),
        )
        .await;
    assert_eq!(started.status.as_u16(), 200, "{}", started.text);
    let state = started.json()["state"].as_str().unwrap().to_owned();
    script_until_verified(&h.graph);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#133005) Two step verification PIN Mismatch",
            "type": "OAuthException", "code": 133005, "fbtrace_id": "A"}}),
    );
    let stopped = h
        .call(
            Call::new(Method::POST, "/v1/signup/complete")
                .key(&key)
                .json(&json!({
                    "state": state, "code": CODE, "pin": PIN,
                    "event": {"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH",
                        "data": {"waba_id": WABA, "phone_number_id": PHONE}}
                })),
        )
        .await;
    assert_eq!(stopped.code(), "onboarding_failed", "{}", stopped.text);
    let attempts = h.call(Call::get("/v1/signup/attempts").key(&key)).await;
    assert_eq!(attempts.status.as_u16(), 200, "{}", attempts.text);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
    let resumed = h
        .call(
            Call::new(Method::POST, "/v1/signup/resume")
                .key(&key)
                .json(&json!({"waba_id": WABA, "pin": NEW_PIN})),
        )
        .await;
    assert_eq!(resumed.status.as_u16(), 200, "{}", resumed.text);
    let reshare = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/admin/wabas/{WABA}/credit-line/reshare"),
            )
            .key(admin),
        )
        .await;
    assert_eq!(reshare.code(), "signup_not_configured", "{}", reshare.text);
    let mut secrets = vec![key, TOKEN.to_owned(), CODE.to_owned(), state];
    // A PIN is six digits, so it cannot be made distinctive: it is matched
    // as a whole token (`Secrets::leaks`), and also in each shape a log
    // line would print it in, which are substrings whatever surrounds them.
    for pin in [PIN, NEW_PIN] {
        secrets.push(pin.to_owned());
        secrets.push(format!("\"{pin}\""));
        secrets.push(format!("{pin}\\\""));
        secrets.push(format!("PIN {pin}"));
        secrets.push(format!("pin {pin}"));
        secrets.push(format!("pin={pin}"));
        secrets.push(format!("pin\":{pin}"));
    }
    secrets
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
#[allow(clippy::too_many_lines)] // one list of every audited action, then their fields
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
    let changes: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["target"] == "audit" && e["fields"]["message"] == "tenant change")
        .map(|e| &e["fields"])
        .collect();
    // A tenant's onboarding is audited too (roadmap M3a): the resumed
    // signup of `signup`, with its tenant, its WABA and its key's id.
    let onboarded: Vec<&&serde_json::Value> = changes
        .iter()
        .filter(|fields| fields["action"] == "signup_resumed")
        .collect();
    assert_eq!(onboarded.len(), 1, "the resumed signup's audit event");
    assert_eq!(onboarded[0]["tenant"], "merchant-77", "{}", onboarded[0]);
    assert_eq!(
        onboarded[0]["waba_id"],
        super::signup::WABA,
        "{}",
        onboarded[0]
    );
    assert!(onboarded[0]["key_id"].is_string(), "{}", onboarded[0]);
    // Roadmap M3f: the completions of `signup_m3f`, each audited once with
    // its tenant, the WABA and the key's id, and no token in them.
    let operator: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["target"] == "audit" && e["fields"]["action"] == "hosted_signup_completed")
        .map(|e| &e["fields"])
        .collect();
    // The hosted claim is the operator's (decision D202): an admin change
    // with the admin key's id, the tenant it named and the WABA.
    assert_eq!(operator.len(), 1, "the hosted claim's audit event");
    assert_eq!(operator[0]["message"], "admin change", "{}", operator[0]);
    assert_eq!(operator[0]["tenant"], "merchant-78", "{}", operator[0]);
    assert_eq!(
        operator[0]["waba_id"],
        super::signup::WABA_2,
        "{}",
        operator[0]
    );
    assert!(operator[0]["admin_key_id"].is_string(), "{}", operator[0]);
    let (action, tenant, waba) = ("signup_completed", "merchant-79", "524126980791431");
    let found: Vec<&&serde_json::Value> = changes
        .iter()
        .filter(|fields| fields["action"] == action)
        .collect();
    assert_eq!(found.len(), 1, "{action}'s audit event");
    assert_eq!(found[0]["tenant"], tenant, "{}", found[0]);
    assert_eq!(found[0]["waba_id"], waba, "{}", found[0]);
    assert!(found[0]["key_id"].is_string(), "{}", found[0]);
    check_erasure_audit(&events, &changes);
    check_group_audit(&changes);
    check_call_audit(&changes);
    check_display_name_audit(&changes);
    check_settings_audit(&changes);
    check_marketing_audit(&events, &changes);
    let deletions: Vec<&serde_json::Value> = changes
        .into_iter()
        .filter(|fields| {
            fields["action"] != "signup_resumed"
                && fields["action"] != "signup_completed"
                && fields["action"] != "customer_erased"
                && fields["action"] != "display_name_change_requested"
                && !fields["action"]
                    .as_str()
                    .is_some_and(|action| SETTINGS_ACTIONS.contains(&action))
                && !fields["action"]
                    .as_str()
                    .is_some_and(|action| MARKETING_ACTIONS.contains(&action))
                && !fields["action"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("group_"))
                && !fields["action"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("call_"))
        })
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
            // Roadmap M5b.
            "template_edited",
            "template_unpaused",
            "templates_archived",
            "templates_unarchived",
            "templates_migrated",
            "template_group_updated",
            "template_group_deleted",
            "two_step_pin_set",
            "number_deregistered",
            "payment_configuration_created",
            "payment_configuration_created",
            "payment_oauth_link_regenerated",
            "payment_configuration_deleted",
            "payment_refunded",
            "payment_refunded",
            // Roadmap M5d.
            "flow_updated",
            "flow_json_uploaded",
            "flow_published",
            "flow_deprecated",
            "flow_deleted",
            // Roadmap M5g: a QR code's deletion, the blocks and the unblock.
            "qr_code_deleted",
            "users_blocked",
            "users_unblocked",
            "users_blocked",
            // Roadmap M5l: the passes (one cut by a timeout), the take, the
            // release; the refused take changed nothing.
            "thread_passed",
            "thread_passed",
            "thread_taken",
            "thread_released",
            "thread_passed"
        ],
        "the template changes', the PIN change's, the deregistration's, the payment changes', the QR code deletion's and the block list changes' audit events"
    );
    for fields in &deletions {
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
    }
    // The number's own events name it by id (roadmap M3d).
    let number_changes: Vec<&&serde_json::Value> = deletions
        .iter()
        .filter(|fields| {
            matches!(
                fields["action"].as_str(),
                Some("two_step_pin_set" | "number_deregistered")
            )
        })
        .collect();
    assert_eq!(number_changes.len(), 2, "{number_changes:?}");
    for fields in number_changes {
        assert_eq!(fields["phone_number_id"], "1972385232742141", "{fields}");
    }
    check_payment_audit(&deletions);
}

/// The erasure of `inbox` (roadmap M2f): one journal entry (target
/// `erasure_journal`), its identities 64 hex digits each, and one audit
/// event (`customer_erased`) with the same erasure id, the tenant, the
/// WABA and the number by id, and counts; `check` makes sure neither, nor
/// any other line, names the customer.
fn check_erasure_audit(events: &[serde_json::Value], changes: &[&serde_json::Value]) {
    let journal: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["target"] == "erasure_journal")
        .map(|e| &e["fields"])
        .collect();
    assert_eq!(journal.len(), 1, "{journal:?}");
    let entry = journal[0];
    assert_eq!(entry["message"], "erasure journal entry", "{entry}");
    assert_eq!(entry["v"], 1, "{entry}");
    assert_eq!(entry["tenant"], "merchant-42", "{entry}");
    assert_eq!(entry["phone_number_id"], "1972385232742141", "{entry}");
    let identities: Vec<&str> = entry["identities"].as_str().unwrap().split(',').collect();
    assert_eq!(identities.len(), 2, "the two contacts named: {entry}");
    for hmac in identities {
        assert!(
            hmac.len() == 64 && hmac.bytes().all(|b| b.is_ascii_hexdigit()),
            "{entry}"
        );
    }
    let audited: Vec<&&serde_json::Value> = changes
        .iter()
        .filter(|fields| fields["action"] == "customer_erased")
        .collect();
    assert_eq!(audited.len(), 1, "{audited:?}");
    let audit = audited[0];
    assert_eq!(audit["erasure_id"], entry["erasure_id"], "{audit}");
    assert_eq!(audit["tenant"], "merchant-42", "{audit}");
    assert_eq!(audit["waba_id"], "102290129340398", "{audit}");
    assert_eq!(audit["phone_number_id"], "1972385232742141", "{audit}");
    assert!(audit["key_id"].is_string(), "{audit}");
    assert_eq!(audit["identities"], 2, "{audit}");
    assert!(audit["messages"].as_u64().unwrap() > 0, "{audit}");
}

/// The display name requests' audit events (roadmap M5c2; design D164) of
/// the scenario `profile`: one answered, one that may have gone through
/// unanswered, each with the tenant, the WABA, the number by id, the key
/// and the length of the name; never the name.
fn check_display_name_audit(changes: &[&serde_json::Value]) {
    let requests: Vec<&serde_json::Value> = changes
        .iter()
        .copied()
        .filter(|fields| fields["action"] == "display_name_change_requested")
        .collect();
    let seen: Vec<(&str, u64)> = requests
        .iter()
        .map(|fields| {
            (
                fields["outcome"].as_str().unwrap_or_default(),
                fields["name_chars"].as_u64().unwrap_or(u64::MAX),
            )
        })
        .collect();
    let chars = DISPLAY_NAME.chars().count() as u64;
    assert_eq!(
        seen,
        [("answered", chars), ("unknown", chars)],
        "the display name requests' audit events"
    );
    for fields in requests {
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
        assert_eq!(fields["phone_number_id"], "1972385232742141", "{fields}");
        let names: Vec<&str> = fields
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for name in names {
            assert!(
                matches!(
                    name,
                    "message"
                        | "action"
                        | "key_id"
                        | "tenant"
                        | "waba_id"
                        | "phone_number_id"
                        | "name_chars"
                        | "outcome"
                ),
                "an unexpected field `{name}`: {fields}"
            );
        }
    }
}

/// The group changes' audit events (roadmap M5f; docs/design/server.md,
/// D155) of the scenario `groups`: in order, each with its outcome and its
/// counts (the participants and join requests a request named, those Meta
/// answered done and refused), the number by id, the tenant, the WABA and
/// the key; and nothing else: no group, no participant, no join request.
fn check_group_audit(changes: &[&serde_json::Value]) {
    let groups: Vec<&serde_json::Value> = changes
        .iter()
        .copied()
        .filter(|fields| {
            fields["action"]
                .as_str()
                .is_some_and(|action| action.starts_with("group_"))
        })
        .collect();
    let seen: Vec<(&str, &str, u64, u64, u64)> = groups
        .iter()
        .map(|fields| {
            (
                fields["action"].as_str().unwrap_or_default(),
                fields["outcome"].as_str().unwrap_or_default(),
                fields["users"].as_u64().unwrap_or(u64::MAX),
                fields["done"].as_u64().unwrap_or(u64::MAX),
                fields["failed"].as_u64().unwrap_or(u64::MAX),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("group_creation_requested", "answered", 0, 0, 0),
            ("group_settings_updated", "answered", 0, 0, 0),
            ("group_picture_set", "answered", 0, 0, 0),
            ("group_invite_link_reset", "answered", 0, 0, 0),
            ("group_invite_link_reset", "unknown", 0, 0, 0),
            ("group_participants_removed", "answered", 3, 0, 0),
            ("group_participants_removed", "unknown", 1, 0, 0),
            ("group_join_requests_approved", "answered", 2, 1, 1),
            ("group_join_requests_rejected", "answered", 1, 1, 0),
            ("group_deleted", "answered", 0, 0, 0),
        ],
        "the group changes' audit events, with their counts"
    );
    for fields in groups {
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
        assert_eq!(fields["phone_number_id"], "1972385232742141", "{fields}");
        // Counts and outcomes only: never a group, a person or a link.
        let names: Vec<&str> = fields
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for name in names {
            assert!(
                matches!(
                    name,
                    "message"
                        | "action"
                        | "key_id"
                        | "tenant"
                        | "waba_id"
                        | "phone_number_id"
                        | "users"
                        | "done"
                        | "failed"
                        | "outcome"
                ),
                "an unexpected field `{name}`: {fields}"
            );
        }
    }
}

/// The calling changes' audit events (roadmap M5e; docs/design/server.md,
/// D216) of the scenario `calling`: in order, each with its outcome, whether
/// recording or transcription was enabled (the consent the other
/// participant is asked for) and, for the settings, the sections changed;
/// the number by id, the tenant, the WABA and the key; and nothing else:
/// no SDP, no SIP host or password, no callee, no call.
fn check_call_audit(changes: &[&serde_json::Value]) {
    let calls: Vec<&serde_json::Value> = changes
        .iter()
        .copied()
        .filter(|fields| {
            fields["action"]
                .as_str()
                .is_some_and(|action| action.starts_with("call_"))
        })
        .collect();
    let seen: Vec<(&str, &str, bool, bool, Option<&str>)> = calls
        .iter()
        .map(|fields| {
            (
                fields["action"].as_str().unwrap_or_default(),
                fields["outcome"].as_str().unwrap_or_default(),
                fields["recording"].as_bool().unwrap_or(true),
                fields["transcription"].as_bool().unwrap_or(true),
                fields["sections"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                "call_settings_updated",
                "answered",
                false,
                false,
                Some("call_hours,sip")
            ),
            (
                "call_settings_updated",
                "answered",
                false,
                false,
                Some("voicemail")
            ),
            ("call_connect_requested", "answered", true, false, None),
            ("call_pre_accepted", "answered", false, false, None),
            ("call_accepted", "answered", false, true, None),
            ("call_rejected", "answered", false, false, None),
            ("call_terminated", "answered", false, false, None),
            ("call_terminated", "unknown", false, false, None),
        ],
        "the calling changes' audit events, with what was enabled"
    );
    for fields in calls {
        assert!(fields["key_id"].is_string(), "{fields}");
        assert_eq!(fields["tenant"], "merchant-42", "{fields}");
        assert_eq!(fields["waba_id"], "102290129340398", "{fields}");
        assert_eq!(fields["phone_number_id"], "1972385232742141", "{fields}");
        let names: Vec<&str> = fields
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for name in names {
            assert!(
                matches!(
                    name,
                    "message"
                        | "action"
                        | "key_id"
                        | "tenant"
                        | "waba_id"
                        | "phone_number_id"
                        | "recording"
                        | "transcription"
                        | "sections"
                        | "outcome"
                ),
                "an unexpected field `{name}`: {fields}"
            );
        }
    }
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
pub fn check(logs: &str, secrets: &Secrets) {
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
    // Roadmap M5d: the Flows data endpoint is exercised too.
    assert!(logged.contains(&("POST".to_owned(), "/webhooks/flows/{waba_id}".to_owned())));
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
    // A group's id (roadmap M5f) is in the library's debug line for the
    // Graph request that names it, and in no line of the service's own,
    // its audit events included.
    let mut requests_naming_groups = 0;
    for line in logs.lines().filter(|l| l.contains(GROUP_ID)) {
        assert!(
            line.contains("\"target\":\"meta_whatsapp_client::request\"")
                && line.contains("\"message\":\"graph request\""),
            "a group's id was logged by the service: {line}"
        );
        requests_naming_groups += 1;
    }
    assert!(requests_naming_groups > 0, "the group routes ran");
    if let Some(leak) = secrets.leaks(logs) {
        panic!("{leak}");
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

/// Hosted signup, several WABAs in one signup and pre-verified numbers
/// (roadmap M3f): neither the deployment's system token, the pool's, the
/// business tokens, the `appsecret_proof`, the PIN nor a pre-verified
/// number's one-time code is logged, on success or on Meta's refusal.
#[allow(clippy::too_many_lines)] // one scenario per feature, read top to bottom
async fn signup_m3f(h: &Harness, admin: &str) -> Vec<String> {
    use super::signup::{
        BUSINESS, HOSTED_PROOF, HOSTED_TOKEN, PHONE_3, POOL_TOKEN, PRE_VERIFIED, PRE_VERIFIED_CODE,
        WABA_2, partner_added, script_waba, success,
    };
    const HOSTED_BUSINESS_TOKEN: &str = "EAAH-hosted-business-token-SENTINEL-for-the-log-test";
    const MULTI_TOKEN: &str = "EAAH-multi-business-token-SENTINEL-for-the-log-test";
    const MULTI_CODE: &str = "AQB-multi-code-SENTINEL-for-the-log-test";
    const HOSTED_PIN: &str = "264817";
    const MULTI_PIN: &str = "739051";
    const WABA_3: &str = "524126980791431";
    const WABA_4: &str = "524126980791432";
    const PHONE_4: &str = "106540352242931";
    const PHONE_5: &str = "106540352242932";

    let mut keys = Vec::new();
    for tenant in ["merchant-78", "merchant-79"] {
        h.tenant(tenant).await;
        let minted = h
            .call(
                Call::new(Method::POST, format!("/v1/admin/tenants/{tenant}/keys"))
                    .key(admin)
                    .json(&json!({"scopes": ["signup"]})),
            )
            .await
            .json();
        keys.push(minted["key"].as_str().unwrap().to_owned());
    }

    // Hosted: the arrival, a claim Meta refuses at the fetch, then a claim
    // that goes through.
    let arrived = h.webhook(&partner_added(WABA_2, BUSINESS)).await;
    assert_eq!(arrived.status.as_u16(), 200, "{}", arrived.text);
    let claim = |pin: &str| {
        Call::new(Method::POST, "/v1/admin/signup/hosted")
            .key(admin)
            .json(&json!({
                "tenant_id": "merchant-78", "waba_id": WABA_2,
                "owner_business_id": BUSINESS, "pin": pin
            }))
    };
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) bad proof {HOSTED_PROOF}"),
            "type": "OAuthException", "code": 100, "fbtrace_id": "A"}}),
    );
    let refused = h.call(claim(HOSTED_PIN)).await;
    assert_eq!(refused.code(), "onboarding_failed", "{}", refused.text);
    h.graph
        .push_json(200, json!({"access_token": HOSTED_BUSINESS_TOKEN}));
    h.graph
        .push_json(200, super::signup::debug_granting(&[WABA_2]));
    script_waba(&h.graph, WABA_2, &[PHONE_3]);
    h.graph.push_json(200, success()); // subscribed_apps
    h.graph.push_json(200, success()); // register
    let claimed = h.call(claim(HOSTED_PIN)).await;
    assert_eq!(claimed.status.as_u16(), 200, "{}", claimed.text);

    // Several WABAs: two granted, the second one's subscription refused.
    let started = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(&keys[1])
                .json(&json!({})),
        )
        .await;
    let state = started.json()["state"].as_str().unwrap().to_owned();
    h.graph.push_json(200, json!({"access_token": MULTI_TOKEN}));
    h.graph
        .push_json(200, super::signup::debug_granting(&[WABA_3, WABA_4]));
    script_waba(&h.graph, WABA_3, &[PHONE_4]);
    h.graph.push_json(200, success());
    h.graph.push_json(200, success());
    script_waba(&h.graph, WABA_4, &[PHONE_5]);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) {MULTI_TOKEN}"),
            "type": "OAuthException", "code": 100, "fbtrace_id": "A"}}),
    );
    let many = h
        .call(
            Call::new(Method::POST, "/v1/signup/complete")
                .key(&keys[1])
                .json(&json!({
                    "state": state, "code": MULTI_CODE, "pin": MULTI_PIN,
                    "event": {"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH",
                        "data": {"waba_id": WABA_3, "phone_number_id": PHONE_4,
                                 "waba_ids": [WABA_3, WABA_4]}}
                })),
        )
        .await;
    assert_eq!(many.status.as_u16(), 200, "{}", many.text);

    // Pre-verified numbers: every call, and Meta refusing a code (its
    // text quoting it).
    let route = "/v1/admin/pre-verified-numbers";
    h.graph.push_json(200, json!({"id": PRE_VERIFIED}));
    let added = h
        .call(
            Call::new(Method::POST, route)
                .key(admin)
                .json(&json!({"phone_number": "+1 555 078 3881"})),
        )
        .await;
    assert_eq!(added.status.as_u16(), 201, "{}", added.text);
    h.graph.push_json(200, success());
    h.call(
        Call::new(Method::POST, format!("{route}/{PRE_VERIFIED}/code"))
            .key(admin)
            .json(&json!({"method": "SMS", "language": "en_US"})),
    )
    .await;
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) wrong code {PRE_VERIFIED_CODE}"),
            "type": "OAuthException", "code": 100, "fbtrace_id": "A"}}),
    );
    h.call(
        Call::new(Method::POST, format!("{route}/{PRE_VERIFIED}/verify"))
            .key(admin)
            .json(&json!({"code": PRE_VERIFIED_CODE})),
    )
    .await;
    h.graph.push_json(200, success());
    let verified = h
        .call(
            Call::new(Method::POST, format!("{route}/{PRE_VERIFIED}/verify"))
                .key(admin)
                .json(&json!({"code": PRE_VERIFIED_CODE})),
        )
        .await;
    assert_eq!(verified.status.as_u16(), 204, "{}", verified.text);
    h.graph.push_json(
        200,
        json!({"data": [{"id": PRE_VERIFIED, "phone_number": "+15550783881"}]}),
    );
    h.call(Call::get(route).key(admin)).await;
    h.graph.push_json(
        200,
        json!({"id": PRE_VERIFIED, "phone_number": "+15550783881"}),
    );
    h.call(Call::get(format!("{route}/{PRE_VERIFIED}")).key(admin))
        .await;
    // A reservation for a tenant, its launch, the release, the deletion.
    let reserved = h
        .call(
            Call::new(Method::PUT, format!("{route}/{PRE_VERIFIED}/reservation"))
                .key(admin)
                .json(&json!({"tenant_id": "merchant-78"})),
        )
        .await;
    assert_eq!(reserved.status.as_u16(), 204, "{}", reserved.text);
    let offered = h
        .call(
            Call::new(Method::POST, "/v1/signup/sessions")
                .key(&keys[0])
                .json(&json!({"pre_verified_phone_ids": [PRE_VERIFIED]})),
        )
        .await;
    assert_eq!(offered.status.as_u16(), 200, "{}", offered.text);
    h.call(
        Call::new(
            Method::DELETE,
            format!("{route}/{PRE_VERIFIED}/reservation"),
        )
        .key(admin),
    )
    .await;
    h.graph.push_json(200, success());
    let deleted = h
        .call(Call::new(Method::DELETE, format!("{route}/{PRE_VERIFIED}")).key(admin))
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text);

    let mut secrets = vec![
        HOSTED_TOKEN.to_owned(),
        POOL_TOKEN.to_owned(),
        HOSTED_BUSINESS_TOKEN.to_owned(),
        MULTI_TOKEN.to_owned(),
        MULTI_CODE.to_owned(),
        HOSTED_PROOF.to_owned(),
        keys[0].clone(),
        keys[1].clone(),
        state,
        "+15550783881".to_owned(),
    ];
    // Six-digit secrets are matched as whole tokens and in the shapes a
    // line would print them in (see `signup`).
    for pin in [HOSTED_PIN, MULTI_PIN, PRE_VERIFIED_CODE] {
        secrets.push(pin.to_owned());
        secrets.push(format!("\"{pin}\""));
        secrets.push(format!("pin={pin}"));
        secrets.push(format!("code={pin}"));
        secrets.push(format!("pin\":{pin}"));
    }
    secrets
}
