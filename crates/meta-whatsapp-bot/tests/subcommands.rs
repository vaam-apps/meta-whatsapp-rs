//! Subcommands and flags (roadmap B1b) over Meta's documented webhook
//! fixtures: which handler runs and with which arguments, each
//! subcommand's own guards and cooldown, flags declared or refused, the
//! help, payloads, reroutes, and what the build refuses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_adapters::store::MemoryKvStore;
use meta_whatsapp_bot::{
    AccessList, Args, Bot, BotBuilder, Command, CooldownKey, Ctx, Invocation, Middleware, Next,
    PropagateErrors, Refusal, Refusals, Trigger,
};
use meta_whatsapp_core::Error;
use meta_whatsapp_core::clock::ManualClock;
use meta_whatsapp_core::error::SinkError;
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::KvStore;
use meta_whatsapp_core::testing::ScriptedTransport;
use pretty_assertions::assert_eq;
use serde_json::json;
use time::macros::datetime;

use common::{NUMBER, Recording, client, event, message_id, send_response, text_event};

const TEXT: &str = "messages/text.json";
const BSUID_ONLY: &str = "bsuid/text_username_no_wa_id.json";
const GROUP: &str = "messages/group_text.json";
const BSUID: &str = "US.13491208655302741918";
/// The sender of `messages/text.json`, keyed by phone number (it carries
/// no BSUID).
const TEXT_SENDER: &str = "16505551234";

/// What one handler saw, run by run: the invocation's subcommand, its
/// positional arguments and its flags as `name` or `name=value`.
#[derive(Debug, Clone, Default)]
struct Runs(Arc<Mutex<Vec<Run>>>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    handler: &'static str,
    subcommand: Option<String>,
    args: Vec<String>,
    flags: Vec<String>,
}

impl Runs {
    /// A handler recording each run under `handler`.
    fn handler(
        &self,
        handler: &'static str,
    ) -> impl Fn(Ctx) -> std::future::Ready<meta_whatsapp_core::Result<()>> + Send + Sync + 'static
    {
        let runs = Arc::clone(&self.0);
        move |ctx: Ctx| {
            let invocation = ctx.invocation().unwrap();
            runs.lock().unwrap().push(Run {
                handler,
                subcommand: invocation.subcommand.clone(),
                args: invocation.args.as_slice().to_vec(),
                flags: invocation
                    .args
                    .flags()
                    .iter()
                    .map(|f| match &f.value {
                        Some(v) => format!("{}={v}", f.name),
                        None => f.name.clone(),
                    })
                    .collect(),
            });
            std::future::ready(Ok(()))
        }
    }

    fn take(&self) -> Vec<Run> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    /// The handlers that ran, in order, and empty the record.
    fn names(&self) -> Vec<&'static str> {
        self.take().into_iter().map(|r| r.handler).collect()
    }
}

