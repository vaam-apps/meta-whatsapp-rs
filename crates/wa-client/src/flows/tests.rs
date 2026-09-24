//! Request/response tests for the Flows management API, against the
//! examples in `flows/guides/flowsapi`.

use std::time::Duration;

use futures::StreamExt;
use http::Method;
use pretty_assertions::assert_eq;
use serde_json::json;
use wa_core::error::TransportError;
use wa_core::testing::{RecordedBody, ScriptedTransport};
use wa_core::{Error, ErrorKind};

use super::*;
use crate::{Client, RetryPolicy};

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// A client that retries, to prove which calls may be replayed.
fn retrying_client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 2,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap()
}

/// The validation error object shared by the create and upload examples.
fn doc_validation_error() -> serde_json::Value {
    json!({
        "error": "INVALID_PROPERTY_VALUE",
        "error_type": "FLOW_JSON_ERROR",
        "message": "Invalid value found for property 'type'.",
        "line_start": 10,
        "line_end": 10,
        "column_start": 21,
        "column_end": 34,
        "pointers": [{
            "line_start": 10,
            "line_end": 10,
            "column_start": 21,
            "column_end": 34,
            "path": "screens [0]. layout.children [0].type"
        }]
    })
}

const DOC_FLOW_JSON: &str = "{\"version\":\"5.0\",\"screens\":[{\"id\":\"WELCOME_SCREEN\",\"layout\":{\"type\":\"SingleColumnLayout\",\"children\":[{\"type\":\"TextHeading\",\"text\":\"Hello World\"},{\"type\":\"Footer\",\"label\":\"Complete\",\"on-click-action\":{\"name\":\"complete\",\"payload\":{}}}]},\"title\":\"Welcome\",\"terminal\":true,\"success\":true,\"data\":{}}]}";

#[tokio::test]
async fn create_sends_the_documented_body() {
    let t = ScriptedTransport::new();
    // Docs example response (its missing comma after "id" fixed).
    t.push_json(
        200,
        json!({"id": "<Flow-ID>", "success": true, "validation_errors": [doc_validation_error()]}),
    );
    let created = client(&t)
        .flows("WABA-ID")
        .create(
            &CreateFlow::new("My first flow", [FlowCategory::Other])
                .flow_json(DOC_FLOW_JSON)
                .publish(true),
        )
        .await
        .unwrap();

    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/WABA-ID/flows");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "name": "My first flow",
            "categories": ["OTHER"],
            "flow_json": DOC_FLOW_JSON,
            "publish": true
        }))
    );
    assert_eq!(created.id.as_str(), "<Flow-ID>");
    assert_eq!(created.success, Some(true));
    let e = &created.validation_errors[0];
    assert_eq!(e.error, "INVALID_PROPERTY_VALUE");
    assert_eq!(e.error_type, "FLOW_JSON_ERROR");
    assert_eq!((e.line_start, e.column_end), (Some(10), Some(34)));
    assert_eq!(
        e.pointers[0].path.as_deref(),
        Some("screens [0]. layout.children [0].type")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn create_with_clone_and_endpoint() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "2"}));
    let created = client(&t)
        .flows("W")
        .create(
            &CreateFlow::new("copy", [FlowCategory::SignUp, FlowCategory::Survey])
                .clone_flow_id("1")
                .endpoint_uri("https://business.com/scheduleappointment"),
        )
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
            "name": "copy",
            "categories": ["SIGN_UP", "SURVEY"],
            "clone_flow_id": "1",
            "endpoint_uri": "https://business.com/scheduleappointment"
        }))
    );
    assert_eq!(created.success, None);
    assert!(created.validation_errors.is_empty());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn invalid_input_never_reaches_the_transport() {
    let t = ScriptedTransport::new();
    let c = client(&t);
    let err = c
        .flows("W")
        .create(&CreateFlow::new("f", []))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(ref v) if v.field == "categories"));
    let err = c.flow("F").update(&UpdateFlow::new()).await.unwrap_err();
    assert!(matches!(err, Error::Validation(_)));
    let err = c.flow("F").upload_flow_json("").await.unwrap_err();
    assert!(matches!(err, Error::Validation(ref v) if v.field == "file"));
    let err = c
        .flow("F")
        .upload_flow_json(vec![b' '; MAX_FLOW_JSON_BYTES + 1])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(ref v) if v.field == "file"));
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn create_is_never_replayed_but_metadata_updates_are() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = retrying_client(&t)
        .flows("W")
        .create(&CreateFlow::new("f", [FlowCategory::Survey]))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1, "a replay could create a second flow");

    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"success": true}));
    retrying_client(&t)
        .flow("F")
        .update(&UpdateFlow::new().name("n"))
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);

    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"success": true, "validation_errors": []}));
    retrying_client(&t)
        .flow("F")
        .upload_flow_json(DOC_FLOW_JSON)
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);

    for publishing in [true, false] {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let flow = retrying_client(&t).flow("F");
        let result = if publishing {
            flow.publish().await
        } else {
            flow.deprecate().await
        };
        assert!(result.is_err());
        assert_eq!(t.requests().len(), 1, "state transitions are not replayed");
    }
}

