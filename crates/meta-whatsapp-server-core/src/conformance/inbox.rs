//! Roadmap M2a: the core's inbox services ([`crate::inbox`]) over a
//! backend's bundle, with no HTTP layer: Meta's webhooks recorded by the
//! core's pipeline ([`crate::inbound::Pipeline`]) into the bundle's
//! `ConversationStore`, read and answered through the core's
//! [`Conversations`] on a number the core's [`Authorizer`] made, replying
//! with a Graph client on a [`ScriptedTransport`].
//!
//! - Acceptance test M2.1, as the core runs it: an inbound message is
//!   listed, then its history read, then a reply sent within the window
//!   (Meta's documented request, the number's token) and recorded
//!   `accepted`, which a status webhook moves to `delivered`; a free-form
//!   reply outside the window is `409 customer_service_window_closed` with
//!   zero requests, a template is not; pages follow the store's cursor.
//! - The binding epoch (decision D10): after a move of the number's WABA
//!   to another tenant, its new holder lists and reads nothing of the
//!   previous holder's (which stays stored: hidden, never purged), and
//!   reads its own; a first onboarding reads history synced from before
//!   its binding. And a number Meta migrated from one tenant's WABA to
//!   another tenant's, neither WABA moving, shows the new tenant none of
//!   the previous one's (roadmap M2h).
//! - The thread's owner: after Meta's `control_taken`, a free-form reply
//!   is `409 thread_owned_elsewhere` with zero requests; with the
//!   number's ownership check off ([`ReplyChecks`] per number), it goes
//!   to Meta; a template needs no ownership.
//! - Only the order's own number opens the services.
//! - Housekeeping applies the inbox store's retention
//!   (`ConversationStore::apply_retention`, under its turn): a message
//!   older than the store's retention is purged, a newer one kept, and
//!   with `Retention::Keep` both are kept.

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::core::ids::{MessageId, PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, AppSecret, VerifyToken};
use meta_whatsapp_rs::core::store::{
    ConversationKey, DeliveryStatus, Direction, Retention, StoredMessage,
};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::inbox::ReplyChecks;
use meta_whatsapp_rs::webhooks::sign;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::messages::Told;
use crate::admin::mint;
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::backend::Backend;
use crate::housekeeping;
use crate::inbound::{Inbound, Pipeline};
use crate::inbox::Conversations;
use crate::messages::{MessageType, ReplyMessage, TextObject};
use crate::model::{BindOutcome, KeyOwner, PageRequest, Scope, TenantId};

/// The Meta app's secret, which signs the deliveries.
const APP_SECRET: &str = "app-secret-of-the-inbox-suite";
/// The customer, by `wa_id` (`webhooks/reference/messages/text`).
const CONTACT: &str = "16505551234";
/// The address a reply to [`CONTACT`] goes to: `+` and the `wa_id`.
const TO: &str = "+16505551234";
const TOKEN: &str = "EAAG-the-merchants-token";

/// Every case, on one backend; the retention's last (it purges by age
/// across the backend's numbers).
pub async fn run(backend: &dyn Backend) {
    an_inbound_message_is_listed_read_and_answered_within_its_window(backend).await;
    a_moved_numbers_history_is_hidden_from_its_new_tenant(backend).await;
    a_number_migrated_between_two_tenants_wabas_hides_its_history(backend).await;
    a_reply_while_another_app_owns_the_thread_is_refused_unless_its_number_turns_the_check_off(
        backend,
    )
    .await;
    the_services_open_on_their_own_number_only(backend).await;
    housekeeping_applies_the_inbox_stores_retention(backend).await;
}

/// A tenant's number, as one request sees it: the order, the caller it
/// admitted (scope `inbox`), the number it owns, and Meta's script.
struct Merchant {
    authz: Authorizer,
    caller: Caller,
    number: OwnedNumber,
    script: ScriptedTransport,
    vault: TokenVault,
}

