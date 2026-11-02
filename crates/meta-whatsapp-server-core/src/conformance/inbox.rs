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
//!   A free-form reply into a window the previous holder's customer
//!   opened is `409 customer_service_window_closed`, with zero requests;
//!   `unread` counts the new holder's messages only.
//! - A capability made before its number left its binding (another WABA
//!   of the tenant, then another tenant) reads nothing, and erases
//!   nothing (roadmap M2f): `503`.
//! - Roadmap M2f: an erasure by one identity reaches every identity the
//!   inbox links, in the inbox and the outbox, the other customer's
//!   records kept; its journal entry, written first, holds keyed HMACs
//!   only, and a journal that fails erases nothing; a deployment without
//!   an erasure key, or a request not confirming the number, erases
//!   nothing.
//! - Only the order's own number opens the services.
//! - Roadmap M2g: the window is what a free-form reply would meet (and the
//!   list's is the same); marking a conversation read resets `unread`
//!   and, only with `notify_customer`, sends one read receipt for the
//!   latest inbound message (none past Meta's 30 days, the bound
//!   included; a refused one resets nothing; a revoke's tombstone skipped,
//!   and the message found past the history's first page; a number that
//!   moves while Meta answers it resets nothing); a message is
//!   found by id on its own number only;
//!   after a move, the previous holder's message is `404` by id to the new
//!   holder, its window is not shown, and no receipt goes for it.
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
use meta_whatsapp_rs::core::clock::Clock as _;
use meta_whatsapp_rs::core::clock::ManualClock;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{MessageId, PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, AppSecret, VerifyToken};
use meta_whatsapp_rs::core::store::{
    ConversationKey, DeliveryStatus, Direction, Retention, StoredMessage, ThreadOwner,
    ThreadOwnership,
};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_rs::core::transport::{HttpRequest, HttpResponse, HttpTransport};
use meta_whatsapp_rs::inbox::ReplyChecks;
use meta_whatsapp_rs::webhooks::sign;
use serde_json::{Value, json};
use time::OffsetDateTime;

use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::secret::SecretBytes;

use super::messages::Told;
use crate::admin::mint;
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::backend::Backend;
use crate::erasure::{ErasureDeployment, ErasureJournal, ErasureKey, Erasures, JournalEntry};
use crate::housekeeping;
use crate::inbound::{Inbound, Pipeline};
use crate::inbox::{Conversations, READ_RECEIPT_MAX_AGE, ReplyWindow};
use crate::messages::{MessageType, ReplyMessage, TextObject};
use crate::model::{BindOutcome, KeyOwner, PageRequest, Scope, TenantId};
use crate::store::RecordStore;

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
    // Boxed: the scenarios' futures are large, and `conformance::run`
    // holds them all (clippy's `large_futures` in the backends' crates).
    Box::pin(an_inbound_message_is_listed_read_and_answered_within_its_window(backend)).await;
    Box::pin(a_moved_numbers_history_is_hidden_from_its_new_tenant(
        backend,
    ))
    .await;
    Box::pin(a_number_migrated_between_two_tenants_wabas_hides_its_history(backend)).await;
    Box::pin(a_capability_made_before_its_number_left_reads_nothing(
        backend,
    ))
    .await;
    Box::pin(
        a_reply_while_another_app_owns_the_thread_is_refused_unless_its_number_turns_the_check_off(
            backend,
        ),
    )
    .await;
    Box::pin(the_services_open_on_their_own_number_only(backend)).await;
    Box::pin(the_window_is_what_a_reply_would_meet(backend)).await;
    Box::pin(
        marking_a_conversation_read_resets_unread_and_receipts_the_latest_inbound_message(backend),
    )
    .await;
    Box::pin(a_read_receipt_skips_a_tombstone_and_reads_past_the_first_page(backend)).await;
    Box::pin(a_number_that_moves_during_the_receipt_resets_nothing(
        backend,
    ))
    .await;
    Box::pin(a_message_is_found_by_id_on_its_own_number(backend)).await;
    Box::pin(the_previous_holders_message_window_and_receipt_never_reach_the_new_holder(backend))
        .await;
    Box::pin(housekeeping_applies_the_inbox_stores_retention(backend)).await;
    Box::pin(an_erasure_reaches_every_identity_and_journals_first(
        backend,
    ))
    .await;
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
    merchant_over(backend, tenant, waba, pn, |script| Arc::new(script)).await
}

