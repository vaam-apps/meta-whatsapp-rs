//! The log half of `webhooks.rs`: the webhook pipeline's lines keep the
//! target the service's operators have always filtered on
//! (`meta_whatsapp_server::events`), and their fields, now that roadmap
//! S5e moved them into the core; a forged delivery writes no line (the
//! adapter logs refusals, at most once a minute per reason), the
//! library's included, as the core checks the signature before the
//! library's handler runs; and nothing logged names the customer or what
//! they wrote. Alone in its binary, as `templates_logs.rs` is: the capture
//! is a thread's default subscriber, and `tracing` caches whether a call
//! site is enabled across threads.
#![cfg(feature = "conformance")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

#[path = "capabilities/support.rs"]
mod support;

mod memory;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AppSecret, VerifyToken};
use meta_whatsapp_rs::webhooks::sign;
use meta_whatsapp_server_core::backend::Backend;
use meta_whatsapp_server_core::inbound::{Inbound, LOG_TARGET, Pipeline, Refused};
use meta_whatsapp_server_core::model::TenantId;
use serde_json::{Value, json};

/// One logged event: its level, target, message and other fields.
#[derive(Debug, Clone)]
struct Line {
    level: tracing::Level,
    target: String,
    message: String,
    fields: BTreeMap<String, String>,
}

/// Every event logged while it is the thread's subscriber: the lines not
/// taken yet, and every line.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<Line>>>, Arc<Mutex<Vec<Line>>>);

impl Logged {
    fn take(&self) -> Vec<Line> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    fn all(&self) -> Vec<Line> {
        self.1.lock().unwrap().clone()
    }
}

impl tracing::Subscriber for Logged {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(BTreeMap<String, String>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(field.name().to_owned(), format!("{value:?}"));
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.insert(field.name().to_owned(), value.to_owned());
            }
        }
        let mut fields = Fields(BTreeMap::new());
        event.record(&mut fields);
        let message = fields.0.remove("message").unwrap_or_default();
        let line = Line {
            level: *event.metadata().level(),
            target: event.metadata().target().to_owned(),
            message,
            fields: fields.0,
        };
        self.1.lock().unwrap().push(line.clone());
        self.0.lock().unwrap().push(line);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

const SECRET: &str = "app-secret-of-the-log-test";
const TENANT: &str = "wh-logs";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const CONTACT: &str = "16505551234";
const TEXT: &str = "Does it come in another color?";

/// webhooks/reference/messages/text, its example, to `pn` of `waba`, sent
/// now (the next second: after the binding's).
fn text(waba: &str, pn: &str, wamid: &str) -> Vec<u8> {
    let at = time::OffsetDateTime::now_utc().unix_timestamp() + 1;
    let payload: Value = json!({
        "object": "whatsapp_business_account",
        "entry": [{"id": waba, "changes": [{"field": "messages", "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": pn},
            "contacts": [{"profile": {"name": "Sheena Nelson"}, "wa_id": CONTACT}],
            "messages": [{"from": CONTACT, "id": wamid, "timestamp": at.to_string(),
                          "type": "text", "text": {"body": TEXT}}]
        }}]}]
    });
    serde_json::to_vec(&payload).unwrap()
}

async fn deliver(pipeline: &Pipeline, secret: &str, body: &[u8]) -> Result<(), Refused> {
    pipeline
        .deliver(
            Some(&sign(&AppSecret::new(secret), body)),
            Bytes::copy_from_slice(body),
        )
        .await
        .map(|_| ())
}

