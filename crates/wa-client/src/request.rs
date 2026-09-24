//! Request building, execution, error decoding, retries and pagination.
//!
//! Every endpoint wrapper in this crate is a thin layer over
//! [`GraphRequest`]: build the path, attach query/body, `send::<T>()`.
//! Keeping auth, retries and error decoding here means an endpoint module
//! never touches the transport directly.

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use http::header::{AUTHORIZATION, HeaderName, HeaderValue, RETRY_AFTER, USER_AGENT};
use http::{HeaderMap, Method};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;
use wa_core::error::{GraphErrorEnvelope, TransportError, ValidationError, snippet};
use wa_core::paging::Page;
use wa_core::secret::AccessToken;
use wa_core::transport::{
    HttpRequest, HttpResponse, Multipart, Part, RequestBody, StreamingResponse,
};
use wa_core::{Error, Result};

use crate::client::Client;

/// How a request authenticates.
#[derive(Clone)]
enum Auth {
    /// The client's token, if it has one.
    Client,
    /// `Authorization: <scheme> <token>` with an explicit token.
    Explicit {
        scheme: &'static str,
        token: AccessToken,
    },
    /// No `Authorization` header (e.g. the code exchange, which carries the
    /// app secret in the query instead).
    None,
}

/// A body that can be materialized once per attempt.
enum BodySpec {
    Empty,
    Bytes {
        content_type: String,
        data: Bytes,
    },
    Multipart(Vec<PartSpec>),
    /// Streams cannot be replayed; taken on the first attempt.
    Stream(Option<RequestBody>),
}

#[derive(Clone)]
struct PartSpec {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    data: Bytes,
}

impl BodySpec {
    fn materialize(&mut self) -> Result<RequestBody> {
        Ok(match self {
            Self::Empty => RequestBody::Empty,
            Self::Bytes { content_type, data } => RequestBody::Bytes {
                content_type: content_type.clone(),
                data: data.clone(),
            },
            Self::Multipart(parts) => RequestBody::Multipart(Multipart {
                parts: parts
                    .iter()
                    .cloned()
                    .map(|p| Part {
                        name: p.name,
                        filename: p.filename,
                        content_type: p.content_type,
                        data: p.data,
                    })
                    .collect(),
            }),
            Self::Stream(body) => body.take().ok_or_else(|| {
                Error::Transport(TransportError::Build(
                    "streamed request body cannot be replayed".into(),
                ))
            })?,
        })
    }

    fn replayable(&self) -> bool {
        !matches!(self, Self::Stream(_))
    }
}

/// A request under construction. Built by [`Client::get`] and friends.
#[must_use = "a GraphRequest does nothing until sent"]
pub struct GraphRequest {
    client: Client,
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: BodySpec,
    /// First builder error (bad path segment, unserializable body/query,
    /// bad header). Kept apart from `body` so a later setter can't erase
    /// it; surfaced on send before anything reaches the transport.
    error: Option<Error>,
    auth: Auth,
    idempotent: bool,
    timeout: Option<Duration>,
    context: &'static str,
}

impl fmt::Debug for GraphRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the query: it can hold `client_secret` or `code`.
        f.debug_struct("GraphRequest")
            .field("method", &self.method)
            .field("path", &self.url.path())
            .finish_non_exhaustive()
    }
}

impl GraphRequest {
    /// A request that fails with `error` when sent (deferred builder error).
    pub(crate) fn invalid(client: Client, method: Method, url: Url, error: Error) -> Self {
        let mut req = Self::new(client, method, url);
        req.fail(error);
        req
    }

