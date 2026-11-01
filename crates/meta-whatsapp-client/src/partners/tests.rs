//! Tests of the partner APIs, with the examples of Meta's pages (doc paths
//! relative to `developers.facebook.com/documentation/business-messaging/whatsapp/`,
//! fetched on 2026-11-01). Where a page shows a placeholder, the value
//! comes from another example of the same page.

use futures::StreamExt;
use http::Method;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::ids::{AppId, BusinessId, MigrationIntentId, SolutionId, WabaId};
use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
use meta_whatsapp_core::{Error, ErrorKind};
use pretty_assertions::assert_eq;
use serde_json::json;

use super::migration::{MigrationIntentInfo, MigrationStatus};
use super::solutions::{
    CreateSolution, ListAppSolutions, ListConnectedClientBusinesses, ListSolutionPartners,
    PartnerStatus, PendingRequestStatus, SolutionInfo, SolutionRole, SolutionStatus,
    TOKEN_WITHHELD,
};
use crate::waba::WabaTask;
use crate::{Client, RetryPolicy};

/// The solution of the guide's accept, reject and deactivation examples.
const SOLUTION: &str = "795033096057724";
/// A sentinel business token: no test output may show it.
const SECRET: &str = "EAAN6tcBzAUBOZC82CW7iR2LiaZBwUHS4Y7FDtQ-SENTINEL";

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("SYSTEM")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

fn retrying_client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("SYSTEM")
        .retry(RetryPolicy {
            max_retries: 3,
            base_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
        })
        .build()
        .unwrap()
}

fn solution() -> SolutionId {
    SolutionId::new(SOLUTION).unwrap()
}

fn assert_empty_body(t: &ScriptedTransport) {
    let req = t.last_request().unwrap();
    assert!(matches!(req.body, RecordedBody::Empty), "{:?}", req.body);
}

// ─── Creation ────────────────────────────────────────────────────────────

/// `multi-partner-solution-embedded-creation`, step 3: the request body
/// and the answer. The app id is step 2's example; the solution id the
/// accept example's.
#[tokio::test]
async fn create_sends_the_embedded_creation_body() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"solution_id": SOLUTION}));
    let request = CreateSolution::new(
        "Social OVD with Lucky Shrub",
        AppId::new("21202248997039").unwrap(),
        vec![WabaTask::Messaging],
        vec![WabaTask::Messaging],
    );
    let created = client(&t)
        .with_token("TECH_PROVIDER_USER".into())
        .partner_solutions()
        .create(&AppId::new("634974688087057").unwrap(), &request)
        .await
        .unwrap();
    assert_eq!(created.solution_id, solution());
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/634974688087057/whatsapp_business_solution"
    );
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TECH_PROVIDER_USER"));
    assert_eq!(
        req.json(),
        Some(json!({
            "owner_permissions": ["MESSAGING"],
            "partner_app_id": "21202248997039",
            "partner_permissions": ["MESSAGING"],
            "solution_name": "Social OVD with Lucky Shrub"
        }))
    );
    assert_eq!(t.remaining(), 0);
}

/// The two checks both pages agree on, made before any request.
#[tokio::test]
async fn create_is_checked_locally_first() {
    let app = || AppId::new("21202248997039").unwrap();
    let cases = [
        (
            CreateSolution::new(" ", app(), vec![WabaTask::Messaging], vec![]),
            "solution_name",
        ),
        (
            CreateSolution::new("S", app(), vec![], vec![WabaTask::Develop]),
            "owner_permissions",
        ),
        (
            CreateSolution::new("S", app(), vec![], vec![]),
            "owner_permissions",
        ),
    ];
    let t = ScriptedTransport::new();
    for (request, field) in cases {
        let err = client(&t)
            .partner_solutions()
            .create(&app(), &request)
            .await
            .unwrap_err();
        let Error::Validation(v) = &err else {
            panic!("{err:?}")
        };
        assert_eq!(v.field, field);
    }
    assert!(t.requests().is_empty());
    // Either side alone may have MESSAGING.
    assert!(
        CreateSolution::new("S", app(), vec![], vec![WabaTask::Messaging])
            .validate()
            .is_ok()
    );
}

/// Every call that creates or changes state is never replayed after a
/// timeout or a server error (the second, scripted success stays unused):
/// a lost answer may have taken effect. Decisive: marking one idempotent
/// fails here.
#[tokio::test]
async fn state_changes_are_never_replayed() {
    for call in 0..7 {
        for failure in 0..2 {
            let t = ScriptedTransport::new();
            if failure == 0 {
                t.push_error(|| TransportError::Timeout);
            } else {
                t.push_json(
                    500,
                    json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
                );
            }
            t.push_json(
                200,
                json!({"success": true, "solution_id": SOLUTION, "id": "1"}),
            );
            let c = retrying_client(&t);
            let api = c.partner_solutions();
            let s = solution();
            let failed = match call {
                0 => api
                    .create(
                        &AppId::new("1").unwrap(),
                        &CreateSolution::new(
                            "S",
                            AppId::new("2").unwrap(),
                            vec![WabaTask::Messaging],
                            vec![],
                        ),
                    )
                    .await
                    .is_err(),
                1 => api.accept(&s).await.is_err(),
                2 => api.reject(&s).await.is_err(),
                3 => api.send_deactivation_request(&s).await.is_err(),
                4 => api.accept_deactivation_request(&s).await.is_err(),
                5 => api.reject_deactivation_request(&s).await.is_err(),
                _ => c
                    .migration_intents()
                    .create(&WabaId::new("102290129340398").unwrap(), &s)
                    .await
                    .is_err(),
            };
            assert!(failed, "call {call}, failure {failure}");
            assert_eq!(t.requests().len(), 1, "call {call}, failure {failure}");
            assert_eq!(t.remaining(), 1, "call {call}, failure {failure}");
        }
    }
}