/// Each line the pipeline writes, under the service's old target with its
/// old fields; none for a forged delivery; nothing of the customer's.
/// Decisive: each line's `target`, and the signature checked before the
/// library's handler (which logs each forged delivery it refuses).
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn the_pipelines_lines_keep_the_services_target_and_fields() {
    let logged = Logged::default();
    let _guard = tracing::subscriber::set_default(logged.clone());
    let backend = memory::Memory::default();
    let records = backend.records();
    let tenant = TenantId::parse(TENANT).unwrap();
    records.create_tenant(&tenant, "").await.unwrap();
    records
        .bind_waba(
            &tenant,
            &WabaId::new(WABA).unwrap(),
            &[PhoneNumberId::new(PN).unwrap()],
        )
        .await
        .unwrap();
    let inbound = Inbound::new(
        vec![AppSecret::new(SECRET)],
        VerifyToken::new("verify-token-of-the-log-test"),
    )
    .unwrap();
    let pipeline = Pipeline::new(inbound, &backend, Arc::new(()));
    logged.take();

    // A forged delivery: refused, and not a line from anyone.
    let owned = text(WABA, PN, "wamid.logs.1");
    let refused = deliver(&pipeline, "a-forger-s-secret", &owned).await;
    assert!(matches!(refused, Err(Refused::Unauthenticated)));
    let lines = logged.take();
    assert!(lines.is_empty(), "a forged delivery logged {lines:?}");

    // An owned event: `event recorded`, at debug.
    deliver(&pipeline, SECRET, &owned).await.unwrap();
    let lines: Vec<Line> = logged
        .take()
        .into_iter()
        .filter(|l| l.target == LOG_TARGET)
        .collect();
    let [line] = lines.as_slice() else {
        panic!("one line of the pipeline's: {lines:?}");
    };
    assert_eq!(
        (line.level, line.message.as_str()),
        (tracing::Level::DEBUG, "event recorded")
    );
    assert_eq!(line.target, "meta_whatsapp_server::events");
    assert_eq!(
        line.fields.keys().map(String::as_str).collect::<Vec<_>>(),
        ["event_type", "sequence", "tenant"]
    );
    assert_eq!(line.fields["event_type"], "message_received");
    assert_eq!(line.fields["tenant"], TENANT);

    // An unowned number's: `operator-only event recorded`, at info, with
    // the reason, the data's size and digest.
    deliver(
        &pipeline,
        SECRET,
        &text(WABA, "106540352249999", "wamid.logs.2"),
    )
    .await
    .unwrap();
    let lines: Vec<Line> = logged
        .take()
        .into_iter()
        .filter(|l| l.target == LOG_TARGET)
        .collect();
    let [line] = lines.as_slice() else {
        panic!("one line of the pipeline's: {lines:?}");
    };
    assert_eq!(
        (line.level, line.message.as_str()),
        (tracing::Level::INFO, "operator-only event recorded")
    );
    assert_eq!(
        line.fields.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "data_bytes",
            "data_sha256",
            "event_type",
            "reason",
            "sequence"
        ]
    );
    assert_eq!(line.fields["reason"], "unowned");
    assert_eq!(line.fields["data_sha256"].len(), 64, "hex SHA-256");

    // A keyless body again: `the event was already in the outbox`, at
    // debug.
    let not_a_webhook = br#"{"not": "a webhook envelope"}"#;
    deliver(&pipeline, SECRET, not_a_webhook).await.unwrap();
    logged.take();
    deliver(&pipeline, SECRET, not_a_webhook).await.unwrap();
    let lines: Vec<Line> = logged
        .take()
        .into_iter()
        .filter(|l| l.target == LOG_TARGET)
        .collect();
    let [line] = lines.as_slice() else {
        panic!("one line of the pipeline's: {lines:?}");
    };
    assert_eq!(
        (line.level, line.message.as_str()),
        (tracing::Level::DEBUG, "the event was already in the outbox")
    );
    assert_eq!(line.fields["event_type"], "unparsed");

    // Nothing logged, by the pipeline or the library, names the customer
    // or what they wrote (the owned message once more: a duplicate).
    deliver(&pipeline, SECRET, &owned).await.unwrap();
    let all = logged.all();
    assert!(!all.is_empty());
    for line in all {
        let all = format!("{line:?}");
        assert!(!all.contains(CONTACT), "{all}");
        assert!(!all.contains(TEXT), "{all}");
    }
}