impl Merchant {
    /// The inbox services on the number, with `checks`, at `clock`'s time.
    fn inbox<'a>(
        &'a self,
        backend: &dyn Backend,
        checks: ReplyChecks,
        clock: &ManualClock,
    ) -> Conversations<'a> {
        Conversations::new(
            &self.authz,
            &self.number,
            backend.conversations(),
            checks,
            Arc::new(clock.clone()),
        )
        .unwrap()
    }

    /// The number opened again (after a move, say).
    async fn reopen(&mut self, pn: &str) {
        self.number = self
            .authz
            .owned_number(&self.caller, PhoneNumberId::new(pn).unwrap())
            .await
            .unwrap();
    }
}

/// `tenant` (created if new) with an `inbox` key, `waba` and `pn` bound to
/// it, and the number's token in the vault; the caller the order admits
/// for that key, and the number it owns.
async fn merchant(backend: &dyn Backend, tenant: &str, waba: &str, pn: &str) -> Merchant {
    let records = backend.records();
    let tenant = TenantId::parse(tenant).unwrap();
    if records.tenant(&tenant).await.unwrap().is_none() {
        records.create_tenant(&tenant, "").await.unwrap();
    }
    let (key, _) = mint(
        records.as_ref(),
        KeyOwner::Tenant(tenant.clone()),
        vec![Scope::Inbox],
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
        VaultKeys::new(VaultKey::generate("inbox-suite").unwrap()),
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
            Scope::Inbox,
        )
        .await
        .unwrap();
    let number = authz.owned_number(&caller, pn).await.unwrap();
    Merchant {
        authz,
        caller,
        number,
        script,
        vault,
    }
}

/// The core's pipeline on `backend`, verifying with the suite's secret.
fn pipeline(backend: &dyn Backend) -> Pipeline {
    let inbound = Inbound::new(
        vec![AppSecret::new(APP_SECRET)],
        VerifyToken::new("verify-token-of-the-inbox-suite"),
    )
    .unwrap();
    Pipeline::new(inbound, backend, Arc::new(Told::default()))
}

/// `payload` delivered as Meta delivers it, signed; it must be delivered.
async fn deliver(pipeline: &Pipeline, payload: &Value) {
    let body = serde_json::to_vec(payload).unwrap();
    pipeline
        .deliver(
            Some(&sign(&AppSecret::new(APP_SECRET), &body)),
            bytes::Bytes::from(body),
        )
        .await
        .unwrap();
}

/// Now, in Unix seconds (Meta dates in whole seconds).
fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// `webhooks/reference/messages/text`, its example: `contact`'s text
/// `body` to `pn` of `waba`, message id `wamid`, dated `at`.
fn text(waba: &str, pn: &str, contact: &str, wamid: &str, body: &str, at: i64) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "changes": [{
                "value": {
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": pn},
                    "contacts": [{"profile": {"name": "Sheena Nelson"}, "wa_id": contact}],
                    "messages": [{
                        "from": contact,
                        "id": wamid,
                        "timestamp": at.to_string(),
                        "type": "text",
                        "text": {"body": body}
                    }]
                },
                "field": "messages"
            }]
        }]
    })
}

/// `webhooks/reference/messages/status`, its example with `status`
/// `delivered`: message `wamid`, sent from `pn` of `waba` to [`CONTACT`],
/// dated `at`.
fn delivered(waba: &str, pn: &str, wamid: &str, at: i64) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "changes": [{
                "value": {
                    "messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "15550783881", "phone_number_id": pn},
                    "statuses": [{
                        "id": wamid,
                        "status": "delivered",
                        "timestamp": at.to_string(),
                        "recipient_id": CONTACT,
                        "pricing": {
                            "billable": false,
                            "pricing_model": "PMP",
                            "type": "free_customer_service",
                            "category": "service"
                        }
                    }]
                },
                "field": "messages"
            }]
        }]
    })
}

