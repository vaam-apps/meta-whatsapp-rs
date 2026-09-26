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
    PropagateErrors, Registrar, SilentRefusals, Trigger,
};
use meta_whatsapp_core::clock::ManualClock;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::sink::EventSink;
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
            assert!(
                ctx.invocation().is_none(),
                "middleware runs before the match"
            );
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

struct Games {
    unloads: Arc<Mutex<Vec<&'static str>>>,
    name: &'static str,
}

#[async_trait]
impl Plugin for Games {
    fn name(&self) -> &str {
        self.name
    }
    fn category(&self) -> &'static str {
        "Games"
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
    // An unloaded bot refuses events, so Meta redelivers them elsewhere.
    assert!(bot.deliver(text_event(TEXT, "/dice-roll")).await.is_err());
}

#[tokio::test]
async fn a_failed_plugin_setup_fails_the_build_in_its_step() {
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
}