/// [`merchant`], its client sending through what `transport` makes of
/// Meta's script.
async fn merchant_over(
    backend: &dyn Backend,
    tenant: &str,
    waba: &str,
    pn: &str,
    transport: impl FnOnce(ScriptedTransport) -> Arc<dyn HttpTransport>,
) -> Merchant {
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
    let client = Client::builder()
        .shared_transport(transport(script.clone()))
        .build()
        .unwrap();
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
    assert_eq!(
        listed.items[0].unread, 1,
        "B's unread message, not A's (the store counts both)"
    );
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
#[allow(clippy::too_many_lines)] // a scenario: it reads top to bottom
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

    // A's customer opened a window on the number, minutes ago: not B's
    // conversation to continue (B cannot see it). A free-form reply is
    // refused before any request, also with the ownership check off; a
    // template goes (B's own conversation, dated after the cut).
    let b_clock = ManualClock::new(OffsetDateTime::now_utc());
    // A handover to this app recorded while A held the number: A's, not
    // B's to trust.
    assert!(
        backend
            .conversations()
            .set_thread_owner(
                &key,
                ThreadOwnership {
                    owner: ThreadOwner::ThisApp,
                    role: None,
                    app_id: None,
                    since: number.attached_at - time::Duration::milliseconds(1),
                },
            )
            .await
            .unwrap()
    );
    for checks in [ReplyChecks::ALL, ReplyChecks::ALL.thread_owner(false)] {
        let refused = b
            .inbox(backend, checks, &b_clock)
            .reply(CONTACT, text_reply("Hello from B"))
            .await
            .unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (409, "customer_service_window_closed"),
            "{checks:?}"
        );
        assert!(!refused.may_have_been_sent());
    }
    assert!(b.script.requests().is_empty(), "refused with zero requests");
    let inbox = b.inbox(backend, ReplyChecks::ALL, &b_clock);
    b.script.push_json(200, accepted("wamid.NMOVE-B-TEMPLATE"));
    inbox.reply(CONTACT, template_reply()).await.unwrap();
    assert_eq!(b.script.requests().len(), 1, "a template is exempt");
    // With the window check off on the number, Meta decides.
    b.script.push_json(200, accepted("wamid.NMOVE-B-UNCHECKED"));
    b.inbox(backend, ReplyChecks::NONE, &b_clock)
        .reply(CONTACT, text_reply("Hello from B"))
        .await
        .unwrap();
    assert_eq!(b.script.requests().len(), 2, "no check: Meta decides");
    // Listed now (B's replies are after the cut), with nothing of A's
    // customer: no `last_inbound_at`, no unread.
    let listed = inbox.list(&first(50)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(
        (listed.items[0].last_inbound_at, listed.items[0].unread),
        (None, 0),
        "A's customer's last message and unread count, shown to B: {listed:?}"
    );
    // A handover to B's app recorded since the cut: B's thread, and with
    // the handover trusted, B's window; not trusted, still refused.
    assert!(
        backend
            .conversations()
            .set_thread_owner(
                &key,
                ThreadOwnership {
                    owner: ThreadOwner::ThisApp,
                    role: None,
                    app_id: None,
                    since: b_clock.now(),
                },
            )
            .await
            .unwrap()
    );
    let untrusted = b
        .inbox(backend, ReplyChecks::ALL.trust_handover(false), &b_clock)
        .reply(CONTACT, text_reply("Hello from B"))
        .await
        .unwrap_err();
    assert_eq!(untrusted.code(), "customer_service_window_closed");
    assert_eq!(b.script.requests().len(), 2, "refused with zero requests");
    b.script.push_json(200, accepted("wamid.NMOVE-B-HANDOVER"));
    inbox
        .reply(CONTACT, text_reply("Hello from B"))
        .await
        .unwrap();
    assert_eq!(b.script.requests().len(), 3, "a handover since the cut");
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
    let inbox = b.inbox(backend, ReplyChecks::ALL, &b_clock);
    let history = inbox.history(CONTACT, &first(50)).await.unwrap();
    let ids: Vec<_> = history.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "wamid.NMOVE-B",
            "wamid.NMOVE-B-UNCHECKED",
            "wamid.NMOVE-B-TEMPLATE",
            "wamid.NMOVE-B-HANDOVER"
        ],
        "B's own messages, and not A's"
    );
    let listed = inbox.list(&first(50)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].last_text.as_deref(), Some("Hello again"));
    assert_eq!(
        listed.items[0].unread, 1,
        "B's unread message, not A's (the store counts both)"
    );
    // A window B's customer opened is B's: a free-form reply goes.
    b.script.push_json(200, accepted("wamid.NMOVE-B-REPLY"));
    inbox
        .reply(CONTACT, text_reply("Hello from B"))
        .await
        .unwrap();
    assert_eq!(b.script.requests().len(), 4);
    assert_eq!(b.script.remaining(), 0);
}