#[tokio::test]
async fn list_parses_the_documented_page_and_passes_the_cursor() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
            "data": [
                {"id": "flow-1", "name": "flow 1", "status": "DRAFT", "categories": ["CONTACT_US"], "validation_errors": []},
                {"id": "flow-2", "name": "flow 2", "status": "PUBLISHED", "categories": ["SURVEY"], "validation_errors": []},
                {"id": "flow-3", "name": "flow 3", "status": "DRAFT", "categories": ["LEAD_GENERATION"], "validation_errors": []}
            ],
            "paging": {"cursors": {"before": "QVFI...", "after": "QVFI..."}}
        }),
    );
    t.push_json(200, json!({"data": []}));
    let flows = client(&t).flows("WABA-ID");
    let page = flows.list(&ListFlows::new()).await.unwrap();
    assert_eq!(page.data.len(), 3);
    assert_eq!(page.data[1].status, Some(FlowStatus::Published));
    assert_eq!(page.data[2].categories, vec![FlowCategory::LeadGeneration]);
    assert_eq!(page.data[0].name.as_deref(), Some("flow 1"));
    let first = t.last_request().unwrap();
    assert_eq!(first.method, Method::GET);
    assert_eq!(first.path(), "/v25.0/WABA-ID/flows");
    assert_eq!(first.url.query(), None);

    flows
        .list(&ListFlows::new().after("QVFI...").before("QVFB..."))
        .await
        .unwrap();
    let next = t.last_request().unwrap();
    assert_eq!(next.query("after").as_deref(), Some("QVFI..."));
    assert_eq!(next.query("before").as_deref(), Some("QVFB..."));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn list_stream_follows_cursors() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"data": [{"id": "1"}], "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/next"}}),
    );
    t.push_json(
        200,
        json!({"data": [{"id": "2"}], "paging": {"cursors": {"after": "c2"}}}),
    );
    let ids: Vec<String> = client(&t)
        .flows("W")
        .list_stream()
        .map(|f| f.unwrap().id.into_inner())
        .collect()
        .await;
    assert_eq!(ids, ["1", "2"]);
    let reqs = t.requests();
    assert_eq!(reqs[1].path(), "/v25.0/W/flows");
    assert_eq!(reqs[1].query("after").as_deref(), Some("c1"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn get_requests_every_documented_field() {
    let t = ScriptedTransport::new();
    // The docs' example response for this call is empty; this body is built
    // from the field table on the same page.
    t.push_json(
        200,
        json!({
            "id": "flow-1",
            "name": "My first flow",
            "status": "THROTTLED",
            "categories": ["APPOINTMENT_BOOKING"],
            "validation_errors": [],
            "json_version": "5.0",
            "data_api_version": "3.0",
            "endpoint_uri": "https://business.com/scheduleappointment",
            "whatsapp_business_account": {"id": "WABA-ID"},
            "application": {"id": "APP-ID", "name": "My app"},
            "a_field_meta_adds_later": 1
        }),
    );
    let flow = client(&t).flow("flow-1").get().await.unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/flow-1");
    assert_eq!(
        req.query("fields").as_deref(),
        Some(
            "id,name,status,categories,validation_errors,json_version,data_api_version,\
endpoint_uri,whatsapp_business_account,application"
        )
    );
    assert_eq!(flow.status, Some(FlowStatus::Throttled));
    assert_eq!(flow.json_version.as_deref(), Some("5.0"));
    assert_eq!(flow.data_api_version.as_deref(), Some("3.0"));
    assert_eq!(
        flow.whatsapp_business_account,
        Some(json!({"id": "WABA-ID"}))
    );
    assert_eq!(flow.preview, None);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn preview_passes_invalidate_and_parses_the_documented_response() {
    let t = ScriptedTransport::new();
    let doc = json!({
        "preview": {
            "preview_url": "https://business.facebook.com/wa/manage/flows/550.../preview/?token=b9d6....",
            "expires_at": "2023-05-21T11:18:09+0000"
        },
        "id": "flow-1"
    });
    t.push_json(200, doc.clone());
    t.push_json(200, doc);
    let flow = client(&t).flow("flow-1");
    let preview = flow.preview(false).await.unwrap();
    assert_eq!(
        t.last_request().unwrap().query("fields").as_deref(),
        Some("preview.invalidate(false)")
    );
    assert!(
        preview
            .preview_url
            .starts_with("https://business.facebook.com/")
    );
    assert!(preview.expires_at().is_some());
    flow.preview(true).await.unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.path(), "/v25.0/flow-1");
    assert_eq!(
        req.query("fields").as_deref(),
        Some("preview.invalidate(true)")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn update_sends_only_what_changes() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"success": true}));
    let flow = client(&t).flow("FLOW-ID");
    flow.update(&UpdateFlow::new().name("New flow name"))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/FLOW-ID");
    assert_eq!(req.json(), Some(json!({"name": "New flow name"})));

    flow.update(
        &UpdateFlow::new()
            .categories([FlowCategory::CustomerSupport])
            .endpoint_uri("https://business.com/v2"),
    )
    .await
    .unwrap();
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(
            json!({"categories": ["CUSTOMER_SUPPORT"], "endpoint_uri": "https://business.com/v2"})
        )
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn upload_is_the_documented_multipart_form() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"success": true, "validation_errors": [doc_validation_error()]}),
    );
    let upload = client(&t)
        .flow("FLOW_ID")
        .upload_flow_json(DOC_FLOW_JSON)
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/FLOW_ID/assets");
    assert_eq!(req.bearer(), Some("TOKEN"));
    let RecordedBody::Multipart(parts) = &req.body else {
        panic!("expected multipart, got {:?}", req.body);
    };
    let names: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(names, ["file", "name", "asset_type"]);
    let (filename, content_type, data) = req.multipart_field("file").unwrap();
    assert_eq!(filename, Some("flow.json"));
    assert_eq!(content_type, Some("application/json"));
    assert_eq!(&data[..], DOC_FLOW_JSON.as_bytes());
    let (filename, content_type, data) = req.multipart_field("name").unwrap();
    assert_eq!((filename, content_type), (None, None));
    assert_eq!(&data[..], b"flow.json");
    assert_eq!(
        &req.multipart_field("asset_type").unwrap().2[..],
        b"FLOW_JSON"
    );

    assert!(upload.success);
    assert_eq!(upload.validation_errors.len(), 1);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn assets_parse_the_documented_page() {
    let t = ScriptedTransport::new();
    let doc = json!({
        "data": [{
            "name": "flow.json",
            "asset_type": "FLOW_JSON",
            "download_url": "https://scontent.xx.fbcdn.net/m1/v/t0.57323-24/An_Hq0jnfJ..."
        }],
        "paging": {"cursors": {"before": "QVFIU...", "after": "QVFIU..."}}
    });
    t.push_json(200, doc.clone());
    t.push_json(200, doc.clone());
    t.push_json(200, doc);
    let flow = client(&t).flow("FLOW-ID");
    let page = flow.assets(&ListFlowAssets::new()).await.unwrap();
    assert_eq!(page.data[0].asset_type, FlowAssetType::FlowJson);
    assert_eq!(page.data[0].name, "flow.json");
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/FLOW-ID/assets");
    assert_eq!(req.url.query(), None);
    flow.assets(&ListFlowAssets::new().after("QVFIU..."))
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().query("after").as_deref(),
        Some("QVFIU...")
    );
    // The documented page has no `next` link, so the stream stops after one.
    let all: Vec<FlowAsset> = flow.assets_stream().map(|a| a.unwrap()).collect().await;
    assert_eq!(all.len(), 1);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn lifecycle_calls_hit_the_documented_edges() {
    let t = ScriptedTransport::new();
    for _ in 0..3 {
        t.push_json(200, json!({"success": true}));
    }
    let flow = client(&t).flow("FLOW-ID");
    flow.publish().await.unwrap();
    flow.deprecate().await.unwrap();
    flow.delete().await.unwrap();
    let seen: Vec<(Method, String)> = t
        .requests()
        .into_iter()
        .map(|r| (r.method.clone(), r.path().to_owned()))
        .collect();
    assert_eq!(
        seen,
        [
            (Method::POST, "/v25.0/FLOW-ID/publish".to_owned()),
            (Method::POST, "/v25.0/FLOW-ID/deprecate".to_owned()),
            (Method::DELETE, "/v25.0/FLOW-ID".to_owned()),
        ]
    );
    for r in t.requests() {
        assert_eq!(r.body, RecordedBody::Empty);
        assert_eq!(r.bearer(), Some("TOKEN"));
    }
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn graph_errors_and_unsuccessful_answers_surface() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException", "code": 100, "fbtrace_id": "x"}}),
    );
    t.push_json(200, json!({"success": false}));
    let flow = client(&t).flow("FLOW-ID");
    let err = flow.publish().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert!(flow.delete().await.is_err(), "success: false is an error");
    assert_eq!(t.remaining(), 0);
}