    /// Record a builder error; the first one wins.
    fn fail(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    pub(crate) fn new(client: Client, method: Method, url: Url) -> Self {
        let idempotent = matches!(method, Method::GET | Method::DELETE | Method::HEAD);
        Self {
            client,
            method,
            url,
            headers: HeaderMap::new(),
            body: BodySpec::Empty,
            error: None,
            auth: Auth::Client,
            idempotent,
            timeout: None,
            context: "Graph API response",
        }
    }

    /// Append a query parameter.
    pub fn query(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.url
            .query_pairs_mut()
            .append_pair(key, &value.to_string());
        self
    }

    /// Append a query parameter when `value` is `Some`.
    pub fn query_opt(self, key: &str, value: Option<impl fmt::Display>) -> Self {
        match value {
            Some(v) => self.query(key, v),
            None => self,
        }
    }

    /// Append a query parameter whose value is JSON (Graph takes arrays and
    /// objects this way, e.g. `template_ids=["1","2"]`).
    pub fn query_json(mut self, key: &str, value: &impl Serialize) -> Self {
        match serde_json::to_string(value) {
            Ok(s) => {
                self.url.query_pairs_mut().append_pair(key, &s);
            }
            Err(e) => {
                self.fail(ValidationError::new(key, format!("not serializable: {e}")).into());
            }
        }
        self
    }

    /// Append every field of `params` (a struct serializing to a flat map of
    /// scalars; `None` fields are skipped, nested values are JSON-encoded).
    pub fn query_struct(mut self, params: &impl Serialize) -> Self {
        let value = match serde_json::to_value(params) {
            Ok(v) => v,
            Err(e) => {
                self.fail(ValidationError::new("query", format!("not serializable: {e}")).into());
                return self;
            }
        };
        if let serde_json::Value::Object(map) = value {
            let mut pairs = self.url.query_pairs_mut();
            for (k, v) in map {
                match v {
                    serde_json::Value::Null => {}
                    serde_json::Value::String(s) => {
                        pairs.append_pair(&k, &s);
                    }
                    other => {
                        pairs.append_pair(&k, &other.to_string());
                    }
                }
            }
        }
        self
    }

    /// JSON body.
    pub fn json(mut self, body: &impl Serialize) -> Self {
        match serde_json::to_vec(body) {
            Ok(data) => {
                self.body = BodySpec::Bytes {
                    content_type: "application/json".into(),
                    data: Bytes::from(data),
                };
            }
            Err(e) => {
                self.fail(ValidationError::new("body", format!("not serializable: {e}")).into());
            }
        }
        self
    }

    /// Raw body.
    pub fn bytes(mut self, content_type: impl Into<String>, data: impl Into<Bytes>) -> Self {
        self.body = BodySpec::Bytes {
            content_type: content_type.into(),
            data: data.into(),
        };
        self
    }

    /// `multipart/form-data` body.
    pub fn multipart(mut self, form: Multipart) -> Self {
        self.body = BodySpec::Multipart(
            form.parts
                .into_iter()
                .map(|p| PartSpec {
                    name: p.name,
                    filename: p.filename,
                    content_type: p.content_type,
                    data: p.data,
                })
                .collect(),
        );
        self
    }

    /// Streamed body. Disables retries for this request.
    pub fn stream_body(mut self, body: RequestBody) -> Self {
        self.body = BodySpec::Stream(Some(body));
        self
    }

    /// Extra header. Invalid values are reported on send.
    pub fn header(mut self, name: &'static str, value: impl AsRef<str>) -> Self {
        match HeaderValue::from_str(value.as_ref()) {
            Ok(v) => {
                self.headers.insert(HeaderName::from_static(name), v);
            }
            Err(_) => self.fail(ValidationError::new(name, "invalid header value").into()),
        }
        self
    }

    /// Authenticate with `token` instead of the client's.
    pub fn bearer(mut self, token: &AccessToken) -> Self {
        self.auth = Auth::Explicit {
            scheme: "Bearer",
            token: token.clone(),
        };
        self
    }

    /// Authenticate with `Authorization: OAuth <token>` (the Resumable Upload
    /// API requires this scheme).
    pub fn oauth(mut self, token: &AccessToken) -> Self {
        self.auth = Auth::Explicit {
            scheme: "OAuth",
            token: token.clone(),
        };
        self
    }

    /// Send no `Authorization` header.
    pub fn no_auth(mut self) -> Self {
        self.auth = Auth::None;
        self
    }

    /// Override whether this request may be replayed on transient errors.
    /// Defaults to `true` for GET/DELETE/HEAD, `false` otherwise.
    pub fn idempotent(mut self, idempotent: bool) -> Self {
        self.idempotent = idempotent;
        self
    }

    /// Per-request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Name used in [`Error::Decode`] messages, e.g. `"send message response"`.
    pub fn context(mut self, context: &'static str) -> Self {
        self.context = context;
        self
    }

    fn build_attempt(&mut self) -> Result<HttpRequest> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        let body = self.body.materialize()?;
        let mut headers = self.headers.clone();
        if let Ok(ua) = HeaderValue::from_str(&self.client.shared.user_agent) {
            headers.insert(USER_AGENT, ua);
        }
        let auth = match &self.auth {
            Auth::Client => self.client.token.as_ref().map(|t| ("Bearer", t)),
            Auth::Explicit { scheme, token } => Some((*scheme, token)),
            Auth::None => None,
        };
        if let Some((scheme, token)) = auth {
            if !credential_host_allowed(&self.client, &self.url) {
                return Err(ValidationError::new(
                    "url",
                    format!(
                        "refusing to send credentials to `{}`: not the Graph endpoint or a Meta media host",
                        self.url.host_str().unwrap_or_default()
                    ),
                )
                .into());
            }
            let mut value = HeaderValue::from_str(&format!("{scheme} {}", token.expose_secret()))
                .map_err(|_| {
                Error::Transport(TransportError::Build(
                    "access token contains characters not allowed in a header".into(),
                ))
            })?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        Ok(HttpRequest {
            method: self.method.clone(),
            url: self.url.clone(),
            headers,
            body,
            timeout: Some(self.timeout.unwrap_or(self.client.shared.timeout)),
        })
    }

    /// Send, check the status, and return the raw 2xx response.
    pub async fn send_raw(mut self) -> Result<HttpResponse> {
        let policy = self.client.shared.retry;
        let replayable = self.body.replayable();
        let mut attempt = 0u32;
        loop {
            let request = self.build_attempt()?;
            tracing::debug!(method = %self.method, path = %self.url.path(), attempt, "graph request");
            let outcome = match self.client.shared.transport.send(request).await {
                Ok(resp) if resp.status.is_success() => return Ok(resp),
                Ok(resp) => {
                    let retry_after = retry_after(&resp.headers);
                    (decode_error(&resp), retry_after)
                }
                Err(e) => (Error::Transport(e), None),
            };
            let (error, retry_after) = outcome;
            if !replayable || !policy.should_retry(attempt, &error, self.idempotent) {
                return Err(error);
            }
            let delay = policy.delay(attempt, retry_after);
            // Log the classification, not the error text: a transport
            // error's message can embed the request URL, whose query may
            // carry `client_secret` or an Embedded Signup `code`.
            tracing::debug!(
                kind = ?error.kind(),
                code = error.graph().map(|g| g.code),
                ?delay,
                attempt,
                "retrying graph request"
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    /// Send and decode a JSON response.
    pub async fn send<T: DeserializeOwned>(self) -> Result<T> {
        let context = self.context;
        let resp = self.send_raw().await?;
        decode_json(context, &resp.body)
    }

    /// Send and require `{"success": true}`.
    pub async fn send_success(self) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct Success {
            success: bool,
        }
        let context = self.context;
        let resp = self.send_raw().await?;
        let s: Success = decode_json(context, &resp.body)?;
        if s.success {
            Ok(())
        } else {
            Err(Error::Http {
                status: resp.status.as_u16(),
                body_snippet: snippet(&resp.body),
            })
        }
    }

    /// Send and stream the response body (media download). Not retried.
    pub async fn send_streaming(mut self) -> Result<StreamingResponse> {
        let request = self.build_attempt()?;
        let resp = self.client.shared.transport.send_streaming(request).await?;
        if resp.status.is_success() {
            return Ok(resp);
        }
        let buffered = resp.collect().await?;
        Err(decode_error(&buffered))
    }

    /// Follow `paging.cursors.after` until the last page, yielding items.
    ///
    /// Re-issues *this* request with an `after` parameter rather than
    /// following the absolute `paging.next` URL, so credentials are never
    /// sent anywhere but the configured endpoint. Only for GET requests
    /// (anything else yields one validation error).
    pub fn paginate<T>(self) -> impl Stream<Item = Result<T>> + Send + 'static
    where
        T: DeserializeOwned + Send + 'static,
    {
        struct State {
            base: GraphRequest,
            after: Option<String>,
            buffer: std::vec::IntoIter<serde_json::Value>,
            done: bool,
        }
        let state = State {
            base: self,
            after: None,
            buffer: Vec::new().into_iter(),
            done: false,
        };
        futures::stream::unfold(state, |mut st| async move {
            loop {
                if let Some(raw) = st.buffer.next() {
                    let item = serde_json::from_value::<T>(raw)
                        .map_err(|e| Error::decode(st.base.context, e, b"(page item)"));
                    return Some((item, st));
                }
                if st.done {
                    return None;
                }
                if let Some(e) = st.base.error.take() {
                    st.done = true;
                    return Some((Err(e), st));
                }
                if st.base.method != Method::GET {
                    st.done = true;
                    let err = ValidationError::new("method", "only GET requests can be paginated");
                    return Some((Err(err.into()), st));
                }
                let mut req = st.base.clone_get();
                if let Some(after) = &st.after {
                    req = req.query("after", after);
                }
                match req.send::<Page<serde_json::Value>>().await {
                    Ok(page) => {
                        let next = page.next_cursor().map(str::to_owned);
                        st.done = next.is_none() || next == st.after;
                        st.after = next;
                        st.buffer = page.data.into_iter();
                    }
                    Err(e) => {
                        st.done = true;
                        return Some((Err(e), st));
                    }
                }
            }
        })
    }

    /// Clone a body-less request (pagination).
    fn clone_get(&self) -> Self {
        Self {
            client: self.client.clone(),
            method: self.method.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            body: BodySpec::Empty,
            error: None,
            auth: self.auth.clone(),
            idempotent: self.idempotent,
            timeout: self.timeout,
            context: self.context,
        }
    }
}

/// Hosts that may receive an `Authorization` header: the configured Graph
/// endpoint, and Meta's media CDN over HTTPS (media download URLs returned
/// by `GET /{media-id}` live on `lookaside.fbsbx.com` and require the token).
fn credential_host_allowed(client: &Client, url: &Url) -> bool {
    if client.shared.endpoint.same_origin(url) {
        return true;
    }
    url.scheme() == "https"
        && url.host_str().is_some_and(|h| {
            h == "graph.facebook.com"
                || h.ends_with(".fbsbx.com")
                || h.ends_with(".facebook.com")
                || h.ends_with(".whatsapp.net")
        })
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let secs: u64 = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs))
}

/// Turn a non-2xx response into an [`Error`].
pub(crate) fn decode_error(resp: &HttpResponse) -> Error {
    match serde_json::from_slice::<GraphErrorEnvelope>(&resp.body) {
        Ok(env) => {
            let mut e = env.error;
            e.http_status = Some(resp.status.as_u16());
            Error::from(e)
        }
        Err(_) => Error::Http {
            status: resp.status.as_u16(),
            body_snippet: snippet(&resp.body),
        },
    }
}

/// Decode a JSON body, attaching context and a snippet on failure.
pub(crate) fn decode_json<T: DeserializeOwned>(context: &'static str, body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| Error::decode(context, e, body))
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    use super::*;
    use crate::retry::RetryPolicy;

    fn client(t: &ScriptedTransport) -> Client {
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

    #[tokio::test]
    async fn sends_bearer_json_and_decodes() {
        #[derive(serde::Deserialize)]
        struct R {
            id: String,
        }
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "42"}));
        let r: R = client(&t)
            .post("123/messages")
            .json(&json!({"a": 1}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.id, "42");
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/123/messages");
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(req.json(), Some(json!({"a": 1})));
        assert!(req.header("user-agent").unwrap().starts_with("wa-rs/"));
    }