/// A capability holds the number only while its binding does: one made
/// before the number moved to another WABA of its tenant, was released
/// from there and bound to another tenant (its first WABA's binding never
/// changing) reads nothing, `503 storage_unavailable` (retryable), rather
/// than the new holder's history under its own, absent, cut. Decisive: the
/// number's binding in `OwnedNumber::still_held` (the WABA's epoch alone
/// still holds).
pub async fn a_capability_made_before_its_number_left_reads_nothing(backend: &dyn Backend) {
    let (waba_b, waba_b2, waba_c, pn) = (
        "102290129340338",
        "102290129340337",
        "102290129340336",
        "106540352242852",
    );
    let records = backend.records();
    let pipeline = pipeline(backend);
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let b = merchant(backend, "inbox-left-b", waba_b, pn).await;
    let tenant_b = TenantId::parse("inbox-left-b").unwrap();
    // To B's other WABA, then released by it: B's first WABA's binding
    // stays as the capability read it.
    for numbers in [vec![PhoneNumberId::new(pn).unwrap()], vec![]] {
        assert_eq!(
            records
                .bind_waba(&tenant_b, &WabaId::new(waba_b2).unwrap(), &numbers)
                .await
                .unwrap(),
            BindOutcome::Bound
        );
    }
    let c = merchant(backend, "inbox-left-c", waba_c, pn).await;
    deliver(
        &pipeline,
        &text(
            waba_c,
            pn,
            CONTACT,
            "wamid.LEFT-C",
            "My address is 1 Hacker Way",
            now() + 1,
        ),
    )
    .await;
    let theirs = c.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(
        theirs
            .history(CONTACT, &first(50))
            .await
            .unwrap()
            .items
            .len(),
        1,
        "C reads its own"
    );
    let stale = b.inbox(backend, ReplyChecks::ALL, &clock);
    let listed = stale.list(&first(50)).await.unwrap_err();
    assert_eq!(
        (listed.status(), listed.code()),
        (503, "storage_unavailable")
    );
    let history = stale.history(CONTACT, &first(50)).await.unwrap_err();
    assert_eq!(
        (history.status(), history.code()),
        (503, "storage_unavailable")
    );
    // Roadmap M2g: nor C's window, C's message, the list's windows, or a
    // receipt for C's message with B's token.
    for refused in [
        stale.window(CONTACT).await.map(|_| ()).unwrap_err(),
        stale.message("wamid.LEFT-C").await.map(|_| ()).unwrap_err(),
        stale
            .list_with_windows(&first(50))
            .await
            .map(|_| ())
            .unwrap_err(),
        stale.mark_read(CONTACT, true).await.unwrap_err(),
    ] {
        assert_eq!(
            (refused.status(), refused.code()),
            (503, "storage_unavailable")
        );
    }
    assert!(
        b.script.requests().is_empty(),
        "a receipt for the new holder's message, with the previous holder's token"
    );
    // Roadmap M2f: nor an erasure of C's customer on the number, which
    // stays C's.
    let deployment =
        ErasureDeployment::new(ErasureKey::new(SecretBytes::new(ERASURE_KEY.to_vec())).unwrap());
    let outbox = backend.outbox();
    let refused = b
        .erasures(backend, outbox.as_ref(), Some(&deployment), &clock)
        .erase(pn, &[CONTACT.to_owned()])
        .await
        .unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (503, "storage_unavailable")
    );
    assert_eq!(
        theirs
            .history(CONTACT, &first(50))
            .await
            .unwrap()
            .items
            .len(),
        1,
        "C's customer was erased through B's capability"
    );
}

/// A journal that keeps its entries, or refuses them all.
#[derive(Default)]
struct KeptJournal {
    entries: std::sync::Mutex<Vec<JournalEntry>>,
    refuse: bool,
}

#[async_trait::async_trait]
impl ErasureJournal for KeptJournal {
    async fn record(&self, entry: &JournalEntry) -> Result<(), StorageError> {
        if self.refuse {
            return Err(StorageError::Backend(anyhow::anyhow!(
                "the suite's journal failure"
            )));
        }
        self.entries.lock().unwrap().push(entry.clone());
        Ok(())
    }
}

impl Merchant {
    /// The erasure on the number, with `deployment`, at `clock`'s time.
    fn erasures<'a>(
        &'a self,
        backend: &dyn Backend,
        outbox: &'a dyn crate::outbox::Outbox,
        deployment: Option<&'a ErasureDeployment>,
        clock: &ManualClock,
    ) -> Erasures<'a> {
        Erasures::new(
            &self.authz,
            &self.caller,
            &self.number,
            backend.conversations(),
            outbox,
            deployment,
            Arc::new(clock.clone()),
        )
        .unwrap()
    }
}

/// The erasure key of the suite.
const ERASURE_KEY: &[u8] = b"erasure-key-of-the-inbox-suite-32b";

