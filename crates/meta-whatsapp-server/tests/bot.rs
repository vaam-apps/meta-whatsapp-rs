//! Roadmap M5k, slice 2: the bot over HTTP (docs/design/server.md, section
//! 4.2, D460 to D467). The rules a tenant keeps for a number are checked,
//! kept and tried in order on the number's inbound messages (M5.1, M5.2); a
//! matching message is answered exactly once under every redelivery, its
//! cooldown is respected under a fake clock, a free-form answer never goes
//! outside the 24-hour window, and a number never answers from a rule of
//! another tenant.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::bot::*;
use common::meta::{bytes, sent_now};
use common::{Call, Harness};
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::transport::{HttpRequest, HttpResponse, HttpTransport};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_rs::webhooks::{WebhookPayload, events};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server_core::bot::BotSettings;
use meta_whatsapp_server_core::inbound::MessageActions;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// A rule is made, listed in the order it is tried (priority, then age),
/// read, replaced and deleted; a number never configured reads as off.
/// Decisive: `Bot::create_rule`, `replace_rule`, `delete_rule` and the
/// stored order.
#[tokio::test]
async fn rules_are_created_listed_replaced_and_deleted() {
    let w = world().await;
    let fresh = w.h.call(settings(&w.key, PN)).await;
    assert_eq!(fresh.status, StatusCode::OK, "{}", fresh.text);
    assert_eq!(
        fresh.json(),
        json!({"enabled": false, "owners": [], "banned": [], "rules": 0, "max_rules": 50})
    );

    let first =
        w.h.call(create(&w.key, PN, &command("Help", "hello")))
            .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    let first = first.json();
    let id = first["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("br_") && id.len() == 35, "{id}");
    // Normalised: words in lower case, the default prefix, the defaults.
    assert_eq!(
        first["trigger"],
        json!({"type": "command", "names": ["help"], "prefixes": ["/"]})
    );
    assert_eq!(first["action"], json!({"type": "reply", "text": "hello"}));
    assert_eq!(first["enabled"], true);
    assert_eq!(first["priority"], 100);
    assert_eq!(first["chat"], "private");
    assert_eq!(first["owners_only"], false);
    assert_eq!(first["continue_after"], false);
    assert!(first["created_at"].is_string());

    let mut early = command("menu", "the menu");
    early["priority"] = json!(10);
    early["description"] = json!("Today's menu");
    let early = w.h.call(create(&w.key, PN, &early)).await.json();
    let listed = w.h.call(rules(&w.key, PN)).await.json();
    let order: Vec<&str> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, [early["id"].as_str().unwrap(), id.as_str()]);

    let read = w.h.call(Call::get(rule_path(PN, &id)).key(&w.key)).await;
    assert_eq!(read.json(), first);

    let replaced =
        w.h.call(
            Call::new(Method::PUT, rule_path(PN, &id))
                .key(&w.key)
                .json(&command("help", "hello again")),
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.text);
    assert_eq!(replaced.json()["id"], id.as_str());
    assert_eq!(replaced.json()["created_at"], first["created_at"]);
    assert_eq!(replaced.json()["action"]["text"], "hello again");

    let gone =
        w.h.call(Call::new(Method::DELETE, rule_path(PN, &id)).key(&w.key))
            .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.text);
    for call in [
        Call::get(rule_path(PN, &id)).key(&w.key),
        Call::new(Method::DELETE, rule_path(PN, &id)).key(&w.key),
        Call::new(Method::PUT, rule_path(PN, &id))
            .key(&w.key)
            .json(&command("help", "x")),
    ] {
        assert_eq!(w.h.call(call).await.status, StatusCode::NOT_FOUND);
    }
    // An id that is not a rule's shape is as missing as one never made.
    assert_eq!(
        w.h.call(Call::get(rule_path(PN, "x")).key(&w.key))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(w.h.call(settings(&w.key, PN)).await.json()["rules"], 1);
}

