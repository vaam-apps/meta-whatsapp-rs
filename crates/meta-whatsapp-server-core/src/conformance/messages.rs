//! Roadmap S5b: the core's message services ([`crate::messages`]) under
//! its idempotency engine ([`crate::idempotency::run`]), over a backend's
//! bundle, with no HTTP layer: the core's [`Authorizer`] over the
//! backend's records and a vault on its key/value store, the backend's
//! idempotency records, and a Graph client on a [`ScriptedTransport`].
//! Acceptance test M1.4, as the core runs it: a send carries the number's
//! token from the vault; a digits-only phone number is refused before any
//! request; a scripted timeout reports `may_have_been_sent`, and the same
//! key replays that answer with no second request; a scripted `131047` is
//! `409` and releases the key, so the same key sends once the window is
//! open; a success is replayed byte for byte. And the key is the
//! admitted caller's: a [`Caller`] another [`Authorizer`] made is refused
//! before the key is read, with a key or without one. What the adapter's
//! metrics count is told to its [`Observer`]: each repeat's outcome, and
//! each Graph error's code. The services open only on a number their own
//! [`Authorizer`] made, and marking read sends Meta's documented request,
//! with the typing indicator when asked and never for a blank id.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::client::messages::OutboundMessage;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::{Value, json};

use crate::admin::mint;
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::backend::Backend;
use crate::error::ServiceError;
use crate::idempotency::{self, Answer, Fingerprint, Keeper};
use crate::messages::{self, MessageType, Messages, RecipientObject, SendMessage, TextObject};
use crate::model::{BindOutcome, IdempotencyClaim, IdempotencyKey, KeyOwner, Scope, TenantId};
use crate::observe::Observer;
use crate::store::IdempotencyRecords;

/// The tenant sending, its WABA, its number and the token in the vault.
const TENANT: &str = "msg-sender";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
/// The tenant, WABA and number of the second case (the cases share a
/// backend: [`run`]).
const READER: (&str, &str, &str) = ("msg-reader", "102290129340399", "106540352242923");
/// The tenant, WABA and number of the legacy fingerprint's case.
const LEGACY: (&str, &str, &str) = ("msg-legacy", "102290129340397", "106540352242921");
const TOKEN: &str = "EAAG-the-merchants-token";
const TO: &str = "+16505551234";
/// messages/mark-message-as-read, the example's message id.
const RECEIVED: &str = "wamid.HBgLMTY1MDM4Nzk0MzkVAgARGBJDQjZCMzlEQUE4OTJBMTE4RTUA";

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_timeout_may_have_been_sent_and_its_key_replays_it_without_a_second_request(backend).await;
    the_services_open_on_their_own_number_and_mark_read_as_meta_documents(backend).await;
    a_key_claimed_before_s6_replays_under_its_legacy_fingerprint(backend).await;
}

/// What the operations told the adapter's metrics, in order: each Graph
/// error's code, each repeat's outcome, and what the webhook pipeline
/// counts (`conformance::webhooks`): each new row's type and audience,
/// each run of duplicates by stage, each failure's stage.
#[derive(Default)]
pub(crate) struct Told {
    graph_errors: Mutex<Vec<&'static str>>,
    repeats: Mutex<Vec<&'static str>>,
    webhook_events: Mutex<Vec<(&'static str, &'static str)>>,
    webhook_duplicates: Mutex<Vec<(&'static str, u64)>>,
    webhook_failures: Mutex<Vec<&'static str>>,
}

impl Observer for Told {
    fn graph_error(&self, code: &'static str) {
        self.graph_errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(code);
    }

    fn idempotency(&self, outcome: &'static str) {
        self.repeats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(outcome);
    }

    fn webhook_event(&self, event_type: &'static str, audience: &'static str) {
        self.webhook_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((event_type, audience));
    }

    fn webhook_duplicates(&self, stage: &'static str, count: u64) {
        self.webhook_duplicates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((stage, count));
    }

