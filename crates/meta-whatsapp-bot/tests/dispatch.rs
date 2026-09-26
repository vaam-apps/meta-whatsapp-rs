//! Dispatch over Meta's documented webhook fixtures: commands, aliases,
//! payloads, guards (ban, scope, owner, cooldown), middleware, listeners,
//! plugins, errors, and the exact requests replies make.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_bot::{
    AccessList, Bot, Command, Ctx, ErrorHandler, Listen, MarkRead, Middleware, Next, Plugin,
    PropagateErrors, Refusal, Refusals, Registrar, ReplyRefusals, SilentRefusals, Trigger,
};
use meta_whatsapp_core::clock::ManualClock;
use meta_whatsapp_core::error::{SinkError, StorageError};
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_core::testing::ScriptedTransport;
use meta_whatsapp_core::{Error, ErrorKind};
use pretty_assertions::assert_eq;
use serde_json::json;
use time::macros::datetime;

use common::{NUMBER, Recording, client, event, message_id, send_response, text_event};

const TEXT: &str = "messages/text.json";
const BSUID_ONLY: &str = "bsuid/text_username_no_wa_id.json";
const GROUP: &str = "messages/group_text.json";
const MENU_TAP: &str =
    "pages/business-phone-numbers.conversational-components__webhook_payload_2.json";
const BSUID: &str = "US.13491208655302741918";
const GROUP_ID: &str = "HBgLMTY1MDM4Nzk0MzkVAgASGBQzQTRBNjU5OUFFRTAzODEwMTQ0RgA";