/// Each invalid rule is a `422` naming its field and quoting nothing; the
/// number's limit of rules is one; a key without the scope is `403`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of refusals, then the limits
async fn a_rule_is_checked_and_bounded() {
    let w = world_with(BotSettings {
        max_rules: 2,
        ..BotSettings::default()
    })
    .await;
    let field = |reply: common::Reply| {
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.text
        );
        assert_eq!(reply.code(), "invalid_request");
        reply.json()["error"]["field"].as_str().map(str::to_owned)
    };
    let with = |change: &dyn Fn(&mut Value)| {
        let mut body = command("help", "hello");
        change(&mut body);
        body
    };
    for (body, expected) in [
        (
            with(&|b| b["trigger"] = json!({"type": "regex", "pattern": "(a+)+$"})),
            "body",
        ),
        (with(&|b| b["stray"] = json!(1)), "body"),
        (
            with(&|b| b["trigger"]["names"] = json!([])),
            "trigger.names",
        ),
        (
            with(&|b| b["trigger"]["names"] = json!(["a b"])),
            "trigger.names",
        ),
        (
            with(&|b| b["trigger"]["prefixes"] = json!(["a"])),
            "trigger.prefixes",
        ),
        (
            with(&|b| b["action"]["text"] = json!("x".repeat(4097))),
            "action.text",
        ),
        (with(&|b| b["action"]["text"] = json!(" ")), "action.text"),
        (
            with(&|b| b["cooldown_seconds"] = json!(0)),
            "cooldown_seconds",
        ),
        (with(&|b| b["priority"] = json!(10_001)), "priority"),
        (with(&|b| b["name"] = json!("")), "name"),
        (
            with(&|b| {
                b["action"] =
                    json!({"type": "template", "template": {"language": {"code": "en_US"}}});
            }),
            "action.template",
        ),
        (
            with(&|b| {
                b["trigger"] = json!({"type": "any"});
                b["action"] = json!({"type": "help"});
            }),
            "action",
        ),
    ] {
        let reply = w.h.call(create(&w.key, PN, &body)).await;
        let text = reply.text.clone();
        assert_eq!(field(reply).as_deref(), Some(expected), "{body}: {text}");
    }
    add(&w, &command("one", "1")).await;
    add(&w, &command("two", "2")).await;
    assert_eq!(
        field(w.h.call(create(&w.key, PN, &command("three", "3"))).await).as_deref(),
        Some("rules")
    );
    // The lists: normalised, bounded.
    let lists = w
        .h
        .call(put(
            &w.key,
            PN,
            &json!({"enabled": true, "owners": ["+16505551234", "16505551234"], "banned": ["US.1"]}),
        ))
        .await;
    assert_eq!(lists.status, StatusCode::OK, "{}", lists.text);
    assert_eq!(lists.json()["owners"], json!(["16505551234"]));
    assert_eq!(lists.json()["banned"], json!(["US.1"]));
    assert_eq!(
        field(
            w.h.call(put(
                &w.key,
                PN,
                &json!({"enabled": true, "owners": ["a b"]})
            ))
            .await
        )
        .as_deref(),
        Some("owners")
    );
    assert_eq!(
        field(w.h.call(put(&w.key, PN, &json!({"owners": []}))).await).as_deref(),
        Some("body")
    );
    // The scope: a key allowed to send is not thereby allowed a bot.
    let sender =
        w.h.tenant_key(TENANT, &[Scope::Send, Scope::Broadcasts])
            .await;
    let refused = w.h.call(rules(&sender, PN)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.code(), "forbidden");
}

/// An `Idempotency-Key` on a rule's creation: the same key and body add
/// one rule, another body under it is refused.
#[tokio::test]
async fn a_rule_created_twice_under_one_key_is_one_rule() {
    let w = world().await;
    let call = |body: &Value| create(&w.key, PN, body).header("idempotency-key", "k-1");
    let first = w.h.call(call(&command("help", "hello"))).await;
    let again = w.h.call(call(&command("help", "hello"))).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.text);
    assert_eq!(first.json()["id"], again.json()["id"]);
    assert_eq!(w.h.call(settings(&w.key, PN)).await.json()["rules"], 1);
    let other = w.h.call(call(&command("help", "another"))).await;
    assert_eq!(other.code(), "idempotency_key_reused");
}