    fn webhook_failure(&self, stage: &'static str) {
        self.webhook_failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(stage);
    }
}

impl Told {
    pub(crate) fn graph_errors(&self) -> Vec<&'static str> {
        self.graph_errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn repeats(&self) -> Vec<&'static str> {
        self.repeats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every new row's type and audience, and forgets them.
    pub(crate) fn take_webhook_events(&self) -> Vec<(&'static str, &'static str)> {
        std::mem::take(
            &mut *self
                .webhook_events
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Every run of duplicates by stage, and forgets them.
    pub(crate) fn take_webhook_duplicates(&self) -> Vec<(&'static str, u64)> {
        std::mem::take(
            &mut *self
                .webhook_duplicates
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Every failure's stage, and forgets them.
    pub(crate) fn take_webhook_failures(&self) -> Vec<&'static str> {
        std::mem::take(
            &mut *self
                .webhook_failures
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

/// The error body the cases keep: the code and `may_have_been_sent`, as
/// an adapter's renderer would write them.
fn render(error: &ServiceError) -> Vec<u8> {
    serde_json::to_vec(&json!({"error": {
        "code": error.code(),
        "may_have_been_sent": error.may_have_been_sent(),
    }}))
    .unwrap()
}

/// messages/text-messages, "Example response".
fn accepted() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": TO, "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

/// A text to `phone`, as a request carries it.
fn text_to(phone: &str) -> SendMessage {
    let mut request = SendMessage::new(
        RecipientObject {
            phone: Some(phone.to_owned()),
            ..RecipientObject::default()
        },
        MessageType::Text,
    );
    request.text = Some(TextObject {
        body: "Your order shipped.".to_owned(),
        preview_url: None,
    });
    request
}

/// The service as one request sees it: the order, the caller it admitted,
/// the number it owns, where keys are kept, and what it told the metrics.
struct Service {
    authz: Authorizer,
    caller: Caller,
    number: OwnedNumber,
    records: Arc<dyn IdempotencyRecords>,
    vault: TokenVault,
    script: ScriptedTransport,
    told: Told,
}

impl Service {
    fn keeper(&self) -> Keeper<'_> {
        Keeper::new(
            &self.records,
            Duration::from_secs(60),
            Duration::from_secs(3600),
            render,
        )
        .observed_by(&self.told)
    }

    /// The message services on the number, observed.
    fn messages(&self) -> Messages<'_> {
        Messages::new(&self.authz, &self.number)
            .unwrap()
            .observed_by(&self.told)
    }

    /// The core's send, the accepted message as `202` and its id, under
    /// `key` (or none), for `caller`.
    async fn send_as(
        &self,
        caller: &Caller,
        key: Option<&str>,
        message: &OutboundMessage,
        fingerprint: &Fingerprint,
    ) -> Result<Answer, ServiceError> {
        let keeper = self.keeper();
        let messages = self.messages();
        idempotency::run(
            &keeper,
            &self.authz,
            caller,
            key.map(|key| IdempotencyKey::parse(key).unwrap()),
            fingerprint,
            async {
                let accepted = messages.send(message).await?;
                let body = json!({"message_id": accepted.message_id.as_str()});
                Ok(Answer::new(202, serde_json::to_vec(&body).unwrap()))
            },
        )
        .await
    }

    async fn send(
        &self,
        key: &str,
        message: &OutboundMessage,
        fingerprint: &Fingerprint,
    ) -> Result<Answer, ServiceError> {
        self.send_as(&self.caller, Some(key), message, fingerprint)
            .await
    }

    /// Another [`Authorizer`] over the same records and vault, with a
    /// Graph client of its own: the same tenant's key admits a [`Caller`]
    /// it made, and owns the same number, but neither is this order's.
    fn other(&self, backend: &dyn Backend) -> (Authorizer, ScriptedTransport) {
        let script = ScriptedTransport::new();
        let client = Client::builder().transport(script.clone()).build().unwrap();
        let other = Authorizer::new(backend.records(), self.vault.clone(), client).unwrap();
        (other, script)
    }
}

/// `tenant` with a `send` key, its WABA and number bound, and the
/// number's token in the vault; the caller the order admits for that key,
/// and the number it owns.
async fn service(backend: &dyn Backend, (tenant, waba, pn): (&str, &str, &str)) -> Service {
    let records = backend.records();
    let tenant = TenantId::parse(tenant).unwrap();
    records.create_tenant(&tenant, "").await.unwrap();
    let (key, _) = mint(
        records.as_ref(),
        KeyOwner::Tenant(tenant.clone()),
        vec![Scope::Send],
        String::new(),
        None,
    )
    .await
    .unwrap();
    let waba = WabaId::new(waba).unwrap();
    let pn = PhoneNumberId::new(pn).unwrap();
    let bound = records
        .bind_waba(&tenant, &waba, std::slice::from_ref(&pn))
        .await
        .unwrap();
    assert_eq!(bound, BindOutcome::Bound);
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("messages-suite").unwrap()),
    )
    .unwrap();
    vault
        .store(
            &StoredBusinessToken::new(waba, AccessToken::new(TOKEN)).phone_number_ids([pn.clone()]),
        )
        .await
        .unwrap();
    let script = ScriptedTransport::new();
    let client = Client::builder().transport(script.clone()).build().unwrap();
    let authz = Authorizer::new(records, vault.clone(), client).unwrap();
    let caller = authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Send,
        )
        .await
        .unwrap();
    let number = authz.owned_number(&caller, pn).await.unwrap();
    Service {
        authz,
        caller,
        number,
        records: backend.idempotency(),
        vault,
        script,
        told: Told::default(),
    }
}

/// A key for `tenant` that `authz` (another order) admits.
async fn caller_of(backend: &dyn Backend, authz: &Authorizer, tenant: &str) -> Caller {
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(TenantId::parse(tenant).unwrap()),
        vec![Scope::Send],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Send,
        )
        .await
        .unwrap()
}

/// The decisive case of roadmap S5b (M1.4's), with no HTTP layer.
///
/// 1. A digits-only phone number is refused on `to.phone` before any
///    request.
/// 2. A send Meta does not answer in time reports `504 timeout`,
///    `may_have_been_sent`; it went out once, with the number's token.
/// 3. The same key and request: the kept answer, `replayed`, byte for
///    byte, with no second request (the script has nothing left to
///    answer one).
/// 4. A `131047` (outside the 24-hour window) proves nothing was sent:
///    `409 customer_service_window_closed`, and the key is released, so
///    the same key sends once the window is open (the second request
///    reaches Meta); that success is then replayed, with no third. The
///    same key for another body, the same body on another number, or
///    for another operation (roadmap S6: the fingerprint is the
///    operation and its canonical input) is `422
///    idempotency_key_reused`, with nothing sent.
/// 5. A [`Caller`] another [`Authorizer`] made (one over the same
///    records and vault, for the same key) is refused, `403 forbidden`,
///    before the key is read: the tenant's kept answer is not replayed to
///    it, and nothing is sent. Without a key too: the admission comes
///    first, not with the claim.
///
/// Along the way, the [`Observer`] is told each repeat's outcome
/// (`replayed`, `reused`: the server's `wa_server_idempotency_total`) and
/// each Graph error's code (`customer_service_window_closed`; a timeout is
/// none: `wa_server_graph_errors_total`).
///
/// Decisive: running the operation without claiming the key (no lease),
/// or releasing a key whose outcome may have been sent, fails step 3 (a
/// second send, which nothing answers); keeping a key whose outcome
/// proves nothing was sent fails step 4; dropping the caller's admission
/// fails step 5, and admitting it only with a key fails its keyless
/// send; not telling the observer fails the outcomes and codes.
pub async fn a_timeout_may_have_been_sent_and_its_key_replays_it_without_a_second_request(
    backend: &dyn Backend,
) {
    let service = service(backend, (TENANT, WABA, PN)).await;
    let script = &service.script;
    let number = PhoneNumberId::new(PN).unwrap();
    let fingerprint = Fingerprint::send_message(&number, &json!({"to": {"phone": TO}}));

    // 1. Refused before any request.
    let refused = messages::message(text_to("16505551234")).unwrap_err();
    assert_eq!(
        (refused.status(), refused.code(), refused.field()),
        (422, "invalid_request", Some("to.phone"))
    );
    assert!(script.requests().is_empty(), "a request was sent");

    // 2. A timeout may have sent it.
    let message = messages::message(text_to(TO)).unwrap();
    script.push_error(|| TransportError::Timeout);
    let timeout = service
        .send("order:1234:shipped", &message, &fingerprint)
        .await
        .unwrap_err();
    assert_eq!((timeout.status(), timeout.code()), (504, "timeout"));
    assert!(timeout.may_have_been_sent(), "{timeout:?}");
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path(), format!("/v25.0/{PN}/messages"));
    assert_eq!(requests[0].bearer(), Some(TOKEN), "the vault's token");
    assert_eq!(requests[0].json().unwrap()["to"], TO);
    assert_eq!(script.remaining(), 0);
    assert!(
        service.told.graph_errors().is_empty(),
        "a timeout is no Graph error"
    );
    assert!(service.told.repeats().is_empty());