// ─── Reading a solution ──────────────────────────────────────────────────

/// `multi-partner-solutions`, "Via API": the fields to check before
/// accepting, and the example answer.
#[tokio::test]
async fn get_before_accepting_matches_the_guide() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
          "name": "Social OVD with Lucky Shrub",
          "owner_permissions": [
            "MANAGE", "DEVELOP", "MANAGE_TEMPLATES", "MANAGE_PHONE", "VIEW_COST",
            "MANAGE_EXTENSIONS", "VIEW_PHONE_ASSETS", "MANAGE_PHONE_ASSETS",
            "VIEW_TEMPLATES", "VIEW_INSIGHTS"
          ],
          "partners": {
            "data": [
              {
                "partner_permissions": [
                  "MANAGE", "DEVELOP", "MANAGE_TEMPLATES", "MANAGE_PHONE", "VIEW_COST",
                  "MANAGE_EXTENSIONS", "VIEW_PHONE_ASSETS", "MANAGE_PHONE_ASSETS",
                  "VIEW_TEMPLATES", "VIEW_INSIGHTS"
                ],
                "partner_app": {
                  "link": "https://www.facebook.com/games/?app_id=21202248997039",
                  "name": "Lucky Shrub",
                  "id": "21202248997039"
                },
                "id": "795033099391057"
              }
            ],
            "paging": {
              "cursors": {
                "before": "QVFIUl9hX0RqLUZAPemJQVWdsYTl5WlBsY0lCb0FNTExOY2N2NzJtRENZAbDd3azBNXzhPZAndqaU5sSXdfWWJaSXJ1S2pqMi0tQUdUdm1LTGZATUDNIdGRNNE1B",
                "after": "QVFIUl9hX0RqLUZAPemJQVWdsYTl5WlBsY0lCb0FNTExOY2N2NzJtRENZAbDd3azBNXzhPZAndqaU5sSXdfWWJaSXJ1S2pqMi0tQUdUdm1LTGZATUDNIdGRNNE1B"
              }
            }
          },
          "id": "795033096057724"
        }),
    );
    let info = client(&t)
        .partner_solutions()
        .get(
            &solution(),
            &[
                "name",
                "owner_permissions",
                "partners{partner_permissions,partner_app}",
            ],
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/795033096057724");
    assert_eq!(
        req.query("fields").as_deref(),
        Some("name,owner_permissions,partners{partner_permissions,partner_app}")
    );
    assert_eq!(req.bearer(), Some("SYSTEM"));
    assert_eq!(info.id, solution());
    assert_eq!(info.name.as_deref(), Some("Social OVD with Lucky Shrub"));
    let owner = info.owner_permissions.unwrap();
    assert_eq!(owner.len(), 10);
    assert_eq!(owner[0], WabaTask::Manage);
    assert_eq!(owner[9], WabaTask::ViewInsights);
    let partners = info.partners.unwrap();
    let partner = &partners.data[0];
    assert_eq!(partner.id.as_deref(), Some("795033099391057"));
    let app = partner.partner_app.as_ref().unwrap();
    assert_eq!(app.id, Some(AppId::new("21202248997039").unwrap()));
    assert_eq!(app.name.as_deref(), Some("Lucky Shrub"));
    assert_eq!(
        partner.partner_permissions.as_ref().unwrap()[2],
        WabaTask::ManageTemplates
    );
    assert_eq!(t.remaining(), 0);
}

/// `multi-partner-solutions`, "Get fields on a solution".
#[tokio::test]
async fn get_fields_on_a_solution_matches_the_guide() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
          "name": "Social OVD with Lucky Shrub",
          "status": "ACTIVE",
          "partners": {
            "data": [
              {
                "partner_app": {
                  "link": "https://www.socialoverdrive.com/",
                  "name": "Social Overdrive",
                  "id": "637576208107267"
                },
                "status": "ACCEPTED",
                "id": "17602267745704"
              }
            ],
            "paging": {
              "cursors": {
                "before": "QVFIUmxnSE9LUFliNzlUTWdhTlYzQjBtekprSC0wQUdoZAGRYbFlzeUpDMG9yNkF1OHYyel9tcUlBbGhFckxJQ1Y3UFZA4dUkycEk0WDJwRGYzT2JYbVhEdFdB",
                "after": "QVFIUmxnSE9LUFliNzlUTWdhTlYzQjBtekprSC0wQUdoZAGRYbFlzeUpDMG9yNkF1OHYyel9tcUlBbGhFckxJQ1Y3UFZA4dUkycEk0WDJwRGYzT2JYbVhEdFdB"
              }
            }
          },
          "id": "17602267745700"
        }),
    );
    let info = client(&t)
        .partner_solutions()
        .get(
            &SolutionId::new("17602267745700").unwrap(),
            &["name", "status", "partners"],
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.path(), "/v25.0/17602267745700");
    assert_eq!(req.query("fields").as_deref(), Some("name,status,partners"));
    assert_eq!(info.status, Some(SolutionStatus::Active));
    let partners = info.partners.unwrap();
    assert_eq!(partners.data[0].status, Some(PartnerStatus::Accepted));
    assert_eq!(partners.next_cursor(), None, "no `next` link: one page");
    assert_eq!(t.remaining(), 0);
}

