//! `ReqwestTransport` against a local axum server: no internet needed.
#![cfg(feature = "reqwest")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Multipart as FormData, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use futures::StreamExt;
use http::Method;
use meta_whatsapp_adapters::http::ReqwestTransport;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::transport::{
    ByteStream, HttpRequest, HttpTransport, Multipart, RequestBody,
};
use serde_json::{Value, json};
use tokio::sync::Notify;
use url::Url;

/// Hand-offs between a test and the server, to prove streaming.
#[derive(Default)]
struct Shared {
    /// Test → server: "I have the first download chunk, send the rest".
    download_go: Notify,
    /// Server → test: "I have the first upload chunk".
    upload_first: Notify,
}

struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl Server {
    async fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let app = Router::new()
            .route("/echo", post(echo))
            .route("/teapot", get(teapot))
            .route("/multipart", post(multipart))
            .route("/upload", post(upload))
            .route("/download", get(download))
            .route("/slow", get(slow))
            .route("/stall-body", get(stall_body))
            .route("/hop/{port}", get(hop))
            .route("/loop", get(redirect_loop))
            .route("/headers", get(headers))
            .route("/echo-proxy", get(echo_proxy))
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { addr, shared }
    }

    fn url(&self, path_and_query: &str) -> Url {
        Url::parse(&format!("http://{}{path_and_query}", self.addr)).unwrap()
    }
}

fn header(headers: &HeaderMap, name: &str) -> Value {
    headers
        .get(name)
        .map_or(Value::Null, |v| json!(v.to_str().unwrap()))
}

async fn echo(request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let reply = json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path(),
        "query": parts.uri.query(),
        "content_type": header(&parts.headers, "content-type"),
        "content_length": header(&parts.headers, "content-length"),
        "x_test": header(&parts.headers, "x-test"),
        "body": serde_json::from_slice::<Value>(&body).unwrap(),
    });
    (StatusCode::CREATED, [("x-reply", "yes")], axum::Json(reply)).into_response()
}

async fn teapot() -> Response {
    (
        StatusCode::IM_A_TEAPOT,
        axum::Json(json!({"error": {"code": 100, "message": "short and stout"}})),
    )
        .into_response()
}

async fn multipart(headers: HeaderMap, mut form: FormData) -> axum::Json<Value> {
    let mut parts = Vec::new();
    while let Some(field) = form.next_field().await.unwrap() {
        let name = field.name().map(str::to_owned);
        let file_name = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(str::to_owned);
        let data = field.bytes().await.unwrap();
        parts.push(json!({
            "name": name,
            "file_name": file_name,
            "content_type": content_type,
            "data": data.to_vec(),
        }));
    }
    axum::Json(json!({
        "content_type": header(&headers, "content-type"),
        "content_length": header(&headers, "content-length"),
        "parts": parts,
    }))
}

async fn upload(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: Body,
) -> axum::Json<Value> {
    let mut stream = body.into_data_stream();
    let mut received = Vec::new();
    let mut frames = 0;
    while let Some(chunk) = stream.next().await {
        received.extend_from_slice(&chunk.unwrap());
        frames += 1;
        if frames == 1 {
            shared.upload_first.notify_one();
        }
    }
    axum::Json(json!({
        "body": String::from_utf8(received).unwrap(),
        "content_type": header(&headers, "content-type"),
        "content_length": header(&headers, "content-length"),
        "transfer_encoding": header(&headers, "transfer-encoding"),
    }))
}

async fn download(State(shared): State<Arc<Shared>>) -> Response {
    let chunks = futures::stream::unfold(0u8, move |step| {
        let shared = Arc::clone(&shared);
        async move {
            let chunk: &'static [u8] = match step {
                0 => b"part-1;",
                1 => {
                    // Only after the client has seen part 1: a transport
                    // that buffers the whole body would wait here forever.
                    shared.download_go.notified().await;
                    b"part-2;"
                }
                2 => b"part-3",
                _ => return None,
            };
            Some((Ok::<_, Infallible>(Bytes::from_static(chunk)), step + 1))
        }
    });
    Response::new(Body::from_stream(chunks))
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "too late"
}

async fn stall_body() -> Response {
    let head = futures::stream::once(async { Ok::<_, Infallible>(Bytes::from_static(b"head")) });
    Response::new(Body::from_stream(head.chain(futures::stream::pending())))
}

/// Redirect to `/headers` on another local server (another origin).
async fn hop(axum::extract::Path(port): axum::extract::Path<u16>) -> Response {
    (
        StatusCode::FOUND,
        [("location", format!("http://127.0.0.1:{port}/headers"))],
    )
        .into_response()
}

