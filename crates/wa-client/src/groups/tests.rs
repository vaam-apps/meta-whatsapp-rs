use std::time::Duration;

use futures::StreamExt;
use http::Method;
use serde_json::json;
use wa_core::ErrorKind;
use wa_core::error::TransportError;
use wa_core::testing::{RecordedBody, ScriptedTransport};

use super::*;
use crate::RetryPolicy;

const GROUP: &str = "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD";

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

fn retrying(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 1,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap()
}

fn validation_field(err: &Error) -> &str {
    match err {
        Error::Validation(v) => &v.field,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

fn jpeg(len: usize) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
    v.resize(len.max(4), 0);
    v
}

// ── Create / list ───────────────────────────────────────────────────────

#[tokio::test]
async fn create_matches_docs_request_and_schema_response() {
    let t = ScriptedTransport::new();
    // groups-management-api: 200 is {messaging_product, request_id}.
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "request_id": "req-1"}),
    );
    let created = client(&t)
        .groups("12784358810")
        .create(&CreateGroup {
            subject: "New Purchase Inquiry".into(),
            description: Some("Jim, an existing client, would like to learn about new car purchase options for current year models.".into()),
            join_approval_mode: Some(JoinApprovalMode::ApprovalRequired),
        })
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/12784358810/groups");
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "subject": "New Purchase Inquiry",
            "description": "Jim, an existing client, would like to learn about new car purchase options for current year models.",
            "join_approval_mode": "approval_required"
        }))
    );
    assert_eq!(created.request_id.as_deref(), Some("req-1"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn create_minimal_is_not_replayed() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = retrying(&t)
        .groups("1")
        .create(&CreateGroup::new("Watch Enthusiasts"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    let reqs = t.requests();
    assert_eq!(reqs.len(), 1, "creating is never replayed");
    assert_eq!(
        reqs[0].json(),
        Some(json!({"messaging_product": "whatsapp", "subject": "Watch Enthusiasts"}))
    );
    assert_eq!(t.remaining(), 0);
}

fn groups_page(after: Option<&str>, ids: &[&str]) -> serde_json::Value {
    let groups: Vec<_> = ids
        .iter()
        .map(|id| json!({"id": id, "subject": "S", "created_at": "1755548877"}))
        .collect();
    let mut page = json!({"data": {"groups": groups}});
    if let Some(after) = after {
        page["paging"] = json!({
            "cursors": {"after": after, "before": "NDMyNzQyODI3OTQw"},
            "next": format!("https://graph.facebook.com/v25.0/1/groups?limit=25&after={after}")
        });
    }
    page
}

#[tokio::test]
async fn list_unnests_data_groups_into_a_page() {
    let t = ScriptedTransport::new();
    // groups/reference "Get active groups" response shape.
    t.push_json(
        200,
        json!({
            "data": {"groups": [
                {"id": "G1", "subject": "Watch Enthusiasts", "created_at": "1755548877"},
                {"id": "G2", "subject": "AI Insights", "created_at": 1755548878}
            ]},
            "paging": {
                "cursors": {"after": "MTAxNTExOTQ1MjAwNzI5NDE=", "before": "NDMyNzQyODI3OTQw"},
                "previous": "https://graph.facebook.com/VERSION/PHONE_NUMBER_ID/groups?limit=10&before=NDMyNzQyODI3OTQw",
                "next": "https://graph.facebook.com/VERSION/PHONE_NUMBER_ID/groups?limit=25&after=MTAxNTExOTQ1MjAwNzI5NDE="
            }
        }),
    );
    let page = client(&t)
        .groups("12784358810")
        .list(&ListGroups {
            limit: Some(10),
            after: Some("A".into()),
            before: Some("B".into()),
        })
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/12784358810/groups");
    assert_eq!(req.url.query(), Some("limit=10&after=A&before=B"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(page.data.len(), 2);
    assert_eq!(page.data[0].id, GroupId::new("G1"));
    assert_eq!(page.data[0].subject.as_deref(), Some("Watch Enthusiasts"));
    assert_eq!(
        page.data[0].created_at.map(OffsetDateTime::unix_timestamp),
        Some(1755548877)
    );
    assert_eq!(
        page.data[1].created_at.map(OffsetDateTime::unix_timestamp),
        Some(1755548878)
    );
    assert_eq!(page.next_cursor(), Some("MTAxNTExOTQ1MjAwNzI5NDE="));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn list_stream_walks_nested_pages_on_the_configured_host() {
    let t = ScriptedTransport::new();
    t.push_json(200, groups_page(Some("c1"), &["G1", "G2"]));
    t.push_json(200, groups_page(None, &["G3"]));
    let ids: Vec<String> = client(&t)
        .groups("1")
        .list_stream(&ListGroups {
            limit: Some(2),
            ..ListGroups::default()
        })
        .map(|g| g.unwrap().id.into_inner())
        .collect()
        .await;
    assert_eq!(ids, ["G1", "G2", "G3"]);
    let reqs = t.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].url.query(), Some("limit=2"));
    assert_eq!(reqs[1].url.host_str(), Some("graph.facebook.com"));
    assert_eq!(reqs[1].path(), "/v25.0/1/groups");
    assert_eq!(reqs[1].url.query(), Some("limit=2&after=c1"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn list_stream_surfaces_errors_and_stops() {
    let t = ScriptedTransport::new();
    t.push_json(200, groups_page(Some("c1"), &["G1"]));
    t.push_json(
        400,
        json!({"error": {"message": "Invalid cursor", "code": 131059}}),
    );
    let items: Vec<Result<GroupSummary>> = client(&t)
        .groups("1")
        .list_stream(&ListGroups::default())
        .collect()
        .await;
    assert_eq!(items.len(), 2);
    assert!(items[0].is_ok());
    assert_eq!(
        items[1].as_ref().unwrap_err().graph().map(|g| g.code),
        Some(131059)
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn list_limit_outside_1_to_1024_and_stream_cursors_are_rejected() {
    let t = ScriptedTransport::new();
    let groups = client(&t).groups("1");
    for bad in [0, 1025] {
        let q = ListGroups {
            limit: Some(bad),
            ..ListGroups::default()
        };
        let err = groups.list(&q).await.unwrap_err();
        assert_eq!(validation_field(&err), "limit");
        let items: Vec<_> = groups.list_stream(&q).collect().await;
        assert_eq!(validation_field(items[0].as_ref().unwrap_err()), "limit");
    }
    for (q, field) in [
        (
            ListGroups {
                after: Some("c".into()),
                ..ListGroups::default()
            },
            "after",
        ),
        (
            ListGroups {
                before: Some("c".into()),
                ..ListGroups::default()
            },
            "before",
        ),
    ] {
        let items: Vec<_> = groups.list_stream(&q).collect().await;
        assert_eq!(items.len(), 1);
        assert_eq!(validation_field(items[0].as_ref().unwrap_err()), field);
    }
    assert!(t.requests().is_empty());
}

// ── Info / settings / picture / delete ───────────────────────────────────

#[tokio::test]
async fn info_requests_fields_and_parses_bsuid_participants() {
    use GroupField as F;
    let t = ScriptedTransport::new();
    // groups/reference "Get group info" sample with real values, plus the
    // business-scoped-user-ids participant fields.
    t.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "id": GROUP,
            "subject": "Artificial Intelligence Insights",
            "creation_timestamp": 683731200,
            "suspended": false,
            "description": "Explore AI developments.",
            "total_participant_count": 6,
            "participants": [
                {"wa_id": "2228675309"},
                {"user_id": "US.13491208655302741918", "parent_user_id": "US.ENT.1", "username": "@ai_fan"}
            ],
            "join_approval_mode": "auto_approve"
        }),
    );
    let info = client(&t)
        .group(GROUP)
        .info(&[
            F::Subject,
            F::Description,
            F::Participants,
            F::JoinApprovalMode,
            F::Suspended,
            F::CreationTimestamp,
            F::TotalParticipantCount,
        ])
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.query("fields").as_deref(),
        Some(
            "subject,description,participants,join_approval_mode,suspended,creation_timestamp,total_participant_count"
        )
    );
    assert_eq!(info.id, Some(GroupId::new(GROUP)));
    assert_eq!(
        info.subject.as_deref(),
        Some("Artificial Intelligence Insights")
    );
    assert_eq!(info.suspended, Some(false));
    assert_eq!(
        info.creation_timestamp.map(OffsetDateTime::unix_timestamp),
        Some(683731200)
    );
    assert_eq!(info.total_participant_count, Some(6));
    assert_eq!(info.join_approval_mode, Some(JoinApprovalMode::AutoApprove));
    assert_eq!(info.participants[0].wa_id, Some(WaId::new("2228675309")));
    assert_eq!(
        info.participants[1],
        GroupParticipant {
            wa_id: None,
            user_id: Some("US.13491208655302741918".into()),
            parent_user_id: Some("US.ENT.1".into()),
            username: Some("@ai_fan".into()),
        }
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn info_without_fields_and_with_quoted_scalars() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"messaging_product": "whatsapp", "id": GROUP}));
    let info = client(&t).group(GROUP).info(&[]).await.unwrap();
    assert_eq!(t.last_request().unwrap().url.query(), None);
    assert_eq!(info.id, Some(GroupId::new(GROUP)));
    assert!(info.participants.is_empty());

    // The guide's sample quotes every scalar.
    t.push_json(
        200,
        json!({"id": GROUP, "creation_timestamp": "683731200", "suspended": "true", "total_participant_count": "6", "join_approval_mode": "something_new"}),
    );
    let info = client(&t).group(GROUP).info(&[]).await.unwrap();
    assert_eq!(info.suspended, Some(true));
    assert_eq!(info.total_participant_count, Some(6));
    assert_eq!(
        info.creation_timestamp.map(OffsetDateTime::unix_timestamp),
        Some(683731200)
    );
    assert_eq!(
        info.join_approval_mode,
        Some(JoinApprovalMode::Other("something_new".into()))
    );

    t.push_json(200, json!({"id": GROUP, "suspended": "maybe"}));
    let err = client(&t).group(GROUP).info(&[]).await.unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err:?}");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn update_settings_body_and_replay() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"success": true}));
    retrying(&t)
        .group(GROUP)
        .update(&GroupSettingsUpdate {
            subject: Some("Watch Enthusiasts".into()),
            description: Some("Join our community to discuss the latest timepieces.".into()),
        })
        .await
        .unwrap();
    let reqs = t.requests();
    assert_eq!(reqs.len(), 2, "settings updates are idempotent");
    assert_eq!(reqs[1].method, Method::POST);
    assert_eq!(reqs[1].path(), format!("/v25.0/{GROUP}"));
    assert_eq!(reqs[1].bearer(), Some("TOKEN"));
    assert_eq!(
        reqs[1].json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "subject": "Watch Enthusiasts",
            "description": "Join our community to discuss the latest timepieces."
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn undocumented_bodies_count_as_success_unless_they_say_false() {
    let t = ScriptedTransport::new();
    let g = client(&t).group(GROUP);
    let update = GroupSettingsUpdate {
        description: Some("d".into()),
        ..GroupSettingsUpdate::default()
    };
    t.push_json(200, json!({}));
    g.update(&update).await.unwrap();
    t.push_bytes(200, "text/plain", "");
    g.update(&update).await.unwrap();
    t.push_json(200, json!({"success": false}));
    let err = g.update(&update).await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 200, .. }), "{err:?}");
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": "false"}),
    );
    let err = g.delete_invite_link().await.unwrap_err();
    assert!(matches!(err, Error::Http { .. }), "{err:?}");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn set_picture_uploads_multipart_jpeg() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    let picture = jpeg(1024);
    client(&t)
        .group(GROUP)
        .set_picture(picture.clone())
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    let (_, _, product) = req.multipart_field("messaging_product").unwrap();
    assert_eq!(&product[..], b"whatsapp");
    let (filename, content_type, data) = req.multipart_field("file").unwrap();
    assert_eq!(filename, Some("group-picture.jpg"));
    assert_eq!(content_type, Some("image/jpeg"));
    assert_eq!(&data[..], &picture[..]);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn picture_must_be_a_non_empty_jpeg_of_at_most_5_mib() {
    let t = ScriptedTransport::new();
    let g = client(&t).group(GROUP);
    let err = g.set_picture(Vec::<u8>::new()).await.unwrap_err();
    assert_eq!(validation_field(&err), "file");
    let err = g
        .set_picture(jpeg(PICTURE_MAX_BYTES + 1))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "file");
    let png = b"\x89PNG\r\n\x1a\n".to_vec();
    let err = g.set_picture(png).await.unwrap_err();
    assert_eq!(validation_field(&err), "file");
    assert!(t.requests().is_empty());
    t.push_json(200, json!({"success": true}));
    g.set_picture(jpeg(PICTURE_MAX_BYTES)).await.unwrap();
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn delete_group() {
    let t = ScriptedTransport::new();
    // groups-query-api: DELETE answers {success: boolean}.
    t.push_json(200, json!({"success": true}));
    client(&t).group(GROUP).delete().await.unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::DELETE);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(req.body, RecordedBody::Empty, "no request body is required");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn group_id_that_would_change_the_path_is_rejected() {
    let t = ScriptedTransport::new();
    for bad in ["", "G1/participants"] {
        let err = client(&t).group(bad).delete().await.unwrap_err();
        assert_eq!(validation_field(&err), "group_id", "{bad:?}");
        let err = client(&t).group(bad).info(&[]).await.unwrap_err();
        assert_eq!(validation_field(&err), "group_id");
        let err = client(&t)
            .groups("1")
            .unpin_message(&GroupId::new(bad), &MessageId::new("wamid.1"))
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "group_id");
    }
    assert!(t.requests().is_empty());
}