/// Roadmap M2f: an erasure on a number, as the core runs it.
///
/// 1. A customer's text under their phone number, then a message carrying
///    both their phone number and their BSUID (the inbox links the two),
///    and another customer's text, each in the inbox and the outbox.
/// 2. With no erasure key, `409 erasure_not_configured`; with the
///    number's id not confirmed, `422` on `confirm`; with a journal that
///    fails, `503`: nothing is erased in any of them.
/// 3. Named by their BSUID alone, the person is erased: the journal's
///    entry (before anything is deleted) holds the HMAC of each identity
///    the inbox connects, never one in clear; both conversations and
///    their outbox rows are gone, the other customer's stay.
/// 4. A repeat answers the same way and erases nothing more.
///
/// Decisive: the closure over `identities` (the phone-keyed thread
/// stays), the journal before the deletions (a failing one erases
/// nothing), the outbox's erasure (its rows stay), the configuration and
/// confirmation checks.
#[allow(clippy::too_many_lines)] // a scenario: it reads top to bottom
pub async fn an_erasure_reaches_every_identity_and_journals_first(backend: &dyn Backend) {
    const BSUID: &str = "US.13491208655302741918";
    const OTHER: &str = "12125557890";
    let (waba, pn) = ("102290129340329", "106540352242829");
    let m = merchant(backend, "inbox-erase", waba, pn).await;
    let pipeline = pipeline(backend);
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let at = now() + 1;
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.ERASE-1", "hi", at),
    )
    .await;
    // Both identities on one message (`business-scoped-user-ids`).
    let mut linked = text(waba, pn, CONTACT, "wamid.ERASE-2", "it's me", at);
    linked["entry"][0]["changes"][0]["value"]["contacts"][0]["user_id"] = json!(BSUID);
    linked["entry"][0]["changes"][0]["value"]["messages"][0]["from_user_id"] = json!(BSUID);
    deliver(&pipeline, &linked).await;
    deliver(
        &pipeline,
        &text(waba, pn, OTHER, "wamid.ERASE-3", "hello", at),
    )
    .await;
    let tenant = TenantId::parse("inbox-erase").unwrap();
    let rows = || async {
        let page = backend
            .outbox()
            .page(&crate::outbox::EventQuery {
                tenant: tenant.clone(),
                after: None,
                types: None,
                phone_number_id: None,
                limit: 100,
                max_bytes: 8 * 1024 * 1024,
            })
            .await
            .unwrap();
        page.events
            .into_iter()
            .map(|event| event.data)
            .collect::<Vec<_>>()
    };
    let naming = |rows: &[String], id: &str| rows.iter().filter(|d| d.contains(id)).count();
    assert_eq!(naming(&rows().await, CONTACT), 2, "{:?}", rows().await);
    let inbox = m.inbox(backend, ReplyChecks::ALL, &clock);
    let threads = || async {
        let listed = inbox.list(&first(50)).await.unwrap();
        let mut contacts: Vec<String> = listed
            .items
            .into_iter()
            .map(|summary| summary.key.contact)
            .collect();
        contacts.sort();
        contacts
    };
    let before = threads().await;
    assert!(before.contains(&CONTACT.to_owned()), "{before:?}");
    assert!(before.contains(&OTHER.to_owned()), "{before:?}");
    let key = || ErasureKey::new(SecretBytes::new(ERASURE_KEY.to_vec())).unwrap();
    let journal = Arc::new(KeptJournal::default());
    let configured = ErasureDeployment::new(key()).with_journal(journal.clone());
    let refusing = ErasureDeployment::new(key()).with_journal(Arc::new(KeptJournal {
        refuse: true,
        ..KeptJournal::default()
    }));
    let outbox = backend.outbox();
    let bsuid = [BSUID.to_owned()];

    // 2. Refused, nothing erased.
    let off = m
        .erasures(backend, outbox.as_ref(), None, &clock)
        .erase(pn, &bsuid)
        .await
        .unwrap_err();
    assert_eq!((off.status(), off.code()), (409, "erasure_not_configured"));
    let unconfirmed = m
        .erasures(backend, outbox.as_ref(), Some(&configured), &clock)
        .erase("106540352242828", &bsuid)
        .await
        .unwrap_err();
    assert_eq!(
        (unconfirmed.status(), unconfirmed.field()),
        (422, Some("confirm"))
    );
    let failed = m
        .erasures(backend, outbox.as_ref(), Some(&refusing), &clock)
        .erase(pn, &bsuid)
        .await
        .unwrap_err();
    assert_eq!(
        (failed.status(), failed.code()),
        (503, "storage_unavailable")
    );
    assert_eq!(threads().await, before, "a refused erasure erased");
    assert_eq!(
        naming(&rows().await, CONTACT),
        2,
        "a refused erasure erased"
    );
    assert!(journal.entries.lock().unwrap().is_empty());

    // 3. By the BSUID alone: every identity the inbox connects.
    let erased = m
        .erasures(backend, outbox.as_ref(), Some(&configured), &clock)
        .erase(pn, &bsuid)
        .await
        .unwrap();
    assert!(erased.erasure_id.starts_with("era_"), "{erased:?}");
    assert_eq!(erased.erased_at, clock.now());
    let entries = journal.entries.lock().unwrap().clone();
    assert_eq!(entries.len(), 1);
    let pn_id = PhoneNumberId::new(pn).unwrap();
    let mut expected = vec![
        key().journal_hmac(&pn_id, BSUID),
        key().journal_hmac(&pn_id, CONTACT),
    ];
    expected.sort();
    assert_eq!(entries[0].identities, expected, "every identity, hashed");
    assert_eq!(entries[0].erasure_id, erased.erasure_id);
    assert_eq!(entries[0].tenant, tenant);
    assert_eq!(entries[0].phone_number_id, pn_id);
    assert!(!format!("{:?}", entries[0]).contains(CONTACT), "in clear");
    assert_eq!(
        threads().await,
        [OTHER.to_owned()],
        "the other customer stays"
    );
    let left = rows().await;
    assert_eq!(naming(&left, CONTACT), 0, "{left:?}");
    assert_eq!(naming(&left, BSUID), 0, "{left:?}");
    assert_eq!(naming(&left, OTHER), 1, "{left:?}");
    for contact in [CONTACT, BSUID] {
        assert!(
            inbox
                .history(contact, &first(50))
                .await
                .unwrap()
                .items
                .is_empty(),
            "{contact}"
        );
    }

    // 4. A repeat: the same answer, nothing more erased.
    let again = m
        .erasures(backend, outbox.as_ref(), Some(&configured), &clock)
        .erase(pn, &bsuid)
        .await
        .unwrap();
    assert_ne!(again.erasure_id, erased.erasure_id);
    assert_eq!(threads().await, [OTHER.to_owned()]);
    assert_eq!(naming(&rows().await, OTHER), 1);
    let entries = journal.entries.lock().unwrap().clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[1].identities,
        [key().journal_hmac(&pn_id, BSUID)],
        "the links went with the first erasure"
    );
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