/// `conversation-routing/thread-control`, its `control_taken` example:
/// another app takes [`CONTACT`]'s thread on `pn` of `waba`, at `at`.
fn control_taken(waba: &str, pn: &str, at: i64) -> Value {
    json!({
        "object": "whatsapp_business_account",
        "entry": [{
            "id": waba,
            "changes": [{
                "field": "messaging_handovers",
                "value": {
                    "messaging_product": "whatsapp",
                    "sender": {"phone_number": CONTACT},
                    "recipient": {"phone_number_id": pn, "display_phone_number": "15550783881"},
                    "type": "control_taken",
                    "timestamp": at.to_string(),
                    "control_taken": {
                        "previous_owner_role": "customer_service",
                        "new_owner_role": "escalation",
                        "metadata": "Human agent stepping in"
                    }
                }
            }]
        }]
    })
}

/// messages/text-messages, "Example response", for message `wamid`.
fn accepted(wamid: &str) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": TO, "wa_id": CONTACT}],
        "messages": [{"id": wamid}]
    })
}

/// A free-form text reply.
fn text_reply(body: &str) -> ReplyMessage {
    let mut reply = ReplyMessage::new(MessageType::Text);
    reply.text = Some(TextObject {
        body: body.to_owned(),
        preview_url: None,
    });
    reply
}

/// A template reply (`messages/send-template-messages`).
fn template_reply() -> ReplyMessage {
    let mut reply = ReplyMessage::new(MessageType::Template);
    reply.template = Some(json!({"name": "order_update", "language": {"code": "en_US"}}));
    reply
}

/// A first page of `limit`.
fn first(limit: usize) -> PageRequest {
    PageRequest { after: None, limit }
}