    #[tokio::test]
    async fn graph_errors_are_decoded_with_status() {
        let t = ScriptedTransport::new();
        t.push_json(
            400,
            json!({"error": {"message": "(#131047) Re-engagement message", "code": 131047, "type": "OAuthException"}}),
        );
        let err = client(&t)
            .post("1/messages")
            .send::<serde_json::Value>()
            .await
            .unwrap_err();
        let g = err.graph().unwrap();
        assert_eq!(g.code, 131_047);
        assert_eq!(g.http_status, Some(400));
    }

    #[tokio::test]
    async fn non_graph_error_body_becomes_http_error() {
        let t = ScriptedTransport::new();
        t.push_bytes(502, "text/html", "<html>bad gateway</html>");
        t.push_bytes(502, "text/html", "<html>bad gateway</html>");
        t.push_bytes(502, "text/html", "<html>bad gateway</html>");
        let err = client(&t)
            .get("1")
            .send::<serde_json::Value>()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Http { status: 502, .. }));
        assert_eq!(t.requests().len(), 3, "GET retried twice");
    }

    #[tokio::test]
    async fn post_is_not_replayed_after_timeout_but_is_after_throttle() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let err = client(&t)
            .post("1/messages")
            .send::<serde_json::Value>()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Transport(TransportError::Timeout)));
        assert_eq!(t.requests().len(), 1);

        let t = ScriptedTransport::new();
        t.push_json(400, json!({"error": {"message": "rate", "code": 130429}}));
        t.push_json(200, json!({"ok": true}));
        let v: serde_json::Value = client(&t).post("1/messages").send().await.unwrap();
        assert_eq!(v, json!({"ok": true}));
        assert_eq!(t.requests().len(), 2);
    }

    #[tokio::test]
    async fn explicit_and_absent_auth() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({}));
        t.push_json(200, json!({}));
        let c = client(&t);
        let _: serde_json::Value = c.get("a").bearer(&"OTHER".into()).send().await.unwrap();
        let _: serde_json::Value = c.get("b").no_auth().send().await.unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].bearer(), Some("OTHER"));
        assert_eq!(reqs[1].header("authorization"), None);
    }

    #[tokio::test]
    async fn paginates_by_cursor_on_the_configured_host() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [1, 2], "paging": {"cursors": {"after": "c1"}, "next": "https://evil.example/next"}}),
        );
        t.push_json(
            200,
            json!({"data": [3], "paging": {"cursors": {"after": "c2"}}}),
        );
        let items: Vec<u32> = client(&t)
            .get("waba/phone_numbers")
            .query("limit", 2)
            .paginate::<u32>()
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(items, vec![1, 2, 3]);
        let reqs = t.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].url.host_str(), Some("graph.facebook.com"));
        assert_eq!(reqs[1].query("after").as_deref(), Some("c1"));
        assert_eq!(reqs[1].query("limit").as_deref(), Some("2"));
    }

    #[tokio::test]
    async fn query_struct_skips_nulls_and_encodes_nested() {
        #[derive(Serialize)]
        struct Q {
            name: Option<&'static str>,
            status: Option<&'static str>,
            ids: Vec<u8>,
        }
        let t = ScriptedTransport::new();
        t.push_json(200, json!({}));
        let _: serde_json::Value = client(&t)
            .get("x")
            .query_struct(&Q {
                name: None,
                status: Some("APPROVED"),
                ids: vec![1, 2],
            })
            .send()
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.query("name"), None);
        assert_eq!(req.query("status").as_deref(), Some("APPROVED"));
        assert_eq!(req.query("ids").as_deref(), Some("[1,2]"));
    }

    #[tokio::test]
    async fn credentials_never_leave_meta_hosts() {
        let t = ScriptedTransport::new();
        t.push_bytes(200, "image/jpeg", "jpg");
        let c = client(&t);
        let ok = c
            .request_url(
                Method::GET,
                Url::parse("https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1")
                    .unwrap(),
            )
            .send_raw()
            .await;
        assert!(ok.is_ok());
        assert_eq!(t.last_request().unwrap().bearer(), Some("TOKEN"));

        let err = c
            .request_url(
                Method::GET,
                Url::parse("https://evil.example/steal").unwrap(),
            )
            .send_raw()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
        let err = c
            .request_url(
                Method::GET,
                Url::parse("http://lookaside.fbsbx.com/x").unwrap(),
            )
            .send_raw()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "plain http refused");
        assert_eq!(
            t.requests().len(),
            1,
            "refused requests never reach the transport"
        );
    }

    #[tokio::test]
    async fn segment_paths_cannot_be_escaped() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({}));
        let c = client(&t);
        let _: serde_json::Value = c
            .post_at(&["123/subscribed_apps", "messages"])
            .send()
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().path(),
            "/v25.0/123%2Fsubscribed_apps/messages"
        );
        let err = c
            .post_at(&["..", "messages"])
            .json(&json!({"a": 1}))
            .send::<serde_json::Value>()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
        assert_eq!(
            t.requests().len(),
            1,
            "invalid path never reaches the transport"
        );
    }

    #[tokio::test]
    async fn success_false_is_an_error() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": false}));
        assert!(client(&t).post("x").send_success().await.is_err());
    }
}
