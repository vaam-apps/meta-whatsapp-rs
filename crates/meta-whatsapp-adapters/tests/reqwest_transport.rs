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
use meta_whatsapp_adapters::http::credential_redirect_policy;
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
    /// The `Authorization` header of every request `/record` answered
    /// (`None` when there was none), in order.
    recorded: std::sync::Mutex<Vec<Option<String>>>,
}

impl Shared {
    fn recorded(&self) -> Vec<Option<String>> {
        self.recorded.lock().unwrap().clone()
    }
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
            .route("/within", get(within_host))
            .route("/within-post", post(within_host_post))
            .route("/hop-within/{port}", get(hop_within))
            .route("/ping-pong/{port}", get(ping_pong))
            .route("/record", get(record).post(record))
            .route("/redirect-to", get(redirect_to).post(redirect_to))
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

/// Redirect to `/record` on this server (the same origin).
async fn within_host() -> Response {
    (StatusCode::FOUND, [("location", "/record")]).into_response()
}

/// `307` to `/record` on this server: the method and body are replayed.
async fn within_host_post() -> Response {
    (StatusCode::TEMPORARY_REDIRECT, [("location", "/record")]).into_response()
}

/// Redirect to `/within` on another local server, which redirects within
/// itself.
async fn hop_within(axum::extract::Path(port): axum::extract::Path<u16>) -> Response {
    (
        StatusCode::FOUND,
        [("location", format!("http://127.0.0.1:{port}/within"))],
    )
        .into_response()
}

/// Redirect to `/ping-pong/{this server's port}` on the server at `port`,
/// which answers the same: two origins redirecting to each other forever.
async fn ping_pong(
    axum::extract::Path(port): axum::extract::Path<u16>,
    request: Request,
) -> Response {
    let host = request.headers()["host"].to_str().unwrap();
    let own_port = host.rsplit(':').next().unwrap();
    (
        StatusCode::FOUND,
        [(
            "location",
            format!("http://127.0.0.1:{port}/ping-pong/{own_port}?client_secret=s3cr3t-value"),
        )],
    )
        .into_response()
}

/// Records the `Authorization` header it got, and answers 200.
async fn record(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> &'static str {
    let authorization = headers
        .get("authorization")
        .map(|v| v.to_str().unwrap().to_owned());
    shared.recorded.lock().unwrap().push(authorization);
    "recorded"
}

/// `?to=<location>&status=<3xx>`: a redirect to `to`, verbatim (UTF-8 bytes
/// included), with `status` (default `302`).
async fn redirect_to(request: Request) -> Response {
    let query = request.uri().query().unwrap_or_default();
    let mut location = None;
    let mut status = StatusCode::FOUND;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match &*key {
            "to" => location = Some(value.into_owned()),
            "status" => status = StatusCode::from_bytes(value.as_bytes()).unwrap(),
            _ => {}
        }
    }
    let location = axum::http::HeaderValue::from_bytes(location.unwrap().as_bytes()).unwrap();
    (status, [(axum::http::header::LOCATION, location)]).into_response()
}