// ── Invite links ────────────────────────────────────────────────────────

#[tokio::test]
async fn invite_link_get_reset_delete() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "invite_link": "https://chat.whatsapp.com/ABC"}),
    );
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "invite_link": "https://chat.whatsapp.com/DEF"}),
    );
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "success": "true"}),
    );
    let g = client(&t).group(GROUP);
    let link = g.invite_link().await.unwrap();
    assert_eq!(link.invite_link, "https://chat.whatsapp.com/ABC");
    let link = g.reset_invite_link().await.unwrap();
    assert_eq!(link.invite_link, "https://chat.whatsapp.com/DEF");
    g.delete_invite_link().await.unwrap();

    let reqs = t.requests();
    let path = format!("/v25.0/{GROUP}/invite_link");
    assert_eq!(reqs[0].method, Method::GET);
    assert_eq!(reqs[0].path(), path);
    assert_eq!(reqs[0].json(), None);
    assert_eq!(reqs[1].method, Method::POST);
    assert_eq!(reqs[1].path(), path);
    assert_eq!(
        reqs[1].json(),
        Some(json!({"messaging_product": "whatsapp"}))
    );
    assert_eq!(reqs[2].method, Method::DELETE);
    assert_eq!(reqs[2].path(), path);
    assert_eq!(
        reqs[2].json(),
        Some(json!({"messaging_product": "whatsapp"}))
    );
    assert!(reqs.iter().all(|r| r.bearer() == Some("TOKEN")));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn reset_invite_link_is_not_replayed() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = retrying(&t)
        .group(GROUP)
        .reset_invite_link()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1);
    assert_eq!(t.remaining(), 0);
}