/// Meta's defaults when no field is asked for: no `fields` parameter.
/// Values Meta adds parse as `Other`, kept verbatim.
#[tokio::test]
async fn get_without_fields_sends_none_and_keeps_new_values() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"id": "1", "status": "PAUSED", "status_for_pending_request": "NONE",
               "owner_permissions": ["FUTURE_TASK"], "something_new": 1}),
    );
    let info = client(&t)
        .partner_solutions()
        .get(&SolutionId::new("1").unwrap(), &[])
        .await
        .unwrap();
    assert_eq!(t.last_request().unwrap().query("fields"), None);
    assert_eq!(info.status, Some(SolutionStatus::Other("PAUSED".into())));
    assert_eq!(
        info.status_for_pending_request,
        Some(PendingRequestStatus::NoRequest)
    );
    assert_eq!(
        info.owner_permissions,
        Some(vec![WabaTask::Other("FUTURE_TASK".into())])
    );
    assert_eq!(t.remaining(), 0);
}

/// `solution-details-api`'s 403 example.
#[tokio::test]
async fn a_solution_the_app_cannot_see_is_a_permission_error() {
    let t = ScriptedTransport::new();
    t.push_json(
        403,
        json!({
            "error": {
                "message": "Your app doesn't have permission to access this Multi-Partner Solution",
                "type": "OAuthException",
                "code": 200,
                "error_subcode": 1349174,
                "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
                "error_user_title": "Permission Denied",
                "error_user_msg": "Your app doesn't have permission to access this resource"
            }
        }),
    );
    let err = client(&t)
        .partner_solutions()
        .get(&solution(), &[])
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Permission);
    assert_eq!(err.graph().unwrap().error_subcode, Some(1349174));
    assert_eq!(t.remaining(), 0);
}

// ─── Accept, reject, deactivation ────────────────────────────────────────

/// `multi-partner-solutions`: the accept, reject and send-deactivation
/// examples are bodiless POSTs answered `{"success": true}`.
#[tokio::test]
async fn accept_reject_and_send_deactivation_match_the_guide() {
    for (call, edge) in [
        (0, "accept"),
        (1, "reject"),
        (2, "send_deactivation_request"),
    ] {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let api = client(&t).partner_solutions();
        match call {
            0 => api.accept(&solution()).await,
            1 => api.reject(&solution()).await,
            _ => api.send_deactivation_request(&solution()).await,
        }
        .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST, "{edge}");
        assert_eq!(req.path(), format!("/v25.0/{SOLUTION}/{edge}"));
        assert_eq!(req.url.query(), None, "{edge}");
        assert_eq!(req.bearer(), Some("SYSTEM"), "{edge}");
        assert_empty_body(&t);
        assert_eq!(t.remaining(), 0);
    }
}

/// `{"success": false}` is not a success.
#[tokio::test]
async fn success_false_is_an_error() {
    for call in 0..3 {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": false}));
        let api = client(&t).partner_solutions();
        let failed = match call {
            0 => api.accept(&solution()).await.is_err(),
            1 => api.accept_deactivation_request(&solution()).await.is_err(),
            _ => api.reject_deactivation_request(&solution()).await.is_err(),
        };
        assert!(failed, "call {call}");
        assert_eq!(t.remaining(), 0);
    }
}

/// `solution-accept-api`'s 422 example (its JSON with the trailing comma
/// removed): a solution that is not `INITIATED`.
#[tokio::test]
async fn accepting_a_solution_in_the_wrong_state_is_an_invalid_parameter() {
    let t = ScriptedTransport::new();
    t.push_json(
        422,
        json!({"error": {
            "message": "Solution cannot be accepted: solution is not in INITIATED status",
            "code": 100,
            "error_subcode": 2207051
        }}),
    );
    let err = client(&t)
        .partner_solutions()
        .accept(&solution())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert_eq!(err.graph().unwrap().error_subcode, Some(2207051));
    assert_eq!(t.remaining(), 0);
}

