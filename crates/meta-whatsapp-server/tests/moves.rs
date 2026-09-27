//! Roadmap S2, the security review's M1 and L1: a WABA moving to another
//! tenant while a request acts on it. A capability's token never
//! unsubscribes the new holder's app, a tenant's deletion skips the WABA
//! rather than answering for it, and the admin unbind never deletes the
//! new holder's binding. The move lands at a chosen read
//! (`common::moving`), on memory: what is checked here is the routes'
//! handling, the backends' atomic parts being the store suites'.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;

use common::capture::{Captured, subscriber};
use common::moving::{Hook, Moving, move_waba};
use common::{Call, Harness};
use meta_whatsapp_rs::adapters::store::MemoryKvStore;
use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{NumberStatus, Scope, TenantId};
use meta_whatsapp_server::store::MemoryStore;
use pretty_assertions::assert_eq;
use serde_json::json;

const WABA: &str = "102290129340398";
const PN: &str = "1972385232742141";

struct Setup {
    h: Harness,
    moving: Arc<Moving>,
    admin: String,
    /// merchant-a's key, scope `numbers`.
    key: String,
}

/// merchant-a holds the WABA, with its token; merchant-b exists. Meta
/// answers one unsubscribe (which the tests expect never to be sent).
async fn setup() -> Setup {
    let moving = Arc::new(Moving::new(Arc::new(MemoryStore::new())));
    let h = Harness::on(moving.clone(), Arc::new(MemoryKvStore::new()));
    let admin = h.admin_key().await;
    h.tenant("merchant-a").await;
    h.tenant("merchant-b").await;
    h.connect("merchant-a", WABA, &[PN], "TOKEN-OF-A").await;
    let key = h.tenant_key("merchant-a", &[Scope::Numbers]).await;
    h.graph.push_json(200, json!({"success": true}));
    Setup {
        h,
        moving,
        admin,
        key,
    }
}

impl Setup {
    /// The WABA moved to merchant-b, with its token or none yet.
    fn move_to_b(&self, token: Option<&'static str>) -> Hook {
        let (store, vault) = (self.h.store.clone(), self.h.vault.clone());
        Box::pin(async move {
            move_waba(
                store.as_ref(),
                &vault,
                &WabaId::new(WABA),
                &PhoneNumberId::new(PN),
                "merchant-b",
                token,
            )
            .await;
        })
    }

    fn disconnect(&self) -> Call {
        Call::new(Method::DELETE, format!("/v1/wabas/{WABA}")).key(&self.key)
    }

    fn unbind(&self) -> Call {
        Call::new(Method::DELETE, format!("/v1/admin/wabas/{WABA}/binding")).key(&self.admin)
    }

    fn delete_tenant(&self) -> Call {
        Call::new(Method::DELETE, "/v1/admin/tenants/merchant-a").key(&self.admin)
    }

    /// merchant-b holds the WABA, its number `connected`, with `token`
    /// (none: its token not stored yet); Meta was never called.
    async fn held_by_b(&self, token: Option<&str>, case: &str) {
        assert!(!self.moving.armed(), "{case}: the move never ran");
        let binding = self.h.store.waba(&WabaId::new(WABA)).await.unwrap();
        assert_eq!(
            binding.map(|b| b.tenant_id.as_str().to_owned()).as_deref(),
            Some("merchant-b"),
            "{case}: merchant-b's binding"
        );
        let number = self.h.store.number(&PhoneNumberId::new(PN)).await.unwrap();
        assert_eq!(
            number.map(|n| n.status),
            Some(NumberStatus::Connected),
            "{case}: merchant-b's number"
        );
        let stored = self.h.vault.get(&WabaId::new(WABA)).await.unwrap();
        assert_eq!(
            stored
                .map(|t| t.token.expose_secret().to_owned())
                .as_deref(),
            token,
            "{case}: merchant-b's token"
        );
        assert!(
            self.h.graph.requests().is_empty(),
            "{case}: Meta was called with merchant-a's token: {:?}",
            self.h.graph.requests()
        );
    }
}

/// `503 storage_unavailable`, retryable.
fn assert_moved(reply: &common::Reply, case: &str) {
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
        "{case}: {}",
        reply.text
    );
    assert_eq!(reply.json()["error"]["retryable"], true, "{case}");
}

/// M1: a tenant's deletion whose listing is stale (the WABA moved to
/// merchant-b once listed, with merchant-b's token or before it is stored)
/// skips the WABA, never answers for it: the next round finds merchant-a
/// holding nothing, and deletes it. merchant-b keeps its binding and token,
/// and Meta is never called. Decisive: `delete_tenant` reading the
/// binding again when it cannot open it (`still_listed`), and skipping it
/// (answering the error instead: `503`).
#[tokio::test]
async fn a_tenant_deletion_skips_a_waba_moved_since_its_listing() {
    for token in [Some("TOKEN-OF-B"), None] {
        let s = setup().await;
        s.moving.after_listing(s.move_to_b(token));
        let deleted = s.h.call(s.delete_tenant()).await;
        assert_eq!(
            deleted.status,
            StatusCode::NO_CONTENT,
            "{token:?}: {}",
            deleted.text
        );
        assert!(
            s.h.store
                .tenant(&TenantId::parse("merchant-a").unwrap())
                .await
                .unwrap()
                .is_none()
        );
        s.held_by_b(token, &format!("{token:?}")).await;
    }
}