/// Redirect to itself, query included, forever.
async fn redirect_loop(request: Request) -> Response {
    let location = request.uri().to_string();
    (StatusCode::FOUND, [("location", location)]).into_response()
}

/// What a redirect target gets to see.
async fn headers(headers: HeaderMap) -> axum::Json<Value> {
    axum::Json(json!({
        "authorization": header(&headers, "authorization"),
        "referer": header(&headers, "referer"),
    }))
}

/// Answers a proxied (absolute-form) request with the host it was for.
async fn echo_proxy(request: Request) -> axum::Json<Value> {
    axum::Json(json!({
        "host": header(request.headers(), "host"),
        "uri": request.uri().to_string(),
    }))
}

fn transport() -> ReqwestTransport {
    ReqwestTransport::new().unwrap()
}

/// Every rendering of `err` and its whole source chain.
fn renderings(err: &TransportError) -> Vec<String> {
    let mut out = vec![format!("{err}"), format!("{err:?}")];
    let mut source = std::error::Error::source(err);
    while let Some(s) = source {
        out.push(format!("{s}"));
        out.push(format!("{s:?}"));
        source = s.source();
    }
    out
}

/// Fail instead of hanging when streaming does not happen.
async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{what} did not happen within 5s"))
}

#[tokio::test]
async fn json_round_trip_with_headers_and_query() {
    let server = Server::start().await;
    let mut request = HttpRequest::new(Method::POST, server.url("/echo?fields=id,name&x=1"));
    request.headers.insert("x-test", "abc".parse().unwrap());
    // Stale values the body must override rather than duplicate.
    request
        .headers
        .insert("content-type", "text/plain".parse().unwrap());
    request
        .headers
        .insert("content-length", "999".parse().unwrap());
    let payload =
        json!({"messaging_product": "whatsapp", "to": "15551234567", "text": {"body": "héllo"}});
    request.body = RequestBody::json(payload.to_string());

    let response = transport().send(request).await.unwrap();

    assert_eq!(response.status, StatusCode::CREATED);
    assert_eq!(response.headers["x-reply"], "yes");
    let echoed: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(
        echoed,
        json!({
            "method": "POST",
            "path": "/echo",
            "query": "fields=id,name&x=1",
            "content_type": "application/json",
            "content_length": payload.to_string().len().to_string(),
            "x_test": "abc",
            "body": payload,
        })
    );
}

#[tokio::test]
async fn non_2xx_is_a_response_not_an_error() {
    let server = Server::start().await;
    let response = transport()
        .send(HttpRequest::new(Method::GET, server.url("/teapot")))
        .await
        .unwrap();
    assert_eq!(response.status, StatusCode::IM_A_TEAPOT);
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["code"], 100);
}

#[tokio::test]
async fn multipart_parts_arrive_intact_and_in_order() {
    let server = Server::start().await;
    // Every byte value, plus CRLF and a fake boundary, in the file.
    let mut file: Vec<u8> = (0..=255).collect();
    file.extend_from_slice(b"\r\n--boundary\r\n\r\n");
    let mut request = HttpRequest::new(Method::POST, server.url("/multipart"));
    request
        .headers
        .insert("content-type", "application/json".parse().unwrap());
    request.body = RequestBody::Multipart(
        Multipart::new()
            .text("messaging_product", "whatsapp")
            .text("type", "image/png")
            .file("file", "photo é.png", "image/png", file.clone()),
    );

    let response = transport().send(request).await.unwrap();

    assert_eq!(response.status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&response.body).unwrap();
    assert!(
        got["content_type"]
            .as_str()
            .unwrap()
            .starts_with("multipart/form-data; boundary="),
        "the form's own content type wins: {}",
        got["content_type"]
    );
    assert!(
        got["content_length"].is_string(),
        "known part lengths give the form a Content-Length"
    );
    assert_eq!(
        got["parts"],
        json!([
            {"name": "messaging_product", "file_name": null, "content_type": null, "data": b"whatsapp".to_vec()},
            {"name": "type", "file_name": null, "content_type": null, "data": b"image/png".to_vec()},
            {"name": "file", "file_name": "photo é.png", "content_type": "image/png", "data": file},
        ])
    );
}