/// Accepting and rejecting a deactivation request: the guide's
/// `{"success": true}` is `None`; `accept-deactivation-request-api`'s
/// example, the solution afterwards.
#[tokio::test]
async fn deactivation_answers_decode_in_both_shapes() {
    for (accept, edge) in [
        (true, "accept_deactivation_request"),
        (false, "reject_deactivation_request"),
    ] {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(
            200,
            json!({
                "id": "12345678901234567",
                "name": "Sample Business Solution Partnership",
                "status": "DEACTIVATED",
                "status_for_pending_request": "NONE",
                "owner_permissions": ["DEVELOP", "MANAGE"]
            }),
        );
        let api = client(&t).partner_solutions();
        let answers: Vec<Option<SolutionInfo>> = if accept {
            vec![
                api.accept_deactivation_request(&solution()).await.unwrap(),
                api.accept_deactivation_request(&solution()).await.unwrap(),
            ]
        } else {
            vec![
                api.reject_deactivation_request(&solution()).await.unwrap(),
                api.reject_deactivation_request(&solution()).await.unwrap(),
            ]
        };
        assert_eq!(answers[0], None, "{edge}");
        let after = answers[1].as_ref().unwrap();
        assert_eq!(after.id, SolutionId::new_unchecked("12345678901234567"));
        assert_eq!(after.status, Some(SolutionStatus::Deactivated));
        assert_eq!(
            after.owner_permissions,
            Some(vec![WabaTask::Develop, WabaTask::Manage])
        );
        for req in t.requests() {
            assert_eq!(req.method, Method::POST, "{edge}");
            assert_eq!(req.path(), format!("/v25.0/{SOLUTION}/{edge}"));
            assert!(matches!(req.body, RecordedBody::Empty), "{edge}");
        }
        assert_eq!(t.remaining(), 0);
    }
}

// ─── The solution token ──────────────────────────────────────────────────

/// `multi-partner-solutions`, "Getting client business tokens": the
/// request, and the guide's wrapped answer; `access-token-api`'s bare
/// schema with `expires_at` decodes too. Neither answer's `Debug` shows
/// the token.
#[tokio::test]
async fn access_token_reads_both_shapes_and_never_shows_the_token() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"data": [{"access_token": SECRET}]}));
    t.push_json(
        200,
        json!({"access_token": SECRET, "expires_at": 1767225600}),
    );
    let api = client(&t).partner_solutions();
    let s = SolutionId::new("303610109049230").unwrap();
    let business = BusinessId::new("2729063490586005").unwrap();
    let wrapped = api.access_token(&s, &business).await.unwrap();
    let bare = api.access_token(&s, &business).await.unwrap();
    assert_eq!(wrapped.access_token.expose_secret(), SECRET);
    assert_eq!(wrapped.expires_at, None);
    assert_eq!(bare.access_token.expose_secret(), SECRET);
    assert_eq!(bare.expires_at, Some(1767225600));
    for token in [&wrapped, &bare] {
        let shown = format!("{token:?}");
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(!shown.contains("SENTINEL"), "{shown}");
    }
    for req in t.requests() {
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/303610109049230/access_token");
        assert_eq!(
            req.query("business_id").as_deref(),
            Some("2729063490586005")
        );
        assert_eq!(req.bearer(), Some("SYSTEM"));
    }
    assert_eq!(t.remaining(), 0);
}