/// `/redirect-to` on `base` (a URL with no query), redirecting to `to`.
fn redirect_url(base: &Url, status: u16, to: &str) -> Url {
    let mut url = base.join("/redirect-to").unwrap();
    url.query_pairs_mut()
        .append_pair("status", &status.to_string())
        .append_pair("to", to);
    url
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

const TOKEN: &str = "Bearer EAAG-token";

/// `request` carrying `name: value`.
fn with_header(mut request: HttpRequest, name: &'static str, value: &str) -> HttpRequest {
    request.headers.insert(name, value.parse().unwrap());
    request
}

/// reqwest sends a credential header again on a redirect to the same scheme,
/// host and port. The client checked only the URL it asked for (roadmap
/// L10a: one path on `api.facebook.com`), so a request with credentials
/// does not follow such a redirect: the 3xx comes back as the response and
/// the target is never asked. Without credentials the redirect is followed,
/// as before.
#[tokio::test]
async fn a_redirect_within_the_origin_never_carries_credentials() {
    for (name, value) in [
        ("authorization", TOKEN),
        ("authorization", "OAuth EAAG-token"),
        ("proxy-authorization", "Basic cHJveHk6c2VjcmV0"),
        ("cookie", "session=s3cr3t-value"),
    ] {
        let server = Server::start().await;
        let request = with_header(
            HttpRequest::new(Method::GET, server.url("/within")),
            name,
            value,
        );
        let response = within("redirected request", transport().send(request))
            .await
            .unwrap();
        assert_eq!(
            server.shared.recorded(),
            [],
            "{name}: the redirect target was asked"
        );
        assert_eq!(response.status, StatusCode::FOUND, "{name}: not followed");
        assert_eq!(response.headers["location"], "/record", "{name}");

        // The streaming path stops the same way.
        let request = with_header(
            HttpRequest::new(Method::GET, server.url("/within")),
            name,
            value,
        );
        let response = within("redirected stream", transport().send_streaming(request))
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::FOUND, "{name}: not followed");
        assert!(server.shared.recorded().is_empty(), "{name}");

        // A 307 would replay the method and the body there too.
        let mut request = with_header(
            HttpRequest::new(Method::POST, server.url("/within-post")),
            name,
            value,
        );
        request.body = RequestBody::json(r#"{"a":1}"#);
        let response = within("redirected POST", transport().send(request))
            .await
            .unwrap();
        assert_eq!(
            response.status,
            StatusCode::TEMPORARY_REDIRECT,
            "{name}: not followed"
        );
        assert!(server.shared.recorded().is_empty(), "{name}");
    }

    // Without credentials the same redirect is followed.
    let server = Server::start().await;
    let response = within(
        "redirected request",
        transport().send(HttpRequest::new(Method::GET, server.url("/within"))),
    )
    .await
    .unwrap();
    assert_eq!(response.status, StatusCode::OK, "followed");
    assert_eq!(&response.body[..], b"recorded");
    assert_eq!(server.shared.recorded(), [None]);
}

/// reqwest puts the request's original headers back on every hop and drops
/// the credentials only when that hop changes origin. After a redirect to
/// another origin (followed, without the token), a second redirect within
/// that origin would carry the token there: the transport stops at it.
#[tokio::test]
async fn a_second_redirect_within_another_origin_never_carries_the_token() {
    let (origin, other) = (Server::start().await, Server::start().await);
    let url = || origin.url(&format!("/hop-within/{}", other.addr.port()));
    let request = with_header(HttpRequest::new(Method::GET, url()), "authorization", TOKEN);
    let response = within("redirected request", transport().send(request))
        .await
        .unwrap();
    assert_eq!(
        other.shared.recorded(),
        [],
        "the other origin's redirect target was asked"
    );
    assert_eq!(
        response.status,
        StatusCode::FOUND,
        "the first redirect is followed, the second is not"
    );
    assert_eq!(response.headers["location"], "/record");

    // Without credentials the whole chain is followed.
    let response = within(
        "redirected request",
        transport().send(HttpRequest::new(Method::GET, url())),
    )
    .await
    .unwrap();
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(other.shared.recorded(), [None]);
}

/// Redirects between two origins are followed without the credentials, and
/// still bounded: a request with a token stops after reqwest's usual limit,
/// with an error that shows no URL.
#[tokio::test]
async fn credentialed_redirects_between_origins_are_bounded() {
    let (a, b) = (Server::start().await, Server::start().await);
    let request = with_header(
        HttpRequest::new(
            Method::GET,
            a.url(&format!("/ping-pong/{}?code=c0de-value", b.addr.port())),
        ),
        "authorization",
        TOKEN,
    );
    let err = within("redirect ping-pong", transport().send(request))
        .await
        .unwrap_err();
    assert!(matches!(err, TransportError::Backend(_)), "{err:?}");
    for rendered in renderings(&err) {
        assert!(
            !rendered.contains("s3cr3t-value")
                && !rendered.contains("c0de-value")
                && !rendered.contains("/ping-pong"),
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
#[allow(deprecated)] // `with_client`: its `Debug` must stay opaque while it exists
async fn debug_does_not_expose_the_client() {
    assert_eq!(format!("{:?}", transport()), "ReqwestTransport { .. }");
    let custom = ReqwestTransport::with_client(reqwest::Client::new());
    assert_eq!(format!("{custom:?}"), "ReqwestTransport { .. }");
}

/// `url` with user info: reqwest sends it as `Authorization: Basic`.
fn with_user_info(mut url: Url, user: &str, password: Option<&str>) -> Url {
    url.set_username(user).unwrap();
    url.set_password(password).unwrap();
    url
}

/// A URL's user name and password are a credential too: reqwest takes them
/// out of the URL and sends them as `Authorization: Basic`, a header it
/// keeps on a redirect that keeps scheme, host and port and puts back on
/// every hop, like one the caller set. So a request whose URL has user info
/// follows no such redirect either: not within its origin, and not within
/// another origin after a hop there.
#[tokio::test]
async fn a_url_with_user_info_never_follows_a_redirect_with_it() {
    for (user, password, basic) in [
        (
            "user",
            Some("s3cr3t-value"),
            "Basic dXNlcjpzM2NyM3QtdmFsdWU=",
        ),
        ("user", None, "Basic dXNlcjo="),
        ("", Some("s3cr3t-value"), "Basic OnMzY3IzdC12YWx1ZQ=="),
    ] {
        // The URL asked for gets it: the user info is sent, not dropped.
        let server = Server::start().await;
        let url = with_user_info(server.url("/record"), user, password);
        let response = within(
            "request",
            transport().send(HttpRequest::new(Method::GET, url)),
        )
        .await
        .unwrap();
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(server.shared.recorded(), [Some(basic.to_owned())]);

        // A redirect within the origin is not followed, buffered or
        // streamed.
        let server = Server::start().await;
        let url = || with_user_info(server.url("/within"), user, password);
        let response = within(
            "redirected request",
            transport().send(HttpRequest::new(Method::GET, url())),
        )
        .await
        .unwrap();
        assert_eq!(response.status, StatusCode::FOUND, "{basic}: not followed");
        let response = within(
            "redirected stream",
            transport().send_streaming(HttpRequest::new(Method::GET, url())),
        )
        .await
        .unwrap();
        assert_eq!(response.status, StatusCode::FOUND, "{basic}: not followed");
        assert_eq!(
            server.shared.recorded(),
            [],
            "{basic}: the target was asked"
        );

        // After a hop to another origin (followed, without it), a second
        // hop within that origin is not.
        let (origin, other) = (Server::start().await, Server::start().await);
        let url = with_user_info(
            origin.url(&format!("/hop-within/{}", other.addr.port())),
            user,
            password,
        );
        let response = within(
            "redirected request",
            transport().send(HttpRequest::new(Method::GET, url)),
        )
        .await
        .unwrap();
        assert_eq!(response.status, StatusCode::FOUND, "{basic}");
        assert_eq!(response.headers["location"], "/record", "{basic}");
        assert_eq!(
            other.shared.recorded(),
            [],
            "{basic}: the other origin's redirect target was asked"
        );
    }
}

/// The origin check reads the URLs as reqwest does (parsed): a `Location`
/// that spells the same host or port another way is the same origin, where
/// reqwest would keep the token, so the hop is not followed.
#[tokio::test]
async fn a_redirect_to_the_same_origin_spelled_another_way_is_not_followed() {
    let server = Server::start().await;
    let port = server.addr.port();
    for to in [
        format!("http://2130706433:{port}/record"),
        format!("http://0x7f.1:{port}/record"),
        format!("http://127.1:{port}/record"),
        format!("http://127.0.0.1:0{port}/record"),
        format!("HTTP://127.0.0.1:{port}/record"),
        format!("//127.0.0.1:{port}/record"),
        format!("http://127.0.0.1:{port}/./x/../record"),
    ] {
        let request = with_header(
            HttpRequest::new(Method::GET, redirect_url(&server.url("/"), 302, &to)),
            "authorization",
            TOKEN,
        );
        let response = within("redirected request", transport().send(request))
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::FOUND, "{to}: not followed");
        assert!(
            server.shared.recorded().is_empty(),
            "{to}: the target was asked"
        );
    }
}

/// A `303`, and a `301` or `302` answering a `POST`, turn the request into a
/// `GET` without its body; the headers stay, so the same rule applies:
/// within the origin the redirect is not followed, and after a hop to
/// another origin (followed as a `GET`, without the token), a second hop
/// within it is not.
#[tokio::test]
async fn a_redirect_that_turns_a_post_into_a_get_keeps_the_rule() {
    for status in [303, 302, 301] {
        let server = Server::start().await;
        let mut request = with_header(
            HttpRequest::new(
                Method::POST,
                redirect_url(&server.url("/"), status, "/record"),
            ),
            "authorization",
            TOKEN,
        );
        request.body = RequestBody::json(r#"{"a":1}"#);
        let response = within("redirected POST", transport().send(request))
            .await
            .unwrap();
        assert_eq!(response.status.as_u16(), status, "not followed");
        assert!(
            server.shared.recorded().is_empty(),
            "{status}: the target was asked"
        );

        let (origin, other) = (Server::start().await, Server::start().await);
        let to = other.url("/within").to_string();
        let mut request = with_header(
            HttpRequest::new(Method::POST, redirect_url(&origin.url("/"), status, &to)),
            "authorization",
            TOKEN,
        );
        request.body = RequestBody::json(r#"{"a":1}"#);
        let response = within("redirected POST", transport().send(request))
            .await
            .unwrap();
        assert_eq!(
            response.status,
            StatusCode::FOUND,
            "{status}: the first hop only"
        );
        assert_eq!(response.headers["location"], "/record", "{status}");
        assert!(
            other.shared.recorded().is_empty(),
            "{status}: the target was asked"
        );
    }
}

/// A caller outside `meta-whatsapp-client` may build the header name in any
/// case: `HeaderMap` keeps names lowercase, so it is the same credential.
#[tokio::test]
async fn a_credential_header_named_in_any_case_is_a_credential() {
    for name in [
        &b"AUTHORIZATION"[..],
        b"Authorization",
        b"Proxy-Authorization",
        b"COOKIE",
    ] {
        let server = Server::start().await;
        let mut request = HttpRequest::new(Method::GET, server.url("/within"));
        request.headers.insert(
            http::HeaderName::from_bytes(name).unwrap(),
            "Bearer EAAG-token".parse().unwrap(),
        );
        let response = within("redirected request", transport().send(request))
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::FOUND);
        assert!(server.shared.recorded().is_empty());
    }
}

/// `with_clients` sends a request with a credential through the
/// credentialed client and any other through the plain one. With
/// `credential_redirect_policy()` on the first, a transport built from
/// reqwest's own clients (the plain one follows every redirect) never sends
/// a credential past the URL asked for, within its origin or within
/// another after a hop there, and still follows redirects without one.
#[tokio::test]
async fn with_clients_never_sends_a_credential_to_a_second_hop() {
    let transport = || {
        ReqwestTransport::with_clients(
            reqwest::Client::new(),
            reqwest::Client::builder()
                .redirect(credential_redirect_policy())
                .build()
                .unwrap(),
        )
    };
    for (name, value) in [
        ("authorization", TOKEN),
        ("proxy-authorization", "Basic cHJveHk6c2VjcmV0"),
        ("cookie", "session=s3cr3t-value"),
    ] {
        let server = Server::start().await;
        let request = with_header(
            HttpRequest::new(Method::GET, server.url("/within")),
            name,
            value,
        );
        let response = within("redirected request", transport().send(request))
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::FOUND, "{name}: not followed");
        assert!(server.shared.recorded().is_empty(), "{name}");

        let (origin, other) = (Server::start().await, Server::start().await);
        let url = origin.url(&format!("/hop-within/{}", other.addr.port()));
        let request = with_header(HttpRequest::new(Method::GET, url), name, value);
        let response = within("redirected stream", transport().send_streaming(request))
            .await
            .unwrap();
        assert_eq!(
            response.status,
            StatusCode::FOUND,
            "{name}: the first hop only"
        );
        assert!(other.shared.recorded().is_empty(), "{name}: second hop");
    }

    // User info in the URL is a credential too.
    let server = Server::start().await;
    let url = with_user_info(server.url("/within"), "user", Some("s3cr3t-value"));
    let response = within(
        "redirected request",
        transport().send(HttpRequest::new(Method::GET, url)),
    )
    .await
    .unwrap();
    assert_eq!(
        response.status,
        StatusCode::FOUND,
        "user info: not followed"
    );
    assert!(server.shared.recorded().is_empty());

    // Without a credential, the plain client follows.
    let (origin, other) = (Server::start().await, Server::start().await);
    let url = origin.url(&format!("/hop-within/{}", other.addr.port()));
    let response = within(
        "redirected request",
        transport().send(HttpRequest::new(Method::GET, url)),
    )
    .await
    .unwrap();
    assert_eq!(response.status, StatusCode::OK, "followed");
    assert_eq!(other.shared.recorded(), [None]);
}

/// `with_client` uses its one client for every request, those without a
/// credential included: built with `credential_redirect_policy()`, it
/// follows no redirect within an origin for anyone.
#[tokio::test]
#[allow(deprecated)] // what `with_client` still does, while it exists
async fn with_client_uses_its_client_for_every_request() {
    let transport = ReqwestTransport::with_client(
        reqwest::Client::builder()
            .redirect(credential_redirect_policy())
            .build()
            .unwrap(),
    );
    let server = Server::start().await;
    for request in [
        with_header(
            HttpRequest::new(Method::GET, server.url("/within")),
            "authorization",
            TOKEN,
        ),
        HttpRequest::new(Method::GET, server.url("/within")),
    ] {
        let response = within("redirected request", transport.send(request))
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::FOUND);
    }
    assert!(server.shared.recorded().is_empty());
}

/// `credential_redirect_policy()` compares origins as reqwest does, after
/// URL parsing: the default port spelled out or left out, an IPv6 literal
/// in another form, an IDN in Unicode, punycode or full-width letters are
/// the same origin (reqwest would keep the token: not followed), and a
/// trailing dot or another port is another (followed, without it). A local
/// proxy answers for every host, so none needs to resolve.
#[tokio::test]
async fn the_credential_redirect_policy_compares_origins_as_parsed() {
    let proxy = Server::start().await;
    let client = || {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::http(format!("http://{}", proxy.addr)).unwrap())
            .redirect(credential_redirect_policy())
            .build()
            .unwrap()
    };
    let transport = ReqwestTransport::with_clients(client(), client());
    let same_origin = [
        ("http://probe.invalid/", "http://probe.invalid:80/record"),
        ("http://probe.invalid:80/", "http://probe.invalid/record"),
        ("http://probe.invalid/", "HTTP://PROBE.invalid/record"),
        ("http://[::1]:9/", "http://[0:0:0:0:0:0:0:1]:9/record"),
        (
            "http://[::ffff:127.0.0.1]:9/",
            "http://[::ffff:7f00:1]:9/record",
        ),
        (
            "http://xn--bcher-kva.invalid/",
            "http://B\u{dc}CHER.invalid/record",
        ),
        (
            "http://b\u{fc}cher.invalid/",
            "http://xn--bcher-kva.invalid/record",
        ),
        (
            "http://probe.invalid/",
            "http://\u{ff30}\u{ff32}\u{ff2f}\u{ff22}\u{ff25}.invalid/record",
        ),
    ];
    for (from, to) in same_origin {
        let request = with_header(
            HttpRequest::new(
                Method::GET,
                redirect_url(&Url::parse(from).unwrap(), 302, to),
            ),
            "authorization",
            TOKEN,
        );
        let response = within("redirected request", transport.send(request))
            .await
            .unwrap();
        assert_eq!(
            response.status,
            StatusCode::FOUND,
            "{from} -> {to}: not followed"
        );
        assert!(proxy.shared.recorded().is_empty(), "{from} -> {to}");
    }
    let other_origin = [
        ("http://probe.invalid/", "http://probe.invalid./record"),
        ("http://probe.invalid/", "http://probe.invalid:81/record"),
        ("http://[::1]:9/", "http://[::2]:9/record"),
    ];
    for (i, (from, to)) in other_origin.into_iter().enumerate() {
        let request = with_header(
            HttpRequest::new(
                Method::GET,
                redirect_url(&Url::parse(from).unwrap(), 302, to),
            ),
            "authorization",
            TOKEN,
        );
        let response = within("redirected request", transport.send(request))
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::OK, "{from} -> {to}: followed");
        assert_eq!(
            proxy.shared.recorded(),
            vec![None; i + 1],
            "{from} -> {to}: without the token"
        );
    }
}

/// Another scheme on the same host and port is another origin too: reqwest
/// drops the token on that hop, so `credential_redirect_policy()` follows
/// it. Through a local proxy that cannot tunnel `https`, following ends in
/// an error, not in the 302 a check without the scheme would return.
#[tokio::test]
async fn a_hop_that_changes_only_the_scheme_is_followed() {
    let proxy = Server::start().await;
    let tunnelling = || {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.addr)).unwrap())
            .redirect(credential_redirect_policy())
            .build()
            .unwrap()
    };
    let transport = ReqwestTransport::with_clients(tunnelling(), tunnelling());
    let request = with_header(
        HttpRequest::new(
            Method::GET,
            redirect_url(
                &Url::parse("http://probe.invalid:443/").unwrap(),
                302,
                "https://probe.invalid:443/record",
            ),
        ),
        "authorization",
        TOKEN,
    );
    let result = within("redirected request", transport.send(request)).await;
    assert!(
        result.is_err(),
        "http -> https on the same port: followed, got {:?}",
        result.map(|r| r.status)
    );
    assert!(proxy.shared.recorded().is_empty());
}