/// Roadmap M2g: the window as a reply meets it.
///
/// 1. A contact with no conversation: closed, opened never.
/// 2. After the customer's message: open, closing 24 hours after it; the
///    list's window is the same, beside the same summary.
/// 3. 25 hours later: closed, its `closes_at` past, and a free-form reply
///    refused alike; with the number's window check off, open (Meta
///    decides).
/// 4. After `control_taken`: closed while the window is not (another app
///    owns the thread), as the reply's refusal says; with the ownership
///    check off, open.
///
/// Nothing asks Meta. Decisive: the library's `check_reply` behind
/// `open` (the recorded window alone opens it in 4).
pub async fn the_window_is_what_a_reply_would_meet(backend: &dyn Backend) {
    let (waba, pn) = ("102290129340328", "106540352242842");
    let m = merchant(backend, "inbox-window", waba, pn).await;
    let pipeline = pipeline(backend);
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let inbox = m.inbox(backend, ReplyChecks::ALL, &clock);

    // 1. Nothing recorded.
    let never = ReplyWindow {
        open: false,
        closes_at: None,
    };
    assert_eq!(inbox.window(CONTACT).await.unwrap(), never);
    assert_eq!(
        inbox.window("a\nb").await.unwrap_err().field(),
        Some("contact")
    );

    // 2. The customer writes.
    let at = now();
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.WINDOW-IN", "Is it in stock?", at),
    )
    .await;
    let opened = OffsetDateTime::from_unix_timestamp(at).unwrap();
    let closes = opened + time::Duration::hours(24);
    let open = ReplyWindow {
        open: true,
        closes_at: Some(closes),
    };
    assert_eq!(inbox.window(CONTACT).await.unwrap(), open);
    let listed = inbox.list_with_windows(&first(50)).await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].window, open);
    assert_eq!(
        listed.items[0].summary,
        inbox.list(&first(50)).await.unwrap().items[0],
        "the list's summary, unchanged"
    );

    // 3. Past the window.
    let late = ManualClock::new(opened + time::Duration::hours(25));
    let past = m.inbox(backend, ReplyChecks::ALL, &late);
    assert_eq!(
        past.window(CONTACT).await.unwrap(),
        ReplyWindow {
            open: false,
            closes_at: Some(closes),
        }
    );
    assert_eq!(
        past.reply(CONTACT, text_reply("Still there?"))
            .await
            .unwrap_err()
            .code(),
        "customer_service_window_closed",
        "the window and the reply agree"
    );
    assert!(
        m.inbox(backend, ReplyChecks::NONE, &late)
            .window(CONTACT)
            .await
            .unwrap()
            .open,
        "no check: Meta decides"
    );

    // 4. Another app takes the thread, the window still open.
    deliver(&pipeline, &control_taken(waba, pn, at + 1)).await;
    assert_eq!(
        inbox.window(CONTACT).await.unwrap(),
        ReplyWindow {
            open: false,
            closes_at: Some(closes),
        },
        "another app owns the thread"
    );
    assert_eq!(
        inbox
            .reply(CONTACT, text_reply("Let me check"))
            .await
            .unwrap_err()
            .code(),
        "thread_owned_elsewhere",
        "the window and the reply agree"
    );
    assert!(
        !inbox.list_with_windows(&first(50)).await.unwrap().items[0]
            .window
            .open
    );
    assert!(
        m.inbox(backend, ReplyChecks::ALL.thread_owner(false), &clock)
            .window(CONTACT)
            .await
            .unwrap()
            .open,
        "the number's ownership check off"
    );
    assert!(
        m.script.requests().is_empty(),
        "the window asks Meta nothing"
    );
}

/// Meta's answer to a read receipt (`messages/mark-message-as-read`,
/// "Example response").
fn marked() -> Value {
    json!({"success": true})
}

