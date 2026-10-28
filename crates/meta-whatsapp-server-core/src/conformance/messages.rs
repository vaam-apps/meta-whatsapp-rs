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
//! before the key is read.

use std::sync::Arc;
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
use crate::model::{BindOutcome, IdempotencyKey, KeyOwner, Scope, TenantId};
use crate::store::IdempotencyRecords;

/// The tenant sending, its WABA, its number and the token in the vault.
const TENANT: &str = "msg-sender";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-the-merchants-token";
const TO: &str = "+16505551234";

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_timeout_may_have_been_sent_and_its_key_replays_it_without_a_second_request(backend).await;
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
/// the number it owns, and where keys are kept.
struct Service {
    authz: Authorizer,
    caller: Caller,
    number: OwnedNumber,
    records: Arc<dyn IdempotencyRecords>,
    script: ScriptedTransport,
}

impl Service {
    fn keeper(&self) -> Keeper<'_> {
        Keeper::new(
            &self.records,
            Duration::from_secs(60),
            Duration::from_secs(3600),
            render,
        )
    }

    /// The core's send, the accepted message as `202` and its id, under
    /// `key`, for `caller`.
    async fn send_as(
        &self,
        caller: &Caller,
        key: &str,
        message: &OutboundMessage,
        fingerprint: &Fingerprint,
    ) -> Result<Answer, ServiceError> {
        let keeper = self.keeper();
        let messages = Messages::new(&self.authz, &self.number);
        idempotency::run(
            &keeper,
            &self.authz,
            caller,
            IdempotencyKey::parse(key),
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
        self.send_as(&self.caller, key, message, fingerprint).await
    }
}

/// The tenant with a `send` key, its WABA and number bound, and the
/// number's token in the vault; the caller the order admits for that key,
/// and the number it owns.
async fn service(backend: &dyn Backend) -> Service {
    let records = backend.records();
    let tenant = TenantId::parse(TENANT).unwrap();
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
    let waba = WabaId::new(WABA).unwrap();
    let pn = PhoneNumberId::new(PN).unwrap();
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
    let authz = Authorizer::new(records, vault, client).unwrap();
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
        script,
    }
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
///    reaches Meta); that success is then replayed, with no third.
/// 5. A [`Caller`] another [`Authorizer`] made (one over the same
///    records, for the same key) is refused, `403 forbidden`, before the
///    key is read: the tenant's kept answer is not replayed to it, and
///    nothing is sent.
///
/// Decisive: running the operation without claiming the key (no lease),
/// or releasing a key whose outcome may have been sent, fails step 3 (a
/// second send, which nothing answers); keeping a key whose outcome
/// proves nothing was sent fails step 4; dropping the caller's admission
/// fails step 5.
pub async fn a_timeout_may_have_been_sent_and_its_key_replays_it_without_a_second_request(
    backend: &dyn Backend,
) {
    let service = service(backend).await;
    let script = &service.script;
    let path = format!("/v1/numbers/{PN}/messages");
    let fingerprint = Fingerprint::json("POST", &path, &json!({"to": {"phone": TO}}));

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

    // 5. Another authorizer's caller, for the same key: refused first.
    let other = Authorizer::new(
        backend.records(),
        TokenVault::new(
            backend.kv(),
            VaultKeys::new(VaultKey::generate("messages-suite-other").unwrap()),
        )
        .unwrap(),
        Client::builder()
            .transport(ScriptedTransport::new())
            .build()
            .unwrap(),
    )
    .unwrap();
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(TenantId::parse(TENANT).unwrap()),
        vec![Scope::Send],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    let forged = other
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Send,
        )
        .await
        .unwrap();
    let refused = service
        .send_as(&forged, "order:1234:shipped", &message, &fingerprint)
        .await
        .unwrap_err();
    assert_eq!((refused.status(), refused.code()), (403, "forbidden"));
    assert_eq!(script.requests().len(), 3, "a request was sent");
}