/// An answer that does not decode, or holds no token, is a decode error
/// whose snippet is the token placeholder and whose message is serde's
/// category and position, never the body: decisive against decoding it
/// with `send()`, which would quote a body that names neither `paging`
/// nor `access_token` (the first two).
#[tokio::test]
async fn an_access_token_answer_that_does_not_decode_is_withheld() {
    let bodies = [
        format!(r#"{{"data": "{SECRET}"}}"#),
        format!(r#"{{"token": "{SECRET}"}}"#),
        format!(r#"{{"data": [{{"access_token": "{SECRET}", "expires_at": "{SECRET}"}}]}}"#),
        r#"{"data": []}"#.to_owned(),
        r#"{"access_token": ""}"#.to_owned(),
        format!(r#"{{"access_token": "{SECRET}""#),
    ];
    for body in bodies {
        let t = ScriptedTransport::new();
        t.push_bytes(200, "application/json", body.clone());
        let err = client(&t)
            .partner_solutions()
            .access_token(&solution(), &BusinessId::new("1").unwrap())
            .await
            .unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SENTINEL"), "{body}: {shown}");
        let Error::Decode {
            body_snippet,
            context,
            ..
        } = &err
        else {
            panic!("{body}: {err:?}")
        };
        assert_eq!(body_snippet.as_bytes(), TOKEN_WITHHELD, "{body}");
        assert_eq!(*context, "solution access token response", "{body}");
        assert_eq!(t.remaining(), 0);
    }
}

/// The malformed answers a security review asked for, beyond the ones
/// above: a token field missing from the item, the token nested one level
/// too deep (in the item and around `data`), an item without the token
/// next to one with it, and an `expires_at` that is a numeric string
/// (the reference's type is `integer (int64)`). Each is a withheld decode
/// error.
#[tokio::test]
async fn malformed_token_answers_are_withheld() {
    let bodies = [
        r#"{"data": [{"expires_at": 1767225600}]}"#.to_owned(),
        format!(r#"{{"data": [{{"access_token": {{"value": "{SECRET}"}}}}]}}"#),
        format!(r#"{{"data": {{"data": [{{"access_token": "{SECRET}"}}]}}}}"#),
        format!(r#"{{"access_token": {{"access_token": "{SECRET}"}}}}"#),
        format!(r#"{{"data": [{{"token": "x"}}, {{"access_token": "{SECRET}"}}]}}"#),
        format!(r#"{{"access_token": "{SECRET}", "expires_at": "1767225600"}}"#),
        r#"{"access_token": "  "}"#.to_owned(),
        r#"{"data": [{"access_token": "\t"}]}"#.to_owned(),
    ];
    for body in bodies {
        let t = ScriptedTransport::new();
        t.push_bytes(200, "application/json", body.clone());
        let err = client(&t)
            .partner_solutions()
            .access_token(&solution(), &BusinessId::new("1").unwrap())
            .await
            .unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SENTINEL"), "{body}: {shown}");
        let Error::Decode { body_snippet, .. } = &err else {
            panic!("{body}: {err:?}")
        };
        assert_eq!(body_snippet.as_bytes(), TOKEN_WITHHELD, "{body}");
        assert_eq!(t.remaining(), 0);
    }
}

/// A non-2xx answer that is not Meta's error envelope becomes an
/// `Error::Http` whose snippet is the token placeholder, whatever the body
/// names: the crate's own rule withholds only bodies that name `paging` or
/// `access_token`, and this endpoint's answer is a credential under any
/// key. Meta's envelope stays an `Error::Api`, with its code.
#[tokio::test]
async fn a_failed_token_answer_never_quotes_its_body() {
    for (status, body) in [
        (500, format!(r#"{{"token": "{SECRET}"}}"#)),
        (502, format!("<html>upstream said {SECRET}</html>")),
        (
            403,
            format!(r#"{{"data": [{{"access_token": "{SECRET}"}}]}}"#),
        ),
    ] {
        let t = ScriptedTransport::new();
        t.push_bytes(status, "application/json", body.clone());
        let err = client(&t)
            .partner_solutions()
            .access_token(&solution(), &BusinessId::new("1").unwrap())
            .await
            .unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SENTINEL"), "{body}: {shown}");
        let Error::Http {
            status: got,
            body_snippet,
        } = &err
        else {
            panic!("{body}: {err:?}")
        };
        assert_eq!(*got, status);
        assert_eq!(body_snippet.as_bytes(), TOKEN_WITHHELD, "{body}");
        assert_eq!(t.remaining(), 0);
    }
    // `access-token-api`'s 422 example (its trailing comma removed).
    let t = ScriptedTransport::new();
    t.push_json(
        422,
        json!({"error": {
            "message": "The specified business has not shared a WABA with this Multi-Partner Solution",
            "type": "OAuthException",
            "code": 100
        }}),
    );
    let err = client(&t)
        .partner_solutions()
        .access_token(&solution(), &BusinessId::new("1").unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert_eq!(err.graph().unwrap().code, 100);
}

/// `expires_at` is Meta's value, passed through: absent or `null` is
/// `None`, and a value in the past (or negative) is not interpreted here.
#[tokio::test]
async fn expires_at_is_passed_through() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"access_token": SECRET, "expires_at": null}));
    t.push_json(200, json!({"access_token": SECRET, "expires_at": -1}));
    t.push_json(
        200,
        json!({"data": [{"access_token": SECRET, "expires_at": 0}]}),
    );
    let api = client(&t).partner_solutions();
    let business = BusinessId::new("1").unwrap();
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(
            api.access_token(&solution(), &business)
                .await
                .unwrap()
                .expires_at,
        );
    }
    assert_eq!(seen, [None, Some(-1), Some(0)]);
    assert_eq!(t.remaining(), 0);
}

/// A read: a timeout on the token's GET is retried, with the same query.
#[tokio::test]
async fn the_token_read_is_retried() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"access_token": SECRET, "expires_at": 1}));
    let token = retrying_client(&t)
        .partner_solutions()
        .access_token(&solution(), &BusinessId::new("2729063490586005").unwrap())
        .await
        .unwrap();
    assert_eq!(token.access_token.expose_secret(), SECRET);
    let reqs = t.requests();
    assert_eq!(reqs.len(), 2);
    for req in reqs {
        assert_eq!(req.path(), format!("/v25.0/{SOLUTION}/access_token"));
        assert_eq!(
            req.query("business_id").as_deref(),
            Some("2729063490586005")
        );
    }
    assert_eq!(t.remaining(), 0);
}

// ─── Lists ───────────────────────────────────────────────────────────────

/// `multi-partner-solutions`, "Get solutions associated with your app":
/// the guide's answer, and the reference's filters on the query.
#[tokio::test]
async fn app_solutions_match_the_guide() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
          "data": [
            {
              "name": "Social OVD with Lucky Shrub",
              "status": "INITIATED",
              "status_for_pending_request": "PENDING_ACTIVATION",
              "id": "19702253086782"
            },
            {
              "name": "Social OVD with Social Brew",
              "status": "ACTIVE",
              "status_for_pending_request": "NONE",
              "id": "17602267745700"
            }
          ],
          "paging": {
            "cursors": {
              "before": "QVFIUkxlbkhTZA1VleGwyWHd3SmlSMnlnelhlbUVSSjVYQmU2aXVmb1YyWk9JTkx3b2gwNE9FS3J2ejMzNENxbmh1bWZAqSkZAJUzNfbmF4NmtPaFYxQldaaXR3",
              "after": "QVFIUlgyLTlQYWV0eTNGWXVhcTJnOEhzY1lvUDloVV8wUUxVQk9YMVJ5UGlBZAmx1Q1BjaEVwd0tWdmNvRU9jdGRiNnlrc193alRNaDV2SXZAfN1kybDBibEFR"
            }
          }
        }),
    );
    let page = client(&t)
        .partner_solutions()
        .app_solutions(
            &AppId::new("21202248997039").unwrap(),
            &ListAppSolutions::new()
                .role(SolutionRole::Owner)
                .fields(["id", "name", "owner_app"])
                .limit(25)
                .after("c0"),
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        "/v25.0/21202248997039/whatsapp_business_solutions"
    );
    assert_eq!(req.query("role").as_deref(), Some("OWNER"));
    assert_eq!(req.query("fields").as_deref(), Some("id,name,owner_app"));
    assert_eq!(req.query("limit").as_deref(), Some("25"));
    assert_eq!(req.query("after").as_deref(), Some("c0"));
    assert_eq!(req.query("before"), None);
    assert_eq!(page.data.len(), 2);
    assert_eq!(page.data[0].status, Some(SolutionStatus::Initiated));
    assert_eq!(
        page.data[0].status_for_pending_request,
        Some(PendingRequestStatus::PendingActivation)
    );
    assert_eq!(page.data[1].id, SolutionId::new_unchecked("17602267745700"));
    assert_eq!(t.remaining(), 0);
}

