//! Roadmap M5d2's log half for a Flow's uploaded media (design D454 to
//! D457): captured at `TRACE` for every target, downloading a file,
//! refusing its URL, its keys or its integrity, and the CDN failing, log
//! no part of the URL (its query is the CDN's token), the keys, the
//! hashes, the file name or the file. The error bodies hold none of them
//! either. The download is audited with the tenant, the WABA and the size,
//! and nothing else. Alone in its binary, as `logs.rs` is.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::capture::{Captured, subscriber};
use common::{Call, Harness};
use meta_whatsapp_rs::webhooks::axum::http::Method;
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::flow_media::{CdnFetcher, FetchError, FlowMediaDeployment};
use meta_whatsapp_server_core::webhooks_out::destination::{Pinned, ResolveError, Resolver};
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const KAT: &str = include_str!("flows_data/kat_media.json");
const TOKEN: &str = "oh=00_AfCDN-TOKEN-SENTINEL-77ab";

struct Dns;

#[async_trait]
impl Resolver for Dns {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<IpAddr>, ResolveError> {
        if host.starts_with("private") {
            Ok(vec!["10.0.0.7".parse().unwrap()])
        } else {
            Ok(vec!["157.240.22.35".parse().unwrap()])
        }
    }
}

struct Cdn(Vec<u8>);

#[async_trait]
impl CdnFetcher for Cdn {
    async fn get(&self, target: &Pinned, _: usize, _: Duration) -> Result<Vec<u8>, FetchError> {
        match target.destination.path_and_query.as_str() {
            p if p.starts_with("/redirect") => Err(FetchError::Status(302)),
            p if p.starts_with("/big") => Err(FetchError::TooLarge),
            p if p.starts_with("/tampered") => {
                let mut file = self.0.clone();
                file[0] ^= 1;
                Ok(file)
            }
            _ => Ok(self.0.clone()),
        }
    }
}

/// Decisive: every line of the download's services, the extractors and
/// the error rendering.
#[tokio::test]
async fn a_downloaded_file_and_its_keys_are_in_no_log_line_or_error() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let kat: Value = serde_json::from_str(KAT).unwrap();
    let file = STANDARD.decode(kat["cdn_file"].as_str().unwrap()).unwrap();
    let h = Harness::with_settings(Settings {
        flow_media: Some(FlowMediaDeployment::new(Arc::new(Dns), Arc::new(Cdn(file)))),
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], "TOKEN").await;
    let key = h.tenant_key(TENANT, &[Scope::Flows]).await;
    let metadata = kat["media"]["encryption_metadata"].clone();
    let post = |url: &str, edit: &dyn Fn(&mut Value)| {
        let mut media = kat["media"].clone();
        media["cdn_url"] = json!(url);
        edit(&mut media);
        Call::new(Method::POST, format!("/v1/wabas/{WABA}/flow-media"))
            .key(&key)
            .json(&media)
    };
    let nothing: &dyn Fn(&mut Value) = &|_| {};
    let mut bodies = Vec::new();

    let url = |path: &str, host: &str| format!("https://{host}/{path}?{TOKEN}");
    // Downloaded; then every way to be refused or to fail.
    let ok = h.call(post(&url("v/a", "mmg.whatsapp.net"), nothing)).await;
    assert_eq!(ok.status.as_u16(), 200, "{}", ok.text);
    for (call, status) in [
        (post(&url("v/a", "evil.example"), nothing), 422),
        (post(&url("v/a", "private.whatsapp.net"), nothing), 422),
        (
            post(&format!("http://mmg.whatsapp.net/v/a?{TOKEN}"), nothing),
            422,
        ),
        (post(&url("redirect", "mmg.whatsapp.net"), nothing), 502),
        (post(&url("big", "mmg.whatsapp.net"), nothing), 413),
        (post(&url("tampered", "mmg.whatsapp.net"), nothing), 502),
        (
            post(&url("v/a", "mmg.whatsapp.net"), &|m| {
                m["encryption_metadata"]["plaintext_hash"] = json!(STANDARD.encode([5u8; 32]));
            }),
            502,
        ),
        (
            post(&url("v/a", "mmg.whatsapp.net"), &|m| {
                m["encryption_metadata"]["iv"] = json!("short");
            }),
            422,
        ),
    ] {
        let reply = h.call(call).await;
        assert_eq!(reply.status.as_u16(), status, "{}", reply.text);
        bodies.push(reply.text);
    }

    let logs = captured.text();
    let secrets = [
        TOKEN,
        "oh=00_AfCDN",
        metadata["encryption_key"].as_str().unwrap(),
        metadata["hmac_key"].as_str().unwrap(),
        metadata["iv"].as_str().unwrap(),
        metadata["encrypted_hash"].as_str().unwrap(),
        metadata["plaintext_hash"].as_str().unwrap(),
        "IMG_5237",
        "not really a JPEG",
        "790aba14-5f4a-4dbd",
        "private.whatsapp.net",
        "evil.example",
        "10.0.0.7",
    ];
    for secret in secrets {
        assert!(!logs.contains(secret), "logged: {secret}");
        for body in &bodies {
            assert!(!body.contains(secret), "answered: {secret}: {body}");
        }
    }
    assert!(
        logs.contains("failed its integrity check"),
        "the capture saw the service's lines:\n{logs}"
    );
    let audited: Vec<Value> = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["target"] == "audit")
        .map(|e| e["fields"].clone())
        .collect();
    assert_eq!(audited.len(), 1, "only the success is audited: {audited:?}");
    let fields = &audited[0];
    assert_eq!(fields["action"], "flow_media_downloaded");
    assert_eq!(fields["tenant"], TENANT);
    assert_eq!(fields["waba_id"], WABA);
    assert_eq!(fields["bytes"], 41, "{fields}");
    assert_eq!(fields["key_id"], key.split('_').nth(1).unwrap());
    assert_eq!(
        fields.as_object().unwrap().len(),
        6,
        "no other field: {fields}"
    );
}