/// M2.1 with no HTTP layer.
///
/// 1. A customer's text, delivered by the pipeline: listed (its preview,
///    one unread, the time it was sent), then its history read.
/// 2. A reply within the window: Meta's documented request with the
///    number's token, to `+<wa_id>`; recorded `accepted`, newest first.
/// 3. Meta's `delivered` status: the reply is `delivered`.
/// 4. 25 hours later, a free-form reply is `409
///    customer_service_window_closed` with zero requests; a template goes.
/// 5. Pages: three conversations by two, the cursor exclusive.
pub async fn an_inbound_message_is_listed_read_and_answered_within_its_window(
    backend: &dyn Backend,
) {
    let (waba, pn) = ("102290129340398", "106540352242922");
    let m = merchant(backend, "inbox-m21", waba, pn).await;
    let pipeline = pipeline(backend);
    let sent_at = now();
    deliver(
        &pipeline,
        &text(
            waba,
            pn,
            CONTACT,
            "wamid.M21-IN",
            "Does it come in another color?",
            sent_at,
        ),
    )
    .await;
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let inbox = m.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(inbox.history_from(), None, "a first onboarding");

    // 1. Listed, then read.
    let listed = inbox.list(&first(50)).await.unwrap();
    assert_eq!(listed.items.len(), 1, "{listed:?}");
    let conversation = &listed.items[0];
    assert_eq!(conversation.key.contact, CONTACT);
    assert_eq!(
        conversation.last_text.as_deref(),
        Some("Does it come in another color?")
    );
    assert_eq!(conversation.unread, 1);
    let sent_at = OffsetDateTime::from_unix_timestamp(sent_at).unwrap();
    assert_eq!(conversation.last_inbound_at, Some(sent_at));
    assert_eq!(listed.next_after, None);
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    assert_eq!(history.items.len(), 1);
    let received = &history.items[0];
    assert_eq!(received.id.as_str(), "wamid.M21-IN");
    assert_eq!(received.direction, Direction::Inbound);
    assert_eq!(received.status, DeliveryStatus::Received);
    assert_eq!(
        received.text.as_deref(),
        Some("Does it come in another color?")
    );
    assert_eq!(received.timestamp, sent_at);
    assert!(m.script.requests().is_empty(), "reading asks Meta nothing");

    // 2. A reply within the window.
    m.script.push_json(200, accepted("wamid.M21-REPLY"));
    let reply = inbox
        .reply(CONTACT, text_reply("Yes: blue and green."))
        .await
        .unwrap();
    assert_eq!(reply.message_id.as_str(), "wamid.M21-REPLY");
    let requests = m.script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "POST");
    assert_eq!(requests[0].path(), format!("/v25.0/{pn}/messages"));
    assert_eq!(requests[0].bearer(), Some(TOKEN), "the vault's token");
    assert_eq!(
        requests[0].json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": TO,
            "type": "text",
            "text": {"body": "Yes: blue and green."}
        })
    );
    assert_eq!(m.script.remaining(), 0);
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    let ids: Vec<_> = history.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["wamid.M21-REPLY", "wamid.M21-IN"], "newest first");
    let recorded = &history.items[0];
    assert_eq!(recorded.direction, Direction::Outbound);
    assert_eq!(
        recorded.status,
        DeliveryStatus::Accepted,
        "recorded accepted"
    );
    assert_eq!(recorded.text.as_deref(), Some("Yes: blue and green."));

    // 3. Meta's status moves it on.
    deliver(&pipeline, &delivered(waba, pn, "wamid.M21-REPLY", now())).await;
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    assert_eq!(history.items[0].id.as_str(), "wamid.M21-REPLY");
    assert_eq!(history.items[0].status, DeliveryStatus::Delivered);
    assert!(history.items[0].status_at.is_some());

    // 4. Outside the window.
    let later = ManualClock::new(OffsetDateTime::now_utc() + time::Duration::hours(25));
    let late = m.inbox(backend, ReplyChecks::ALL, &later);
    let refused = late
        .reply(CONTACT, text_reply("Still there?"))
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (409, "customer_service_window_closed")
    );
    assert!(!refused.may_have_been_sent());
    assert_eq!(m.script.requests().len(), 1, "refused with zero requests");
    m.script.push_json(200, accepted("wamid.M21-TEMPLATE"));
    late.reply(CONTACT, template_reply()).await.unwrap();
    assert_eq!(m.script.requests().len(), 2, "a template is exempt");
    assert_eq!(
        m.script.requests()[1].json().unwrap()["template"]["name"],
        "order_update"
    );

    // 5. Pages, by the store's exclusive cursor.
    for (i, contact) in ["16505550001", "16505550002"].into_iter().enumerate() {
        let wamid = format!("wamid.M21-PAGE-{i}");
        deliver(&pipeline, &text(waba, pn, contact, &wamid, "hi", now())).await;
    }
    let page = inbox.list(&first(2)).await.unwrap();
    assert_eq!(page.items.len(), 2);
    let after = page.next_after.clone().expect("a next page");
    let rest = inbox
        .list(&PageRequest {
            after: Some(after),
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(rest.next_after, None);
    let mut contacts: Vec<_> = page
        .items
        .iter()
        .chain(&rest.items)
        .map(|c| c.key.contact.clone())
        .collect();
    assert_eq!(contacts.len(), 3, "no repeats, no gaps");
    contacts.sort();
    assert_eq!(contacts, ["16505550001", "16505550002", CONTACT]);
    let one = inbox.history(CONTACT, &first(1)).await.unwrap();
    let older = inbox
        .history(
            CONTACT,
            &PageRequest {
                after: one.next_after.clone(),
                limit: 50,
            },
        )
        .await
        .unwrap();
    assert_eq!(one.items.len() + older.items.len(), 3);
    assert!(older.items.iter().all(|m| m.id != one.items[0].id));
    let bad = inbox
        .list(&PageRequest {
            after: Some("not a cursor".to_owned()),
            limit: 2,
        })
        .await
        .unwrap_err();
    assert_eq!(bad.field(), Some("cursor"));
}

/// The binding epoch (decision D10), the decisive case of roadmap M2a.
///
/// 1. Tenant A's number holds a customer's message; A reads it.
/// 2. The WABA moves to tenant B (unbound, then bound to B): B lists no
///    conversation and reads no history, while the store keeps A's
///    message (hidden, not purged). B's cut is its binding's start.
/// 3. The customer writes to B: B reads that message alone.
/// 4. A first onboarding (tenant C) reads history synced from before its
///    binding.
pub async fn a_moved_numbers_history_is_hidden_from_its_new_tenant(backend: &dyn Backend) {
    let (waba, pn) = ("102290129340388", "106540352242912");
    let pipeline = pipeline(backend);
    let a = merchant(backend, "inbox-epoch-a", waba, pn).await;
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    // 1. A's customer, in the binding's own second (a first onboarding's).
    deliver(
        &pipeline,
        &text(
            waba,
            pn,
            CONTACT,
            "wamid.EPOCH-A",
            "Where is my order?",
            now(),
        ),
    )
    .await;
    let before = a.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(before.list(&first(50)).await.unwrap().items.len(), 1);

    // 2. The move.
    let records = backend.records();
    assert!(
        records
            .unbind_waba(&WabaId::new(waba).unwrap())
            .await
            .unwrap()
    );
    let mut b = merchant(backend, "inbox-epoch-b", waba, pn).await;
    let binding = records
        .waba(&WabaId::new(waba).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(binding.previous_tenant_unbound_at.is_some(), "a move");
    // The number moved with its WABA (released by the unbind), so it has
    // a cut of its own too, at its own start: the later one counts.
    let number = records
        .number(&PhoneNumberId::new(pn).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(number.previous_tenant_released_at.is_some(), "a move");
    let inbox = b.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(
        inbox.history_from(),
        Some(binding.attached_at.max(number.attached_at))
    );
    let listed = inbox.list(&first(50)).await.unwrap();
    assert!(
        listed.items.is_empty(),
        "the previous holder's conversation is shown to the new one: {listed:?}"
    );
    assert_eq!(listed.next_after, None);
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    assert!(
        history.items.is_empty(),
        "the previous holder's history is shown to the new one: {history:?}"
    );
    let key = ConversationKey::new(PhoneNumberId::new(pn).unwrap(), CONTACT);
    let kept = backend
        .conversations()
        .messages(&key, None, 50)
        .await
        .unwrap();
    assert_eq!(kept.len(), 1, "hidden, never purged");
    drop(inbox);

    // 3. The customer writes to B: after a move, an event of the binding's
    //    own second reaches nobody, so the next one.
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.EPOCH-B", "Hello again", now() + 1),
    )
    .await;
    b.reopen(pn).await;
    let inbox = b.inbox(backend, ReplyChecks::ALL, &clock);
    let listed = inbox.list(&first(50)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].last_text.as_deref(), Some("Hello again"));
    assert!(listed.items[0].last_inbound_at.is_some());
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    let ids: Vec<_> = history.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["wamid.EPOCH-B"], "B's own message, and not A's");
    assert_eq!(history.next_after, None);
    // A's capability, made before the move, reads nothing any more.
    let stale = before.list(&first(50)).await.unwrap_err();
    assert_eq!(stale.code(), "storage_unavailable");

    // 4. A first onboarding keeps what was synced from before it.
    let (waba, pn) = ("102290129340387", "106540352242911");
    let c = merchant(backend, "inbox-epoch-c", waba, pn).await;
    let synced_at = OffsetDateTime::now_utc() - time::Duration::days(2);
    let synced = StoredMessage {
        id: MessageId::new("wamid.EPOCH-SYNCED"),
        conversation: ConversationKey::new(PhoneNumberId::new(pn).unwrap(), CONTACT),
        direction: Direction::Inbound,
        kind: "text".to_owned(),
        text: Some("From before the onboarding".to_owned()),
        payload: json!({"type": "text", "text": {"body": "From before the onboarding"}}),
        status: DeliveryStatus::Received,
        timestamp: synced_at,
        status_at: None,
        error: None,
    };
    assert_eq!(
        backend
            .conversations()
            .append_synced(vec![synced])
            .await
            .unwrap(),
        [true]
    );
    let inbox = c.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(inbox.history_from(), None);
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    assert_eq!(
        history.items.len(),
        1,
        "a first onboarding's synced history"
    );
    assert_eq!(inbox.list(&first(50)).await.unwrap().items.len(), 1);
}