/// `multi-partner-solutions`, "Get partners of a solution".
#[tokio::test]
async fn partners_match_the_guide() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
          "data": [
            {
              "partner_app": {
                "link": "https://www.socialoverdrive.com",
                "name": "Social Overdrive",
                "id": "637576208107267"
              },
              "status": "ACCEPTED",
              "id": "17602267745704"
            }
          ],
          "paging": {
            "cursors": {
              "before": "QVFIUmxnSE9LUFliNzlUTWdhTlYzQjBtekprSC0wQUdoZAGRYbFlzeUpDMG9yNkF1OHYyel9tcUlBbGhFckxJQ1Y3UFZA4dUkycEk0WDJwRGYzT2JYbVhEdFdB",
              "after": "QVFIUmxnSE9LUFliNzlUTWdhTlYzQjBtekprSC0wQUdoZAGRYbFlzeUpDMG9yNkF1OHYyel9tcUlBbGhFckxJQ1Y3UFZA4dUkycEk0WDJwRGYzT2JYbVhEdFdB"
            }
          }
        }),
    );
    let page = client(&t)
        .partner_solutions()
        .partners(
            &SolutionId::new("17602267745700").unwrap(),
            &ListSolutionPartners::new().before("b0"),
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.path(), "/v25.0/17602267745700/partners");
    assert_eq!(req.query("before").as_deref(), Some("b0"));
    assert_eq!(req.query("after"), None);
    let partner = &page.data[0];
    assert_eq!(partner.status, Some(PartnerStatus::Accepted));
    assert_eq!(
        partner.partner_app.as_ref().unwrap().link.as_deref(),
        Some("https://www.socialoverdrive.com")
    );
    assert_eq!(t.remaining(), 0);
}

/// `application-connected-client-businesses` shows no example: the
/// answer here is built from its schema (`data` of `{id, name}`,
/// `paging`).
#[tokio::test]
async fn connected_client_businesses_follow_the_schema() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
            "data": [{"id": "2729063490586005", "name": "Lucky Shrub"}],
            "paging": {"cursors": {"before": "b", "after": "a"}}
        }),
    );
    let page = client(&t)
        .partner_solutions()
        .connected_client_businesses(
            &AppId::new("21202248997039").unwrap(),
            &ListConnectedClientBusinesses::new().limit(100),
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        "/v25.0/21202248997039/connected_client_businesses"
    );
    assert_eq!(req.query("limit").as_deref(), Some("100"));
    assert_eq!(
        page.data[0].id,
        BusinessId::new_unchecked("2729063490586005")
    );
    assert_eq!(page.data[0].name.as_deref(), Some("Lucky Shrub"));
    assert_eq!(t.remaining(), 0);
}

/// The references allow a page of 1 to 100; anything else is refused
/// before a request, by the one-page reads and the streams.
#[tokio::test]
async fn limits_outside_one_to_a_hundred_are_refused() {
    let t = ScriptedTransport::new();
    let api = client(&t).partner_solutions();
    let app = AppId::new("1").unwrap();
    for limit in [0, 101] {
        let errors = [
            api.app_solutions(&app, &ListAppSolutions::new().limit(limit))
                .await
                .unwrap_err(),
            api.connected_client_businesses(
                &app,
                &ListConnectedClientBusinesses::new().limit(limit),
            )
            .await
            .unwrap_err(),
            api.app_solutions_stream(&app, &ListAppSolutions::new().limit(limit))
                .collect::<Vec<_>>()
                .await
                .remove(0)
                .unwrap_err(),
            api.connected_client_businesses_stream(
                &app,
                &ListConnectedClientBusinesses::new().limit(limit),
            )
            .collect::<Vec<_>>()
            .await
            .remove(0)
            .unwrap_err(),
        ];
        for err in errors {
            let Error::Validation(v) = &err else {
                panic!("{err:?}")
            };
            assert_eq!(v.field, "limit");
        }
    }
    assert!(t.requests().is_empty());
}