/// M5.1 and M5.2 over the routes: another tenant's number is `404` on
/// every route before anything is read, a rule of one tenant's number is
/// not found through another's, and nothing of the first changes. Decisive:
/// `Bot::number` (delete it and the second tenant reads and writes the
/// first's bot).
#[tokio::test]
async fn another_tenants_bot_is_not_found() {
    let w = world().await;
    let id = add(&w, &command("help", "hello")).await;
    let rival = w.h.tenant_key(OTHER_TENANT, &[Scope::Bot]).await;
    let any = command("help", "stolen");
    let calls = [
        settings(&rival, PN),
        put(
            &rival,
            PN,
            &json!({"enabled": true, "banned": ["16505551234"]}),
        ),
        rules(&rival, PN),
        create(&rival, PN, &any),
        // A body that is no rule: still `404` (the number first), never `422`.
        create(&rival, PN, &json!({"trigger": 5})),
        put(&rival, PN, &json!({"owners": 7})),
        Call::get(rule_path(PN, &id)).key(&rival),
        Call::new(Method::PUT, rule_path(PN, &id))
            .key(&rival)
            .json(&any),
        Call::new(Method::PUT, rule_path(PN, &id))
            .key(&rival)
            .json(&json!({"trigger": 5})),
        Call::new(Method::DELETE, rule_path(PN, &id)).key(&rival),
        // Their own number, the first's rule.
        Call::get(rule_path(OTHER_PN, &id)).key(&rival),
        Call::new(Method::PUT, rule_path(OTHER_PN, &id))
            .key(&rival)
            .json(&any),
        Call::new(Method::DELETE, rule_path(OTHER_PN, &id)).key(&rival),
    ];
    for call in calls {
        let reply = w.h.call(call).await;
        let own_number = reply.status == StatusCode::OK || reply.status == StatusCode::CREATED;
        if own_number {
            continue;
        }
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    }
    // The first tenant's bot is as it was.
    let mine = w.h.call(rules(&w.key, PN)).await.json();
    assert_eq!(mine["data"].as_array().unwrap().len(), 1, "{mine}");
    assert_eq!(mine["data"][0]["action"]["text"], "hello");
    let status = w.h.call(settings(&w.key, PN)).await.json();
    assert_eq!(status["enabled"], false, "{status}");
    assert_eq!(status["banned"], json!([]), "{status}");
    // And the rival's own number holds no rule of the first's.
    let theirs = w.h.call(rules(&rival, OTHER_PN)).await.json();
    assert_eq!(theirs["data"], json!([]));
}

/// A matching message is answered once: Meta's documented request, the
/// number's token, the conversation's customer, the rule's callback data;
/// the same delivery again, a delivery that failed before the bot and was
/// redelivered, and the same event handed to the bot twice answer once.
/// Decisive: the marker (`put_if_absent` in `BotReplies::answer`): delete
/// it and the direct hand-over answers twice; the lease alone is not the
/// guard.
#[tokio::test]
async fn a_matching_message_is_answered_once_under_redelivery() {
    let w = world().await;
    let id = add(&w, &command("hello", "Hi! How can we help?")).await;
    enable(&w).await;
    script(&w.h, 5);

    // Delivered: one reply.
    let body = inbound("wamid.IN1", "/Hello there");
    deliver(&w.h, &body).await;
    let sent = bodies(&w.h);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["messaging_product"], "whatsapp");
    assert_eq!(sent[0]["type"], "text");
    assert_eq!(sent[0]["text"]["body"], "Hi! How can we help?");
    assert_eq!(sent[0]["biz_opaque_callback_data"], format!("wa-bot:{id}"));
    let request = &w.h.graph.requests()[0];
    assert_eq!(request.method, Method::POST);
    assert!(
        request.url.path().ends_with(&format!("/{PN}/messages")),
        "{}",
        request.url
    );
    assert_eq!(
        request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    // The reply is in the inbox, recorded `accepted`.
    let history =
        w.h.call(
            Call::get(format!(
                "/v1/numbers/{PN}/conversations/{CUSTOMER}/messages"
            ))
            .key(&w.h.tenant_key(TENANT, &[Scope::Inbox]).await),
        )
        .await
        .json();
    assert_eq!(history["data"].as_array().unwrap().len(), 2, "{history}");

    // Meta delivers the same body again: the lease skips it.
    deliver(&w.h, &body).await;
    assert_eq!(w.h.graph.requests().len(), 1);

    // A delivery that failed (the outbox is down) is redelivered: the
    // message is recorded once and answered once.
    let second = inbound("wamid.IN2", "/hello");
    w.h.outbox.fail_next(1);
    let failed = w.h.webhook(&second).await;
    assert_eq!(
        failed.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        failed.text
    );
    assert_eq!(w.h.graph.requests().len(), 1, "the bot runs once recorded");
    deliver(&w.h, &second).await;
    deliver(&w.h, &second).await;
    assert_eq!(w.h.graph.requests().len(), 2);

    // The bot handed the very same event twice (a retry after a failure
    // past the lease, a second replica): the marker answers once.
    let third = events(&WebhookPayload::from_slice(&inbound("wamid.IN3", "/hello")).unwrap());
    let event = third.into_iter().next().unwrap();
    let tenant = meta_whatsapp_server::model::TenantId::parse(TENANT).unwrap();
    let bot = w.h.grant.bot_replies();
    // The message must be in the inbox for the window: record it first.
    deliver(&w.h, &inbound("wamid.IN3", "/hello")).await;
    assert_eq!(w.h.graph.requests().len(), 3);
    bot.on_message(&tenant, &event).await.unwrap();
    bot.on_message(&tenant, &event).await.unwrap();
    assert_eq!(w.h.graph.requests().len(), 3, "answered again");
}

