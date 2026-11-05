//! The one procedure of roadmap S10, `version`, over the adapter's router:
//! the exact request, the exact answer, and what is refused.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use cratestack::axum::body::{Body, to_bytes};
use cratestack::axum::http::{Method, Request, StatusCode, header};
use meta_whatsapp_rs::core::config::ApiVersion;
use meta_whatsapp_server_api_cratestack::{BODY_LIMIT_BYTES, SCHEMA, router};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tower::ServiceExt;

/// What `version` answers with `v25.0` as the Graph API version, as
/// `GET /v1/version` does: the build's revision, when it recorded one.
fn expected() -> Value {
    json!({
        "server": env!("CARGO_PKG_VERSION"),
        "meta_whatsapp_rs_revision": option_env!("META_WHATSAPP_RS_REVISION").unwrap_or("unknown"),
        "graph_api_version": "v25.0",
        "api_version": "v1",
    })
}

/// `method path` with `body` as JSON, through a fresh router.
async fn call(
    method: Method,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let response = router(ApiVersion::DEFAULT)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ACCEPT, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, content_type, bytes.to_vec())
}

#[tokio::test]
async fn version_answers_as_get_v1_version_does() {
    for body in [&b"{}"[..], br#"{"args":{}}"#] {
        let (status, content_type, bytes) = call(
            Method::POST,
            "/$procs/version",
            "application/json",
            body.to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(body));
        assert_eq!(content_type.as_deref(), Some("application/json"));
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected());
    }
}

/// The Graph API version is the one the router was given, not the
/// default.
#[tokio::test]
async fn version_reports_the_graph_api_version_it_was_given() {
    let response = router(ApiVersion::new(24, 0))
        .oneshot(
            Request::post("/$procs/version")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["graph_api_version"], "v24.0");
}

/// A credential changes nothing: every caller is anonymous, and `version`
/// admits them.
#[tokio::test]
async fn version_needs_no_key_and_reads_none() {
    let response = router(ApiVersion::DEFAULT)
        .oneshot(
            Request::post("/$procs/version")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, "Bearer wak_not_a_key")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Procedures are `POST` only (design §8.6), and the router serves the
/// procedures the schema declares, no other path.
#[tokio::test]
async fn only_post_to_a_declared_procedure_is_served() {
    let (status, _, _) = call(
        Method::GET,
        "/$procs/version",
        "application/json",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    for path in ["/$procs/versions", "/v1/version", "/version"] {
        let (status, _, _) = call(Method::POST, path, "application/json", b"{}".to_vec()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

/// A body that is no JSON, or longer than the adapter's bound, is refused
/// in CrateStack's own error shape (design §8.6: two shapes until D16).
#[tokio::test]
async fn a_bad_or_oversized_body_is_refused() {
    let (status, _, bytes) = call(
        Method::POST,
        "/$procs/version",
        "application/json",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // CrateStack's shape, and nothing of the parser's own message.
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({ "code": "CODEC_ERROR", "message": "invalid request payload", "details": null })
    );

    let padding = "x".repeat(BODY_LIMIT_BYTES);
    let oversized = serde_json::to_vec(&json!({ "args": {}, "padding": padding })).unwrap();
    let (status, _, bytes) = call(
        Method::POST,
        "/$procs/version",
        "application/json",
        oversized,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

/// JSON is the wire format (design D19): a caller that accepts only CBOR
/// is refused, and a CBOR body too.
#[tokio::test]
async fn cbor_is_not_spoken() {
    let (status, _, _) = call(
        Method::POST,
        "/$procs/version",
        "application/cbor",
        vec![0xa0], // an empty CBOR map
    )
    .await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);

    let response = router(ApiVersion::DEFAULT)
        .oneshot(
            Request::post("/$procs/version")
                .header(header::CONTENT_TYPE, "application/cbor")
                .header(header::ACCEPT, "application/json")
                .body(Body::from(vec![0xa0]))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

/// The contract the crate exports is the schema it compiles: one
/// procedure, `version`, and no model.
#[test]
fn the_exported_schema_declares_version_alone() {
    let schema = cratestack::parse_schema(SCHEMA).unwrap();
    let procedures: Vec<&str> = schema.procedures.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(procedures, ["version"]);
    assert!(schema.models.is_empty());
}

/// A JSON object of exactly `len` bytes: `{`, spaces, `}`.
fn body_of(len: usize) -> Vec<u8> {
    let mut body = vec![b' '; len];
    body[0] = b'{';
    body[len - 1] = b'}';
    body
}

/// The bound is [`BODY_LIMIT_BYTES`], to the byte: a body of exactly that
/// length is read, one byte more is refused before the procedure runs.
#[tokio::test]
async fn the_body_bound_is_exact() {
    assert_eq!(BODY_LIMIT_BYTES, 1024);
    let (status, _, bytes) = call(
        Method::POST,
        "/$procs/version",
        "application/json",
        body_of(BODY_LIMIT_BYTES),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let (status, _, _) = call(
        Method::POST,
        "/$procs/version",
        "application/json",
        body_of(BODY_LIMIT_BYTES + 1),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

/// A body with no `content-length` (chunked) is bounded while it streams,
/// not trusted by its declared size.
#[tokio::test]
async fn a_streamed_body_past_the_bound_is_refused() {
    let chunks = (0..=BODY_LIMIT_BYTES / 64).map(|_| Ok::<_, std::io::Error>(vec![b' '; 64]));
    let request = Request::post("/$procs/version")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(futures::stream::iter(chunks)))
        .unwrap();
    assert!(request.headers().get(header::CONTENT_LENGTH).is_none());
    let response = router(ApiVersion::DEFAULT).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