fn run(handler: &'static str, subcommand: Option<&str>, args: &[&str], flags: &[&str]) -> Run {
    Run {
        handler,
        subcommand: subcommand.map(str::to_owned),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        flags: flags.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// `/order` with `status` (alias `st`) and `cancel`, recording into `runs`.
fn order(runs: &Runs) -> Command {
    Command::new("order", runs.handler("order"))
        .flag("all")
        .subcommand(
            Command::new("status", runs.handler("status"))
                .alias("st")
                .flag("dry-run")
                .flag("limit"),
        )
        .subcommand(Command::new("cancel", runs.handler("cancel")))
}

async fn bot(builder: BotBuilder) -> Bot {
    builder.build().await.unwrap()
}

// ─── Which handler runs ──────────────────────────────────────────────────

/// Decisive: `/order status 42` runs `status` with `42` as its only
/// argument, and `/order` alone runs the parent. Make the match skip the
/// subcommand (the parent runs instead) and the first assertion fails.
#[tokio::test]
async fn order_status_runs_the_subcommand_and_order_alone_the_parent() {
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .command(order(&runs)))
    .await;

    bot.handle(text_event(TEXT, "/order status 42"))
        .await
        .unwrap();
    assert_eq!(runs.take(), [run("status", Some("status"), &["42"], &[])]);

    bot.handle(text_event(TEXT, "/order")).await.unwrap();
    assert_eq!(runs.take(), [run("order", None, &[], &[])]);
}

/// The exact reply a subcommand sends, through the client, quoting the
/// message; the invocation names the command and the subcommand.
#[tokio::test]
async fn a_subcommand_replies_like_a_command() {
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    let seen = Arc::new(Mutex::new(None));
    let seen_in = Arc::clone(&seen);
    let bot = bot(Bot::builder().client(client(&t)).command(
        Command::new("order", |_ctx: Ctx| async move { Ok(()) }).subcommand(Command::new(
            "status",
            move |ctx: Ctx| {
                let seen = Arc::clone(&seen_in);
                async move {
                    *seen.lock().unwrap() = ctx.invocation().cloned();
                    let order = ctx.args().get(0).unwrap_or("?").to_owned();
                    ctx.reply(format!("Order {order} has shipped.")).await?;
                    Ok(())
                }
            },
        )),
    ))
    .await;

    bot.deliver(text_event(TEXT, "/Order STATUS 42"))
        .await
        .unwrap();

    let invocation = seen.lock().unwrap().clone().unwrap();
    assert_eq!(invocation.command, "order");
    assert_eq!(invocation.subcommand.as_deref(), Some("status"));
    assert_eq!(
        invocation.trigger,
        Trigger::Text {
            prefix: "/".into(),
            name: "order".into()
        }
    );
    assert_eq!(invocation.args.as_slice(), ["42"]);
    assert_eq!(invocation.args.raw(), "42");
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
            "context": {"message_id": message_id(TEXT)},
            "type": "text",
            "text": {"body": "Order 42 has shipped."}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

/// Only the first word, typed outside quotes, names a subcommand: a
/// subcommand's name elsewhere is a positional argument of the parent.
#[tokio::test]
async fn a_subcommand_name_elsewhere_is_an_argument() {
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .command(order(&runs)))
    .await;
    for body in [
        "/order 42 status",
        "/order \"status\" 42",
        "/order \u{201C}status\u{201D} 42",
        "/order --all status 42",
        "/order -- status 42",
        "/order statusx 42",
    ] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(
        runs.take(),
        [
            run("order", None, &["42", "status"], &[]),
            run("order", None, &["status", "42"], &[]),
            run("order", None, &["status", "42"], &[]),
            run("order", None, &["status", "42"], &["all"]),
            run("order", None, &["status", "42"], &[]),
            run("order", None, &["statusx", "42"], &[]),
        ]
    );

    // An alias, any case; a subcommand's name as its own argument; the
    // second subcommand; flags after the subcommand are its own.
    for body in [
        "/order ST 7",
        "/order status status",
        "/order cancel",
        "/order status 42 --dry-run --limit=5",
        "/order status \"--limit=5\"",
    ] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(
        runs.take(),
        [
            run("status", Some("status"), &["7"], &[]),
            run("status", Some("status"), &["status"], &[]),
            run("cancel", Some("cancel"), &[], &[]),
            run("status", Some("status"), &["42"], &["dry-run", "limit=5"]),
            run("status", Some("status"), &["--limit=5"], &[]),
        ]
    );
    // A subcommand's name is no command of its own.
    bot.handle(text_event(TEXT, "/status 42")).await.unwrap();
    assert!(runs.take().is_empty());
}

/// An image caption reads the same way as a text.
#[tokio::test]
async fn a_caption_runs_a_subcommand() {
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .command(order(&runs)))
    .await;
    let mut json = common::fixture_json("messages/image.json");
    json["entry"][0]["changes"][0]["value"]["messages"][0]["image"]["caption"] =
        json!("/order status 9");
    for e in common::events_of(&json) {
        bot.handle(e).await.unwrap();
    }
    assert_eq!(runs.take(), [run("status", Some("status"), &["9"], &[])]);
}

/// A tapped button whose id is a subcommand's payload runs the
/// subcommand, with no arguments.
#[tokio::test]
async fn a_subcommand_payload_runs_the_subcommand() {
    let runs = Runs::default();
    let bot = bot(Bot::builder().outbound(Recording::default()).command(
        Command::new("order", runs.handler("order"))
            .subcommand(Command::new("cancel", runs.handler("cancel")).payload("cancel-button")),
    ))
    .await;
    bot.handle(event("messages/interactive_button_reply.json"))
        .await
        .unwrap();
    assert_eq!(runs.take(), [run("cancel", Some("cancel"), &[], &[])]);
}

// ─── Each subcommand's guards ────────────────────────────────────────────