#[tokio::test]
async fn streaming_download_yields_chunks_before_the_body_ends() {
    let server = Server::start().await;
    let response = within(
        "response headers",
        transport().send_streaming(HttpRequest::new(Method::GET, server.url("/download"))),
    )
    .await
    .unwrap();
    assert_eq!(response.status, StatusCode::OK);
    let mut body = response.body;
    let first = within("first chunk", body.next()).await.unwrap().unwrap();
    assert_eq!(first, "part-1;");
    server.shared.download_go.notify_one();
    let mut rest = Vec::new();
    while let Some(chunk) = within("next chunk", body.next()).await {
        rest.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(rest, b"part-2;part-3");
}

/// `chunk-1;` then, once the server confirms it has it, `chunk-2`.
fn upload_stream(shared: Arc<Shared>) -> ByteStream {
    Box::pin(futures::stream::unfold(0u8, move |step| {
        let shared = Arc::clone(&shared);
        async move {
            let chunk: &'static [u8] = match step {
                0 => b"chunk-1;",
                1 => {
                    // A transport that collects the stream before sending
                    // would wait here forever.
                    shared.upload_first.notified().await;
                    b"chunk-2"
                }
                _ => return None,
            };
            Some((Ok(Bytes::from_static(chunk)), step + 1))
        }
    }))
}

#[tokio::test]
async fn streaming_upload_is_sent_while_produced() {
    for content_length in [Some(15), None] {
        let server = Server::start().await;
        let mut request = HttpRequest::new(Method::POST, server.url("/upload"));
        request.body = RequestBody::Stream {
            content_type: "application/octet-stream".to_owned(),
            content_length,
            stream: upload_stream(Arc::clone(&server.shared)),
        };
        let response = within("streamed upload", transport().send(request))
            .await
            .unwrap();
        let got: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(got["body"], "chunk-1;chunk-2");
        assert_eq!(got["content_type"], "application/octet-stream");
        match content_length {
            Some(len) => {
                assert_eq!(got["content_length"], len.to_string());
                assert_eq!(got["transfer_encoding"], Value::Null);
            }
            None => assert_eq!(got["transfer_encoding"], "chunked"),
        }
    }
}

#[tokio::test]
async fn request_timeout_is_timeout() {
    let server = Server::start().await;
    let mut request = HttpRequest::new(Method::GET, server.url("/slow"));
    request.timeout = Some(Duration::from_millis(200));
    let started = Instant::now();
    let err = transport().send(request).await.unwrap_err();
    assert!(matches!(err, TransportError::Timeout), "{err:?}");
    assert!(err.is_retryable());
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn builder_timeout_applies_when_the_request_has_none() {
    let server = Server::start().await;
    let transport = ReqwestTransport::builder()
        .timeout(Duration::from_millis(200))
        .connect_timeout(Duration::from_secs(1))
        .pool_idle_timeout(None)
        .pool_max_idle_per_host(2)
        .tcp_keepalive(None)
        .build()
        .unwrap();
    let err = transport
        .send(HttpRequest::new(Method::GET, server.url("/slow")))
        .await
        .unwrap_err();
    assert!(matches!(err, TransportError::Timeout), "{err:?}");
}

#[tokio::test]
async fn timeout_while_streaming_the_body_is_timeout() {
    let server = Server::start().await;
    let mut request = HttpRequest::new(Method::GET, server.url("/stall-body"));
    request.timeout = Some(Duration::from_millis(500));
    let response = transport().send_streaming(request).await.unwrap();
    let mut body = response.body;
    assert_eq!(body.next().await.unwrap().unwrap(), "head");
    let err = within("mid-stream timeout", body.next())
        .await
        .expect("an error item, not the end of the stream")
        .unwrap_err();
    assert!(matches!(err, TransportError::Timeout), "{err:?}");

    // Buffered `send` hits the same deadline while reading the body.
    let mut request = HttpRequest::new(Method::GET, server.url("/stall-body"));
    request.timeout = Some(Duration::from_millis(500));
    let err = transport().send(request).await.unwrap_err();
    assert!(matches!(err, TransportError::Timeout), "{err:?}");
}

#[tokio::test]
async fn connection_refused_is_connect_and_never_shows_the_url() {
    // A port that was just free: nothing listens there any more.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let url = Url::parse(&format!(
        "http://{addr}/v25.0/oauth/access_token?client_id=1&client_secret=s3cr3t-value&code=c0de-value"
    ))
    .unwrap();

    let err = transport()
        .send(HttpRequest::new(Method::GET, url))
        .await
        .unwrap_err();

    assert!(matches!(err, TransportError::Connect(_)), "{err:?}");
    assert!(err.is_retryable());
    let TransportError::Connect(source) = &err else {
        unreachable!()
    };
    for rendered in [
        format!("{err}"),
        format!("{err:?}"),
        format!("{source:#}"),
        format!("{source:?}"),
    ] {
        assert!(
            !rendered.contains("s3cr3t-value") && !rendered.contains("c0de-value"),
            "query secrets leaked into an error: {rendered}"
        );
        assert!(!rendered.contains("access_token"), "URL leaked: {rendered}");
    }
}