/// A stream's single item: the validation error naming `field`.
fn refused<T: std::fmt::Debug>(items: &[meta_whatsapp_core::Result<T>], field: &str) {
    assert_eq!(items.len(), 1, "{field}: {items:?}");
    let Err(Error::Validation(v)) = &items[0] else {
        panic!("{field}: {items:?}")
    };
    assert_eq!(v.field, field);
}

/// Each stream follows `after` over two pages, keeps the query's filters,
/// and refuses a caller's cursor as its single item.
#[tokio::test]
async fn streams_follow_cursors_and_refuse_a_callers() {
    let first = json!({
        "data": [{"id": "1"}],
        "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/v25.0/x?after=c1"}
    });
    let last = json!({"data": [{"id": "2"}]});
    for call in 0..3 {
        let t = ScriptedTransport::new();
        t.push_json(200, first.clone());
        t.push_json(200, last.clone());
        let api = client(&t).partner_solutions();
        let app = AppId::new("21202248997039").unwrap();
        let ids: Vec<String> = match call {
            0 => {
                api.app_solutions_stream(&app, &ListAppSolutions::new().limit(10))
                    .map(|r| r.unwrap().id.as_str().to_owned())
                    .collect()
                    .await
            }
            1 => {
                api.connected_client_businesses_stream(
                    &app,
                    &ListConnectedClientBusinesses::new().limit(10),
                )
                .map(|r| r.unwrap().id.as_str().to_owned())
                .collect()
                .await
            }
            _ => {
                api.partners_stream(&solution(), &ListSolutionPartners::new())
                    .map(|r| r.unwrap().id.unwrap())
                    .collect()
                    .await
            }
        };
        assert_eq!(ids, ["1", "2"], "call {call}");
        let reqs = t.requests();
        assert_eq!(reqs[0].query("after"), None, "call {call}");
        assert_eq!(reqs[1].query("after").as_deref(), Some("c1"), "call {call}");
        if call < 2 {
            assert_eq!(reqs[1].query("limit").as_deref(), Some("10"), "call {call}");
        }
        assert_eq!(t.remaining(), 0);
    }

    let t = ScriptedTransport::new();
    let api = client(&t).partner_solutions();
    let app = AppId::new("1").unwrap();
    for field in ["after", "before"] {
        let apps = if field == "after" {
            ListAppSolutions::new().after("x")
        } else {
            ListAppSolutions::new().before("x")
        };
        let partners = if field == "after" {
            ListSolutionPartners::new().after("x")
        } else {
            ListSolutionPartners::new().before("x")
        };
        let connected = if field == "after" {
            ListConnectedClientBusinesses::new().after("x")
        } else {
            ListConnectedClientBusinesses::new().before("x")
        };
        refused(
            &api.app_solutions_stream(&app, &apps)
                .collect::<Vec<_>>()
                .await,
            field,
        );
        refused(
            &api.partners_stream(&solution(), &partners)
                .collect::<Vec<_>>()
                .await,
            field,
        );
        refused(
            &api.connected_client_businesses_stream(&app, &connected)
                .collect::<Vec<_>>()
                .await,
            field,
        );
    }
    assert!(t.requests().is_empty());
}

// ─── Migration intents ───────────────────────────────────────────────────

/// `adding-waba-to-mps`, step 1, and
/// `migrating-wabas-among-solutions-via-meta-business-suite`, step 1: the
/// body names the destination solution; the answer, the intent. The
/// guides' placeholders are filled from `multi-partner-solutions`.
#[tokio::test]
async fn create_migration_intent_matches_the_guides() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "<MIGRATION_INTENT_ID>"}));
    let created = client(&t)
        .with_token("BUSINESS".into())
        .migration_intents()
        .create(
            &WabaId::new("102290129340398").unwrap(),
            &SolutionId::new("303610109049230").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.id, MigrationIntentId::new("<MIGRATION_INTENT_ID>"));
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/102290129340398/set_solution_migration_intent"
    );
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("BUSINESS"));
    assert!(
        matches!(&req.body, RecordedBody::Bytes { content_type, .. } if content_type == "application/json"),
        "{:?}",
        req.body
    );
    assert_eq!(req.json(), Some(json!({"solution_id": "303610109049230"})));
    assert_eq!(t.remaining(), 0);
}