/// Decisive: a subcommand's own cooldown refuses its second run. Drop the
/// subcommand's guard check (its cooldown, read from the subcommand) and
/// the second `/order status` runs.
#[tokio::test]
async fn a_subcommands_own_cooldown_refuses_a_second_run() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Runs::default();
    let out = Recording::default();
    let bot = bot(Bot::builder()
        .outbound(out.clone())
        .cooldown_store(kv.clone(), Arc::new(clock.clone()))
        .command(
            Command::new("order", runs.handler("order"))
                .subcommand(
                    Command::new("status", runs.handler("status"))
                        .cooldown(Duration::from_secs(30)),
                )
                .subcommand(Command::new("cancel", runs.handler("cancel"))),
        ))
    .await;

    bot.handle(text_event(TEXT, "/order status 1"))
        .await
        .unwrap();
    clock.advance(Duration::from_secs(10));
    bot.handle(text_event(TEXT, "/order status 2"))
        .await
        .unwrap();
    assert_eq!(runs.names(), ["status"], "ran inside its cooldown");
    assert_eq!(
        out.bodies(),
        ["Please wait 20 s before using this command again."]
    );

    // Its own: the parent and the other subcommand run, another user too.
    bot.handle(text_event(TEXT, "/order")).await.unwrap();
    bot.handle(text_event(TEXT, "/order cancel")).await.unwrap();
    bot.handle(text_event(BSUID_ONLY, "/order status 3"))
        .await
        .unwrap();
    assert_eq!(runs.names(), ["order", "cancel", "status"]);

    // Kept under the path `order status`, in the cooldown namespace; the
    // parent, without a cooldown, has no record.
    let pn = PhoneNumberId::new(NUMBER).unwrap();
    let key = CooldownKey::new(&pn, "order status", TEXT_SENDER).store_key();
    assert!(kv.get(&key).await.unwrap().is_some());
    let parent = CooldownKey::new(&pn, "order", TEXT_SENDER).store_key();
    assert!(kv.get(&parent).await.unwrap().is_none());

    // Expired: it runs again.
    clock.advance(Duration::from_secs(21));
    bot.handle(text_event(TEXT, "/order status 4"))
        .await
        .unwrap();
    assert_eq!(runs.names(), ["status"]);
}

/// A parent's cooldown covers the parent's runs only, under the key it
/// had before subcommands existed.
#[tokio::test]
async fn a_parents_cooldown_leaves_its_subcommands_alone() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .cooldown_store(kv.clone(), Arc::new(clock))
        .command(
            Command::new("order", runs.handler("order"))
                .cooldown(Duration::from_secs(30))
                .subcommand(Command::new("status", runs.handler("status"))),
        ))
    .await;
    for body in ["/order", "/order status 1", "/order", "/order status 2"] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(runs.names(), ["order", "status", "status"]);
    let pn = PhoneNumberId::new(NUMBER).unwrap();
    let parent = CooldownKey::new(&pn, "order", TEXT_SENDER).store_key();
    assert!(kv.get(&parent).await.unwrap().is_some());
}

/// A command's cooldown key, written through the bot, is the literal key
/// the bot wrote before subcommands and flags existed (computed on the
/// commit before B1b for `Imagine`, alias `im`, typed `/IM a cat` from
/// `messages/text.json`): gaining an alias, a flag and a subcommand
/// changes nothing in `wa.bot.cooldown`.
#[tokio::test]
async fn a_commands_cooldown_key_is_unchanged_by_subcommands_and_flags() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .cooldown_store(kv.clone(), Arc::new(clock))
        .command(
            Command::new("Imagine", runs.handler("imagine"))
                .alias("im")
                .flag("hd")
                .cooldown(Duration::from_secs(30))
                .subcommand(Command::new("status", runs.handler("status"))),
        ))
    .await;
    bot.handle(text_event(TEXT, "/IM a cat")).await.unwrap();
    assert_eq!(runs.names(), ["imagine"]);
    // `\x77` is `w`: a rename of the namespace constant cannot rewrite it.
    let pinned = meta_whatsapp_core::store::StoreKey::new(
        "\x77a.bot.cooldown",
        "71833afac08ecce50b45407f9dd40892526eec35ca247f7d5ea1a4f5183a5bcf",
    );
    assert!(kv.get(&pinned).await.unwrap().is_some());
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