/// Roadmap M2h, decisive: Meta migrated a number from tenant A's WABA to
/// tenant B's WABA, neither WABA moving (B's bound long before, A's still
/// A's). B lists and reads none of A's history on the number (kept in the
/// store: hidden, never purged), and reads its own; B's cut is the
/// number's own start under B. Decisive: the number's cut in
/// `authz::history_from` (the WABA's cut alone shows A's history to B).
pub async fn a_number_migrated_between_two_tenants_wabas_hides_its_history(backend: &dyn Backend) {
    let (waba_a, waba_b, pn) = ("102290129340358", "102290129340357", "106540352242872");
    let records = backend.records();
    let tenant_b = TenantId::parse("inbox-nmove-b").unwrap();
    records.create_tenant(&tenant_b, "").await.unwrap();
    assert_eq!(
        records
            .bind_waba(&tenant_b, &WabaId::new(waba_b).unwrap(), &[])
            .await
            .unwrap(),
        BindOutcome::Bound,
        "B's WABA, bound before the number reaches it"
    );
    let a = merchant(backend, "inbox-nmove-a", waba_a, pn).await;
    let pipeline = pipeline(backend);
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    // A's customer, in the binding's own second (a first onboarding's).
    deliver(
        &pipeline,
        &text(
            waba_a,
            pn,
            CONTACT,
            "wamid.NMOVE-A",
            "My address is 1 Hacker Way",
            now(),
        ),
    )
    .await;
    let seen = a.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(seen.list(&first(50)).await.unwrap().items.len(), 1);

    // The migration: A's WABA no longer lists the number, B's does.
    let tenant_a = TenantId::parse("inbox-nmove-a").unwrap();
    assert_eq!(
        records
            .bind_waba(&tenant_a, &WabaId::new(waba_a).unwrap(), &[])
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let b = merchant(backend, "inbox-nmove-b", waba_b, pn).await;
    let number = records
        .number(&PhoneNumberId::new(pn).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(number.previous_tenant_released_at.is_some(), "a move");
    let waba = records
        .waba(&WabaId::new(waba_b).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        waba.previous_tenant_unbound_at, None,
        "B's WABA never moved"
    );
    let inbox = b.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(inbox.history_from(), Some(number.attached_at));
    let listed = inbox.list(&first(50)).await.unwrap();
    assert!(
        listed.items.is_empty(),
        "the previous holder's conversation is shown to the new one: {listed:?}"
    );
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    assert!(
        history.items.is_empty(),
        "the previous holder's history is shown to the new one: {history:?}"
    );
    let key = ConversationKey::new(PhoneNumberId::new(pn).unwrap(), CONTACT);
    let kept = backend
        .conversations()
        .messages(&key, None, 50)
        .await
        .unwrap();
    assert_eq!(kept.len(), 1, "hidden, never purged");
    drop(inbox);

    // The customer writes to B, under B's WABA.
    deliver(
        &pipeline,
        &text(
            waba_b,
            pn,
            CONTACT,
            "wamid.NMOVE-B",
            "Hello again",
            now() + 1,
        ),
    )
    .await;
    let inbox = b.inbox(backend, ReplyChecks::ALL, &clock);
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    let ids: Vec<_> = history.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["wamid.NMOVE-B"], "B's own message, and not A's");
    let listed = inbox.list(&first(50)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].last_text.as_deref(), Some("Hello again"));
}