// ── Join requests ───────────────────────────────────────────────────────

#[tokio::test]
async fn join_requests_page_and_stream() {
    let t = ScriptedTransport::new();
    // groups/reference "Get join requests" + BSUID fields.
    t.push_json(
        200,
        json!({
            "data": [
                {"join_request_id": "MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw", "wa_id": "16505551234", "creation_timestamp": 1755548877},
                {"join_request_id": "J2", "creation_timestamp": "1755548878", "user_id": "US.1", "parent_user_id": "US.ENT.1", "username": "@u"}
            ],
            "paging": {"cursors": {
                "before": "eyJvZAmZAzZAXQiOjAsInZAlcnNpb25JZACI6IjE3NTU1NTM3MDUxNzUwNTQ1MTAifQZDZD",
                "after": "eyJvZAmZAzZAXQiOjAsInZAlcnNpb25JZACI6IjE3NTU1NTM3MDUxNzUwNTQ1MTAifQZDZD"
            }}
        }),
    );
    let page = client(&t)
        .group(GROUP)
        .join_requests(&ListJoinRequests {
            after: Some("A".into()),
            before: None,
        })
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}/join_requests"));
    assert_eq!(req.url.query(), Some("after=A"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        page.data[0].join_request_id,
        "MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw"
    );
    assert_eq!(page.data[0].wa_id, Some(WaId::new("16505551234")));
    assert_eq!(
        page.data[0]
            .creation_timestamp
            .map(OffsetDateTime::unix_timestamp),
        Some(1755548877)
    );
    assert_eq!(page.data[1].wa_id, None);
    assert_eq!(page.data[1].user_id, Some(UserId::new("US.1")));
    assert_eq!(page.data[1].username.as_deref(), Some("@u"));

    t.push_json(
        200,
        json!({"data": [{"join_request_id": "J1"}], "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/x"}}),
    );
    t.push_json(200, json!({"data": [{"join_request_id": "J2"}]}));
    let ids: Vec<String> = client(&t)
        .group(GROUP)
        .join_requests_stream(&ListJoinRequests::default())
        .map(|r| r.unwrap().join_request_id)
        .collect()
        .await;
    assert_eq!(ids, ["J1", "J2"]);
    assert_eq!(t.requests()[2].url.query(), Some("after=c1"));

    let items: Vec<_> = client(&t)
        .group(GROUP)
        .join_requests_stream(&ListJoinRequests {
            after: Some("x".into()),
            before: None,
        })
        .collect()
        .await;
    assert_eq!(validation_field(items[0].as_ref().unwrap_err()), "after");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn approve_join_requests_partial_206_is_parsed_not_an_error() {
    let t = ScriptedTransport::new();
    // groups/reference approve response with a failure; `131201` partial
    // success is HTTP 206 (groups/error-codes).
    t.push_json(
        206,
        json!({
            "messaging_product": "whatsapp",
            "approved_join_requests": ["J1"],
            "failed_join_requests": [{
                "join_request_id": "J2",
                "errors": [{
                    "code": 131203,
                    "message": "(#131203) Recipient has not accepted our new Terms of Service and Privacy Policy.",
                    "title": "Unable to add participant to group",
                    "error_data": {"details": "Recipient has not accepted our new Terms of Service and Privacy Policy."}
                }]
            }],
            "errors": [{"code": 131201, "message": "Request partially succeeded", "title": "Request partially succeeded"}]
        }),
    );
    let resp = client(&t)
        .group(GROUP)
        .approve_join_requests(&["J1", "J2"])
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}/join_requests"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({"messaging_product": "whatsapp", "join_requests": ["J1", "J2"]}))
    );
    assert_eq!(resp.approved_join_requests, ["J1"]);
    let failed = &resp.failed_join_requests[0];
    assert_eq!(failed.join_request_id.as_deref(), Some("J2"));
    assert_eq!(failed.errors[0].code, 131203);
    assert_eq!(
        failed.errors[0].title.as_deref(),
        Some("Unable to add participant to group")
    );
    assert_eq!(resp.errors[0].code, 131201);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn reject_join_requests_is_a_delete_with_a_body() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "rejected_join_requests": ["MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw"]}),
    );
    let resp = client(&t)
        .group(GROUP)
        .reject_join_requests(&[String::from("MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw")])
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::DELETE);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}/join_requests"));
    assert_eq!(
        req.json(),
        Some(
            json!({"messaging_product": "whatsapp", "join_requests": ["MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw"]})
        )
    );
    assert_eq!(
        resp.rejected_join_requests,
        ["MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw"]
    );
    assert!(resp.failed_join_requests.is_empty());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn empty_join_request_lists_are_rejected() {
    let t = ScriptedTransport::new();
    let g = client(&t).group(GROUP);
    let none: [&str; 0] = [];
    let err = g.approve_join_requests(&none).await.unwrap_err();
    assert_eq!(validation_field(&err), "join_requests");
    let err = g.reject_join_requests(&none).await.unwrap_err();
    assert_eq!(validation_field(&err), "join_requests");
    assert!(t.requests().is_empty());
}