impl Seen {
    fn take(&self) -> Vec<Refusal> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// The parent's scope and owner-only guard its subcommands; a
/// subcommand's own guards add to them.
#[tokio::test]
async fn the_parents_scope_and_owner_guard_its_subcommands() {
    let runs = Runs::default();
    let seen = Seen::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .refusals(seen.clone())
        .access(AccessList::new().owner(BSUID))
        .command(
            Command::new("admin", runs.handler("admin"))
                .owner_only()
                .subcommand(Command::new("ban", runs.handler("ban"))),
        )
        .command(
            Command::new("poll", runs.handler("poll"))
                .group_only()
                .subcommand(Command::new("close", runs.handler("close"))),
        )
        .command(
            Command::new("shop", runs.handler("shop"))
                .subcommand(Command::new("wipe", runs.handler("wipe")).owner_only())
                .subcommand(Command::new("dm", runs.handler("dm")).private_only()),
        ))
    .await;

    // Not an owner, in a private chat.
    for body in [
        "/admin ban x",
        "/poll close",
        "/shop wipe",
        "/shop dm",
        "/shop",
    ] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    assert_eq!(runs.names(), ["dm", "shop"]);
    assert_eq!(
        seen.take(),
        [Refusal::NotOwner, Refusal::GroupOnly, Refusal::NotOwner]
    );
    // In the group: the group-only parent's subcommand runs, the
    // private-only subcommand is refused.
    bot.handle(text_event(GROUP, "/poll close")).await.unwrap();
    bot.handle(text_event(GROUP, "/shop dm")).await.unwrap();
    assert_eq!(runs.names(), ["close"]);
    assert_eq!(seen.take(), [Refusal::PrivateOnly]);
    // The owner (by BSUID).
    bot.handle(text_event(BSUID_ONLY, "/admin ban x"))
        .await
        .unwrap();
    bot.handle(text_event(BSUID_ONLY, "/shop wipe"))
        .await
        .unwrap();
    assert_eq!(runs.names(), ["ban", "wipe"]);
    assert!(seen.take().is_empty());
}

// ─── Flags ───────────────────────────────────────────────────────────────

/// A flag a flag-declaring command does not declare refuses it, with the
/// usage line and the flags it takes; the refused attempt starts no
/// cooldown.
#[tokio::test]
async fn an_unknown_flag_is_refused_with_the_usage() {
    let clock = ManualClock::new(datetime!(2026-09-26 12:00 UTC));
    let kv = Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone())));
    let runs = Runs::default();
    let out = Recording::default();
    let bot = bot(Bot::builder()
        .outbound(out.clone())
        .cooldown_store(kv, Arc::new(clock))
        .command(
            Command::new("order", runs.handler("order")).subcommand(
                Command::new("status", runs.handler("status"))
                    .usage("<order number> [--dry-run]")
                    .flag("dry-run")
                    .cooldown(Duration::from_secs(30)),
            ),
        )
        .command(
            Command::new("ping", runs.handler("ping"))
                .flag("loud")
                .flag("x"),
        )
        .command(Command::new("say", runs.handler("say")).pass_unknown_flags()))
    .await;

    bot.handle(text_event(TEXT, "/order status 42 --dryrun"))
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/ping --x=1 --y"))
        .await
        .unwrap();
    // A name of 32 characters (not bytes) is repeated; one of 33 is not.
    let short = "é".repeat(32);
    bot.handle(text_event(TEXT, &format!("/ping --{short}")))
        .await
        .unwrap();
    let long = "y".repeat(33);
    bot.handle(text_event(TEXT, &format!("/ping --{long}")))
        .await
        .unwrap();
    assert!(runs.take().is_empty());
    assert_eq!(
        out.bodies(),
        [
            "Unknown option --dryrun.\nUsage: /order status <order number> [--dry-run]\n\
             Options: --dry-run"
                .to_owned(),
            "Unknown option --y.\nOptions: --loud, --x".to_owned(),
            format!("Unknown option --{short}.\nOptions: --loud, --x"),
            "Unknown option.\nOptions: --loud, --x".to_owned(),
        ]
    );
    assert_eq!(out.sent()[0]["context"]["message_id"], message_id(TEXT));

    // No cooldown started: the declared flag runs at once.
    bot.handle(text_event(TEXT, "/order status 42 --dry-run"))
        .await
        .unwrap();
    // Quoted or after `--`, a flag is an argument; `say` takes any flag.
    bot.handle(text_event(TEXT, "/ping \"--y=1\" -- --y"))
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/say --loud hi"))
        .await
        .unwrap();
    assert_eq!(
        runs.take(),
        [
            run("status", Some("status"), &["42"], &["dry-run"]),
            run("ping", None, &["--y=1", "--y"], &[]),
            run("say", None, &["hi"], &["loud"]),
        ]
    );
}