/// A command that counts its runs and records its invocation.
fn counting(name: &str, runs: &Arc<AtomicUsize>) -> Command {
    let runs = Arc::clone(runs);
    Command::new(name, move |_ctx: Ctx| {
        let runs = Arc::clone(&runs);
        async move {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
}

fn count(runs: &Arc<AtomicUsize>) -> usize {
    runs.load(Ordering::SeqCst)
}

// ─── Commands and replies ────────────────────────────────────────────────

/// The conversational-components page's own example: a user picks
/// `/imagine` in Meta's command menu and types a prompt. The reply is the
/// exact documented send request, quoting the message.
#[tokio::test]
async fn a_menu_command_runs_and_its_reply_quotes_the_message() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    let seen = Arc::new(Mutex::new(None));
    let seen_in = Arc::clone(&seen);
    let bot = Bot::builder()
        .client(client(&t))
        .command(Command::new("imagine", move |ctx: Ctx| {
            let seen = Arc::clone(&seen_in);
            async move {
                *seen.lock().unwrap() = ctx.invocation().cloned();
                let prompt = ctx.args().as_slice().join(" ");
                ctx.reply(format!("Imagining: {prompt}")).await?;
                Ok(())
            }
        }))
        .build()
        .await
        .unwrap();

    bot.deliver(event(MENU_TAP)).await.unwrap();

    let invocation = seen.lock().unwrap().clone().unwrap();
    assert_eq!(invocation.command, "imagine");
    assert_eq!(
        invocation.trigger,
        Trigger::Text {
            prefix: "/".into(),
            name: "imagine".into()
        }
    );
    assert_eq!(invocation.args.as_slice(), ["cars", "racing", "on", "Mars"]);
    let request = t.last_request().unwrap();
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.path(), format!("/v25.0/{NUMBER}/messages"));
    assert_eq!(request.bearer(), Some("TOKEN"));
    assert_eq!(
        request.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "context": {"message_id": message_id(MENU_TAP)},
            "type": "text",
            "text": {"body": "Imagining: cars racing on Mars"}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn names_and_aliases_are_case_insensitive_under_every_prefix() {
    let runs = Arc::new(AtomicUsize::new(0));
    let bot = Bot::builder()
        .outbound(Recording::default())
        .prefixes(["/", "!"])
        .command(counting("Ping", &runs).alias("P"))
        .build()
        .await
        .unwrap();
    for body in ["/ping", "!PING", "  /p now", "!P", "/PiNg \"a b\""] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(count(&runs), 5);
    // Not commands: another prefix, no name, an unknown name, plain text.
    for body in [
        "#ping",
        "/",
        "/ pong",
        "/pingx",
        "ping",
        "Does it come in another color?",
    ] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(count(&runs), 5);
    let info = &bot.commands()[0];
    assert_eq!(
        (info.name.as_str(), info.aliases.as_slice()),
        ("ping", &["p".to_owned()][..])
    );
}

/// A user without a phone number (a username adopter): the BSUID keys
/// them and the reply goes to it, with no `to`.
#[tokio::test]
async fn a_bsuid_only_sender_is_keyed_and_answered_by_bsuid() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    let key = Arc::new(Mutex::new(None));
    let key_in = Arc::clone(&key);
    let bot = Bot::builder()
        .client(client(&t))
        .command(Command::new("whoami", move |ctx: Ctx| {
            let key = Arc::clone(&key_in);
            async move {
                let sender = ctx.sender().unwrap();
                assert!(sender.wa_id.is_none());
                *key.lock().unwrap() = sender.key().map(str::to_owned);
                ctx.reply("You are you.").await?;
                Ok(())
            }
        }))
        .build()
        .await
        .unwrap();

    bot.deliver(text_event(BSUID_ONLY, "/whoami"))
        .await
        .unwrap();

    assert_eq!(key.lock().unwrap().as_deref(), Some(BSUID));
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "recipient": BSUID,
            "context": {"message_id": message_id(BSUID_ONLY)},
            "type": "text",
            "text": {"body": "You are you."}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

/// Meta's BSUID example with both identities (its `group_id` removed: a
/// private chat): the BSUID keys the user and receives the reply, and
/// the phone number changes nothing.
#[tokio::test]
async fn with_both_identities_the_bsuid_keys_the_user_and_gets_the_reply() {
    const BOTH: &str = "pages/business-scoped-user-ids__incoming_messages_webhooks.json";
    let message = |user: &str, phone: &str| {
        let mut json = common::fixture_json(BOTH);
        let value = &mut json["entry"][0]["changes"][0]["value"];
        value["contacts"][0]["user_id"] = json!(user);
        value["contacts"][0]["wa_id"] = json!(phone);
        let m = &mut value["messages"][0];
        m.as_object_mut().unwrap().remove("group_id");
        m["from_user_id"] = json!(user);
        m["from"] = json!(phone);
        m["text"]["body"] = json!("/roll");
        common::events_of(&json).remove(0)
    };
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let keys = Arc::new(Mutex::new(Vec::new()));
    let keys_in = Arc::clone(&keys);
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .refusals(SilentRefusals)
        .cooldown_store(kv, Arc::new(clock))
        .command(
            Command::new("roll", move |ctx: Ctx| {
                let keys = Arc::clone(&keys_in);
                async move {
                    keys.lock()
                        .unwrap()
                        .push(ctx.sender().and_then(|s| s.key()).map(str::to_owned));
                    ctx.reply("4").await?;
                    Ok(())
                }
            })
            .cooldown(Duration::from_secs(60)),
        )
        .build()
        .await
        .unwrap();

    bot.handle(message(BSUID, "16505551234")).await.unwrap();
    // The same user on a new phone number: still cooling down.
    bot.handle(message(BSUID, "12125557890")).await.unwrap();
    // Another user who got the old number: not cooling down.
    bot.handle(message("US.10000000000000000001", "16505551234"))
        .await
        .unwrap();

    assert_eq!(
        *keys.lock().unwrap(),
        [
            Some(BSUID.to_owned()),
            Some("US.10000000000000000001".to_owned())
        ]
    );
    let sent = out.sent();
    assert_eq!(sent[0]["recipient"], BSUID);
    assert!(sent[0].get("to").is_none(), "{}", sent[0]);
    assert_eq!(sent.len(), 2);
}

/// Meta's group message example: the reply goes to the group.
#[tokio::test]
async fn a_reply_to_a_group_message_goes_to_the_group() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    let bot = Bot::builder()
        .client(client(&t))
        .command(Command::new("hi", |ctx: Ctx| async move {
            ctx.reply("Hello, group").await?;
            Ok(())
        }))
        .build()
        .await
        .unwrap();
    bot.deliver(text_event(GROUP, "/hi")).await.unwrap();
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "group",
            "to": GROUP_ID,
            "context": {"message_id": message_id(GROUP)},
            "type": "text",
            "text": {"body": "Hello, group"}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

/// Reply buttons, list rows and template quick-reply buttons whose id is a
/// registered payload run their command; other ids are plain messages.
#[tokio::test]
async fn interactive_replies_with_a_registered_payload_run_their_command() {
    let runs = Arc::new(AtomicUsize::new(0));
    let triggers = Arc::new(Mutex::new(Vec::new()));
    let triggers_in = Arc::clone(&triggers);
    let plain = Arc::new(AtomicUsize::new(0));
    let plain_in = Arc::clone(&plain);
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(
            Command::new("cancel", move |ctx: Ctx| {
                let triggers = Arc::clone(&triggers_in);
                async move {
                    triggers
                        .lock()
                        .unwrap()
                        .push(ctx.invocation().unwrap().trigger.clone());
                    Ok(())
                }
            })
            .payload("cancel-button")
            .payload("priority_express"),
        )
        .command(counting("unsubscribe", &runs).payload("Unsubscribe"))
        .listen(Listen::Messages, move |_ctx: Ctx| {
            let plain = Arc::clone(&plain_in);
            async move {
                plain.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();

    bot.handle(event("messages/interactive_button_reply.json"))
        .await
        .unwrap();
    bot.handle(event("messages/interactive_list_reply.json"))
        .await
        .unwrap();
    bot.handle(event("messages/button.json")).await.unwrap();
    assert_eq!(
        *triggers.lock().unwrap(),
        [
            Trigger::Payload("cancel-button".into()),
            Trigger::Payload("priority_express".into())
        ]
    );
    assert_eq!(count(&runs), 1);
    assert_eq!(count(&plain), 0);

    // An id nobody registered: a plain message.
    let mut json = common::fixture_json("messages/interactive_button_reply.json");
    json["entry"][0]["changes"][0]["value"]["messages"][0]["interactive"]["button_reply"]["id"] =
        json!("something-else");
    for e in common::events_of(&json) {
        bot.handle(e).await.unwrap();
    }
    assert_eq!(count(&plain), 1);
}

/// A tap goes through the same guards as typed text: a payload is no way
/// around an owner-only or group-only command, or a ban; and typed text
/// equal to a payload is plain text, not a tap.
#[tokio::test]
async fn payload_triggers_pass_the_same_guards() {
    let wipe_runs = Arc::new(AtomicUsize::new(0));
    let poll_runs = Arc::new(AtomicUsize::new(0));
    let menu_runs = Arc::new(AtomicUsize::new(0));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .access(AccessList::new().owner("US.10000000000000000001"))
        .command(
            counting("wipe", &wipe_runs)
                .owner_only()
                .payload("cancel-button"),
        )
        .command(
            counting("poll", &poll_runs)
                .group_only()
                .payload("priority_express"),
        )
        .command(counting("menu", &menu_runs).payload("Unsubscribe"))
        .build()
        .await
        .unwrap();

    // Meta's reply button (`cancel-button`) and list row
    // (`priority_express`) examples, from a private chat and not an owner.
    bot.handle(event("messages/interactive_button_reply.json"))
        .await
        .unwrap();
    bot.handle(event("messages/interactive_list_reply.json"))
        .await
        .unwrap();
    assert_eq!((count(&wipe_runs), count(&poll_runs)), (0, 0));
    assert_eq!(out.bodies(), ["This command only works in a group."]);

    // The payload typed as text: not a tap.
    for body in ["cancel-button", "Unsubscribe", "priority_express"] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(count(&menu_runs), 0);
    // The tap itself does run it.
    bot.handle(event("messages/button.json")).await.unwrap();
    assert_eq!(count(&menu_runs), 1);

    // A banned sender's tap runs nothing.
    let banned = Bot::builder()
        .outbound(Recording::default())
        .access(AccessList::new().ban_phone("+1 650 555 1234"))
        .command(counting("menu", &menu_runs).payload("Unsubscribe"))
        .build()
        .await
        .unwrap();
    banned.handle(event("messages/button.json")).await.unwrap();
    assert_eq!(count(&menu_runs), 1);
}

// ─── Guards ──────────────────────────────────────────────────────────────

/// Decisive: remove the cooldown check and the second run goes through.
#[tokio::test]
async fn a_cooldown_refuses_a_second_run_until_it_expires() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Arc::new(AtomicUsize::new(0));
    let other = Arc::new(AtomicUsize::new(0));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .cooldown_store(kv, Arc::new(clock.clone()))
        .command(counting("roll", &runs).cooldown(Duration::from_secs(30)))
        .command(counting("other", &other).cooldown(Duration::from_secs(30)))
        .build()
        .await
        .unwrap();

    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    assert_eq!(count(&runs), 1);
    clock.advance(Duration::from_secs(10));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    assert_eq!(count(&runs), 1, "ran inside its cooldown");
    // The default refusal tells the user how long to wait.
    assert_eq!(
        out.bodies(),
        ["Please wait 20 s before using this command again."]
    );
    // Per command and per user: another command, another user, run.
    bot.handle(text_event(TEXT, "/other")).await.unwrap();
    bot.handle(text_event(BSUID_ONLY, "/roll")).await.unwrap();
    assert_eq!((count(&runs), count(&other)), (2, 1));
    // Expired: it runs again.
    clock.advance(Duration::from_secs(21));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    assert_eq!(count(&runs), 3);
}

/// The cooldown is the last guard: an attempt another guard refused
/// starts none.
#[tokio::test]
async fn a_refused_attempt_starts_no_cooldown() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Arc::new(AtomicUsize::new(0));
    let bot = Bot::builder()
        .outbound(Recording::default())
        .cooldown_store(kv, Arc::new(clock))
        .command(
            counting("poll", &runs)
                .group_only()
                .cooldown(Duration::from_secs(60)),
        )
        .build()
        .await
        .unwrap();
    // Refused (a private chat), then from the same user in the group.
    let mut json = common::fixture_json(GROUP);
    json["entry"][0]["changes"][0]["value"]["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("group_id");
    json["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = json!("/poll");
    for e in common::events_of(&json) {
        bot.handle(e).await.unwrap();
    }
    bot.handle(text_event(GROUP, "/poll")).await.unwrap();
    assert_eq!(count(&runs), 1);
}

/// Decisive: remove the ban check and the banned user's command runs.
#[tokio::test]
async fn a_banned_sender_runs_nothing() {
    let runs = Arc::new(AtomicUsize::new(0));
    let listened = Arc::new(AtomicUsize::new(0));
    let listened_in = Arc::clone(&listened);
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .access(AccessList::new().ban(BSUID).ban_phone("+1 212 555 7890"))
        .command(counting("ping", &runs))
        .listen(Listen::All, move |_ctx: Ctx| {
            let listened = Arc::clone(&listened_in);
            async move {
                listened.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();

    bot.handle(text_event(BSUID_ONLY, "/ping")).await.unwrap();
    bot.handle(text_event(BSUID_ONLY, "hello")).await.unwrap();
    assert_eq!((count(&runs), count(&listened)), (0, 0));
    assert!(out.sent().is_empty(), "a ban is silent");

    // Banned by phone number.
    let mut json = common::fixture_json(TEXT);
    json["entry"][0]["changes"][0]["value"]["contacts"][0]["wa_id"] = json!("12125557890");
    json["entry"][0]["changes"][0]["value"]["messages"][0]["from"] = json!("12125557890");
    json["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = json!("/ping");
    for e in common::events_of(&json) {
        bot.handle(e).await.unwrap();
    }
    assert_eq!(count(&runs), 0);

    // Everyone else.
    bot.handle(text_event(TEXT, "/ping")).await.unwrap();
    bot.handle(text_event(TEXT, "hello")).await.unwrap();
    assert_eq!((count(&runs), count(&listened)), (1, 1));
}

/// The sender's identities are completed from the change's contact, so a
/// ban matches whichever the message itself leaves out: a BSUID only in
/// `contacts[].user_id`, a phone number only in `contacts[].wa_id`.
#[tokio::test]
async fn a_ban_matches_identities_only_the_contact_carries() {
    let runs = Arc::new(AtomicUsize::new(0));
    let bot = Bot::builder()
        .outbound(Recording::default())
        .access(
            AccessList::new()
                .ban("US.10000000000000000001")
                .ban_phone("+1 212 555 7890"),
        )
        .command(counting("ping", &runs))
        .build()
        .await
        .unwrap();

    // `from` only on the message; the BSUID is in the contact.
    let mut json = common::fixture_json(TEXT);
    let value = &mut json["entry"][0]["changes"][0]["value"];
    value["contacts"][0]["user_id"] = json!("US.10000000000000000001");
    value["messages"][0]["text"]["body"] = json!("/ping");
    let [by_contact_bsuid] = common::events_of(&json).try_into().unwrap();
    assert!(
        by_contact_bsuid
            .contact()
            .is_some_and(|c| c.user_id.is_some())
    );
    bot.handle(by_contact_bsuid).await.unwrap();

    // `from_user_id` only on the message; the phone number is in the contact.
    let mut json = common::fixture_json(BSUID_ONLY);
    let value = &mut json["entry"][0]["changes"][0]["value"];
    value["contacts"][0]["wa_id"] = json!("12125557890");
    value["messages"][0]["text"]["body"] = json!("/ping");
    let [by_contact_phone] = common::events_of(&json).try_into().unwrap();
    bot.handle(by_contact_phone).await.unwrap();

    assert_eq!(count(&runs), 0);
    // Control: the unmodified senders are not banned.
    bot.handle(text_event(TEXT, "/ping")).await.unwrap();
    bot.handle(text_event(BSUID_ONLY, "/ping")).await.unwrap();
    assert_eq!(count(&runs), 2);
}

/// The wait the default refusal quotes is rounded up to whole seconds.
#[tokio::test]
async fn the_quoted_wait_is_rounded_up() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .cooldown_store(kv, Arc::new(clock.clone()))
        .command(counting("roll", &Arc::default()).cooldown(Duration::from_secs(30)))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    clock.advance(Duration::from_millis(10_500));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    // A new cooldown (one notice per cooldown), refused 0.6 s before its end.
    clock.advance(Duration::from_millis(19_600));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    clock.advance(Duration::from_millis(29_400));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    assert_eq!(
        out.bodies(),
        [
            "Please wait 20 s before using this command again.",
            "Please wait 1 s before using this command again."
        ]
    );
}

/// A `KvStore` whose first `put_if_absent` finds a record that has expired
/// by the time `get` reads it: the race `KvCooldowns` retries once.
#[derive(Debug)]
struct ExpiresMidway {
    inner: MemoryKvStore,
    raced: AtomicUsize,
}

#[async_trait]
impl KvStore for ExpiresMidway {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        if self.raced.load(Ordering::SeqCst) == 1 {
            return Ok(None);
        }
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        if self.raced.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(None);
        }
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// A record that expired between the refused create and the read is gone:
/// the cooldown starts and the command runs.
#[tokio::test]
async fn a_cooldown_that_expires_mid_check_starts_again() {
    let runs = Arc::new(AtomicUsize::new(0));
    let kv = Arc::new(ExpiresMidway {
        inner: MemoryKvStore::new(),
        raced: AtomicUsize::new(0),
    });
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .cooldown_store(kv, Arc::new(meta_whatsapp_core::clock::SystemClock))
        .command(counting("roll", &runs).cooldown(Duration::from_secs(30)))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    assert_eq!(count(&runs), 1);
    assert!(out.sent().is_empty(), "{:?}", out.bodies());
}

/// Decisive: remove either scope check and the refused command runs.
#[tokio::test]
async fn group_only_and_private_only_commands_run_only_there() {
    let group_runs = Arc::new(AtomicUsize::new(0));
    let private_runs = Arc::new(AtomicUsize::new(0));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .command(counting("poll", &group_runs).group_only())
        .command(counting("account", &private_runs).private_only())
        .build()
        .await
        .unwrap();

    bot.handle(text_event(GROUP, "/poll")).await.unwrap();
    bot.handle(text_event(TEXT, "/account")).await.unwrap();
    assert_eq!((count(&group_runs), count(&private_runs)), (1, 1));

    bot.handle(text_event(TEXT, "/poll")).await.unwrap();
    bot.handle(text_event(GROUP, "/account")).await.unwrap();
    assert_eq!((count(&group_runs), count(&private_runs)), (1, 1));

    let sent = out.sent();
    assert_eq!(
        sent[0],
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "context": {"message_id": message_id(TEXT)},
            "type": "text",
            "text": {"body": "This command only works in a group."}
        })
    );
    assert_eq!(sent[1]["recipient_type"], "group");
    assert_eq!(
        sent[1]["text"]["body"],
        "This command only works in a private chat."
    );
    assert_eq!(sent.len(), 2);
}

#[tokio::test]
async fn owner_only_commands_run_for_owners_only_and_refuse_silently() {
    let runs = Arc::new(AtomicUsize::new(0));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .access(AccessList::new().owner(BSUID))
        .command(counting("shutdown", &runs).owner_only())
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/shutdown")).await.unwrap();
    assert_eq!(count(&runs), 0);
    assert!(out.sent().is_empty());
    bot.handle(text_event(BSUID_ONLY, "/shutdown"))
        .await
        .unwrap();
    assert_eq!(count(&runs), 1);
}

#[tokio::test]
async fn silent_refusals_send_nothing() {
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .refusals(SilentRefusals)
        .command(counting("poll", &Arc::default()).group_only())
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/poll")).await.unwrap();
    assert!(out.sent().is_empty());
}

// ─── Middleware and listeners ────────────────────────────────────────────

#[derive(Debug)]
struct Trace(&'static str, Arc<Mutex<Vec<&'static str>>>, bool);

#[async_trait]
impl Middleware for Trace {
    async fn handle(&self, ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
        self.1.lock().unwrap().push(self.0);
        if self.2 { next.run(ctx).await } else { Ok(()) }
    }
}

/// A banned sender's message stops before the middleware: no read
/// receipt, no "typing…", nothing an integrator's middleware does.
#[tokio::test]
async fn a_banned_sender_gets_no_middleware_and_no_read_receipt() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .access(AccessList::new().ban(BSUID))
        .middleware(MarkRead::with_typing_indicator())
        .middleware(Trace("integrator", Arc::clone(&order), true))
        .command(counting("ping", &Arc::default()))
        .build()
        .await
        .unwrap();

    bot.handle(text_event(BSUID_ONLY, "/ping")).await.unwrap();
    bot.handle(text_event(BSUID_ONLY, "hello")).await.unwrap();
    assert!(order.lock().unwrap().is_empty());
    assert!(out.reads.lock().unwrap().is_empty());
    assert!(out.sent().is_empty());

    // Everyone else goes through them.
    bot.handle(text_event(TEXT, "hello")).await.unwrap();
    assert_eq!(*order.lock().unwrap(), ["integrator"]);
    assert_eq!(out.reads.lock().unwrap().len(), 1);
}

/// The ban is about what a sender sends: the status of a message the
/// business sent to a banned user still reaches its listeners (delivery
/// tracking keeps working).
#[tokio::test]
async fn a_ban_leaves_other_events_about_the_user_alone() {
    let statuses = Arc::new(AtomicUsize::new(0));
    let statuses_in = Arc::clone(&statuses);
    let bot = Bot::builder()
        .outbound(Recording::default())
        .access(AccessList::new().ban(BSUID))
        .listen(Listen::event("status_updated"), move |ctx: Ctx| {
            let statuses = Arc::clone(&statuses_in);
            async move {
                assert!(ctx.sender().is_some());
                statuses.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();
    let status = event("bsuid/status_delivered_bsuid_only.json");
    assert!(
        status
            .contact()
            .and_then(|c| c.user_id.as_ref())
            .is_some_and(|user| user.as_str() == BSUID)
    );
    bot.handle(status).await.unwrap();
    assert_eq!(count(&statuses), 1);
}

/// Decisive: a middleware that does not call `next` stops the event.
#[tokio::test]
async fn a_middleware_that_does_not_call_next_stops_the_event() {
    let runs = Arc::new(AtomicUsize::new(0));
    let listened = Arc::new(AtomicUsize::new(0));
    let listened_in = Arc::clone(&listened);
    let order = Arc::new(Mutex::new(Vec::new()));
    let build = |pass: bool| {
        Bot::builder()
            .outbound(Recording::default())
            .middleware(Trace("first", Arc::clone(&order), true))
            .middleware(Trace("gate", Arc::clone(&order), pass))
            .middleware(Trace("last", Arc::clone(&order), true))
            .command(counting("ping", &runs))
            .listen(Listen::All, {
                let listened = Arc::clone(&listened_in);
                move |_ctx: Ctx| {
                    let listened = Arc::clone(&listened);
                    async move {
                        listened.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            })
            .build()
    };

    let stopped = build(false).await.unwrap();
    stopped.handle(text_event(TEXT, "/ping")).await.unwrap();
    stopped.handle(text_event(TEXT, "hello")).await.unwrap();
    assert_eq!((count(&runs), count(&listened)), (0, 0));
    assert_eq!(*order.lock().unwrap(), ["first", "gate", "first", "gate"]);

    order.lock().unwrap().clear();
    let open = build(true).await.unwrap();
    open.handle(text_event(TEXT, "/ping")).await.unwrap();
    open.handle(text_event(TEXT, "hello")).await.unwrap();
    assert_eq!((count(&runs), count(&listened)), (1, 1));
    assert_eq!(
        *order.lock().unwrap(),
        ["first", "gate", "last", "first", "gate", "last"]
    );
}

/// A middleware hands a value on to the handler.
#[tokio::test]
async fn a_middleware_hands_values_on() {
    #[derive(Debug)]
    struct Tier(&'static str);
    #[derive(Debug)]
    struct Load;
    #[async_trait]
    impl Middleware for Load {
        async fn handle(&self, mut ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
            // The match comes before the middleware.
            assert_eq!(ctx.invocation().map(|i| i.command.as_str()), Some("tier"));
            ctx.insert(Tier("gold"));
            next.run(ctx).await
        }
    }
    let seen = Arc::new(Mutex::new(None));
    let seen_in = Arc::clone(&seen);
    let bot = Bot::builder()
        .outbound(Recording::default())
        .middleware(Load)
        .command(Command::new("tier", move |ctx: Ctx| {
            let seen = Arc::clone(&seen_in);
            async move {
                *seen.lock().unwrap() = ctx.get::<Tier>().map(|t| t.0);
                Ok(())
            }
        }))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/tier")).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), Some("gold"));
}

/// `MarkRead::with_typing_indicator`: the documented read receipt with a
/// typing indicator, before the handler's reply.
#[tokio::test]
async fn mark_read_sends_the_read_receipt_with_a_typing_indicator_first() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, send_response());
    let bot = Bot::builder()
        .client(client(&t))
        .middleware(MarkRead::with_typing_indicator())
        .command(Command::new("ping", |ctx: Ctx| async move {
            ctx.reply("pong").await?;
            Ok(())
        }))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/ping")).await.unwrap();
    let requests = t.requests();
    assert_eq!(requests[0].path(), format!("/v25.0/{NUMBER}/messages"));
    assert_eq!(
        requests[0].json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": message_id(TEXT),
            "typing_indicator": {"type": "text"}
        }))
    );
    assert_eq!(requests[1].json().unwrap()["text"]["body"], "pong");
    assert_eq!(t.remaining(), 0);
}

/// A typing indicator Meta refuses falls back to a plain receipt; a
/// receipt that fails never stops the command.
#[tokio::test]
async fn a_failed_read_receipt_falls_back_and_never_stops_the_event() {
    let t = ScriptedTransport::new();
    let refused = json!({"error": {"message": "(#131000) x", "type": "OAuthException", "code": 131000, "fbtrace_id": "A"}});
    t.push_json(400, refused.clone());
    t.push_json(200, json!({"success": true}));
    t.push_json(400, refused.clone());
    t.push_json(400, refused);
    let runs = Arc::new(AtomicUsize::new(0));
    let bot = Bot::builder()
        .client(client(&t))
        .middleware(MarkRead::with_typing_indicator())
        .command(counting("ping", &runs))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/ping")).await.unwrap();
    let requests = t.requests();
    assert!(
        requests[0]
            .json()
            .unwrap()
            .get("typing_indicator")
            .is_some()
    );
    assert_eq!(
        requests[1].json(),
        Some(
            json!({"messaging_product": "whatsapp", "status": "read", "message_id": message_id(TEXT)})
        )
    );
    bot.handle(text_event(TEXT, "/ping")).await.unwrap();
    assert_eq!(count(&runs), 2);
    assert_eq!(t.remaining(), 0);
}

/// Standby copies and the business's own echoes never run a command, and
/// there is nothing to reply to.
#[tokio::test]
async fn standby_copies_and_echoes_never_run_commands() {
    let runs = Arc::new(AtomicUsize::new(0));
    let replies = Arc::new(Mutex::new(Vec::new()));
    let replies_in = Arc::clone(&replies);
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(counting("test", &runs))
        .listen(Listen::All, move |ctx: Ctx| {
            let replies = Arc::clone(&replies_in);
            async move {
                let refused = ctx.reply("hi").await.unwrap_err();
                replies
                    .lock()
                    .unwrap()
                    .push((ctx.event().kind(), refused.kind()));
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();

    let mut standby =
        common::fixture_json("pages/webhooks.reference.standby__inbound_message.json");
    standby["entry"][0]["changes"][0]["value"]["standby"]["messages"][0]["text"]["body"] =
        json!("/test");
    let mut echo = common::fixture_json("fields/smb_message_echoes_text.json");
    echo["entry"][0]["changes"][0]["value"]["message_echoes"][0]["text"]["body"] = json!("/test");
    for e in common::events_of(&standby)
        .into_iter()
        .chain(common::events_of(&echo))
        .chain([event("messages/status_sent.json")])
    {
        bot.handle(e).await.unwrap();
    }
    assert_eq!(count(&runs), 0);
    assert_eq!(
        *replies.lock().unwrap(),
        [
            ("standby_observed", ErrorKind::InvalidParameter),
            ("message_echoed", ErrorKind::InvalidParameter),
            ("status_updated", ErrorKind::InvalidParameter),
        ]
    );
}

#[tokio::test]
async fn listeners_get_the_events_of_their_kind() {
    let kinds = Arc::new(Mutex::new(Vec::new()));
    let listener = |label: &'static str| {
        let kinds = Arc::clone(&kinds);
        move |ctx: Ctx| {
            let kinds = Arc::clone(&kinds);
            async move {
                kinds.lock().unwrap().push((label, ctx.event().kind()));
                Ok(())
            }
        }
    };
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(counting("ping", &Arc::default()))
        .listen(Listen::Messages, listener("messages"))
        .listen(Listen::event("status_updated"), listener("statuses"))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/ping")).await.unwrap();
    bot.handle(text_event(TEXT, "hello")).await.unwrap();
    bot.handle(event("messages/status_sent.json"))
        .await
        .unwrap();
    assert_eq!(
        *kinds.lock().unwrap(),
        [
            ("messages", "message_received"),
            ("statuses", "status_updated")
        ]
    );
}

/// A failing listener does not keep the others from running; the event
/// still fails, once, with the first error.
#[tokio::test]
async fn every_listener_runs_when_one_fails() {
    #[derive(Debug, Clone)]
    struct Kinds(Arc<Mutex<Vec<ErrorKind>>>);
    #[async_trait]
    impl ErrorHandler for Kinds {
        async fn on_error(&self, _: &Ctx, error: Error) -> meta_whatsapp_core::Result<()> {
            self.0.lock().unwrap().push(error.kind());
            Ok(())
        }
    }
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_in = Arc::clone(&ran);
    let failed = Arc::new(Mutex::new(Vec::new()));
    let bot = Bot::builder()
        .outbound(Recording::default())
        .errors(Kinds(Arc::clone(&failed)))
        .listen(Listen::Messages, |_ctx: Ctx| async move {
            Err(Error::Other(anyhow::anyhow!("first listener fails")))
        })
        .listen(Listen::Messages, move |_ctx: Ctx| {
            let ran = Arc::clone(&ran_in);
            async move {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "hello")).await.unwrap();
    assert_eq!(count(&ran), 1);
    assert_eq!(*failed.lock().unwrap(), [ErrorKind::Unknown]);
}

/// Synchronized history (`history`) is another event: a user's `/test` in
/// it is the past, and runs no command.
#[tokio::test]
async fn history_never_runs_commands() {
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in = Arc::clone(&seen);
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .command(counting("test", &runs))
        .listen(Listen::All, move |ctx: Ctx| {
            let seen = Arc::clone(&seen_in);
            async move {
                let refused = ctx.reply("hi").await.unwrap_err();
                seen.lock()
                    .unwrap()
                    .push((ctx.event().kind(), refused.kind()));
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();
    let mut history = common::fixture_json("fields/history_threads.json");
    for thread in history["entry"][0]["changes"][0]["value"]["history"][0]["threads"]
        .as_array_mut()
        .unwrap()
    {
        for message in thread["messages"].as_array_mut().unwrap() {
            if message["type"] == "text" {
                message["text"]["body"] = json!("/test");
            }
        }
    }
    assert!(history.to_string().contains("/test"));
    for e in common::events_of(&history) {
        bot.handle(e).await.unwrap();
    }
    assert_eq!(count(&runs), 0);
    assert!(out.sent().is_empty());
    assert_eq!(
        *seen.lock().unwrap(),
        [("history_synced", ErrorKind::InvalidParameter)]
    );
}

// ─── Errors ──────────────────────────────────────────────────────────────

fn failing() -> Command {
    Command::new("fail", |_ctx: Ctx| async move {
        Err(Error::Other(anyhow::anyhow!("a handler's own failure")))
    })
}

#[tokio::test]
async fn a_failed_handler_is_acknowledged_by_default() {
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(failing())
        .build()
        .await
        .unwrap();
    // Acknowledged: an error would make Meta redeliver the batch and run
    // every command in it again.
    bot.deliver(text_event(TEXT, "/fail")).await.unwrap();
}

#[tokio::test]
async fn propagate_errors_makes_the_delivery_fail() {
    let bot = Bot::builder()
        .outbound(Recording::default())
        .errors(PropagateErrors)
        .command(failing())
        .build()
        .await
        .unwrap();
    let err = bot.deliver(text_event(TEXT, "/fail")).await.unwrap_err();
    assert!(matches!(err, SinkError::Delivery(_)), "{err}");
}

#[tokio::test]
async fn the_error_handler_sees_the_failed_command() {
    #[derive(Debug, Default, Clone)]
    struct Seen(Arc<Mutex<Vec<Option<String>>>>);
    #[async_trait]
    impl ErrorHandler for Seen {
        async fn on_error(&self, ctx: &Ctx, _: Error) -> meta_whatsapp_core::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(ctx.invocation().map(|i| i.command.clone()));
            Ok(())
        }
    }
    let seen = Seen::default();
    let bot = Bot::builder()
        .outbound(Recording::default())
        .errors(seen.clone())
        .command(failing())
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/fail")).await.unwrap();
    assert_eq!(*seen.0.lock().unwrap(), [Some("fail".to_owned())]);
}

// ─── Plugins and building ────────────────────────────────────────────────

#[derive(Debug)]
struct Games {
    unloads: Arc<Mutex<Vec<&'static str>>>,
    name: &'static str,
}

#[async_trait]
impl Plugin for Games {
    fn name(&self) -> &str {
        self.name
    }
    fn category(&self) -> Option<&str> {
        Some("Games")
    }
    async fn setup(&self, registrar: &mut Registrar) -> meta_whatsapp_core::Result<()> {
        assert_eq!(registrar.plugin(), Some(self.name));
        registrar.command(
            Command::new(format!("{}-roll", self.name), |ctx: Ctx| async move {
                ctx.reply("4").await?;
                Ok(())
            })
            .description("Roll a die"),
        );
        Ok(())
    }
    async fn on_unload(&self) -> meta_whatsapp_core::Result<()> {
        self.unloads.lock().unwrap().push(self.name);
        Ok(())
    }
}

#[tokio::test]
async fn plugins_register_in_order_and_unload_in_reverse_once() {
    let unloads = Arc::new(Mutex::new(Vec::new()));
    let plugin = |name| Games {
        unloads: Arc::clone(&unloads),
        name,
    };
    let bot = Bot::builder()
        .outbound(Recording::default())
        .plugin(plugin("dice"))
        .plugin(plugin("coin"))
        .build()
        .await
        .unwrap();
    let commands = bot.commands();
    assert_eq!(commands[0].name, "dice-roll");
    assert_eq!(commands[0].category, "Games");
    assert_eq!(commands[1].plugin.as_deref(), Some("coin"));
    assert_eq!(
        bot.plugins()
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["dice", "coin"]
    );

    bot.unload().await.unwrap();
    bot.unload().await.unwrap();
    assert_eq!(*unloads.lock().unwrap(), ["coin", "dice"]);
    // An unloaded bot refuses events as a closed sink, so Meta redelivers
    // them elsewhere.
    assert!(matches!(
        bot.deliver(text_event(TEXT, "/dice-roll")).await,
        Err(SinkError::Closed)
    ));
    assert!(matches!(
        bot.handle(text_event(TEXT, "/dice-roll")).await,
        Err(Error::Sink(SinkError::Closed))
    ));
}

/// A plugin whose `on_unload` fails does not keep the others from
/// unloading; its error is returned after they all ran.
#[tokio::test]
async fn unload_runs_every_plugin_and_returns_the_first_error() {
    #[derive(Debug)]
    struct Flaky(Arc<Mutex<Vec<&'static str>>>);
    #[async_trait]
    impl Plugin for Flaky {
        fn name(&self) -> &'static str {
            "flaky"
        }
        async fn setup(&self, _: &mut Registrar) -> meta_whatsapp_core::Result<()> {
            Ok(())
        }
        async fn on_unload(&self) -> meta_whatsapp_core::Result<()> {
            self.0.lock().unwrap().push("flaky");
            Err(meta_whatsapp_core::error::ConfigError::new("flaky unload").into())
        }
    }
    let unloads = Arc::new(Mutex::new(Vec::new()));
    let bot = Bot::builder()
        .outbound(Recording::default())
        .plugin(Games {
            unloads: Arc::clone(&unloads),
            name: "dice",
        })
        .plugin(Flaky(Arc::clone(&unloads)))
        .plugin(Games {
            unloads: Arc::clone(&unloads),
            name: "coin",
        })
        .build()
        .await
        .unwrap();
    let err = bot.unload().await.unwrap_err();
    assert!(
        matches!(&err, Error::Config(e) if e.0 == "flaky unload"),
        "{err}"
    );
    assert_eq!(*unloads.lock().unwrap(), ["coin", "flaky", "dice"]);
}

#[tokio::test]
async fn a_failed_plugin_setup_fails_the_build_in_its_step() {
    #[derive(Debug)]
    struct Broken;
    #[async_trait]
    impl Plugin for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }
        async fn setup(&self, _: &mut Registrar) -> meta_whatsapp_core::Result<()> {
            Err(meta_whatsapp_core::error::ConfigError::new("missing API key").into())
        }
    }
    let err = Bot::builder()
        .outbound(Recording::default())
        .plugin(Broken)
        .build()
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Step {
                step: "plugin_setup",
                ..
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn the_build_refuses_ambiguous_or_incomplete_registrations() {
    let noop = || |_ctx: Ctx| async move { Ok(()) };
    let config = |builder: meta_whatsapp_bot::BotBuilder| async move {
        match builder.build().await {
            Err(Error::Config(e)) => e.0,
            other => panic!("expected a ConfigError, got {other:?}"),
        }
    };
    let base = || Bot::builder().outbound(Recording::default());

    assert!(config(Bot::builder()).await.contains("outbound"));
    assert!(
        config(
            base()
                .command(Command::new("a", noop()).alias("B"))
                .command(Command::new("b", noop()))
        )
        .await
        .contains("`b`")
    );
    assert!(
        config(base().command(Command::new("two words", noop())))
            .await
            .contains("whitespace")
    );
    assert!(
        config(base().command(Command::new("", noop())))
            .await
            .contains("empty")
    );
    assert!(
        config(
            base()
                .command(Command::new("a", noop()).payload("x"))
                .command(Command::new("b", noop()).payload("x"))
        )
        .await
        .contains("payload")
    );
    assert!(
        config(base().command(Command::new("a", noop()).payload(" ")))
            .await
            .contains("empty payload")
    );
    assert!(
        config(base().command(Command::new("a", noop()).cooldown(Duration::from_secs(5))))
            .await
            .contains("cooldown store")
    );
    let kv = Arc::new(MemoryKvStore::new());
    assert!(
        config(
            base()
                .cooldown_store(kv, Arc::new(meta_whatsapp_core::clock::SystemClock))
                .command(Command::new("a", noop()).cooldown(Duration::ZERO))
        )
        .await
        .contains("zero")
    );
    let unloads = Arc::new(Mutex::new(Vec::new()));
    assert!(
        config(
            base()
                .plugin(Games {
                    unloads: Arc::clone(&unloads),
                    name: "x"
                })
                .plugin(Games { unloads, name: "x" })
        )
        .await
        .contains("two plugins")
    );
    // A listener that could never run: a misspelt event kind, a blank
    // message type.
    assert!(
        config(base().listen(Listen::event("status_update"), noop()))
            .await
            .contains("`status_update`")
    );
    assert!(
        config(base().listen(Listen::message_type(" "), noop()))
            .await
            .contains("blank message type")
    );
    // Every kind the library reports is accepted.
    let mut every = base();
    for kind in meta_whatsapp_webhooks::WebhookEvent::KINDS {
        every = every.listen(Listen::event(*kind), noop());
    }
    every.build().await.unwrap();
}

// ─── Order: ban → match → middleware → guards (review P1-1) ─────────────

/// Decisive: a banned sender, with `MarkRead::with_typing_indicator` and a
/// replying listener registered, makes no request at all: no read receipt,
/// no "typing…", no reply. (Move the ban check after the middleware and
/// the receipt goes out.)
#[tokio::test]
async fn a_banned_sender_makes_no_request_at_all() {
    let t = ScriptedTransport::new();
    let bot = Bot::builder()
        .client(client(&t))
        .access(AccessList::new().ban(BSUID))
        .middleware(MarkRead::with_typing_indicator())
        .command(Command::new("ping", |ctx: Ctx| async move {
            ctx.reply("pong").await?;
            Ok(())
        }))
        .listen(Listen::All, |ctx: Ctx| async move {
            ctx.reply("heard you").await?;
            Ok(())
        })
        .build()
        .await
        .unwrap();

    bot.deliver(text_event(BSUID_ONLY, "/ping")).await.unwrap();
    bot.deliver(text_event(BSUID_ONLY, "hello")).await.unwrap();
    assert!(t.requests().is_empty(), "{:?}", t.requests());
    assert_eq!(t.remaining(), 0);

    // Control: anyone else gets the receipt with its typing indicator, then
    // the reply.
    t.push_json(200, json!({"success": true}));
    t.push_json(200, send_response());
    bot.deliver(text_event(TEXT, "/ping")).await.unwrap();
    let requests = t.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .json()
            .unwrap()
            .get("typing_indicator")
            .is_some()
    );
    assert_eq!(requests[1].json().unwrap()["text"]["body"], "pong");
    assert_eq!(t.remaining(), 0);
}

/// The match comes before the middleware, so a middleware can act for some
/// commands only; the command's guards still come after it.
#[tokio::test]
async fn a_middleware_sees_the_matched_command_before_its_guards() {
    /// Stops `/admin` for everyone (a middleware for one command).
    #[derive(Debug)]
    struct NoAdmin(Arc<Mutex<Vec<Option<String>>>>);
    #[async_trait]
    impl Middleware for NoAdmin {
        async fn handle(&self, ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
            let command = ctx.invocation().map(|i| i.command.clone());
            self.0.lock().unwrap().push(command.clone());
            if command.as_deref() == Some("admin") {
                return Ok(());
            }
            next.run(ctx).await
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let admin = Arc::new(AtomicUsize::new(0));
    let roll = Arc::new(AtomicUsize::new(0));
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .cooldown_store(kv, Arc::new(clock))
        .middleware(NoAdmin(Arc::clone(&seen)))
        .command(counting("admin", &admin).alias("a"))
        .command(counting("roll", &roll).cooldown(Duration::from_secs(60)))
        .build()
        .await
        .unwrap();

    bot.handle(text_event(TEXT, "/A")).await.unwrap();
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    bot.handle(text_event(TEXT, "hello")).await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [
            Some("admin".to_owned()),
            Some("roll".to_owned()),
            Some("roll".to_owned()),
            None
        ]
    );
    assert_eq!((count(&admin), count(&roll)), (0, 1));
    // The second `/roll` passed the middleware and met its cooldown after.
    assert_eq!(out.bodies().len(), 1);
}

/// Decisive: five attempts inside one running cooldown get one "please
/// wait", not five; a new cooldown gets its own. The `Refusals` still sees
/// every refusal, with `notify` set on the first only.
#[tokio::test]
async fn a_running_cooldown_is_told_once() {
    #[derive(Debug, Clone, Default)]
    struct Seen(Arc<Mutex<Vec<Refusal>>>, ReplyRefusals);
    #[async_trait]
    impl Refusals for Seen {
        async fn refused(&self, ctx: &Ctx, refusal: &Refusal) -> meta_whatsapp_core::Result<()> {
            self.0.lock().unwrap().push(*refusal);
            self.1.refused(ctx, refusal).await
        }
    }
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Arc::new(AtomicUsize::new(0));
    let out = Recording::default();
    let seen = Seen::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .refusals(seen.clone())
        .cooldown_store(kv, Arc::new(clock.clone()))
        .command(counting("roll", &runs).cooldown(Duration::from_secs(30)))
        .build()
        .await
        .unwrap();

    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    for _ in 0..5 {
        clock.advance(Duration::from_secs(1));
        bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    }
    assert_eq!(count(&runs), 1);
    assert_eq!(
        out.bodies(),
        ["Please wait 29 s before using this command again."]
    );
    let notified: Vec<bool> = seen
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|r| matches!(r, Refusal::CoolingDown { notify: true, .. }))
        .collect();
    assert_eq!(notified, [true, false, false, false, false]);

    // Another user is told on their own.
    bot.handle(text_event(BSUID_ONLY, "/roll")).await.unwrap();
    bot.handle(text_event(BSUID_ONLY, "/roll")).await.unwrap();
    assert_eq!(out.bodies().len(), 2);

    // Expired: it runs again, and its own cooldown is told once more.
    clock.advance(Duration::from_secs(30));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    clock.advance(Duration::from_secs(1));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    assert_eq!(count(&runs), 3);
    assert_eq!(
        out.bodies().last().map(String::as_str),
        Some("Please wait 29 s before using this command again.")
    );
    assert_eq!(out.bodies().len(), 3);
}

// ─── Unknown commands, captions, reactions, message types ──────────────

/// An unknown `/name` goes to the unknown-command handler, which gets the
/// name as the parser read it; without one, the listeners get it.
#[tokio::test]
async fn an_unknown_command_reaches_its_handler_with_the_name() {
    let names = Arc::new(Mutex::new(Vec::new()));
    let names_in = Arc::clone(&names);
    let listened = Arc::new(Mutex::new(Vec::new()));
    let listened_in = Arc::clone(&listened);
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .command(counting("ping", &Arc::default()))
        .unknown_command(move |ctx: Ctx| {
            let names = Arc::clone(&names_in);
            async move {
                let unknown = ctx.unknown_command().unwrap();
                names.lock().unwrap().push(unknown.name.clone());
                ctx.reply(format!(
                    "There is no /{}. Send /help to see what I can do.",
                    unknown.name
                ))
                .await?;
                Ok(())
            }
        })
        .listen(Listen::Messages, move |ctx: Ctx| {
            let listened = Arc::clone(&listened_in);
            async move {
                listened
                    .lock()
                    .unwrap()
                    .push(ctx.text().unwrap_or_default().to_owned());
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();

    bot.handle(text_event(TEXT, "/Pnig now")).await.unwrap();
    bot.handle(text_event(TEXT, "/ping")).await.unwrap();
    bot.handle(text_event(TEXT, "hello")).await.unwrap();
    bot.handle(text_event(TEXT, "/")).await.unwrap();
    assert_eq!(*names.lock().unwrap(), ["pnig"]);
    assert_eq!(
        out.bodies(),
        ["There is no /pnig. Send /help to see what I can do."]
    );
    assert_eq!(*listened.lock().unwrap(), ["hello", "/"]);

    // Without an unknown-command handler, a listener gets it and the name.
    let unknown = Arc::new(Mutex::new(Vec::new()));
    let unknown_in = Arc::clone(&unknown);
    let plain = Bot::builder()
        .outbound(Recording::default())
        .listen(Listen::Messages, move |ctx: Ctx| {
            let unknown = Arc::clone(&unknown_in);
            async move {
                unknown
                    .lock()
                    .unwrap()
                    .push(ctx.unknown_command().map(|c| c.name.clone()));
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();
    plain.handle(text_event(TEXT, "/nope")).await.unwrap();
    plain.handle(text_event(TEXT, "nope")).await.unwrap();
    assert_eq!(*unknown.lock().unwrap(), [Some("nope".to_owned()), None]);
}

/// A media fixture with its caption replaced.
fn captioned(path: &str, media: &str, caption: &str) -> meta_whatsapp_webhooks::WebhookEvent {
    let mut json = common::fixture_json(path);
    let slot = &mut json["entry"][0]["changes"][0]["value"]["messages"][0][media]["caption"];
    assert!(slot.is_string(), "{path} has no {media} caption");
    *slot = json!(caption);
    common::events_of(&json).remove(0)
}

/// Meta's image and video examples, captioned `/sticker big`: the caption
/// is the command, the media stays in the message.
#[tokio::test]
async fn an_image_or_video_caption_runs_its_command() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in = Arc::clone(&seen);
    let listened = Arc::new(AtomicUsize::new(0));
    let listened_in = Arc::clone(&listened);
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(Command::new("sticker", move |ctx: Ctx| {
            let seen = Arc::clone(&seen_in);
            async move {
                let invocation = ctx.invocation().unwrap();
                seen.lock().unwrap().push((
                    invocation.trigger.clone(),
                    invocation.args.as_slice().to_vec(),
                    ctx.message()
                        .and_then(|m| m.message_type())
                        .map(str::to_owned),
                ));
                Ok(())
            }
        }))
        .listen(Listen::Messages, move |_ctx: Ctx| {
            let listened = Arc::clone(&listened_in);
            async move {
                listened.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();

    bot.handle(captioned("messages/image.json", "image", "/sticker big"))
        .await
        .unwrap();
    bot.handle(captioned("messages/video.json", "video", "/STICKER"))
        .await
        .unwrap();
    let caption = |name: &str| Trigger::Caption {
        prefix: "/".into(),
        name: name.into(),
    };
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (
                caption("sticker"),
                vec!["big".to_owned()],
                Some("image".to_owned())
            ),
            (caption("sticker"), vec![], Some("video".to_owned())),
        ]
    );
    // Meta's own captions are plain messages.
    bot.handle(event("messages/image.json")).await.unwrap();
    bot.handle(event("messages/video.json")).await.unwrap();
    assert_eq!(count(&listened), 2);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

/// Decisive for the switch: with `commands_from_captions(false)` a caption
/// is never a command nor an unknown one; the media goes to the listeners.
/// Typed text still is a command.
#[tokio::test]
async fn captions_can_be_left_out_of_the_match() {
    let runs = Arc::new(AtomicUsize::new(0));
    let unknown = Arc::new(AtomicUsize::new(0));
    let unknown_in = Arc::clone(&unknown);
    let images = Arc::new(Mutex::new(Vec::new()));
    let images_in = Arc::clone(&images);
    let bot = Bot::builder()
        .outbound(Recording::default())
        .commands_from_captions(false)
        .command(counting("sticker", &runs))
        .unknown_command(move |_ctx: Ctx| {
            let unknown = Arc::clone(&unknown_in);
            async move {
                unknown.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .listen(Listen::Messages, move |ctx: Ctx| {
            let images = Arc::clone(&images_in);
            async move {
                images.lock().unwrap().push((
                    ctx.invocation().is_some(),
                    ctx.unknown_command().is_some(),
                    ctx.message()
                        .and_then(|m| m.message_type())
                        .map(str::to_owned),
                ));
                Ok(())
            }
        })
        .build()
        .await
        .unwrap();

    bot.handle(captioned("messages/image.json", "image", "/sticker big"))
        .await
        .unwrap();
    bot.handle(captioned("messages/video.json", "video", "/nope"))
        .await
        .unwrap();
    assert_eq!((count(&runs), count(&unknown)), (0, 0));
    assert_eq!(
        *images.lock().unwrap(),
        [
            (false, false, Some("image".to_owned())),
            (false, false, Some("video".to_owned()))
        ]
    );
    bot.handle(text_event(TEXT, "/sticker")).await.unwrap();
    bot.handle(text_event(TEXT, "/nope")).await.unwrap();
    assert_eq!((count(&runs), count(&unknown)), (1, 1));
}

/// `ctx.react` sends the documented reaction (`messages/reaction-messages`)
/// to the message, from the number it arrived on.
#[tokio::test]
async fn react_sends_the_documented_reaction() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    let bot = Bot::builder()
        .client(client(&t))
        .command(Command::new("report", |ctx: Ctx| async move {
            ctx.react("\u{23F3}").await?;
            Ok(())
        }))
        .build()
        .await
        .unwrap();
    bot.deliver(text_event(TEXT, "/report")).await.unwrap();
    let request = t.last_request().unwrap();
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.path(), format!("/v25.0/{NUMBER}/messages"));
    assert_eq!(
        request.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "type": "reaction",
            "reaction": {"message_id": message_id(TEXT), "emoji": "\u{23F3}"}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

/// `Listen::MessageType` gets the messages of Meta's `type` no command took.
#[tokio::test]
async fn message_type_listeners_get_their_type_only() {
    let kinds = Arc::new(Mutex::new(Vec::new()));
    let listener = |label: &'static str| {
        let kinds = Arc::clone(&kinds);
        move |_ctx: Ctx| {
            let kinds = Arc::clone(&kinds);
            async move {
                kinds.lock().unwrap().push(label);
                Ok(())
            }
        }
    };
    let bot = Bot::builder()
        .outbound(Recording::default())
        .command(counting("ping", &Arc::default()))
        .listen(Listen::message_type("reaction"), listener("reaction"))
        .listen(Listen::message_type("image"), listener("image"))
        .build()
        .await
        .unwrap();
    bot.handle(event("messages/reaction.json")).await.unwrap();
    bot.handle(event("messages/image.json")).await.unwrap();
    bot.handle(text_event(TEXT, "hello")).await.unwrap();
    bot.handle(event("messages/status_sent.json"))
        .await
        .unwrap();
    // A caption that is a command is the command's, not the listener's.
    bot.handle(captioned("messages/image.json", "image", "/ping"))
        .await
        .unwrap();
    assert_eq!(*kinds.lock().unwrap(), ["reaction", "image"]);
}

// ─── The order under review: every path, reroutes, replicas ─────────────

/// Every path a received message can take: a command, a command with
/// arguments, an unknown command, plain text, an image and a video
/// captioned with a command or an unknown one, a plain image, a reply
/// button, a list row, a template quick-reply button and a reaction.
/// Meta's fixtures, all from `+1 650 555 1234`.
fn every_path() -> Vec<(&'static str, meta_whatsapp_webhooks::WebhookEvent)> {
    vec![
        ("command", text_event(TEXT, "/ping")),
        ("command with arguments", text_event(TEXT, "/ping now")),
        ("unknown command", text_event(TEXT, "/nope")),
        ("plain text", text_event(TEXT, "hello")),
        (
            "image caption command",
            captioned("messages/image.json", "image", "/sticker big"),
        ),
        (
            "video caption unknown command",
            captioned("messages/video.json", "video", "/nope"),
        ),
        ("plain image", event("messages/image.json")),
        (
            "reply button payload",
            event("messages/interactive_button_reply.json"),
        ),
        (
            "list row payload",
            event("messages/interactive_list_reply.json"),
        ),
        ("quick-reply button payload", event("messages/button.json")),
        ("reaction", event("messages/reaction.json")),
    ]
}

/// A bot where every path of [`every_path`] answers: `MarkRead` with a
/// typing indicator, commands (typed, captioned, tapped), an
/// unknown-command handler and listeners of every kind, all replying.
fn answering_everything(builder: meta_whatsapp_bot::BotBuilder) -> meta_whatsapp_bot::BotBuilder {
    let reply = |text: &'static str| {
        move |ctx: Ctx| async move {
            ctx.reply(text).await?;
            Ok(())
        }
    };
    builder
        .middleware(MarkRead::with_typing_indicator())
        .command(Command::new("ping", reply("pong")))
        .command(Command::new("sticker", reply("sticker")))
        .command(
            Command::new("tap", reply("tapped"))
                .payload("cancel-button")
                .payload("priority_express")
                .payload("Unsubscribe"),
        )
        .unknown_command(reply("unknown"))
        .listen(Listen::All, reply("all"))
        .listen(Listen::Messages, reply("messages"))
        .listen(Listen::message_type("image"), reply("image"))
        .listen(Listen::event("message_received"), reply("received"))
}

/// Decisive for check "a banned sender triggers nothing": on every path,
/// with every kind of handler registered, a banned sender makes no request
/// at all (no receipt, no "typing…", no reply) and the `Refusals` hears of
/// each message as `Banned`. The control: the same events from a sender
/// who is not banned each make a receipt and a reply.
#[tokio::test]
async fn a_banned_sender_makes_no_request_on_any_path() {
    #[derive(Debug, Clone, Default)]
    struct Seen(Arc<Mutex<Vec<Refusal>>>);
    #[async_trait]
    impl Refusals for Seen {
        async fn refused(&self, _: &Ctx, refusal: &Refusal) -> meta_whatsapp_core::Result<()> {
            self.0.lock().unwrap().push(*refusal);
            Ok(())
        }
    }
    let t = ScriptedTransport::new();
    let seen = Seen::default();
    let banned = answering_everything(
        Bot::builder()
            .client(client(&t))
            .refusals(seen.clone())
            .access(AccessList::new().ban_phone("+1 650 555 1234")),
    )
    .build()
    .await
    .unwrap();
    let paths = every_path();
    for (path, event) in paths.clone() {
        banned.deliver(event).await.unwrap();
        assert!(t.requests().is_empty(), "{path}: {:?}", t.requests());
    }
    assert_eq!(t.remaining(), 0);
    assert_eq!(*seen.0.lock().unwrap(), vec![Refusal::Banned; paths.len()]);

    // Control: not banned, every path is answered.
    let out = Recording::default();
    let open = answering_everything(Bot::builder().outbound(out.clone()))
        .build()
        .await
        .unwrap();
    for (path, event) in paths {
        let (sent, reads) = (out.sent().len(), out.reads.lock().unwrap().len());
        open.handle(event).await.unwrap();
        assert_eq!(out.reads.lock().unwrap().len(), reads + 1, "{path}");
        assert!(out.sent().len() > sent, "{path}: no reply");
    }
}

/// A middleware may hand an event to another command
/// (`Ctx::with_invocation`): `go <name>` runs `<name>`. The command it lands
/// on still meets its own guards (owner-only, scope, cooldown), so a
/// reroute is no way around them; for an owner it runs (control).
#[tokio::test]
async fn a_rerouted_event_meets_the_guards_of_the_command_it_lands_on() {
    #[derive(Debug)]
    struct Reroute;
    #[async_trait]
    impl Middleware for Reroute {
        async fn handle(&self, ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
            let target = ctx
                .text()
                .and_then(|t| t.strip_prefix("go "))
                .map(str::to_owned);
            let ctx = match target {
                Some(name) => ctx.with_invocation(meta_whatsapp_bot::Invocation::new(
                    name.clone(),
                    Trigger::Text {
                        prefix: String::new(),
                        name,
                    },
                    meta_whatsapp_bot::Args::default(),
                )),
                None => ctx,
            };
            next.run(ctx).await
        }
    }
    #[derive(Debug, Clone, Default)]
    struct Seen(Arc<Mutex<Vec<Refusal>>>);
    #[async_trait]
    impl Refusals for Seen {
        async fn refused(&self, _: &Ctx, refusal: &Refusal) -> meta_whatsapp_core::Result<()> {
            self.0.lock().unwrap().push(*refusal);
            Ok(())
        }
    }
    let (admin, poll, roll) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let seen = Seen::default();
    let bot = Bot::builder()
        .outbound(Recording::default())
        .refusals(seen.clone())
        .access(AccessList::new().owner(BSUID))
        .cooldown_store(kv, Arc::new(clock))
        .middleware(Reroute)
        .command(counting("admin", &admin).owner_only())
        .command(counting("poll", &poll).group_only())
        .command(counting("roll", &roll).cooldown(Duration::from_secs(60)))
        .build()
        .await
        .unwrap();

    // Plain text, so the match found no command: the middleware's is the
    // only invocation.
    bot.handle(text_event(TEXT, "go admin")).await.unwrap();
    bot.handle(text_event(TEXT, "go poll")).await.unwrap();
    bot.handle(text_event(TEXT, "go roll")).await.unwrap();
    bot.handle(text_event(TEXT, "go roll")).await.unwrap();
    assert_eq!((count(&admin), count(&poll), count(&roll)), (0, 0, 1));
    let refusals = seen.0.lock().unwrap().clone();
    assert_eq!(refusals.len(), 3, "{refusals:?}");
    assert_eq!(refusals[..2], [Refusal::NotOwner, Refusal::GroupOnly]);
    assert!(
        matches!(refusals[2], Refusal::CoolingDown { notify: true, .. }),
        "{refusals:?}"
    );

    // Control: the owner's reroute runs the owner-only command.
    bot.handle(text_event(BSUID_ONLY, "go admin"))
        .await
        .unwrap();
    assert_eq!(count(&admin), 1);
}

/// A `KvStore` that holds every operation on a cooldown notice marker until
/// `n` of them are waiting, then lets them all go: the worst interleaving
/// of `n` replicas refusing the same user at once, on one runtime thread.
#[derive(Debug)]
struct NoticeRendezvous {
    inner: MemoryKvStore,
    n: usize,
    arrived: AtomicUsize,
}

impl NoticeRendezvous {
    async fn meet(&self, key: &StoreKey) {
        if key.namespace() != meta_whatsapp_bot::COOLDOWN_NOTICE_NAMESPACE {
            return;
        }
        let me = self.arrived.fetch_add(1, Ordering::SeqCst) + 1;
        let round = me.div_ceil(self.n) * self.n;
        while self.arrived.load(Ordering::SeqCst) < round {
            tokio::task::yield_now().await;
        }
    }
}

#[async_trait]
impl KvStore for NoticeRendezvous {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.meet(key).await;
        self.inner.get(key).await
    }
    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.meet(key).await;
        self.inner.put(key, value, expiry).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.meet(key).await;
        self.inner.put_if_absent(key, value, expiry).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.meet(key).await;
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.meet(key).await;
        self.inner.delete(key).await
    }
}

/// Decisive: two replicas (two bots, each with its own `KvCooldowns`) on
/// one store, four refusals of one user's running cooldown at once, every
/// marker operation held until all four reach it: exactly one notice. A
/// marker read before it is written (not one atomic `put_if_absent`) lets
/// all four notify.
#[tokio::test]
async fn replicas_on_one_store_tell_a_running_cooldown_once() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(NoticeRendezvous {
        inner: MemoryKvStore::with_clock(Arc::new(clock.clone())),
        n: 4,
        arrived: AtomicUsize::new(0),
    });
    let runs = Arc::new(AtomicUsize::new(0));
    let (out_a, out_b) = (Recording::default(), Recording::default());
    let replica = |out: &Recording| {
        Bot::builder()
            .outbound(out.clone())
            .cooldown_store(kv.clone(), Arc::new(clock.clone()))
            .command(counting("roll", &runs).cooldown(Duration::from_secs(30)))
            .build()
    };
    let (a, b) = (
        replica(&out_a).await.unwrap(),
        replica(&out_b).await.unwrap(),
    );
    a.handle(text_event(TEXT, "/roll")).await.unwrap();
    clock.advance(Duration::from_secs(1));
    let (r1, r2, r3, r4) = tokio::join!(
        a.handle(text_event(TEXT, "/roll")),
        b.handle(text_event(TEXT, "/roll")),
        a.handle(text_event(TEXT, "/roll")),
        b.handle(text_event(TEXT, "/roll")),
    );
    for r in [r1, r2, r3, r4] {
        r.unwrap();
    }
    assert_eq!(count(&runs), 1);
    let mut notices = out_a.bodies();
    notices.extend(out_b.bodies());
    assert_eq!(
        notices,
        ["Please wait 29 s before using this command again."]
    );
    assert_eq!(kv.arrived.load(Ordering::SeqCst), 4);
}

/// Decisive: the marker ends when its cooldown ends, not a period after the
/// first refusal. Refused 29 s into a 30 s cooldown, run again at 31 s, then
/// refused at 32 s: that new cooldown's refusal is told (a marker that
/// lived 30 s from its own write would still be there until 59 s).
#[tokio::test]
async fn the_notice_marker_ends_with_its_cooldown() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Arc::new(AtomicUsize::new(0));
    let out = Recording::default();
    let bot = Bot::builder()
        .outbound(out.clone())
        .cooldown_store(kv, Arc::new(clock.clone()))
        .command(counting("roll", &runs).cooldown(Duration::from_secs(30)))
        .build()
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    clock.advance(Duration::from_secs(29));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    clock.advance(Duration::from_secs(2));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    clock.advance(Duration::from_secs(1));
    bot.handle(text_event(TEXT, "/roll")).await.unwrap();
    assert_eq!(count(&runs), 2);
    assert_eq!(
        out.bodies(),
        [
            "Please wait 1 s before using this command again.",
            "Please wait 29 s before using this command again."
        ]
    );
}