/// Meta's `control_taken`: another app owns the thread.
///
/// 1. A free-form reply is `409 thread_owned_elsewhere`, zero requests.
/// 2. A template goes (it needs no ownership).
/// 3. With the number's ownership check off, the free-form reply goes to
///    Meta, which decides.
pub async fn a_reply_while_another_app_owns_the_thread_is_refused_unless_its_number_turns_the_check_off(
    backend: &dyn Backend,
) {
    let (waba, pn) = ("102290129340378", "106540352242902");
    let m = merchant(backend, "inbox-owner", waba, pn).await;
    let pipeline = pipeline(backend);
    let at = now();
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.OWNER-IN", "I need a human", at),
    )
    .await;
    deliver(&pipeline, &control_taken(waba, pn, at + 1)).await;
    let clock = ManualClock::new(OffsetDateTime::now_utc() + time::Duration::minutes(1));

    // 1. Refused before any request.
    let inbox = m.inbox(backend, ReplyChecks::ALL, &clock);
    let refused = inbox
        .reply(CONTACT, text_reply("Let me check"))
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (409, "thread_owned_elsewhere")
    );
    assert!(!refused.may_have_been_sent());
    assert!(m.script.requests().is_empty(), "refused with zero requests");

    // 2. A template needs no ownership.
    m.script.push_json(200, accepted("wamid.OWNER-TEMPLATE"));
    inbox.reply(CONTACT, template_reply()).await.unwrap();
    assert_eq!(m.script.requests().len(), 1);

    // 3. The number's check off: Meta decides.
    let partner = ReplyChecks::ALL.thread_owner(false);
    let inbox = m.inbox(backend, partner, &clock);
    assert_eq!(inbox.reply_checks(), partner);
    m.script.push_json(200, accepted("wamid.OWNER-REPLY"));
    let sent = inbox
        .reply(CONTACT, text_reply("Escalation partner here"))
        .await
        .unwrap();
    assert_eq!(sent.message_id.as_str(), "wamid.OWNER-REPLY");
    assert_eq!(m.script.requests().len(), 2);
    assert_eq!(m.script.remaining(), 0);
}