/// Decisive: a command that declares no flag reads none. `/say --hi
/// there` runs with both words as its arguments, as before flags
/// existed, and nothing is refused. Make every command read flags and
/// `--hi` is refused instead.
#[tokio::test]
async fn a_command_without_flags_keeps_dashes_as_arguments() {
    let runs = Runs::default();
    let seen = Seen::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .refusals(seen.clone())
        .command(Command::new("say", runs.handler("say")))
        .command(order(&runs))
        .command(
            Command::new("shop", runs.handler("shop"))
                .subcommand(Command::new("find", runs.handler("find")).flag("all")),
        ))
    .await;

    bot.handle(text_event(TEXT, "/say --hi there"))
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/say -- \"--q\" —x --a=b --"))
        .await
        .unwrap();
    // Each command its own set: `order` declares `--all`, its `cancel`
    // nothing; `shop` nothing, its `find` `--all`.
    bot.handle(text_event(TEXT, "/order cancel --all x"))
        .await
        .unwrap();
    bot.handle(text_event(TEXT, "/order --all")).await.unwrap();
    bot.handle(text_event(TEXT, "/shop --all x")).await.unwrap();
    bot.handle(text_event(TEXT, "/shop find --all x"))
        .await
        .unwrap();
    assert_eq!(
        runs.take(),
        [
            run("say", None, &["--hi", "there"], &[]),
            run("say", None, &["--", "--q", "—x", "--a=b", "--"], &[]),
            run("cancel", Some("cancel"), &["--all", "x"], &[]),
            run("order", None, &[], &["all"]),
            run("shop", None, &["--all", "x"], &[]),
            run("find", Some("find"), &["x"], &["all"]),
        ]
    );
    assert!(seen.take().is_empty());
    // A flag-declaring command still refuses an undeclared one.
    bot.handle(text_event(TEXT, "/shop find --none"))
        .await
        .unwrap();
    assert!(runs.take().is_empty());
    assert_eq!(seen.take(), [Refusal::UnknownFlag { index: 0 }]);
}

/// The flag check comes after the owner check: a non-owner's flag gets
/// the silent owner refusal, never the command's usage.
#[tokio::test]
async fn a_non_owner_is_never_told_an_owner_commands_usage() {
    let runs = Runs::default();
    let seen = Seen::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .refusals(seen.clone())
        .access(AccessList::new().owner(BSUID))
        .command(
            Command::new("wipe", runs.handler("wipe"))
                .owner_only()
                .usage("[--force]")
                .flag("force"),
        ))
    .await;
    bot.handle(text_event(TEXT, "/wipe --everything"))
        .await
        .unwrap();
    bot.handle(text_event(BSUID_ONLY, "/wipe --everything"))
        .await
        .unwrap();
    assert!(runs.take().is_empty());
    assert_eq!(
        seen.take(),
        [Refusal::NotOwner, Refusal::UnknownFlag { index: 0 }]
    );
    // The index is the first undeclared flag's.
    bot.handle(text_event(BSUID_ONLY, "/wipe --force --everything"))
        .await
        .unwrap();
    assert_eq!(seen.take(), [Refusal::UnknownFlag { index: 1 }]);
    // Under an owner-only parent, a subcommand's flags are checked after
    // the parent's owner guard.
    let bot = bot_with_owner_parent(&runs, &seen).await;
    bot.handle(text_event(TEXT, "/admin purge --everything"))
        .await
        .unwrap();
    bot.handle(text_event(BSUID_ONLY, "/admin purge --everything"))
        .await
        .unwrap();
    assert!(runs.take().is_empty());
    assert_eq!(
        seen.take(),
        [Refusal::NotOwner, Refusal::UnknownFlag { index: 0 }]
    );
}