// ── Participants ────────────────────────────────────────────────────────

#[tokio::test]
async fn remove_participants_by_phone_and_bsuid() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    client(&t)
        .group(GROUP)
        .remove_participants(&[
            Recipient::phone("+17865347866"),
            Recipient::user("US.13491208655302741918"),
        ])
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::DELETE);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}/participants"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "participants": [{"user": "+17865347866"}, {"user_id": "US.13491208655302741918"}]
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn add_participants_by_phone() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({}));
    client(&t)
        .group(GROUP)
        .add_participants(&[Recipient::phone("+7669992245")])
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{GROUP}/participants"));
    assert_eq!(
        req.json(),
        Some(json!({"messaging_product": "whatsapp", "participants": [{"user": "+7669992245"}]}))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn participant_lists_are_validated() {
    let t = ScriptedTransport::new();
    let g = client(&t).group(GROUP);
    let err = g.remove_participants(&[]).await.unwrap_err();
    assert_eq!(validation_field(&err), "participants");
    let nine = vec![Recipient::phone("+1"); MAX_PARTICIPANTS_PER_REQUEST + 1];
    let err = g.remove_participants(&nine).await.unwrap_err();
    assert_eq!(validation_field(&err), "participants");
    let err = g.add_participants(&nine).await.unwrap_err();
    assert_eq!(validation_field(&err), "participants");
    let both = Recipient::PhoneAndUser {
        phone: "+1".into(),
        user: "US.1".into(),
    };
    let err = g
        .remove_participants(&[Recipient::phone("+2"), both])
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "participants[1]");
    let err = g
        .remove_participants(&[Recipient::group("G2")])
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "participants[0]");
    let err = g
        .add_participants(&[Recipient::user("US.1")])
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "participants[0]");
    assert!(t.requests().is_empty());

    t.push_json(200, json!({}));
    let eight = vec![Recipient::phone("+1"); MAX_PARTICIPANTS_PER_REQUEST];
    g.remove_participants(&eight).await.unwrap();
    assert_eq!(t.remaining(), 0);
}