/// Roadmap M2g, decisive: marking a conversation read.
///
/// 1. Without `notify_customer`: `unread` is 0, and Meta is asked nothing.
/// 2. With it: `unread` is 0, and Meta got one read receipt, with the
///    number's token, in Meta's documented shape, for the latest inbound
///    message only (not the business's reply recorded after it, nor an
///    earlier message of the customer's).
/// 3. Meta refusing the receipt (`131009`): `422 invalid_parameter`, and
///    the unread count is kept.
/// 4. Past Meta's 30 days: no receipt, the count reset.
///
/// Decisive: the receipt's guard on `notify_customer`, the latest
/// inbound message's choice, and the store's reset.
pub async fn marking_a_conversation_read_resets_unread_and_receipts_the_latest_inbound_message(
    backend: &dyn Backend,
) {
    let (waba, pn) = ("102290129340318", "106540352242832");
    let m = merchant(backend, "inbox-read", waba, pn).await;
    let pipeline = pipeline(backend);
    let at = now();
    for (i, wamid) in ["wamid.READ-1", "wamid.READ-2"].into_iter().enumerate() {
        deliver(
            &pipeline,
            &text(
                waba,
                pn,
                CONTACT,
                wamid,
                "Hello?",
                at + i64::try_from(i).unwrap(),
            ),
        )
        .await;
    }
    let clock = ManualClock::new(OffsetDateTime::now_utc() + time::Duration::seconds(5));
    let inbox = m.inbox(backend, ReplyChecks::ALL, &clock);
    let unread = async || inbox.list(&first(50)).await.unwrap().items[0].unread;
    assert_eq!(unread().await, 2);
    m.script.push_json(200, accepted("wamid.READ-REPLY"));
    inbox.reply(CONTACT, text_reply("Checking")).await.unwrap();

    // 1. Without notify_customer.
    inbox.mark_read(CONTACT, false).await.unwrap();
    assert_eq!(unread().await, 0);
    assert_eq!(m.script.requests().len(), 1, "no receipt asked");

    // 2. With it: the latest inbound message, the reply newer still.
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.READ-3", "Any news?", at + 2),
    )
    .await;
    assert_eq!(unread().await, 1);
    m.script.push_json(200, marked());
    inbox.mark_read(CONTACT, true).await.unwrap();
    assert_eq!(unread().await, 0);
    let requests = m.script.requests();
    assert_eq!(requests.len(), 2, "one read receipt");
    let receipt = &requests[1];
    assert_eq!(receipt.method.as_str(), "POST");
    assert_eq!(receipt.path(), format!("/v25.0/{pn}/messages"));
    assert_eq!(receipt.bearer(), Some(TOKEN), "the number's token");
    assert_eq!(
        receipt.json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": "wamid.READ-3"
        }),
        "the latest inbound message only"
    );

    // 3. Meta refuses: nothing reset.
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.READ-4", "Hello??", at + 3),
    )
    .await;
    m.script.push_json(
        400,
        json!({"error": {"message": "(#131009) Parameter value is not valid",
                         "type": "OAuthException", "code": 131009,
                         "fbtrace_id": "AbCdEfReadReceipt"}}),
    );
    let refused = inbox.mark_read(CONTACT, true).await.unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (422, "invalid_parameter")
    );
    assert_eq!(unread().await, 1, "a refused receipt resets nothing");
    assert_eq!(m.script.requests().len(), 3);

    // 4. Too old for a receipt: the count reset all the same.
    let later = ManualClock::new(OffsetDateTime::now_utc() + time::Duration::days(31));
    m.inbox(backend, ReplyChecks::ALL, &later)
        .mark_read(CONTACT, true)
        .await
        .unwrap();
    assert_eq!(unread().await, 0);
    assert_eq!(m.script.requests().len(), 3, "no receipt past 30 days");
    assert_eq!(m.script.remaining(), 0);
}

/// Roadmap M2g's review: the read receipt names the customer's latest
/// received message, wherever it is among the conversation's latest
/// `UNREAD_SCAN` messages, and while Meta still takes it.
///
/// 1. A revoke's tombstone newer than the customer's text (a revoke that
///    came before its message: never received) is skipped, and 150 sent
///    messages newer still put the text on the history's second page: the
///    receipt names the text, read 30 days to the second after it was sent
///    (Meta's "within 30 days of receipt", the bound included).
/// 2. One second later: no receipt, the count reset all the same.
pub async fn a_read_receipt_skips_a_tombstone_and_reads_past_the_first_page(backend: &dyn Backend) {
    let (waba, pn) = ("102290129340319", "106540352242833");
    let m = merchant(backend, "inbox-read-scan", waba, pn).await;
    let pipeline = pipeline(backend);
    let at = now();
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.SCAN-TEXT", "Are you open?", at),
    )
    .await;
    let store = backend.conversations();
    let key = ConversationKey::new(PhoneNumberId::new(pn).unwrap(), CONTACT);
    let sent_at = OffsetDateTime::from_unix_timestamp(at).unwrap();
    assert!(
        store
            .revoke(
                &key,
                &MessageId::new("wamid.SCAN-NEVER-RECEIVED"),
                Direction::Inbound,
                sent_at + time::Duration::seconds(1),
            )
            .await
            .unwrap(),
        "a tombstone"
    );
    for i in 0..150_i64 {
        let stored = store
            .append(StoredMessage {
                id: MessageId::new(format!("wamid.SCAN-SENT-{i}")),
                conversation: key.clone(),
                direction: Direction::Outbound,
                kind: "text".to_owned(),
                text: Some("We are".to_owned()),
                payload: json!({"type": "text", "text": {"body": "We are"}}),
                status: DeliveryStatus::Accepted,
                timestamp: sent_at + time::Duration::seconds(2 + i),
                status_at: None,
                error: None,
            })
            .await
            .unwrap();
        assert!(stored);
    }

    // 1. Past the tombstone and the first page, at the 30 days' bound.
    let bound = ManualClock::new(sent_at + READ_RECEIPT_MAX_AGE);
    let reader = m.inbox(backend, ReplyChecks::ALL, &bound);
    m.script.push_json(200, marked());
    reader.mark_read(CONTACT, true).await.unwrap();
    let [receipt] = m.script.requests().try_into().unwrap();
    assert_eq!(receipt.path(), format!("/v25.0/{pn}/messages"));
    assert_eq!(receipt.bearer(), Some(TOKEN));
    assert_eq!(
        receipt.json().unwrap(),
        json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": "wamid.SCAN-TEXT"
        }),
        "the customer's latest received message, not the tombstone"
    );

    // 2. A second past the bound: none.
    let past = ManualClock::new(sent_at + READ_RECEIPT_MAX_AGE + time::Duration::seconds(1));
    m.inbox(backend, ReplyChecks::ALL, &past)
        .mark_read(CONTACT, true)
        .await
        .unwrap();
    assert_eq!(m.script.requests().len(), 1, "no receipt past 30 days");
    assert_eq!(m.script.remaining(), 0);
}