/// A cooldown holds a person back from the same rule for its period under
/// a fake clock, and only that rule and that person: the next person, and
/// the same person after the period, are answered. Decisive:
/// `KvCooldowns::try_start` in `BotReplies::answer`.
#[tokio::test]
async fn a_cooldown_is_respected_under_a_fake_clock() {
    let w = world().await;
    let mut rule = command("promo", "20% off today");
    rule["cooldown_seconds"] = json!(60);
    add(&w, &rule).await;
    add(&w, &command("hours", "9 to 5")).await;
    enable(&w).await;
    script(&w.h, 8);
    deliver(&w.h, &inbound("wamid.C1", "/promo")).await;
    assert_eq!(w.h.graph.requests().len(), 1);
    // Again within the minute: nothing; another rule of theirs: answered.
    w.clock.advance(Duration::from_secs(30));
    deliver(&w.h, &inbound("wamid.C2", "/promo")).await;
    assert_eq!(w.h.graph.requests().len(), 1, "cooldown ignored");
    deliver(&w.h, &inbound("wamid.C3", "/hours")).await;
    assert_eq!(w.h.graph.requests().len(), 2);
    // Another person is not held back by theirs.
    deliver(&w.h, &inbound_from("16505550000", "wamid.C4", "/promo")).await;
    assert_eq!(w.h.graph.requests().len(), 3);
    // Past the minute, the first person is answered again.
    w.clock.advance(Duration::from_secs(31));
    deliver(&w.h, &inbound("wamid.C5", "/promo")).await;
    assert_eq!(w.h.graph.requests().len(), 4);
}

/// Outside the 24-hour window a free-form answer is not sent (the inbox's
/// guard, no request); a template the rule names for that case is. The
/// message is an old one the deployment chose to answer. Decisive: the
/// window check of `Conversations::reply`, and `outside_window_template`.
#[tokio::test]
async fn outside_the_window_there_is_no_free_form_answer() {
    let w = world_with(BotSettings {
        max_message_age: Duration::from_hours(72),
        ..BotSettings::default()
    })
    .await;
    add(&w, &command("hello", "free-form")).await;
    let mut fallback = command("hi", "free-form");
    fallback["action"]["outside_window_template"] =
        json!({"name": "we_are_here", "language": {"code": "en_US"}});
    add(&w, &fallback).await;
    add(&w, &json!({
        "trigger": {"type": "keyword", "words": ["order"], "mode": "contains"},
        "action": {"type": "template", "template": {"name": "order_status", "language": {"code": "en_US"}}},
    }))
    .await;
    enable(&w).await;
    script(&w.h, 3);
    // 25 hours after the customer wrote: the window is closed.
    w.clock.advance(Duration::from_hours(25));
    deliver(&w.h, &inbound("wamid.W1", "/hello")).await;
    assert!(
        w.h.graph.requests().is_empty(),
        "a free-form answer went out"
    );
    deliver(&w.h, &inbound("wamid.W2", "/hi")).await;
    let sent = bodies(&w.h);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["type"], "template");
    assert_eq!(sent[0]["template"]["name"], "we_are_here");
    // A template rule needs no window at all.
    deliver(&w.h, &inbound("wamid.W3", "where is my order")).await;
    let sent = bodies(&w.h);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[1]["template"]["name"], "order_status");
}

/// An outage's backlog is not answered: a message older than the age the
/// deployment allows (an hour by default) gets nothing.
#[tokio::test]
async fn a_backlog_is_not_answered() {
    let w = world().await;
    add(&w, &command("hello", "hi")).await;
    enable(&w).await;
    w.clock.advance(Duration::from_hours(2));
    deliver(&w.h, &inbound("wamid.O1", "/hello")).await;
    assert!(w.h.graph.requests().is_empty());
}