// ── Pin / unpin ─────────────────────────────────────────────────────────

#[tokio::test]
async fn pin_matches_docs_request_and_response() {
    let t = ScriptedTransport::new();
    // groups/groups-messaging "Pin and unpin group message" response.
    t.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": GROUP, "wa_id": GROUP}],
            "messages": [{"id": "wamid.HBgLM..."}]
        }),
    );
    let resp = client(&t)
        .groups("756079150920219")
        .pin_message(&GroupId::new(GROUP), &MessageId::new("wamid.HBgLM..."), 4)
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/756079150920219/messages");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "group",
            "to": GROUP,
            "type": "pin",
            "pin": {"type": "pin", "message_id": "wamid.HBgLM...", "expiration_days": 4}
        }))
    );
    assert_eq!(resp.messages[0].id, MessageId::new("wamid.HBgLM..."));
    assert_eq!(resp.contacts[0].wa_id.as_deref(), Some(GROUP));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn unpin_omits_expiration_days() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "messages": [{"id": "wamid.2"}]}),
    );
    client(&t)
        .groups("1")
        .unpin_message(&GroupId::new(GROUP), &MessageId::new("wamid.HBgLM..."))
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "group",
            "to": GROUP,
            "type": "pin",
            "pin": {"type": "unpin", "message_id": "wamid.HBgLM..."}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn pin_duration_must_be_1_to_30_days() {
    let t = ScriptedTransport::new();
    let groups = client(&t).groups("1");
    for bad in [0, 31] {
        let err = groups
            .pin_message(&GroupId::new(GROUP), &MessageId::new("wamid.1"), bad)
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "pin.expiration_days", "{bad}");
    }
    assert!(t.requests().is_empty());
    t.push_json(200, json!({"messages": [{"id": "wamid.1"}]}));
    t.push_json(200, json!({"messages": [{"id": "wamid.1"}]}));
    groups
        .pin_message(&GroupId::new(GROUP), &MessageId::new("wamid.1"), 1)
        .await
        .unwrap();
    groups
        .pin_message(&GroupId::new(GROUP), &MessageId::new("wamid.1"), 30)
        .await
        .unwrap();
    assert_eq!(t.remaining(), 0);
}