/// Meta's script behind a WABA that leaves its tenant (unbound) while
/// the first request is on its way: a move during a read receipt.
struct UnbindingOnSend {
    script: ScriptedTransport,
    records: Arc<dyn RecordStore>,
    waba: WabaId,
    unbound: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for UnbindingOnSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnbindingOnSend").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl HttpTransport for UnbindingOnSend {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if !self.unbound.swap(true, std::sync::atomic::Ordering::SeqCst) {
            assert!(self.records.unbind_waba(&self.waba).await.unwrap());
        }
        self.script.send(request).await
    }
}

/// Roadmap M2g's review: a number that leaves its tenant while Meta
/// answers the read receipt resets nothing (`503 storage_unavailable`):
/// the conversation's count is no longer this tenant's to reset.
pub async fn a_number_that_moves_during_the_receipt_resets_nothing(backend: &dyn Backend) {
    let (waba, pn) = ("102290129340320", "106540352242834");
    let records = backend.records();
    let m = merchant_over(backend, "inbox-read-moving", waba, pn, |script| {
        Arc::new(UnbindingOnSend {
            script,
            records: Arc::clone(&records),
            waba: WabaId::new(waba).unwrap(),
            unbound: std::sync::atomic::AtomicBool::new(false),
        })
    })
    .await;
    deliver(
        &pipeline(backend),
        &text(waba, pn, CONTACT, "wamid.MOVING-1", "Hello?", now()),
    )
    .await;
    let unread = async || {
        backend
            .conversations()
            .conversations(&PhoneNumberId::new(pn).unwrap(), None, 10)
            .await
            .unwrap()[0]
            .unread
    };
    assert_eq!(unread().await, 1);
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    m.script.push_json(200, marked());
    let moved = m
        .inbox(backend, ReplyChecks::ALL, &clock)
        .mark_read(CONTACT, true)
        .await
        .unwrap_err();
    assert_eq!((moved.status(), moved.code()), (503, "storage_unavailable"));
    let [receipt] = m.script.requests().try_into().unwrap();
    assert_eq!(receipt.json().unwrap()["message_id"], "wamid.MOVING-1");
    assert_eq!(unread().await, 1, "reset after the number moved");
    assert_eq!(m.script.remaining(), 0);
}