async fn bot_with_owner_parent(runs: &Runs, seen: &Seen) -> Bot {
    bot(Bot::builder()
        .outbound(Recording::default())
        .refusals(seen.clone())
        .access(AccessList::new().owner(BSUID))
        .command(
            Command::new("admin", runs.handler("admin"))
                .owner_only()
                .subcommand(
                    Command::new("purge", runs.handler("purge"))
                        .usage("[--force]")
                        .flag("force"),
                ),
        ))
    .await
}

/// The middleware sees the arguments as the matched command reads them:
/// flags apart for one that declares them, every word positional for one
/// that declares none.
#[tokio::test]
async fn the_middleware_sees_the_commands_own_reading_of_its_arguments() {
    /// The positional arguments and the flags' names one event showed.
    type Shown = (Vec<String>, Vec<String>);
    #[derive(Debug, Clone, Default)]
    struct Peek(Arc<Mutex<Vec<Shown>>>);
    #[async_trait]
    impl Middleware for Peek {
        async fn handle(&self, ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
            let args = ctx.args();
            self.0.lock().unwrap().push((
                args.as_slice().to_vec(),
                args.flags().iter().map(|f| f.name.clone()).collect(),
            ));
            next.run(ctx).await
        }
    }
    let peek = Peek::default();
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .middleware(peek.clone())
        .command(order(&runs))
        .command(Command::new("say", runs.handler("say"))))
    .await;
    for body in [
        "/order --all 1",
        "/order status 42 --dry-run",
        "/order cancel --all",
        "/say --hi there",
    ] {
        bot.handle(text_event(TEXT, body)).await.unwrap();
    }
    let strings = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        *peek.0.lock().unwrap(),
        [
            (strings(&["1"]), strings(&["all"])),
            (strings(&["42"]), strings(&["dry-run"])),
            (strings(&["--all"]), strings(&[])),
            (strings(&["--hi", "there"]), strings(&[])),
        ]
    );
    assert_eq!(runs.names(), ["order", "status", "cancel", "say"]);
}

// ─── The help ────────────────────────────────────────────────────────────

/// Each visible subcommand gets a line under its command, named under
/// the command's name; hidden ones are left out of the help (and still
/// run); Meta's menu lists commands only.
#[tokio::test]
async fn subcommands_show_under_their_command_in_the_help() {
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .command(
            Command::new("order", runs.handler("order"))
                .alias("o")
                .usage("<order number>")
                .description("Your orders")
                .subcommand(
                    Command::new("status", runs.handler("status"))
                        .alias("st")
                        .usage("<order number> [--dry-run]")
                        .description("Where is my order?")
                        .flag("dry-run"),
                )
                .subcommand(Command::new("cancel", runs.handler("cancel")))
                .subcommand(Command::new("debug", runs.handler("debug")).hidden()),
        )
        .command(Command::new("ping", runs.handler("ping")).description("Pong")))
    .await;
    assert_eq!(
        bot.help(),
        "*General*\n/order, /o <order number> — Your orders\n\
         /order status, /order st <order number> [--dry-run] — Where is my order?\n\
         /order cancel\n\
         /ping — Pong"
    );
    let sections = bot.help_sections();
    let names: Vec<_> = sections[0].commands[0]
        .subcommands
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["status", "cancel"]);
    // The registry keeps the hidden one, and its flags and category.
    let order = &bot.commands()[0];
    assert_eq!(order.subcommands.len(), 3);
    assert!(order.subcommands[2].hidden);
    assert_eq!(order.subcommands[0].flags, ["dry-run"]);
    assert_eq!(order.subcommands[0].aliases, ["st"]);
    assert_eq!(order.subcommands[0].category, "General");
    bot.handle(text_event(TEXT, "/o debug")).await.unwrap();
    assert_eq!(runs.names(), ["debug"]);
    // The menu: commands, never subcommands.
    let menu: Vec<_> = bot
        .command_menu()
        .unwrap()
        .into_iter()
        .map(|c| c.command_name)
        .collect();
    assert_eq!(menu, ["order", "ping"]);
}

// ─── Reroutes ────────────────────────────────────────────────────────────

