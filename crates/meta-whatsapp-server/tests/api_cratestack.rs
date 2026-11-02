//! The CrateStack API adapter as the binary compiles it in (roadmap S10,
//! the `api-cratestack` feature): its one procedure answers what the axum
//! adapter's route answers, byte for byte in JSON, on the same state.
#![cfg(feature = "api-cratestack")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use http_body_util::BodyExt;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{Request, StatusCode, header};
use meta_whatsapp_server::api_cratestack;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn the_version_procedure_answers_as_get_v1_version() {
    let h = Harness::new();
    let rest = h.call(Call::get("/v1/version")).await;
    assert_eq!(rest.status, StatusCode::OK);

    // The facade's axum and CrateStack's are one crate: its router takes
    // the facade's request type.
    let response = api_cratestack::router(h.state.graph_api_version())
        .oneshot(
            Request::post("/$procs/version")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let procedure: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(procedure, rest.json());
}