/// `migrating-wabas-among-solutions-via-meta-business-suite`, Solution
/// Partner step 2: the guide's answer as printed (placeholders and all)
/// decodes; with `ACCEPTED`, the partner goes on.
#[tokio::test]
async fn get_migration_intent_matches_the_guide() {
    let example = json!({
      "solution": {
        "name": "<DESTINATION_SOLUTION_NAME>",
        "status": "<DESTINATION_SOLUTION_STATUS>",
        "status_for_pending_request": "<DESTINATION_SOLUTION_PENDING_REQUEST_STATUS>",
        "id": "<DESTINATION_SOLUTION_ID>"
      },
      "status": "<MIGRATION_INTENT_STATUS>",
      "destination_waba": {
        "id": "<BUSINESS_CUSTOMER_WABA_ID>",
        "name": "<BUSINESS_CUSTOMER_WABA_NAME>",
        "currency": "<BUSINESS_CUSTOMER_WABA_CURRENCY>",
        "timezone_id": "<BUSINESS_CUSTOMER_WABA_TIMEZONE>",
        "business_type": "ent",
        "message_template_namespace": "<BUSINESS_CUSTOMER_WABA_TEMPLATE_NAMESPACE>"
      },
      "id": "<MIGRATION_INTENT_ID>"
    });
    let mut accepted = example.clone();
    accepted["status"] = json!("ACCEPTED");
    accepted["solution"]["status"] = json!("ACTIVE");
    accepted["solution"]["id"] = json!("303610109049230");
    accepted["destination_waba"]["id"] = json!("102290129340398");
    let t = ScriptedTransport::new();
    t.push_json(200, example);
    t.push_json(200, accepted);
    let api = client(&t).with_token("BUSINESS".into()).migration_intents();
    let intent = MigrationIntentId::new("<MIGRATION_INTENT_ID>");
    let printed = api.get(&intent, &[]).await.unwrap();
    assert_eq!(
        printed.status,
        Some(MigrationStatus::Other("<MIGRATION_INTENT_STATUS>".into()))
    );
    assert!(!printed.is_accepted());
    let waba = printed.destination_waba.as_ref().unwrap();
    assert_eq!(waba.business_type.as_deref(), Some("ent"));
    assert_eq!(
        waba.currency.as_deref(),
        Some("<BUSINESS_CUSTOMER_WABA_CURRENCY>")
    );
    let info: MigrationIntentInfo = api
        .get(
            &intent,
            &["id", "status", "destination_waba", "solution", "waba"],
        )
        .await
        .unwrap();
    assert!(info.is_accepted());
    assert_eq!(info.status, Some(MigrationStatus::Accepted));
    let solution = info.solution.unwrap();
    assert_eq!(solution.id, SolutionId::new_unchecked("303610109049230"));
    assert_eq!(solution.status, Some(SolutionStatus::Active));
    assert_eq!(
        info.destination_waba.unwrap().id,
        Some(WabaId::new_unchecked("102290129340398"))
    );
    assert_eq!(info.waba, None);
    let reqs = t.requests();
    // The placeholder holds `<` and `>`: one encoded segment.
    assert_eq!(reqs[0].path(), "/v25.0/%3CMIGRATION_INTENT_ID%3E");
    assert_eq!(reqs[0].query("fields"), None);
    assert_eq!(
        reqs[1].query("fields").as_deref(),
        Some("id,status,destination_waba,solution,waba")
    );
    for req in &reqs {
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.bearer(), Some("BUSINESS"));
    }
    assert_eq!(t.remaining(), 0);
}

/// The reference's statuses all have a variant.
#[test]
fn migration_statuses_are_the_references() {
    for (wire, status) in [
        ("ACCEPTED", MigrationStatus::Accepted),
        ("COMPLETED", MigrationStatus::Completed),
        ("FAILED", MigrationStatus::Failed),
        ("FAILED_TO_COMPLETE", MigrationStatus::FailedToComplete),
        ("IN_PROGRESS", MigrationStatus::InProgress),
        ("INITIATED", MigrationStatus::Initiated),
        ("READY_TO_COMPLETE", MigrationStatus::ReadyToComplete),
        ("REJECTED", MigrationStatus::Rejected),
    ] {
        assert_eq!(
            serde_json::from_value::<MigrationStatus>(json!(wire)).unwrap(),
            status
        );
        assert_eq!(serde_json::to_value(&status).unwrap(), json!(wire));
    }
}

// ─── Ids in paths ────────────────────────────────────────────────────────

/// An id holding `/` is refused when typed in, and stays one encoded
/// segment when it came from Meta (unchecked) or is opaque.
#[tokio::test]
async fn an_id_holding_a_slash_stays_one_segment() {
    assert_eq!(
        SolutionId::new("1/accept").unwrap_err().field,
        "solution_id"
    );
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"id": "1/x"}));
    let c = client(&t);
    c.partner_solutions()
        .accept(&SolutionId::new_unchecked("1/reject"))
        .await
        .unwrap();
    c.migration_intents()
        .get(&MigrationIntentId::new("1/x"), &[])
        .await
        .unwrap();
    let reqs = t.requests();
    assert_eq!(reqs[0].path(), "/v25.0/1%2Freject/accept");
    assert_eq!(reqs[1].path(), "/v25.0/1%2Fx");
    assert_eq!(t.remaining(), 0);
}