/// A middleware may invoke a subcommand (`Invocation::with_subcommand`);
/// it meets that subcommand's guards and runs its handler. One the
/// command lacks is a configuration error.
#[tokio::test]
async fn a_rerouted_subcommand_runs_and_an_unknown_one_fails() {
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
                Some(sub) => ctx.with_invocation(
                    Invocation::new(
                        "order",
                        Trigger::Text {
                            prefix: String::new(),
                            name: "order".into(),
                        },
                        // Either way: the bot reads them as the command does.
                        if sub == "cancel" {
                            Args::parse_with_flags("7 --dry-run")
                        } else {
                            Args::parse("7 --dry-run")
                        },
                    )
                    .with_subcommand(sub),
                ),
                None => ctx,
            };
            next.run(ctx).await
        }
    }
    let runs = Runs::default();
    let bot = bot(Bot::builder()
        .outbound(Recording::default())
        .errors(PropagateErrors)
        .middleware(Reroute)
        .command(order(&runs)))
    .await;
    // The arguments are read as the command they reach reads them, however
    // the middleware built them: `status` declares `--dry-run`, `cancel`
    // no flag.
    bot.deliver(text_event(TEXT, "go status")).await.unwrap();
    bot.deliver(text_event(TEXT, "go cancel")).await.unwrap();
    assert_eq!(
        runs.take(),
        [
            run("status", Some("status"), &["7"], &["dry-run"]),
            run("cancel", Some("cancel"), &["7", "--dry-run"], &[]),
        ]
    );
    let err = bot
        .deliver(text_event(TEXT, "go nowhere"))
        .await
        .unwrap_err();
    let SinkError::Delivery(err) = err else {
        panic!("{err}")
    };
    let err = err.downcast_ref::<Error>().unwrap();
    assert!(
        matches!(err, Error::Config(e) if e.0.contains("no registered subcommand")),
        "{err}"
    );
    assert!(runs.take().is_empty());
}

// ─── The build ───────────────────────────────────────────────────────────

#[tokio::test]
async fn the_build_refuses_subcommands_and_flags_that_cannot_work() {
    let noop = || |_ctx: Ctx| async move { Ok(()) };
    let config = |builder: BotBuilder| async move {
        match builder.build().await {
            Err(Error::Config(e)) => e.0,
            other => panic!("expected a ConfigError, got {other:?}"),
        }
    };
    let base = || Bot::builder().outbound(Recording::default());
    let parent = || Command::new("order", noop());

    // Nested subcommands.
    assert!(
        config(
            base().command(
                parent().subcommand(
                    Command::new("status", noop()).subcommand(Command::new("x", noop()))
                )
            )
        )
        .await
        .contains("nest one level")
    );
    // A subcommand word twice under one command (case folded).
    assert!(
        config(
            base().command(
                parent()
                    .subcommand(Command::new("status", noop()).alias("s"))
                    .subcommand(Command::new("S", noop()))
            )
        )
        .await
        .contains("subcommand word `s` of `order`")
    );
    // A blank subcommand name.
    assert!(
        config(base().command(parent().subcommand(Command::new("two words", noop()))))
            .await
            .contains("`order two words`")
    );
    // A subcommand's payload taken by a command.
    assert!(
        config(
            base()
                .command(Command::new("a", noop()).payload("x"))
                .command(parent().subcommand(Command::new("b", noop()).payload("x")))
        )
        .await
        .contains("payload `x`")
    );
    // A subcommand's cooldown without a store.
    assert!(
        config(base().command(
            parent().subcommand(Command::new("b", noop()).cooldown(Duration::from_secs(5)))
        ))
        .await
        .contains("`order b` has a cooldown")
    );
    // Flags that could never be typed, and one declared twice.
    for bad in [
        "",
        "--dry-run",
        "-x",
        "—x",
        "dry run",
        "tab\tx",
        "a=b",
        "\"q\"",
        "\u{201C}q",
        "q\u{201D}",
    ] {
        assert!(
            config(base().command(Command::new("a", noop()).flag(bad)))
                .await
                .contains("cannot be typed"),
            "{bad:?}"
        );
    }
    assert!(
        config(base().command(parent().subcommand(Command::new("b", noop()).flag("a b"))))
            .await
            .contains("`order b`: the flag `a b` cannot be typed")
    );
    assert!(
        config(base().command(parent().subcommand(Command::new("b", noop()).flag("x").flag("x"))))
            .await
            .contains("`order b`: the flag `x` is declared twice")
    );
    // A subcommand may share a command's name: it is read under its parent.
    base()
        .command(Command::new("status", noop()))
        .command(parent().subcommand(Command::new("status", noop())))
        .build()
        .await
        .unwrap();
}