// ── Validation of subject/description; errors ───────────────────────────

#[tokio::test]
async fn subject_and_description_limits() {
    let t = ScriptedTransport::new();
    let groups = client(&t).groups("1");
    let g = client(&t).group(GROUP);
    for bad in ["", "   ", &"s".repeat(SUBJECT_MAX_CHARS + 1)] {
        let err = groups.create(&CreateGroup::new(bad)).await.unwrap_err();
        assert_eq!(validation_field(&err), "subject", "{bad:?}");
        let err = g
            .update(&GroupSettingsUpdate {
                subject: Some(bad.to_owned()),
                ..GroupSettingsUpdate::default()
            })
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "subject");
    }
    let long_description = "d".repeat(DESCRIPTION_MAX_CHARS + 1);
    let err = groups
        .create(&CreateGroup {
            description: Some(long_description.clone()),
            ..CreateGroup::new("ok")
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "description");
    let err = g
        .update(&GroupSettingsUpdate {
            description: Some(long_description),
            ..GroupSettingsUpdate::default()
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "description");
    assert!(t.requests().is_empty());

    // Whitespace is trimmed by Meta, so it does not count toward the limit;
    // 128 multi-byte characters are fine.
    t.push_json(200, json!({"request_id": "r"}));
    let padded = format!("  {}  ", "é".repeat(SUBJECT_MAX_CHARS));
    groups.create(&CreateGroup::new(padded)).await.unwrap();
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn graph_errors_are_decoded() {
    let t = ScriptedTransport::new();
    // groups/error-codes: 131208 group rate limit (429), 131041 unknown group.
    t.push_json(
        400,
        json!({"error": {"message": "(#131041) Group unknown", "type": "OAuthException", "code": 131041}}),
    );
    let err = client(&t).group(GROUP).invite_link().await.unwrap_err();
    assert_eq!(err.graph().map(|g| g.code), Some(131041));
    assert_eq!(err.graph().and_then(|g| g.http_status), Some(400));
    assert_eq!(err.kind(), ErrorKind::Unknown);
    assert_eq!(t.remaining(), 0);
}