/// L1: the WABA moves to merchant-b (who subscribed the app again) once a
/// capability of merchant-a's is made, before Meta is called: the
/// unsubscribe with merchant-a's token is never sent. The tenant's
/// disconnection and the admin unbind answer `503` (retryable: the repeat
/// sees the WABA as it is now); a tenant's deletion skips the WABA and
/// deletes merchant-a. merchant-b keeps everything. Decisive: the
/// `OwnedWaba::still_bound` re-check before `unsubscribe_app` in each of
/// the three routes.
#[tokio::test]
async fn a_waba_moved_once_its_capability_is_made_is_not_unsubscribed() {
    // The `waba` read the capability's own re-read is, in each route: the
    // move lands right after it.
    for (route, reads) in [("disconnect", 2), ("unbind", 2), ("delete_tenant", 1)] {
        let s = setup().await;
        s.moving
            .after_waba_read(reads, s.move_to_b(Some("TOKEN-OF-B")));
        match route {
            "disconnect" => assert_moved(&s.h.call(s.disconnect()).await, route),
            "unbind" => assert_moved(&s.h.call(s.unbind()).await, route),
            _ => {
                let deleted = s.h.call(s.delete_tenant()).await;
                assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
            }
        }
        s.held_by_b(Some("TOKEN-OF-B"), route).await;
        assert_eq!(s.h.graph.remaining(), 1, "{route}");
    }
}

/// M1, with the new holder's token not stored yet (an attach between its
/// binding and its token): the WABA moves while a capability's vault read
/// runs, which then finds no token. That absence is merchant-b's state,
/// not merchant-a's binding's: `503`, like any move, never
/// `number_not_connected`, on which the admin unbind would delete
/// merchant-b's binding without a capability (and the tenant would be told
/// its WABA has no token). Decisive: `Authorizer::open` reading the
/// binding again before it answers for the vault's read.
#[tokio::test]
async fn a_waba_moved_before_its_new_token_is_stored_is_refused_as_moved() {
    for route in ["disconnect", "unbind"] {
        let s = setup().await;
        // Right after the first read of the binding (the route's own for
        // the unbind, step 4 for the disconnection): before the vault's.
        s.moving.after_waba_read(1, s.move_to_b(None));
        let reply =
            s.h.call(if route == "disconnect" {
                s.disconnect()
            } else {
                s.unbind()
            })
            .await;
        assert_moved(&reply, route);
        s.held_by_b(None, route).await;
    }
}

/// L1's warning: the WABA moves to merchant-b after the disconnection's
/// re-check, while Meta unsubscribes the app with merchant-a's token (the
/// window no read closes). `forget` then deletes nothing, the answer is
/// `503`, and a `warn` line says the new holder's subscription may be
/// gone, naming the WABA and no token. When only the token moved (a
/// refresh for merchant-a: the binding holds), `forget` deletes nothing
/// either, and no such line is logged. Decisive: the warning in
/// `OwnedWaba::forget`, and its condition (the binding moved).
#[tokio::test]
async fn forgetting_a_waba_that_moved_warns_the_new_holder_may_have_lost_its_subscription() {
    const WARNING: &str = "the new holder's webhook subscription may have been removed";
    for moved in [true, false] {
        let s = setup().await;
        let captured = Captured::default();
        let _logs = tracing::subscriber::set_default(subscriber(&captured));
        let hook: Hook = if moved {
            s.move_to_b(Some("TOKEN-OF-B"))
        } else {
            let vault = s.h.vault.clone();
            Box::pin(async move {
                vault
                    .store(
                        &StoredBusinessToken::new(
                            WabaId::new(WABA),
                            AccessToken::new("TOKEN-OF-A-REFRESHED"),
                        )
                        .phone_number_ids([PhoneNumberId::new(PN)]),
                    )
                    .await
                    .unwrap();
            })
        };
        // After the handler's re-check (the third read).
        s.moving.after_waba_read(3, hook);
        assert_moved(&s.h.call(s.disconnect()).await, "forget");
        assert!(!s.moving.armed());
        let [unsubscribe] = s.h.graph.requests().try_into().unwrap();
        assert_eq!(unsubscribe.bearer(), Some("TOKEN-OF-A"));
        let logs = captured.text();
        let warned: Vec<serde_json::Value> = logs
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| {
                event["fields"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains(WARNING))
            })
            .collect();
        if moved {
            let [warning] = warned.as_slice() else {
                panic!("one warning: {logs}")
            };
            assert_eq!(warning["level"], "WARN");
            assert_eq!(warning["fields"]["waba_id"], WABA);
        } else {
            assert!(warned.is_empty(), "the binding held: {logs}");
        }
        for token in ["TOKEN-OF-A", "TOKEN-OF-B", "TOKEN-OF-A-REFRESHED"] {
            assert!(!logs.contains(token), "{token} logged");
        }
    }
}

/// The attach's confirmation, on memory: an attach whose WABA moves to
/// another tenant right after its binding, right after its store, or
/// between its read of its binding and its store, answers `503` and leaves
/// no token of its own under the other tenant's binding
/// (`common::moving::an_attach_whose_waba_moves_takes_back_its_token`;
/// live on Postgres in `live_postgres.rs`).
#[tokio::test]
async fn an_attach_whose_waba_moves_takes_back_its_token() {
    common::moving::an_attach_whose_waba_moves_takes_back_its_token(
        Arc::new(MemoryStore::new()),
        Arc::new(MemoryKvStore::new()),
    )
    .await;
}