/// A number never answers from another tenant's rules, and never as
/// another tenant: after the number is bound to another tenant, the first
/// one's rules are inert (the new holder reads an empty, switched-off bot),
/// and a bot that is switched off, empty or banned for the sender sends
/// nothing. Decisive: the tenant compare in `BotReplies::handle` (delete it
/// and the second tenant's customers are answered with the first's text),
/// and `Authorizer::bot_sender`.
#[tokio::test]
async fn a_rebound_number_never_answers_from_the_first_tenants_rules() {
    let w = world().await;
    add(&w, &command("hello", "from the first tenant")).await;
    enable(&w).await;
    script(&w.h, 4);
    deliver(&w.h, &inbound("wamid.R1", "/hello")).await;
    assert_eq!(w.h.graph.requests().len(), 1);

    // The WABA moves to the other tenant (unbound, then bound again).
    let second = meta_whatsapp_server::model::TenantId::parse(OTHER_TENANT).unwrap();
    let waba = meta_whatsapp_rs::core::ids::WabaId::new(WABA).unwrap();
    let number = meta_whatsapp_rs::core::ids::PhoneNumberId::new(PN).unwrap();
    assert!(w.h.store.unbind_waba(&waba).await.unwrap());
    w.h.store
        .bind_waba(&second, &waba, &[number])
        .await
        .unwrap();
    w.h.vault
        .store(
            &meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken::new(
                waba.clone(),
                meta_whatsapp_rs::core::secret::AccessToken::new("EAAG-second"),
            )
            .phone_number_ids(vec![
                meta_whatsapp_rs::core::ids::PhoneNumberId::new(PN).unwrap(),
            ]),
        )
        .await
        .unwrap();
    // The rebinding is dated a second later than the first message.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let mut late = common::meta::text(WABA, PN, "wamid.R2");
    late = common::meta::dated(late, sent_now() + 2);
    late["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = json!("/hello");
    deliver(&w.h, &bytes(&late)).await;
    assert_eq!(
        w.h.graph.requests().len(),
        1,
        "the number answered from the first tenant's rules"
    );
    // The new holder reads an empty, switched-off bot; the first reads none.
    let rival = w.h.tenant_key(OTHER_TENANT, &[Scope::Bot]).await;
    let seen = w.h.call(settings(&rival, PN)).await.json();
    assert_eq!(seen["enabled"], false, "{seen}");
    assert_eq!(seen["rules"], 0, "{seen}");
    assert_eq!(
        w.h.call(settings(&w.key, PN)).await.status,
        StatusCode::NOT_FOUND
    );
    // Every request that went out used the first tenant's token, once.
    let tokens: Vec<_> =
        w.h.graph
            .requests()
            .iter()
            .map(|r| {
                r.headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
    assert!(tokens.iter().all(|t| t.contains(TOKEN)), "{tokens:?}");
}

/// The switch, the ban list, the owners, the chat kind: a bot that is off
/// sends nothing; a banned sender gets nothing (BSUID or phone); an
/// `owners_only` rule is the owners'; a `private` rule does not answer a
/// group.
#[tokio::test]
async fn the_switch_the_lists_and_the_guards_decide_who_is_answered() {
    let w = world().await;
    let mut admin = command("admin", "admin menu");
    admin["owners_only"] = json!(true);
    add(&w, &admin).await;
    add(&w, &command("hello", "hi")).await;
    script(&w.h, 6);
    // Off: nothing.
    deliver(&w.h, &inbound("wamid.S1", "/hello")).await;
    assert!(w.h.graph.requests().is_empty());
    // On: answered; not an owner: the owners' rule is not.
    let on = |owners: &[&str], banned: &[&str]| {
        put(
            &w.key,
            PN,
            &json!({"enabled": true, "owners": owners, "banned": banned}),
        )
    };
    assert_eq!(w.h.call(on(&[], &[])).await.status, StatusCode::OK);
    deliver(&w.h, &inbound("wamid.S2", "/hello")).await;
    assert_eq!(w.h.graph.requests().len(), 1);
    deliver(&w.h, &inbound("wamid.S3", "/admin")).await;
    assert_eq!(
        w.h.graph.requests().len(),
        1,
        "a non-owner ran an owner rule"
    );
    // An owner, named with a `+`.
    assert_eq!(
        w.h.call(on(&["+16505551234"], &[])).await.status,
        StatusCode::OK
    );
    deliver(&w.h, &inbound("wamid.S4", "/admin")).await;
    assert_eq!(w.h.graph.requests().len(), 2);
    // Banned by phone: nothing, not even the plain rule.
    assert_eq!(
        w.h.call(on(&[], &["16505551234"])).await.status,
        StatusCode::OK
    );
    deliver(&w.h, &inbound("wamid.S5", "/hello")).await;
    assert_eq!(
        w.h.graph.requests().len(),
        2,
        "a banned sender was answered"
    );
    // A group message is not a private chat's.
    assert_eq!(w.h.call(on(&[], &[])).await.status, StatusCode::OK);
    let mut group = common::meta::text(WABA, PN, "wamid.S6");
    group["entry"][0]["changes"][0]["value"]["messages"][0]["text"]["body"] = json!("/hello");
    group["entry"][0]["changes"][0]["value"]["messages"][0]["group_id"] =
        json!("120363000000000000@g.us");
    deliver(&w.h, &bytes(&group)).await;
    assert_eq!(
        w.h.graph.requests().len(),
        2,
        "a private rule answered a group"
    );
}

/// Keywords, a tapped button, `any` and the generated help, end to end;
/// the first rule that answers decides, and `continue_after` lets the next
/// one answer too.
#[tokio::test]
async fn rules_are_tried_in_order_and_the_first_answer_decides() {
    let w = world().await;
    let mut menu = command("menu", "the menu");
    menu["description"] = json!("Show the menu");
    menu["priority"] = json!(1);
    add(&w, &menu).await;
    add(
        &w,
        &json!({
            "trigger": {"type": "command", "names": ["help"]},
            "action": {"type": "help"},
            "priority": 2,
        }),
    )
    .await;
    add(
        &w,
        &json!({
            "trigger": {"type": "keyword", "words": ["Hours"], "mode": "exact"},
            "action": {"type": "reply", "text": "9 to 5"},
            "priority": 3,
        }),
    )
    .await;
    add(
        &w,
        &json!({
            "trigger": {"type": "payload", "ids": ["MENU_BTN"]},
            "action": {"type": "reply", "text": "button tapped"},
            "priority": 4,
        }),
    )
    .await;
    let mut greet = json!({
        "trigger": {"type": "keyword", "words": ["hello"], "mode": "contains"},
        "action": {"type": "reply", "text": "greeting"},
        "priority": 5,
        "continue_after": true,
    });
    greet["enabled"] = json!(true);
    add(&w, &greet).await;
    add(
        &w,
        &json!({
            "trigger": {"type": "any"},
            "action": {"type": "reply", "text": "fallback"},
            "priority": 6,
        }),
    )
    .await;
    enable(&w).await;
    script(&w.h, 12);
    let texts = |h: &Harness| -> Vec<String> {
        bodies(h)
            .iter()
            .map(|b| b["text"]["body"].as_str().unwrap_or("").to_owned())
            .collect()
    };
    deliver(&w.h, &inbound("wamid.T1", "/HELP")).await;
    assert_eq!(texts(&w.h), ["/menu - Show the menu"]);
    deliver(&w.h, &inbound("wamid.T2", "hours")).await;
    deliver(&w.h, &tapped("wamid.T3", "MENU_BTN")).await;
    // `hello` continues into `any`: two answers.
    deliver(&w.h, &inbound("wamid.T4", "well hello!")).await;
    // Something that matches only `any`; and an image reaches it too.
    deliver(&w.h, &inbound("wamid.T5", "xyz")).await;
    deliver(&w.h, &image("wamid.T6")).await;
    assert_eq!(
        texts(&w.h),
        [
            "/menu - Show the menu",
            "9 to 5",
            "button tapped",
            "greeting",
            "fallback",
            "fallback",
            "fallback",
        ]
    );
}

/// Meta refusing an answer neither fails the delivery nor repeats it, and
/// a number with no token answers nothing. The failure is not retried:
/// the answer was claimed.
#[tokio::test]
async fn a_refused_answer_is_not_retried_and_does_not_fail_the_delivery() {
    let w = world().await;
    add(&w, &command("hello", "hi")).await;
    enable(&w).await;
    w.h.graph.push_json(
        400,
        json!({"error": {"message": "(#131047) Re-engagement message", "type": "OAuthException",
                         "code": 131_047, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let body = inbound("wamid.F1", "/hello");
    deliver(&w.h, &body).await;
    assert_eq!(w.h.graph.requests().len(), 1);
    deliver(&w.h, &body).await;
    assert_eq!(w.h.graph.requests().len(), 1, "retried");
}

/// Replies draw from the number's budget, the one broadcasts share: at a
/// rate of one a second, two answers are a second apart on the fake clock.
#[tokio::test]
async fn answers_wait_for_the_numbers_budget() {
    let clock = meta_whatsapp_rs::core::clock::ManualClock::new(time::OffsetDateTime::now_utc());
    let settings = meta_whatsapp_server::state::Settings {
        broadcasts: meta_whatsapp_server_core::broadcasts::BroadcastSettings {
            rate_per_second: 1,
            timer: Some(std::sync::Arc::new(clock.clone())),
            ..Default::default()
        },
        ..common::test_settings()
    };
    let h = Harness::with_clock(common::Stores::memory(), settings, clock.clone(), |g| {
        std::sync::Arc::new(g)
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Bot]).await;
    let w = World { h, key, clock };
    add(&w, &command("hello", "hi")).await;
    enable(&w).await;
    script(&w.h, 3);
    let started = w.clock.now();
    for n in 0..3 {
        deliver(&w.h, &inbound(&format!("wamid.B{n}"), "/hello")).await;
    }
    assert_eq!(w.h.graph.requests().len(), 3);
    assert!(
        w.clock.now() - started >= time::Duration::seconds(2),
        "three answers at one a second took {}",
        w.clock.now() - started
    );
}

/// Meta's text example with its first message changed by `change`.
fn edited(wamid: &str, change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut payload = common::meta::text(WABA, PN, wamid);
    change(&mut payload["entry"][0]["changes"][0]["value"]["messages"][0]);
    bytes(&payload)
}

/// What is not a message to answer is never answered, not even by `any`:
/// a reaction, a deletion, a system message, an unsupported type, a status
/// webhook (the bot's own answers' echo) and the number's own message.
/// Decisive: `input_of`'s `_ => None`, the self guard in
/// `BotReplies::handle`; delete either and a loop or a spurious answer
/// appears.
#[tokio::test]
async fn what_is_not_a_message_to_answer_is_never_answered() {
    let w = world().await;
    add(
        &w,
        &json!({"trigger": {"type": "any"}, "action": {"type": "reply", "text": "hi"}}),
    )
    .await;
    enable(&w).await;
    script(&w.h, 8);
    let strip = |m: &mut Value| {
        m.as_object_mut().unwrap().remove("text");
    };
    deliver(
        &w.h,
        &edited("wamid.X1", |m| {
            strip(m);
            m["type"] = json!("reaction");
            m["reaction"] = json!({"message_id": "wamid.OLD", "emoji": "\u{1F44D}"});
        }),
    )
    .await;
    deliver(
        &w.h,
        &edited("wamid.X2", |m| {
            strip(m);
            m["type"] = json!("revoke");
            m["revoke"] = json!({"original_message_id": "wamid.OLD"});
        }),
    )
    .await;
    deliver(
        &w.h,
        &edited("wamid.X3", |m| {
            strip(m);
            m["type"] = json!("system");
            m["system"] = json!({"body": "changed", "type": "user_changed_number", "wa_id": "16505550000"});
        }),
    )
    .await;
    deliver(
        &w.h,
        &edited("wamid.X4", |m| {
            strip(m);
            m["type"] = json!("a_type_nobody_knows");
        }),
    )
    .await;
    // A status webhook (what the bot's own answers raise).
    let status = bytes(&common::meta::delivered(WABA, PN, "wamid.BOT0"));
    deliver(&w.h, &status).await;
    // The number's own message: its display number is the sender.
    let own = common::meta::text(WABA, PN, "wamid.X5");
    let display = own["entry"][0]["changes"][0]["value"]["metadata"]["display_phone_number"]
        .as_str()
        .unwrap()
        .to_owned();
    let digits: String = display.chars().filter(char::is_ascii_digit).collect();
    deliver(&w.h, &inbound_from(&digits, "wamid.X5", "hello myself")).await;
    assert!(
        w.h.graph.requests().is_empty(),
        "answered what is not a message: {:?}",
        bodies(&w.h)
    );
    // The control: a text from a customer is answered.
    deliver(&w.h, &inbound("wamid.X6", "hello")).await;
    assert_eq!(w.h.graph.requests().len(), 1);
}

/// A `payload` rule answers a tapped button or row, never a text that
/// spells the id: a customer cannot forge a tap by typing. Decisive:
/// `Input::Text` never matches `Trigger::Payload`.
#[tokio::test]
async fn a_typed_button_id_is_not_a_tap() {
    let w = world().await;
    add(
        &w,
        &json!({"trigger": {"type": "payload", "ids": ["MENU_BTN"]},
                "action": {"type": "reply", "text": "button tapped"}}),
    )
    .await;
    enable(&w).await;
    script(&w.h, 2);
    deliver(&w.h, &inbound("wamid.P1", "MENU_BTN")).await;
    assert!(w.h.graph.requests().is_empty(), "a typed id was a tap");
    deliver(&w.h, &tapped("wamid.P2", "MENU_BTN")).await;
    assert_eq!(w.h.graph.requests().len(), 1);
}

/// A sender is known by its BSUID as by its phone: a ban by BSUID alone
/// stops a message that carries both, an owner by BSUID alone passes an
/// `owners_only` rule, and a stranger with neither does not.
#[tokio::test]
async fn senders_are_known_by_their_bsuid() {
    let w = world().await;
    let mut admin = command("admin", "admin menu");
    admin["owners_only"] = json!(true);
    add(&w, &admin).await;
    add(&w, &command("hello", "hi")).await;
    script(&w.h, 6);
    let from_user = |wamid: &str, body: &str, user: &str| {
        edited(wamid, |m| {
            m["text"]["body"] = json!(body);
            m["from_user_id"] = json!(user);
        })
    };
    let on = |owners: &[&str], banned: &[&str]| {
        put(
            &w.key,
            PN,
            &json!({"enabled": true, "owners": owners, "banned": banned}),
        )
    };
    assert_eq!(w.h.call(on(&[CUSTOMER_BSUID], &[])).await.status, StatusCode::OK);
    deliver(&w.h, &from_user("wamid.U1", "/admin", CUSTOMER_BSUID)).await;
    assert_eq!(w.h.graph.requests().len(), 1, "an owner by BSUID");
    deliver(&w.h, &from_user("wamid.U2", "/admin", "US.99999999999")).await;
    // The phone of this message is the customer's, not an owner's entry.
    assert_eq!(w.h.graph.requests().len(), 1, "a stranger ran an owner rule");
    assert_eq!(w.h.call(on(&[], &[CUSTOMER_BSUID])).await.status, StatusCode::OK);
    deliver(&w.h, &from_user("wamid.U3", "/hello", CUSTOMER_BSUID)).await;
    assert_eq!(w.h.graph.requests().len(), 1, "a banned BSUID was answered");
    deliver(&w.h, &from_user("wamid.U4", "/hello", "US.99999999999")).await;
    assert_eq!(w.h.graph.requests().len(), 2);
}

/// The floor under every rule: a sender gets a dozen answers a minute from
/// a number, however many rules and messages (two bots answering each
/// other), and the next minute starts again. Decisive:
/// `BotReplies::within_floor`.
#[tokio::test]
async fn a_sender_cannot_drive_unbounded_answers() {
    let w = world().await;
    add(
        &w,
        &json!({"trigger": {"type": "any"}, "action": {"type": "reply", "text": "hi"}}),
    )
    .await;
    enable(&w).await;
    script(&w.h, 40);
    let most = meta_whatsapp_server_core::bot::SENDER_ANSWERS_PER_MINUTE as usize;
    w.clock.advance(Duration::from_secs(
        60 - w.clock.now().unix_timestamp().rem_euclid(60) as u64,
    ));
    for n in 0..most + 5 {
        deliver(&w.h, &inbound(&format!("wamid.L{n}"), "ping")).await;
    }
    assert_eq!(w.h.graph.requests().len(), most);
    // Another person is not held back; the next minute starts again.
    deliver(&w.h, &inbound_from("16505550000", "wamid.L90", "ping")).await;
    assert_eq!(w.h.graph.requests().len(), most + 1);
    w.clock.advance(Duration::from_secs(60));
    deliver(&w.h, &inbound("wamid.L91", "ping")).await;
    assert_eq!(w.h.graph.requests().len(), most + 2);
}

/// A transport whose requests never come back.
struct Hangs {
    sent: Arc<AtomicUsize>,
}

impl std::fmt::Debug for Hangs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hangs").finish()
    }
}

#[async_trait]
impl HttpTransport for Hangs {
    async fn send(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

/// A send that never returns cannot hold a delivery past the bot's budget:
/// the delivery is answered `200`, and a redelivery does not send again
/// (the answer was claimed). Decisive: the `BUDGET` timeout in
/// `MessageActions::on_message`; delete it and the delivery waits for the
/// request deadline (and then fails).
#[tokio::test(start_paused = true)]
async fn a_hung_send_does_not_hold_the_delivery() {
    let clock = meta_whatsapp_rs::core::clock::ManualClock::new(time::OffsetDateTime::now_utc());
    let sent = Arc::new(AtomicUsize::new(0));
    let hang = Hangs { sent: sent.clone() };
    let h = Harness::with_clock(
        common::Stores::memory(),
        common::test_settings(),
        clock.clone(),
        move |_graph| Arc::new(hang),
    );
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Bot]).await;
    let w = World { h, key, clock };
    add(&w, &command("hello", "hi")).await;
    enable(&w).await;
    let body = inbound("wamid.H1", "/hello");
    let started = tokio::time::Instant::now();
    deliver(&w.h, &body).await;
    let took = started.elapsed();
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    assert!(
        took >= meta_whatsapp_server_core::bot::BUDGET
            && took < meta_whatsapp_server_core::bot::BUDGET + Duration::from_secs(5),
        "the delivery took {took:?}"
    );
    // Meta redelivers the same message under another delivery: claimed.
    deliver(&w.h, &body).await;
    assert_eq!(sent.load(Ordering::SeqCst), 1);
}