    // 3. The same key: the kept answer, and no second request.
    let replayed = service
        .send("order:1234:shipped", &message, &fingerprint)
        .await
        .unwrap_or_else(|error| panic!("not replayed: {error:?}"));
    assert!(replayed.replayed());
    assert_eq!(replayed.status(), 504);
    assert_eq!(
        replayed.body(),
        render(&timeout).as_slice(),
        "byte for byte"
    );
    assert_eq!(script.requests().len(), 1, "a second request was sent");
    assert_eq!(script.remaining(), 0);
    assert_eq!(service.told.repeats(), ["replayed"]);

    // 4. 131047 releases the key (support/error-codes: 131047).
    script.push_json(
        400,
        json!({"error": {"message": "(#131047) Re-engagement message", "type": "OAuthException",
                         "code": 131047,
                         "error_data": {"messaging_product": "whatsapp",
                                        "details": "Message failed to send because more than 24 hours have passed since the customer last replied to this number."},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let closed = service
        .send("k-window", &message, &fingerprint)
        .await
        .unwrap_err();
    assert_eq!(
        (closed.status(), closed.code()),
        (409, "customer_service_window_closed")
    );
    assert!(!closed.may_have_been_sent());
    assert_eq!(closed.graph().map(|g| g.code), Some(131047));
    assert_eq!(
        service.told.graph_errors(),
        ["customer_service_window_closed"]
    );
    script.push_json(200, accepted());
    let sent = service
        .send("k-window", &message, &fingerprint)
        .await
        .unwrap_or_else(|error| panic!("the released key did not send: {error:?}"));
    assert!(!sent.replayed());
    assert_eq!(sent.status(), 202);
    assert_eq!(
        script.requests().len(),
        3,
        "the second request reached Meta"
    );
    let again = service
        .send("k-window", &message, &fingerprint)
        .await
        .unwrap();
    assert!(again.replayed());
    assert_eq!((again.status(), again.body()), (202, sent.body()));
    assert_eq!(script.requests().len(), 3, "a third request was sent");
    assert_eq!(script.remaining(), 0);
    // Another request under that key: reused, and nothing sent.
    let elsewhere = Fingerprint::send_message(&number, &json!({"to": {"phone": "+16505550000"}}));
    let key_reused = service
        .send("k-window", &message, &elsewhere)
        .await
        .unwrap_err();
    assert_eq!(
        (key_reused.status(), key_reused.code()),
        (422, "idempotency_key_reused")
    );
    assert_eq!(script.requests().len(), 3, "a reused key sent");
    // The same body on another number, or for another operation, is
    // another request too: the fingerprint is the operation and its
    // canonical input (roadmap S6), as the backend's records keep it.
    let body = json!({"to": {"phone": TO}});
    for other in [
        Fingerprint::send_message(&PhoneNumberId::new("106540352242923").unwrap(), &body),
        Fingerprint::create_template(&WabaId::new(WABA).unwrap(), &body),
    ] {
        let key_reused = service
            .send("k-window", &message, &other)
            .await
            .unwrap_err();
        assert_eq!(
            (key_reused.status(), key_reused.code()),
            (422, "idempotency_key_reused")
        );
    }
    assert_eq!(script.requests().len(), 3, "a reused key sent");
    assert_eq!(
        service.told.repeats(),
        ["replayed", "replayed", "reused", "reused", "reused"]
    );

    // 5. Another authorizer's caller, for the same key: refused first.
    let (other, other_script) = service.other(backend);
    let forged = caller_of(backend, &other, TENANT).await;
    for key in [Some("order:1234:shipped"), None] {
        let refused = service
            .send_as(&forged, key, &message, &fingerprint)
            .await
            .unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (403, "forbidden"),
            "{key:?}"
        );
    }
    assert_eq!(script.requests().len(), 3, "a request was sent");
    assert!(other_script.requests().is_empty());
    assert_eq!(
        service.told.repeats(),
        ["replayed", "replayed", "reused", "reused", "reused"]
    );
}

/// Roadmap S6's window: a key a release before S6 claimed, stored under
/// the fingerprint of then (the request's method, its path as sent and
/// its body: `Fingerprint::legacy_v1_json`), keeps replaying for as long
/// as its record lives, through the backend's records.
///
/// 1. A key claimed and completed under the legacy fingerprint (by hand,
///    as that release stored it), repeated with the same request (its new
///    fingerprint `or_legacy` its legacy one): the kept answer,
///    `replayed`, with no request to Meta.
/// 2. The same key with another body (its own legacy fingerprint too):
///    `422 idempotency_key_reused`, nothing sent; and the same request
///    without its legacy fingerprint, `422` too.
/// 3. A fresh key claimed by such a request stores the new fingerprint.
///
/// Decisive: the legacy arm of `Fingerprint::matches` (step 1 answers
/// `422`), a legacy match that ignores the body (step 2 replays), a claim
/// storing the legacy fingerprint (step 3).
pub async fn a_key_claimed_before_s6_replays_under_its_legacy_fingerprint(backend: &dyn Backend) {
    let service = service(backend, LEGACY).await;
    let script = &service.script;
    let (tenant, _, pn) = LEGACY;
    let tenant = TenantId::parse(tenant).unwrap();
    let number = PhoneNumberId::new(pn).unwrap();
    let path = format!("/v1/numbers/{pn}/messages");
    let body = json!({"to": {"phone": TO}, "type": "text", "text": {"body": "hi"}});
    let request = |body: &Value| {
        Fingerprint::send_message(&number, body)
            .or_legacy(Fingerprint::legacy_v1_json("POST", &path, body))
    };
    let message = messages::message(text_to(TO)).unwrap();

    // 1. Claimed and completed before the upgrade, under the legacy
    //    fingerprint.
    let key = IdempotencyKey::parse("order:before-s6").unwrap();
    let legacy = Fingerprint::legacy_v1_json("POST", &path, &body);
    let kept = br#"{"message_id":"wamid.KEPT-BEFORE-S6"}"#;
    let claimed = service
        .records
        .claim_idempotency_key(
            &tenant,
            &key,
            legacy.as_bytes(),
            "a-claim-before-s6",
            Duration::from_secs(60),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    assert_eq!(claimed, IdempotencyClaim::Claimed);
    assert!(
        service
            .records
            .complete_idempotency_key(&tenant, &key, "a-claim-before-s6", 202, kept)
            .await
            .unwrap()
    );
    let replayed = service
        .send("order:before-s6", &message, &request(&body))
        .await
        .unwrap_or_else(|error| panic!("the legacy key did not replay: {error:?}"));
    assert!(replayed.replayed());
    assert_eq!((replayed.status(), replayed.body()), (202, &kept[..]));
    assert!(script.requests().is_empty(), "a legacy key sent again");

    // 2. Another body under that key, legacy or not: another request.
    let other = json!({"to": {"phone": TO}, "type": "text", "text": {"body": "ho"}});
    for fingerprint in [request(&other), Fingerprint::send_message(&number, &body)] {
        let reused = service
            .send("order:before-s6", &message, &fingerprint)
            .await
            .unwrap_err();
        assert_eq!(
            (reused.status(), reused.code()),
            (422, "idempotency_key_reused")
        );
    }
    assert!(script.requests().is_empty(), "a reused key sent");
    assert_eq!(service.told.repeats(), ["replayed", "reused", "reused"]);

    // 3. A fresh key stores the new fingerprint.
    script.push_json(200, accepted());
    let sent = service
        .send("order:after-s6", &message, &request(&body))
        .await
        .unwrap();
    assert!(!sent.replayed());
    assert_eq!(script.requests().len(), 1);
    let probe = service
        .records
        .claim_idempotency_key(
            &tenant,
            &IdempotencyKey::parse("order:after-s6").unwrap(),
            &[0; 32],
            "a-probe",
            Duration::from_secs(60),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let IdempotencyClaim::Existing(record) = probe else {
        panic!("the fresh key was not kept: {probe:?}")
    };
    assert_eq!(
        record.fingerprint,
        *Fingerprint::send_message(&number, &body).as_bytes(),
        "a claim stores the new fingerprint"
    );
    assert_eq!(script.remaining(), 0);
}

/// The services open on a number only their own [`Authorizer`] made, and
/// mark read as Meta documents it.
///
/// 1. `Messages::new` with a number another [`Authorizer`] made (over the
///    same records and vault, so it owns the same number), or with this
///    one's number through the other order, is `403 forbidden`; the other
///    order opens on its own.
/// 2. Marking read a blank id is `422` on `message_id`, before any
///    request.
/// 3. Marking read sends messages/mark-message-as-read's request with the
///    number's token; with the typing indicator, typing-indicators' one.
/// 4. Meta refusing it is its error, with Meta's code, and told to the
///    [`Observer`].
///
/// Decisive: `Messages::new`'s `admit_number` fails step 1; the blank
/// check fails step 2; ignoring the typing indicator fails step 3; not
/// telling the observer fails step 4.
pub async fn the_services_open_on_their_own_number_and_mark_read_as_meta_documents(
    backend: &dyn Backend,
) {
    let service = service(backend, READER).await;
    let script = &service.script;
    let (_, _, pn) = READER;

    // 1. Only the order's own number.
    let (other, other_script) = service.other(backend);
    let forged = caller_of(backend, &other, READER.0).await;
    let theirs = other
        .owned_number(&forged, PhoneNumberId::new(pn).unwrap())
        .await
        .unwrap();
    for (opened, which) in [
        (Messages::new(&service.authz, &theirs), "their number"),
        (
            Messages::new(&other, &service.number),
            "through the other order",
        ),
    ] {
        let refused = opened.map(|_| ()).unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (403, "forbidden"),
            "{which}"
        );
    }
    assert!(Messages::new(&other, &theirs).is_ok());

    // 2. A blank id: refused before any request.
    let messages = service.messages();
    let blank = messages.mark_read(" ", false).await.unwrap_err();
    assert_eq!(
        (blank.status(), blank.code(), blank.field()),
        (422, "invalid_request", Some("message_id"))
    );
    assert!(script.requests().is_empty(), "a request was sent");

    // 3. Meta's request, with the typing indicator when asked.
    for typing in [false, true] {
        script.push_json(200, json!({"success": true}));
        messages.mark_read(RECEIVED, typing).await.unwrap();
        let request = script.requests().pop().unwrap();
        assert_eq!(request.path(), format!("/v25.0/{pn}/messages"));
        assert_eq!(request.bearer(), Some(TOKEN), "the vault's token");
        let mut expected =
            json!({"messaging_product": "whatsapp", "status": "read", "message_id": RECEIVED});
        if typing {
            // typing-indicators, "Request syntax".
            expected["typing_indicator"] = json!({"type": "text"});
        }
        assert_eq!(request.json().unwrap(), expected, "typing: {typing}");
    }

    // 4. Meta's refusal (an unknown message): its code, told.
    script.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException",
                         "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = messages.mark_read(RECEIVED, false).await.unwrap_err();
    assert_eq!(refused.graph().map(|g| g.code), Some(100));
    assert_eq!(service.told.graph_errors(), [refused.code()]);
    assert_eq!(script.requests().len(), 3);
    assert_eq!(script.remaining(), 0);
    assert!(other_script.requests().is_empty());
}