#[tokio::test]
async fn bad_content_types_are_build_errors() {
    let server = Server::start().await;
    let mut request = HttpRequest::new(Method::POST, server.url("/echo"));
    request.body = RequestBody::Bytes {
        content_type: "text/plain\r\nx-injected: 1".to_owned(),
        data: Bytes::from_static(b"{}"),
    };
    let err = transport().send(request).await.unwrap_err();
    assert!(matches!(err, TransportError::Build(_)), "{err:?}");
    assert!(!err.is_retryable());

    let mut request = HttpRequest::new(Method::POST, server.url("/multipart"));
    request.body =
        RequestBody::Multipart(Multipart::new().file("file", "a.bin", "not a mime", vec![1]));
    let err = transport().send(request).await.unwrap_err();
    assert!(matches!(err, TransportError::Build(_)), "{err:?}");
}

#[tokio::test]
async fn redirects_carry_neither_credentials_nor_the_previous_url() {
    let (origin, target) = (Server::start().await, Server::start().await);
    let mut request = HttpRequest::new(
        Method::GET,
        origin.url(&format!(
            "/hop/{}?client_secret=s3cr3t-value&code=c0de-value",
            target.addr.port()
        )),
    );
    request
        .headers
        .insert("authorization", "Bearer EAAG-token".parse().unwrap());

    let response = within("redirected request", transport().send(request))
        .await
        .unwrap();

    assert_eq!(response.status, StatusCode::OK, "the redirect is followed");
    let seen: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(
        seen,
        json!({"authorization": null, "referer": null}),
        "another origin gets no token and no Referer (which would carry the query)"
    );
}

#[tokio::test]
async fn redirect_loop_errors_never_show_the_url() {
    let server = Server::start().await;
    let err = within(
        "redirect loop",
        transport().send(HttpRequest::new(
            Method::GET,
            server.url("/loop?client_secret=s3cr3t-value&code=c0de-value"),
        )),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, TransportError::Backend(_)), "{err:?}");
    for rendered in renderings(&err) {
        assert!(
            !rendered.contains("s3cr3t-value")
                && !rendered.contains("c0de-value")
                && !rendered.contains("/loop"),
            "the URL leaked into a redirect error: {rendered}"
        );
    }
}

/// The child half of [`proxy_env_vars_are_honoured`]: run in a process whose
/// `HTTP_PROXY` points at a local server (env vars cannot be set in-process
/// here: `unsafe_code` is forbidden). Ignored in a normal run.
#[tokio::test]
#[ignore = "run by proxy_env_vars_are_honoured in a child process"]
async fn proxy_env_child() {
    assert!(
        std::env::var("HTTP_PROXY").is_ok(),
        "run by proxy_env_vars_are_honoured, which sets HTTP_PROXY"
    );
    // `.invalid` never resolves: only a proxy can answer this.
    let mut request = HttpRequest::new(
        Method::GET,
        Url::parse("http://meta-whatsapp-rs-proxy-probe.invalid/echo-proxy?x=1").unwrap(),
    );
    request.timeout = Some(Duration::from_secs(10));
    let response = transport().send(request).await.unwrap();
    assert_eq!(response.status, StatusCode::OK);
    let seen: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(seen["host"], "meta-whatsapp-rs-proxy-probe.invalid");
}

/// Integrators behind an egress proxy configure it the usual way.
#[tokio::test]
async fn proxy_env_vars_are_honoured() {
    let proxy = Server::start().await;
    let proxy_url = format!("http://{}", proxy.addr);
    let exe = std::env::current_exe().unwrap();
    let child = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(exe);
        cmd.args([
            "proxy_env_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        for var in [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "NO_PROXY",
            "no_proxy",
            "REQUEST_METHOD",
        ] {
            cmd.env_remove(var);
        }
        cmd.env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .output()
            .unwrap()
    });
    let output = tokio::time::timeout(Duration::from_secs(60), child)
        .await
        .expect("the child test finished within 60s")
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the child test must actually run, not be filtered out:\n{stdout}"
    );
}

#[tokio::test]
async fn debug_does_not_expose_the_client() {
    assert_eq!(format!("{:?}", transport()), "ReqwestTransport { .. }");
    let custom = ReqwestTransport::with_client(reqwest::Client::new());
    assert_eq!(format!("{custom:?}"), "ReqwestTransport { .. }");
}