/// Roadmap M2g: a message by id, on its own number only. A received
/// message and a sent reply are found, with their conversation; another
/// number's message (the same tenant's), an unknown id, and a malformed one
/// are not. Nothing asks Meta.
pub async fn a_message_is_found_by_id_on_its_own_number(backend: &dyn Backend) {
    let (waba, pn, other_waba, other_pn) = (
        "102290129340308",
        "106540352242822",
        "102290129340307",
        "106540352242821",
    );
    let m = merchant(backend, "inbox-by-id", waba, pn).await;
    let other = merchant(backend, "inbox-by-id", other_waba, other_pn).await;
    let pipeline = pipeline(backend);
    let at = now();
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.BY-ID-1", "Where is it?", at),
    )
    .await;
    deliver(
        &pipeline,
        &text(other_waba, other_pn, CONTACT, "wamid.BY-ID-2", "Hi", at),
    )
    .await;
    let clock = ManualClock::new(OffsetDateTime::now_utc() + time::Duration::seconds(2));
    let inbox = m.inbox(backend, ReplyChecks::ALL, &clock);
    let found = inbox.message("wamid.BY-ID-1").await.unwrap();
    assert_eq!(found.id.as_str(), "wamid.BY-ID-1");
    assert_eq!(found.conversation.contact, CONTACT);
    assert_eq!(found.direction, Direction::Inbound);
    assert_eq!(found.text.as_deref(), Some("Where is it?"));
    m.script.push_json(200, accepted("wamid.BY-ID-REPLY"));
    inbox.reply(CONTACT, text_reply("Tomorrow")).await.unwrap();
    let sent = inbox.message("wamid.BY-ID-REPLY").await.unwrap();
    assert_eq!(
        (sent.direction, sent.status),
        (Direction::Outbound, DeliveryStatus::Accepted)
    );
    for missing in ["wamid.BY-ID-2", "wamid.NOWHERE"] {
        let refused = inbox.message(missing).await.unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (404, "not_found"),
            "{missing}"
        );
    }
    assert!(
        other
            .inbox(backend, ReplyChecks::ALL, &clock)
            .message("wamid.BY-ID-2")
            .await
            .is_ok(),
        "its own number's"
    );
    for bad in ["", " ", "a\u{0}b"] {
        assert_eq!(
            inbox.message(bad).await.unwrap_err().field(),
            Some("message_id"),
            "{bad:?}"
        );
    }
    let long = "w".repeat(crate::inbox::MAX_MESSAGE_ID_BYTES + 1);
    assert_eq!(
        inbox.message(&long).await.unwrap_err().field(),
        Some("message_id")
    );
    assert_eq!(m.script.requests().len(), 1, "the lookups ask Meta nothing");
    assert!(other.script.requests().is_empty());
}

/// Roadmap M2g, decisive: after a move of the number's WABA to another
/// tenant, the previous holder's message is `404` by id to the new holder
/// (while its own capability found it), its customer's window shows the
/// new holder neither open nor any time, and marking the conversation read
/// sends no receipt for the previous holder's message; once the customer
/// writes to the new holder, its message is found, its window open, and
/// its receipt sent. Decisive: the cut in each lookup (`Conversations::
/// message`'s, the window's `closes_at` and `open`, the receipt's).
pub async fn the_previous_holders_message_window_and_receipt_never_reach_the_new_holder(
    backend: &dyn Backend,
) {
    let (waba, pn) = ("102290129340298", "106540352242812");
    let pipeline = pipeline(backend);
    let a = merchant(backend, "inbox-epoch2-a", waba, pn).await;
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    deliver(
        &pipeline,
        &text(
            waba,
            pn,
            CONTACT,
            "wamid.EPOCH2-A",
            "My address is 1 Hacker Way",
            now(),
        ),
    )
    .await;
    let theirs = a.inbox(backend, ReplyChecks::ALL, &clock);
    assert!(theirs.message("wamid.EPOCH2-A").await.is_ok(), "A's own");
    assert!(theirs.window(CONTACT).await.unwrap().open, "A's window");
    drop(theirs);

    // The move.
    let records = backend.records();
    assert!(
        records
            .unbind_waba(&WabaId::new(waba).unwrap())
            .await
            .unwrap()
    );
    let mut b = merchant(backend, "inbox-epoch2-b", waba, pn).await;
    let inbox = b.inbox(backend, ReplyChecks::ALL, &clock);
    assert!(inbox.history_from().is_some(), "a cut");
    let refused = inbox.message("wamid.EPOCH2-A").await.unwrap_err();
    assert_eq!(
        (refused.status(), refused.code()),
        (404, "not_found"),
        "the previous holder's message, shown to the new one"
    );
    assert_eq!(
        inbox.window(CONTACT).await.unwrap(),
        ReplyWindow {
            open: false,
            closes_at: None,
        },
        "the previous holder's customer's window, shown to the new one"
    );
    assert!(
        inbox
            .list_with_windows(&first(50))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    inbox.mark_read(CONTACT, true).await.unwrap();
    assert!(
        b.script.requests().is_empty(),
        "a receipt for the previous holder's message, sent with the new holder's token"
    );
    drop(inbox);

    // The customer writes to B.
    let at = now() + 1;
    deliver(
        &pipeline,
        &text(waba, pn, CONTACT, "wamid.EPOCH2-B", "Hello again", at),
    )
    .await;
    b.reopen(pn).await;
    let inbox = b.inbox(backend, ReplyChecks::ALL, &clock);
    assert_eq!(
        inbox.message("wamid.EPOCH2-B").await.unwrap().id.as_str(),
        "wamid.EPOCH2-B"
    );
    let closes = OffsetDateTime::from_unix_timestamp(at).unwrap() + time::Duration::hours(24);
    assert_eq!(
        inbox.window(CONTACT).await.unwrap(),
        ReplyWindow {
            open: true,
            closes_at: Some(closes),
        }
    );
    b.script.push_json(200, marked());
    inbox.mark_read(CONTACT, true).await.unwrap();
    let [receipt] = b.script.requests().try_into().unwrap();
    assert_eq!(receipt.json().unwrap()["message_id"], "wamid.EPOCH2-B");
    assert_eq!(b.script.remaining(), 0);
    assert!(
        inbox.message("wamid.EPOCH2-A").await.is_err(),
        "still hidden"
    );
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