/// The services open only on a number their own [`Authorizer`] made: one
/// another order made is `403 forbidden`, and nothing is read or sent.
pub async fn the_services_open_on_their_own_number_only(backend: &dyn Backend) {
    let (waba, pn) = ("102290129340368", "106540352242892");
    let m = merchant(backend, "inbox-admit", waba, pn).await;
    let script = ScriptedTransport::new();
    let client = Client::builder().transport(script.clone()).build().unwrap();
    let other = Authorizer::new(backend.records(), m.vault.clone(), client).unwrap();
    let refused = Conversations::new(
        &other,
        &m.number,
        backend.conversations(),
        ReplyChecks::ALL,
        Arc::new(ManualClock::new(OffsetDateTime::now_utc())),
    )
    .unwrap_err();
    assert_eq!((refused.status(), refused.code()), (403, "forbidden"));
    assert!(script.requests().is_empty() && m.script.requests().is_empty());
}

/// Housekeeping applies the retention the inbox store was built with:
/// one round, under its turn, purges a message dated a day past the
/// store's cutoff and keeps one of now; under `Retention::Keep` it keeps
/// both. Decisive: the round's `apply_retention`.
pub async fn housekeeping_applies_the_inbox_stores_retention(backend: &dyn Backend) {
    let pn = PhoneNumberId::new("106540352242882").unwrap();
    let store = backend.conversations();
    let now = OffsetDateTime::now_utc();
    let retention = store.retention();
    let old_at = match retention.cutoff(now) {
        Some(cutoff) => cutoff - time::Duration::days(1),
        None => now - time::Duration::days(400),
    };
    let message = |id: &str, at: OffsetDateTime| StoredMessage {
        id: MessageId::new(id),
        conversation: ConversationKey::new(pn.clone(), CONTACT),
        direction: Direction::Inbound,
        kind: "text".to_owned(),
        text: Some("hi".to_owned()),
        payload: json!({"type": "text", "text": {"body": "hi"}}),
        status: DeliveryStatus::Received,
        timestamp: at,
        status_at: None,
        error: None,
    };
    assert!(
        store
            .append(message("wamid.RETENTION-OLD", old_at))
            .await
            .unwrap()
    );
    assert!(
        store
            .append(message("wamid.RETENTION-NEW", now))
            .await
            .unwrap()
    );
    assert!(
        housekeeping::round(backend, Duration::from_hours(7 * 24)).await,
        "the round took its turn"
    );
    let key = ConversationKey::new(pn.clone(), CONTACT);
    let left: Vec<String> = store
        .messages(&key, None, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id.as_str().to_owned())
        .collect();
    if retention == Retention::Keep {
        assert_eq!(left, ["wamid.RETENTION-NEW", "wamid.RETENTION-OLD"], "kept");
    } else {
        assert_eq!(
            left,
            ["wamid.RETENTION-NEW"],
            "a message past the inbox's retention ({retention:?}) survived housekeeping"
        );
    }
}
