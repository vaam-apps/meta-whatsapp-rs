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
use http::header::{
    AUTHORIZATION, COOKIE, HeaderName, HeaderValue, PROXY_AUTHORIZATION, RETRY_AFTER, USER_AGENT,
};
use http::{HeaderMap, Method};
use meta_whatsapp_core::error::{GraphErrorEnvelope, TransportError, ValidationError, snippet};
use meta_whatsapp_core::paging::Page;
use meta_whatsapp_core::secret::AccessToken;
use meta_whatsapp_core::transport::{
    HttpRequest, HttpResponse, Multipart, Part, RequestBody, StreamingResponse,
};
use meta_whatsapp_core::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

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

    /// Extra header. An invalid name or value is reported on send, as a
    /// [`ValidationError`] on `name`, before anything is sent.
    ///
    /// Credentials are refused the same way, whatever the name's case:
    /// `Authorization`, `Proxy-Authorization` and `Cookie`. Attach a token
    /// with [`Self::bearer`] or [`Self::oauth`] (or the client's own): the
    /// credential rules check where those go, and a header would go
    /// anywhere, [`Self::no_auth`] and plain `http` included. (A URL with a
    /// user name or password is refused on send for the same reason.) Any
    /// other header goes wherever the request goes, redirects included: put
    /// no secret in one.
    pub fn header(mut self, name: &'static str, value: impl AsRef<str>) -> Self {
        let Ok(header) = HeaderName::from_bytes(name.as_bytes()) else {
            self.fail(ValidationError::new(name, "invalid header name").into());
            return self;
        };
        if CREDENTIAL_HEADERS.contains(&header) {
            self.fail(
                ValidationError::new(
                    name,
                    "credentials go through bearer() or oauth(), which the credential \
                     rules check, never through header()",
                )
                .into(),
            );
            return self;
        }
        match HeaderValue::from_str(value.as_ref()) {
            Ok(v) => {
                self.headers.insert(header, v);
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

    /// Send no `Authorization` header ([`Self::header`] cannot add one, nor
    /// can user info in the URL, which is refused).
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
        // A transport sends user info as `Authorization: Basic` (reqwest
        // does), to the URL's host: a credential no rule would check.
        if !self.url.username().is_empty() || self.url.password().is_some() {
            return Err(ValidationError::new(
                "url",
                "refusing a URL with a user name or password: it would go out as a \
                 credential; use bearer() or oauth(), which the credential rules check",
            )
            .into());
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
            if !credential_host_allowed(&self.client, &self.method, &self.url) {
                return Err(ValidationError::new(
                    "url",
                    format!(
                        "refusing to send credentials to `{}` ({}): {AllowedTargets}",
                        self.url.host_str().unwrap_or_default(),
                        self.method,
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
        let policy = self.client.retry;
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

    /// [`Self::send`] for a response that names people (the send
    /// responses): a decode failure carries neither the body nor serde's
    /// message, see [`decode_json_private`].
    pub(crate) async fn send_private<T: DeserializeOwned>(self) -> Result<T> {
        let context = self.context;
        let resp = self.send_raw().await?;
        decode_json_private(context, &resp.body)
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
            seen: std::collections::HashSet<String>,
            buffer: std::vec::IntoIter<serde_json::Value>,
            done: bool,
        }
        let state = State {
            base: self,
            after: None,
            seen: std::collections::HashSet::new(),
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
                        // Stop on the last page, and on any cursor already
                        // followed (a→b→a would otherwise loop forever).
                        let repeated = next.as_ref().is_some_and(|c| !st.seen.insert(c.clone()));
                        if repeated {
                            tracing::warn!("pagination cursor repeated; stopping");
                        }
                        st.done = next.is_none() || repeated;
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

/// `{"success": false}` from a call that answers `success` next to its
/// payload is an error, as in [`GraphRequest::send_success`].
pub(crate) async fn send_checked<T: DeserializeOwned>(
    request: GraphRequest,
    context: &'static str,
) -> Result<T> {
    #[derive(serde::Deserialize)]
    struct Success {
        #[serde(default)]
        success: Option<bool>,
    }
    let resp = request.context(context).send_raw().await?;
    let flag: Success = decode_json(context, &resp.body)?;
    if flag.success == Some(false) {
        return Err(Error::Http {
            status: resp.status.as_u16(),
            body_snippet: snippet(&resp.body),
        });
    }
    decode_json(context, &resp.body)
}

/// The check every `…_stream(&query)` makes first: the stream manages the
/// cursors itself (re-issuing the request with `after`), so a query that
/// already carries one is refused with a [`ValidationError`] naming it,
/// yielded as the stream's single item.
pub(crate) fn reject_cursors(after: Option<&str>, before: Option<&str>) -> Result<()> {
    for (field, cursor) in [("after", after), ("before", before)] {
        if cursor.is_some() {
            return Err(
                ValidationError::new(field, "streams manage cursors; leave it unset").into(),
            );
        }
    }
    Ok(())
}

/// `stream`, or — when it could not be built — a stream whose single item
/// is the error. The one way a `…_stream()` method reports a request that
/// failed before sending (a bad `limit`, a cursor the stream manages
/// itself): as its first and only item, never a panic or an empty stream.
pub(crate) fn stream_or_error<S, T>(
    stream: Result<S>,
) -> impl Stream<Item = Result<T>> + Send + 'static
where
    S: Stream<Item = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    use futures::StreamExt;
    match stream {
        Ok(stream) => stream.left_stream(),
        Err(error) => futures::stream::once(futures::future::ready(Err(error))).right_stream(),
    }
}

/// [`GraphRequest::paginate`] a request that may have failed to build, see
/// [`stream_or_error`].
pub(crate) fn paginate_or_error<T>(
    request: Result<GraphRequest>,
) -> impl Stream<Item = Result<T>> + Send + 'static
where
    T: DeserializeOwned + Send + 'static,
{
    stream_or_error(request.map(GraphRequest::paginate::<T>))
}

/// Media download URLs (from `GET /{media-id}` and media webhooks) point at
/// this host, and Meta refuses the download without the token
/// (`business-phone-numbers/media`).
pub(crate) const MEDIA_HOST: &str = "lookaside.fbsbx.com";

/// The host of Meta's Parent BSUID Accounts API (`business-scoped-user-ids`,
/// § Get parent BSUID account): `GET https://api.facebook.com/{BUSINESS_ID}/parent-bsuid-accounts`,
/// with no API version in the path. Meta's changelog entry of May 28, 2026
/// on that page corrected the host from `graph.facebook.com` to this one.
pub(crate) const PARENT_BSUID_ACCOUNTS_HOST: &str = "api.facebook.com";

/// The edge of the Parent BSUID Accounts API, after the business id.
pub(crate) const PARENT_BSUID_ACCOUNTS_EDGE: &str = "parent-bsuid-accounts";

/// Headers that carry a credential, which [`GraphRequest::header`] refuses:
/// a token goes only through the `Auth` the credential rules check.
const CREDENTIAL_HEADERS: [HeaderName; 3] = [AUTHORIZATION, PROXY_AUTHORIZATION, COOKIE];

/// One segment of a [`PathRule::Exact`] path.
enum Segment {
    /// This text, byte for byte (case included).
    Literal(&'static str),
    /// One or more ASCII digits: a Graph object id.
    Digits,
}

impl Segment {
    fn matches(&self, segment: &str) -> bool {
        match self {
            Self::Literal(literal) => segment == *literal,
            Self::Digits => !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()),
        }
    }
}

/// Which URLs on a [`CredentialRule`]'s host the rule lets a token reach.
enum PathRule {
    /// Any path and query: media download URLs are links Meta mints.
    Any,
    /// Exactly these segments, and nothing else in the URL: no query, no
    /// fragment, no user name or password. The path is compared as it will
    /// be sent: still percent-encoded (`%2F` is not a `/`, `%31` not a
    /// digit), and after URL parsing resolved `.` and `..` (a
    /// [`Url`] never holds one).
    Exact(&'static [Segment]),
}

impl PathRule {
    fn matches(&self, url: &Url) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(pattern) => {
                if url.query().is_some()
                    || url.fragment().is_some()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return false;
                }
                let Some(mut segments) = url.path_segments() else {
                    return false;
                };
                for want in *pattern {
                    match segments.next() {
                        Some(segment) if want.matches(segment) => {}
                        _ => return false,
                    }
                }
                segments.next().is_none()
            }
        }
    }

    fn describe(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => Ok(()),
            Self::Exact(pattern) => pattern.iter().try_for_each(|segment| match segment {
                Segment::Literal(literal) => write!(f, "/{literal}"),
                Segment::Digits => f.write_str("/{digits}"),
            }),
        }
    }
}

/// A request, other than one to the configured Graph endpoint, that may
/// carry a token: `https` on the default port (every rule; not data, so no
/// rule can loosen it), this exact host, this method, this path.
struct CredentialRule {
    /// The host, compared exactly after URL parsing (which lowercases it and
    /// maps IDNs to punycode): no subdomain, suffix or trailing dot.
    host: &'static str,
    /// `None` for any method.
    method: Option<Method>,
    /// The URLs on the host it covers.
    path: PathRule,
}

impl CredentialRule {
    fn allows(&self, method: &Method, url: &Url) -> bool {
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.host_str() == Some(self.host)
            && self.method.as_ref().is_none_or(|m| m == method)
            && self.path.matches(url)
    }
}

impl fmt::Display for CredentialRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(method) = &self.method {
            write!(f, "{method} ")?;
        }
        write!(f, "https://{}", self.host)?;
        self.path.describe(f)
    }
}

/// Where a token may go besides the configured Graph endpoint: one line per
/// documented need, each reviewed on its own (a new line widens where
/// credentials go: it needs the security review).
///
/// 1. `https://lookaside.fbsbx.com`, any method and path: media download
///    URLs (see [`MEDIA_HOST`]).
/// 2. `GET https://api.facebook.com/{digits}/parent-bsuid-accounts`, and
///    nothing else on that host: the Parent BSUID Accounts API (see
///    [`PARENT_BSUID_ACCOUNTS_HOST`]), which Meta serves there and not on
///    Graph. No version segment, no query, no other method.
///
/// Neither depends on the configured Graph endpoint: behind a proxy
/// ([`crate::ClientBuilder::endpoint`]) the token goes to the proxy and to
/// these, never to `graph.facebook.com`.
static CREDENTIAL_RULES: &[CredentialRule] = &[
    CredentialRule {
        host: MEDIA_HOST,
        method: None,
        path: PathRule::Any,
    },
    CredentialRule {
        host: PARENT_BSUID_ACCOUNTS_HOST,
        method: Some(Method::GET),
        path: PathRule::Exact(&[
            Segment::Digits,
            Segment::Literal(PARENT_BSUID_ACCOUNTS_EDGE),
        ]),
    },
];

/// Whether a `method` request to `url` may carry an `Authorization` header:
/// it goes to the configured Graph endpoint (scheme, host and port), or one
/// of the [`CREDENTIAL_RULES`] allows it. Nothing else — not other Meta
/// hosts (CDN links such as `*.fbcdn.net` or `*.whatsapp.net` need no
/// token), not subdomains, and not production Graph when the client is
/// configured for a proxy.
fn credential_host_allowed(client: &Client, method: &Method, url: &Url) -> bool {
    client.shared.endpoint.same_origin(url)
        || CREDENTIAL_RULES.iter().any(|rule| rule.allows(method, url))
}

/// The refusal's reason: the rules, as text (never the URL's path or query).
struct AllowedTargets;

impl fmt::Display for AllowedTargets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not the configured Graph endpoint")?;
        for rule in CREDENTIAL_RULES {
            write!(f, ", nor {rule}")?;
        }
        Ok(())
    }
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

/// A decode error for a response that names people: `why` replaces serde's
/// message, and the body snippet is a placeholder.
pub(crate) fn withheld_decode_error(context: &'static str, why: impl fmt::Display) -> Error {
    Error::decode(
        context,
        <serde_json::Error as serde::de::Error>::custom(why),
        b"(withheld: the response names the recipient)",
    )
}

/// Decode a body that names people — send responses echo the recipient's
/// phone number in `contacts[]`. On failure neither the body nor serde's
/// message (which quotes the offending value) reaches the error, only the
/// error category and position.
pub(crate) fn decode_json_private<T: DeserializeOwned>(
    context: &'static str,
    body: &[u8],
) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| {
        withheld_decode_error(
            context,
            format_args!(
                "{:?} error at line {} column {} (details withheld)",
                e.classify(),
                e.line(),
                e.column()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use serde_json::json;

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
        assert!(
            req.header("user-agent")
                .unwrap()
                .starts_with("meta-whatsapp-rs/")
        );
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

    /// `with_retry` changes the copy's policy only; `with_token` keeps it.
    #[tokio::test]
    async fn with_retry_replaces_the_policy_of_the_copy_only() {
        let t = ScriptedTransport::new();
        let base = client(&t);
        let none = base
            .clone()
            .with_retry(RetryPolicy::NONE)
            .with_token("OTHER".into());
        assert_eq!(none.retry_policy(), RetryPolicy::NONE);
        assert_eq!(base.retry_policy().max_retries, 2);

        t.push_json(400, json!({"error": {"message": "rate", "code": 130429}}));
        let err = none
            .post("1/messages")
            .send::<serde_json::Value>()
            .await
            .unwrap_err();
        assert_eq!(err.kind(), meta_whatsapp_core::ErrorKind::RateLimited);
        assert_eq!(t.requests().len(), 1, "no replay under RetryPolicy::NONE");
        assert_eq!(t.last_request().unwrap().bearer(), Some("OTHER"));

        t.push_json(400, json!({"error": {"message": "rate", "code": 130429}}));
        t.push_json(200, json!({"ok": true}));
        let v: serde_json::Value = base.post("1/messages").send().await.unwrap();
        assert_eq!(v, json!({"ok": true}));
        assert_eq!(t.requests().len(), 3, "the original still replays");
        assert_eq!(t.remaining(), 0);
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
    async fn pagination_stops_on_a_cursor_cycle() {
        let t = ScriptedTransport::new();
        let page = |d: u32, after: &str| json!({"data": [d], "paging": {"cursors": {"after": after}, "next": "https://graph.facebook.com/x"}});
        t.push_json(200, page(1, "a"));
        t.push_json(200, page(2, "b"));
        t.push_json(200, page(3, "a")); // back to a: must not loop
        t.push_json(200, page(4, "b"));
        let items: Vec<u32> = client(&t)
            .get("x")
            .paginate::<u32>()
            .take(10)
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(items, vec![1, 2, 3]);
        assert_eq!(t.remaining(), 1, "the cycle is not followed");
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

    /// Where an absolute URL may take the token: the configured Graph
    /// endpoint and `https://lookaside.fbsbx.com` (media downloads), and
    /// nothing else — not other Meta hosts, not look-alikes, not
    /// `graph.facebook.com` when the client is configured for a proxy.
    /// An explicit token (`bearer`/`oauth`) obeys the same host allowlist as
    /// the client's own; `no_auth` requests carry nothing and may go anywhere.
    #[tokio::test]
    async fn explicit_tokens_obey_the_same_allowlist() {
        let foreign = || Url::parse("https://evil.example/x").unwrap();
        for scheme in ["bearer", "oauth"] {
            let t = ScriptedTransport::new();
            let req = client(&t).request_url(Method::GET, foreign());
            let req = if scheme == "bearer" {
                req.bearer(&"OTHER".into())
            } else {
                req.oauth(&"OTHER".into())
            };
            let err = req.send_raw().await.unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "url"),
                "{scheme}: {err}"
            );
            assert!(t.requests().is_empty(), "{scheme} reached the transport");
        }
        let t = ScriptedTransport::new();
        t.push_bytes(200, "text/plain", "ok");
        client(&t)
            .request_url(Method::GET, foreign())
            .no_auth()
            .send_raw()
            .await
            .unwrap();
        assert_eq!(t.last_request().unwrap().header("authorization"), None);
    }

    #[tokio::test]
    async fn credentials_go_only_to_the_endpoint_and_the_media_host() {
        let allowed = [
            "https://graph.facebook.com/v25.0/1037543291543636",
            "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1",
            "https://LOOKASIDE.fbsbx.com/x",
            "https://lookaside.fbsbx.com:443/x",
        ];
        let denied = [
            "https://evil.example/steal",
            "http://lookaside.fbsbx.com/x",
            "https://lookaside.fbsbx.com:8443/x",
            "https://lookaside.fbsbx.com./x",
            "https://lookaside.fbsbx.com.evil.example/x",
            "https://evil.lookaside.fbsbx.com/x",
            "https://scontent.xx.fbsbx.com/x",
            "https://www.facebook.com/x",
            "https://business.facebook.com/x",
            "https://pps.whatsapp.net/v/t61.24",
            "https://mmg.whatsapp.net/v/redacted",
            "https://scontent.xx.fbcdn.net/q.png",
            "http://graph.facebook.com/v25.0/1",
            "https://graph.facebook.com:8443/v25.0/1",
            // Right host and port, wrong scheme: only https may carry it.
            "http://lookaside.fbsbx.com:443/x",
            "wss://lookaside.fbsbx.com/x",
            "ws://lookaside.fbsbx.com/x",
        ];
        for url in allowed {
            let t = ScriptedTransport::new();
            t.push_bytes(200, "image/jpeg", "jpg");
            let sent = client(&t)
                .request_url(Method::GET, Url::parse(url).unwrap())
                .send_raw()
                .await;
            assert!(sent.is_ok(), "{url}: {sent:?}");
            assert_eq!(t.last_request().unwrap().bearer(), Some("TOKEN"), "{url}");
            assert_eq!(t.remaining(), 0);
        }
        for url in denied {
            let t = ScriptedTransport::new();
            let err = client(&t)
                .request_url(Method::GET, Url::parse(url).unwrap())
                .send_raw()
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "url"),
                "{url}: {err}"
            );
            assert!(t.requests().is_empty(), "{url} reached the transport");
        }

        // A client configured for a proxy sends its token to the proxy and
        // the media host only; production Graph is just another host then.
        let t = ScriptedTransport::new();
        t.push_bytes(200, "application/json", "{}");
        t.push_bytes(200, "image/jpeg", "jpg");
        let proxied = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .endpoint(
                meta_whatsapp_core::config::GraphEndpoint::custom(
                    "https://graph-proxy.internal/",
                    meta_whatsapp_core::config::ApiVersion::DEFAULT,
                )
                .unwrap(),
            )
            .build()
            .unwrap();
        for url in [
            "https://graph-proxy.internal/v25.0/1",
            "https://lookaside.fbsbx.com/x",
        ] {
            assert!(
                proxied
                    .request_url(Method::GET, Url::parse(url).unwrap())
                    .send_raw()
                    .await
                    .is_ok(),
                "{url}"
            );
        }
        let err = proxied
            .request_url(
                Method::GET,
                Url::parse("https://graph.facebook.com/v25.0/1").unwrap(),
            )
            .send_raw()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
        assert_eq!(t.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_request_that_failed_to_build_streams_its_error_once() {
        let t = ScriptedTransport::new();
        let failed: Result<GraphRequest> = Err(ValidationError::new("limit", "too big").into());
        let items: Vec<Result<u32>> = paginate_or_error(failed).collect().await;
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Err(Error::Validation(v)) if v.field == "limit"));
        assert!(t.requests().is_empty());

        t.push_json(200, json!({"data": [1, 2]}));
        let items: Vec<u32> = paginate_or_error::<u32>(Ok(client(&t).get("x")))
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(items, [1, 2]);
        assert_eq!(t.remaining(), 0);
    }

    /// The query can hold `client_secret`, an Embedded Signup `code` or an
    /// `access_token`: `Debug` (what `?request` logs) shows method and path
    /// only.
    #[test]
    fn debug_never_prints_the_query() {
        let t = ScriptedTransport::new();
        let request = client(&t)
            .get("oauth/access_token")
            .query("client_id", "1234")
            .query("client_secret", "s3cr3t-app-secret")
            .query("code", "AQBx-signup-code")
            .query("access_token", "EAAB-token");
        let debug = format!("{request:?}");
        assert_eq!(
            debug,
            "GraphRequest { method: GET, path: \"/v25.0/oauth/access_token\", .. }"
        );
        for secret in ["s3cr3t", "AQBx", "EAAB", "client_secret", "code=", "?"] {
            assert!(!debug.contains(secret), "{secret} in {debug}");
        }
        let alternate = format!("{request:#?}");
        assert!(!alternate.contains("s3cr3t"), "{alternate}");
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

/// The credential rules beyond the media host: the Parent BSUID Accounts
/// API's one URL on `api.facebook.com` (roadmap L10a). The allow list's
/// earlier tests, in `tests` above, are unchanged.
#[cfg(test)]
mod credential_rule_tests {
    use std::sync::{Arc, Mutex};

    use meta_whatsapp_core::config::{ApiVersion, GraphEndpoint};
    use meta_whatsapp_core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;
    use crate::retry::RetryPolicy;

    const ALLOWED: &str = "https://api.facebook.com/1234567890/parent-bsuid-accounts";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn proxied(t: &ScriptedTransport, base: &str) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .endpoint(GraphEndpoint::custom(base, ApiVersion::DEFAULT).unwrap())
            .build()
            .unwrap()
    }

    /// Sends `method url` with the client's token and asserts it was refused
    /// before the transport: `Error::Validation` on `url`, no request.
    async fn assert_refused(c: &Client, t: &ScriptedTransport, method: Method, url: &str) {
        let before = t.requests().len();
        let err = c
            .request_url(method.clone(), Url::parse(url).unwrap())
            .send_raw()
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "url"),
            "{method} {url}: {err}"
        );
        assert_eq!(
            t.requests().len(),
            before,
            "{method} {url} reached the transport"
        );
    }

    /// The one request the rule exists for: `GET`, `https`, the default
    /// port, `/{digits}/parent-bsuid-accounts`, no version segment. It
    /// reaches the transport with the token, at exactly that URL.
    #[tokio::test]
    async fn the_parent_bsuid_accounts_url_takes_the_token() {
        let allowed = [
            (ALLOWED, ALLOWED),
            (
                "https://api.facebook.com/1/parent-bsuid-accounts",
                "https://api.facebook.com/1/parent-bsuid-accounts",
            ),
            // The default port, spelled out: parsing drops it.
            (
                "https://api.facebook.com:443/1234567890/parent-bsuid-accounts",
                ALLOWED,
            ),
            // Parsing lowercases the host: the same origin, sent lowercase.
            (
                "https://API.Facebook.COM/1234567890/parent-bsuid-accounts",
                ALLOWED,
            ),
        ];
        for (given, sent) in allowed {
            let t = ScriptedTransport::new();
            t.push_json(200, json!({}));
            client(&t)
                .request_url(Method::GET, Url::parse(given).unwrap())
                .send_raw()
                .await
                .unwrap_or_else(|e| panic!("{given}: {e}"));
            let req = t.last_request().unwrap();
            assert_eq!(req.method, Method::GET, "{given}");
            assert_eq!(req.url.as_str(), sent, "{given}");
            assert_eq!(req.bearer(), Some("TOKEN"), "{given}");
            assert_eq!(t.remaining(), 0);
        }
        // An explicit token obeys the same rule as the client's own.
        let t = ScriptedTransport::new();
        t.push_json(200, json!({}));
        client(&t)
            .request_url(Method::GET, Url::parse(ALLOWED).unwrap())
            .bearer(&"OTHER".into())
            .send_raw()
            .await
            .unwrap();
        assert_eq!(t.last_request().unwrap().bearer(), Some("OTHER"));
        for explicit in ["bearer", "oauth"] {
            let t = ScriptedTransport::new();
            let req = client(&t).request_url(
                Method::GET,
                Url::parse("https://api.facebook.com/1234567890/other").unwrap(),
            );
            let req = if explicit == "bearer" {
                req.bearer(&"OTHER".into())
            } else {
                req.oauth(&"OTHER".into())
            };
            let err = req.send_raw().await.unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "url"),
                "{explicit}: {err}"
            );
            assert!(t.requests().is_empty(), "{explicit} reached the transport");
        }
    }

    /// Every other URL on `api.facebook.com`, and the right path anywhere
    /// else, is refused before any request. Removing any part of the rule
    /// (host, path shape, digits, scheme, port, the end of the path, the
    /// empty query) lets one of these through.
    #[tokio::test]
    async fn every_other_url_on_api_facebook_com_is_refused_before_sending() {
        let denied = [
            // Other paths on the host.
            "https://api.facebook.com/",
            "https://api.facebook.com/me",
            "https://api.facebook.com/1234567890",
            "https://api.facebook.com/1234567890/other-edge",
            "https://api.facebook.com/parent-bsuid-accounts",
            "https://api.facebook.com/parent-bsuid-accounts/1234567890",
            "https://api.facebook.com/1234567890/parent_bsuid_accounts",
            // Paths are case-sensitive, unlike the host.
            "https://api.facebook.com/1234567890/Parent-Bsuid-Accounts",
            "https://api.facebook.com/1234567890/PARENT-BSUID-ACCOUNTS",
            // A version segment: this API takes none.
            "https://api.facebook.com/v25.0/1234567890/parent-bsuid-accounts",
            "https://api.facebook.com/v1.0/1234567890/parent-bsuid-accounts",
            "https://api.facebook.com/1234567890/v25.0/parent-bsuid-accounts",
            // A trailing segment, or a trailing slash (an empty segment).
            "https://api.facebook.com/1234567890/parent-bsuid-accounts/extra",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts/",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts/1234567890/parent-bsuid-accounts",
            // An id that is not digits, or no id at all.
            "https://api.facebook.com/abc/parent-bsuid-accounts",
            "https://api.facebook.com/12a4/parent-bsuid-accounts",
            "https://api.facebook.com/-1/parent-bsuid-accounts",
            "https://api.facebook.com/1.5/parent-bsuid-accounts",
            "https://api.facebook.com/me/parent-bsuid-accounts",
            "https://api.facebook.com/%31%32%33/parent-bsuid-accounts",
            "https://api.facebook.com//parent-bsuid-accounts",
            "https://api.facebook.com/%20/parent-bsuid-accounts",
            // Another scheme.
            "http://api.facebook.com/1234567890/parent-bsuid-accounts",
            "http://api.facebook.com:443/1234567890/parent-bsuid-accounts",
            "wss://api.facebook.com/1234567890/parent-bsuid-accounts",
            // Another port.
            "https://api.facebook.com:8443/1234567890/parent-bsuid-accounts",
            "https://api.facebook.com:80/1234567890/parent-bsuid-accounts",
            // A percent-encoded `/` is not a separator: one segment, or an
            // id that is not digits.
            "https://api.facebook.com/1234567890%2Fparent-bsuid-accounts",
            "https://api.facebook.com/1234567890%2F5/parent-bsuid-accounts",
            "https://api.facebook.com/me%2F1234567890/parent-bsuid-accounts",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts%2Fextra",
            // `..` segments: parsing resolves them, and what is left is not
            // the path (see `the_rule_sees_the_url_as_parsed`).
            "https://api.facebook.com/1234567890/../parent-bsuid-accounts",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts/..",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts/../../me",
            "https://api.facebook.com/1234567890/%2e%2e/parent-bsuid-accounts",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts/%2E%2E/other",
            // Anything but the path: a query (even empty), a fragment, a
            // user name or password.
            "https://api.facebook.com/1234567890/parent-bsuid-accounts?fields=id",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts?",
            "https://api.facebook.com/1234567890/parent-bsuid-accounts#frag",
            "https://user@api.facebook.com/1234567890/parent-bsuid-accounts",
            "https://user:pw@api.facebook.com/1234567890/parent-bsuid-accounts",
            "https://:pw@api.facebook.com/1234567890/parent-bsuid-accounts",
            // The right path on another host: look-alikes, subdomains,
            // suffixes, a trailing dot, the pre-correction host behind a
            // proxy is in `the_rule_ignores_the_configured_graph_endpoint`.
            "https://api.facebook.com./1234567890/parent-bsuid-accounts",
            "https://evil.api.facebook.com/1234567890/parent-bsuid-accounts",
            "https://xapi.facebook.com/1234567890/parent-bsuid-accounts",
            "https://api.facebook.com.evil.example/1234567890/parent-bsuid-accounts",
            "https://api.facebook.co/1234567890/parent-bsuid-accounts",
            "https://api-facebook.com/1234567890/parent-bsuid-accounts",
            "https://www.facebook.com/1234567890/parent-bsuid-accounts",
            "https://facebook.com/1234567890/parent-bsuid-accounts",
            "https://evil.example/1234567890/parent-bsuid-accounts",
            "https://lookaside.fbsbx.com.evil.example/1234567890/parent-bsuid-accounts",
            // A Cyrillic `а` (punycode after parsing).
            "https://\u{430}pi.facebook.com/1234567890/parent-bsuid-accounts",
        ];
        for url in denied {
            let t = ScriptedTransport::new();
            assert_refused(&client(&t), &t, Method::GET, url).await;
        }
    }

    /// The rule is `GET` only: every other method on the exact URL is
    /// refused before any request.
    #[tokio::test]
    async fn only_get_takes_the_token_to_api_facebook_com() {
        let t = ScriptedTransport::new();
        let c = client(&t);
        for method in [
            Method::POST,
            Method::DELETE,
            Method::PUT,
            Method::PATCH,
            Method::HEAD,
            Method::OPTIONS,
            Method::from_bytes(b"get").unwrap(),
        ] {
            assert_refused(&c, &t, method, ALLOWED).await;
        }
        let err = c
            .request_url(Method::POST, Url::parse(ALLOWED).unwrap())
            .json(&json!({"a": 1}))
            .send_raw()
            .await
            .unwrap_err();
        assert!(matches!(&err, Error::Validation(v) if v.field == "url"));
        assert!(t.requests().is_empty());
    }

    /// URL parsing normalises before the rule looks: it lowercases the host
    /// and decodes it (IDNA, percent-escapes), turns `\` into `/` and
    /// resolves `.` and `..`. The rule matches the URL as it goes on the
    /// wire, so each of these is the one allowed URL, sent exactly as that:
    /// no `..`, no other spelling, reaches the transport.
    #[tokio::test]
    async fn the_rule_sees_the_url_as_parsed() {
        let upper = Url::parse("https://API.FACEBOOK.COM/1/parent-bsuid-accounts").unwrap();
        assert_eq!(upper.host_str(), Some("api.facebook.com"));

        // `push("..")` is dropped by the `url` crate: it cannot smuggle one
        // in (the endpoint checks its id is digits before pushing it anyway).
        let mut pushed = Url::parse("https://api.facebook.com/").unwrap();
        pushed
            .path_segments_mut()
            .unwrap()
            .pop_if_empty()
            .push("1234567890")
            .push("..")
            .push("parent-bsuid-accounts");
        assert_eq!(pushed.as_str(), ALLOWED);

        let spellings = [
            "https://api.facebook.com/me/../1234567890/./parent-bsuid-accounts",
            "https://api.facebook.com/x/%2e%2e/1234567890/parent-bsuid-accounts",
            "https://api.facebook.com/1234567890\\parent-bsuid-accounts",
            "https://api%2Efacebook.com/1234567890/parent-bsuid-accounts",
            "https://\u{ff41}pi.facebook.com/1234567890/parent-bsuid-accounts",
        ];
        for given in spellings {
            let url = Url::parse(given).unwrap();
            assert_eq!(url.as_str(), ALLOWED, "{given}");
            let t = ScriptedTransport::new();
            t.push_json(200, json!({}));
            client(&t)
                .request_url(Method::GET, url)
                .send_raw()
                .await
                .unwrap();
            let req = t.last_request().unwrap();
            assert_eq!(req.url.as_str(), ALLOWED, "{given}");
            assert_eq!(req.bearer(), Some("TOKEN"), "{given}");
            assert_eq!(t.remaining(), 0);
        }
        let t = ScriptedTransport::new();
        t.push_json(200, json!({}));
        client(&t)
            .request_url(Method::GET, pushed)
            .send_raw()
            .await
            .unwrap();
        assert_eq!(t.last_request().unwrap().url.as_str(), ALLOWED);
    }

    /// Behind a Graph proxy (or a custom base URL), the token goes to the
    /// proxy, to the media host and to the one Parent BSUID Accounts URL:
    /// the rule does not follow the configured endpoint. It widens nothing
    /// else: `graph.facebook.com` (where Meta first documented this API)
    /// stays refused, and so does every other URL or method on
    /// `api.facebook.com`.
    #[tokio::test]
    async fn the_rule_ignores_the_configured_graph_endpoint() {
        for base in [
            "https://graph-proxy.internal/",
            "http://127.0.0.1:8080/graph",
        ] {
            let t = ScriptedTransport::new();
            let c = proxied(&t, base);
            t.push_json(200, json!({}));
            c.request_url(Method::GET, Url::parse(ALLOWED).unwrap())
                .send_raw()
                .await
                .unwrap();
            let req = t.last_request().unwrap();
            assert_eq!(req.url.as_str(), ALLOWED, "{base}");
            assert_eq!(req.bearer(), Some("TOKEN"), "{base}");
            for url in [
                "https://graph.facebook.com/1234567890/parent-bsuid-accounts",
                "https://graph.facebook.com/v25.0/1234567890/parent-bsuid-accounts",
                "https://api.facebook.com/v25.0/1234567890/parent-bsuid-accounts",
                "https://api.facebook.com/1234567890/other-edge",
            ] {
                assert_refused(&c, &t, Method::GET, url).await;
            }
            assert_refused(&c, &t, Method::POST, ALLOWED).await;
            assert_eq!(t.requests().len(), 1, "{base}");
            assert_eq!(t.remaining(), 0);
        }
    }

    /// The refusal says where a token may go, and names the host and the
    /// method: never the path or the query.
    #[tokio::test]
    async fn the_refusal_names_the_rules() {
        let t = ScriptedTransport::new();
        let err = client(&t)
            .request_url(
                Method::POST,
                Url::parse(
                    "https://api.facebook.com/1234567890/parent-bsuid-accounts?q=private-query",
                )
                .unwrap(),
            )
            .send_raw()
            .await
            .unwrap_err();
        let Error::Validation(v) = &err else {
            panic!("{err:?}")
        };
        assert_eq!(v.field, "url");
        assert_eq!(
            v.reason,
            "refusing to send credentials to `api.facebook.com` (POST): not the configured \
             Graph endpoint, nor https://lookaside.fbsbx.com, nor \
             GET https://api.facebook.com/{digits}/parent-bsuid-accounts"
        );
        assert!(!err.to_string().contains("private-query"), "{err}");
        assert!(t.requests().is_empty());
    }

    /// The media host's rule is as it was before the rules became data: any
    /// method, any path, https on the default port.
    #[tokio::test]
    async fn the_media_rule_is_unchanged() {
        for method in [
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::DELETE,
        ] {
            let t = ScriptedTransport::new();
            t.push_bytes(200, "image/jpeg", "jpg");
            client(&t)
                .request_url(
                    method.clone(),
                    Url::parse("https://lookaside.fbsbx.com/any/path?mid=1").unwrap(),
                )
                .send_raw()
                .await
                .unwrap_or_else(|e| panic!("{method}: {e}"));
            let req = t.last_request().unwrap();
            assert_eq!(req.method, method);
            assert_eq!(req.bearer(), Some("TOKEN"), "{method}");
            assert_eq!(t.remaining(), 0);
        }
    }

    /// `header()` cannot attach a credential, in any case of the name, to
    /// any URL, with the client's token, `no_auth()` or no token at all:
    /// `bearer()` and `oauth()`, which the rules check, are the only ways.
    /// The refusal is a validation error on the name, before any request,
    /// that never shows the value.
    #[tokio::test]
    async fn header_never_carries_a_credential() {
        const SENTINEL: &str = "EAAB-SENTINEL-header";
        let names = [
            "authorization",
            "Authorization",
            "AUTHORIZATION",
            "proxy-authorization",
            "Proxy-Authorization",
            "cookie",
            "Cookie",
        ];
        let urls = [
            "https://evil.example/steal",
            "http://evil.example/steal",
            ALLOWED,
            "https://graph.facebook.com/v25.0/me",
        ];
        let t = ScriptedTransport::new();
        let with_token = client(&t);
        let without_token = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        for name in names {
            for url in urls {
                let url = Url::parse(url).unwrap();
                for (how, req) in [
                    (
                        "no_auth",
                        with_token.request_url(Method::GET, url.clone()).no_auth(),
                    ),
                    ("token", with_token.request_url(Method::GET, url.clone())),
                    (
                        "no token",
                        without_token.request_url(Method::GET, url.clone()),
                    ),
                ] {
                    let err = req
                        .header(name, format!("Bearer {SENTINEL}"))
                        .send_raw()
                        .await
                        .unwrap_err();
                    assert!(
                        matches!(&err, Error::Validation(v) if v.field == name),
                        "{name} {url} ({how}): {err}"
                    );
                    let shown = format!("{err} {err:?} {err:#?}");
                    assert!(!shown.contains("SENTINEL"), "{shown}");
                }
            }
        }
        assert!(t.requests().is_empty(), "a credential header was sent");
    }

    /// A header name is parsed, not trusted: any case is accepted (and sent
    /// lowercase), and a name that is no header name is a validation error
    /// on send, never a panic.
    #[tokio::test]
    async fn header_names_are_parsed() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({}));
        let c = client(&t);
        c.get("x")
            .header("X-Custom-Header", "v1")
            .send_raw()
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.header("x-custom-header"), Some("v1"));
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
        for bad in ["bad name", "", "x:y", "\u{e9}", "a\nb"] {
            let err = c.get("x").header(bad, "v").send_raw().await.unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == bad),
                "{bad:?}: {err}"
            );
        }
        assert_eq!(t.requests().len(), 1);
    }

    /// A user name or password in the URL is a credential no rule checks:
    /// reqwest sends it as `Authorization: Basic`, to whatever host the URL
    /// names. It is refused on every URL, with or without a token,
    /// `no_auth()` included, and through a Graph endpoint configured with
    /// one: a validation error on `url`, before any request, that never
    /// shows it.
    #[tokio::test]
    async fn a_url_with_user_info_is_refused() {
        const SENTINEL: &str = "EAAB-SENTINEL-userinfo";
        let urls = [
            format!("https://user:{SENTINEL}@evil.example/steal"),
            format!("http://user:{SENTINEL}@evil.example/steal"),
            format!("https://{SENTINEL}@evil.example/steal"),
            format!("https://:{SENTINEL}@evil.example/steal"),
            format!("https://user:{SENTINEL}@graph.facebook.com/v25.0/me"),
            format!("https://user:{SENTINEL}@lookaside.fbsbx.com/any/path?mid=1"),
        ];
        let t = ScriptedTransport::new();
        let with_token = client(&t);
        let without_token = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let assert_refused = |err: Error, what: &str| {
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "url"),
                "{what}: {err}"
            );
            let shown = format!("{err} {err:?} {err:#?}");
            assert!(!shown.contains("SENTINEL"), "{what}: {shown}");
        };
        for url in &urls {
            let url = Url::parse(url).unwrap();
            for (how, req) in [
                ("token", with_token.request_url(Method::GET, url.clone())),
                (
                    "no_auth",
                    with_token.request_url(Method::GET, url.clone()).no_auth(),
                ),
                (
                    "no token",
                    without_token.request_url(Method::GET, url.clone()),
                ),
            ] {
                assert_refused(req.send_raw().await.unwrap_err(), &format!("{url} ({how})"));
            }
        }
        for base in [
            format!("https://user:{SENTINEL}@proxy.example/"),
            format!("https://{SENTINEL}@proxy.example/"),
        ] {
            let proxied = proxied(&t, &base);
            assert_refused(proxied.get("me").send_raw().await.unwrap_err(), &base);
            assert_refused(
                proxied
                    .get("oauth/access_token")
                    .no_auth()
                    .send_raw()
                    .await
                    .unwrap_err(),
                &base,
            );
        }
        assert!(t.requests().is_empty(), "a URL with user info was sent");

        // The same URLs without it go out.
        t.push_json(200, json!({}));
        with_token
            .request_url(
                Method::GET,
                Url::parse("https://graph.facebook.com/v25.0/me").unwrap(),
            )
            .send_raw()
            .await
            .unwrap();
        assert_eq!(t.remaining(), 0);
    }

    /// A `tracing` subscriber that renders every event and span field.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<String>>);

    struct Render<'a>(&'a mut String);

    impl tracing::field::Visit for Render<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }
    }

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            attrs.record(&mut Render(&mut self.0.lock().unwrap()));
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
            values.record(&mut Render(&mut self.0.lock().unwrap()));
        }
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut out = self.0.lock().unwrap();
            out.push_str(event.metadata().target());
            out.push(' ');
            event.record(&mut Render(&mut out));
            out.push('\n');
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// A sentinel token, as the client's own and as an explicit one (and in
    /// a refused URL's query), never reaches a refusal's error (`Display`,
    /// `Debug`, alternate `Debug`), the request's `Debug`, or the logs; nor
    /// the logs of an accepted request retried once.
    #[tokio::test]
    async fn a_refused_request_never_shows_the_token() {
        use std::fmt::Write as _;

        const SENTINEL: &str = "EAAB-SENTINEL-l10a-7b2450e";
        // As in `embedded_signup::onboard`: a second dispatcher makes every
        // callsite consult this test's capture too.
        let _second = tracing::Dispatch::new(Capture::default());
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());

        let t = ScriptedTransport::new();
        let c = Client::builder()
            .transport(t.clone())
            .access_token(SENTINEL)
            .retry(RetryPolicy {
                max_retries: 1,
                base_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
            })
            .build()
            .unwrap();
        let refused = [
            (
                Method::GET,
                "https://api.facebook.com/1234567890/other-edge",
            ),
            (
                Method::GET,
                "https://api.facebook.com/v25.0/1234567890/parent-bsuid-accounts",
            ),
            (
                Method::GET,
                "https://api.facebook.com/abc/parent-bsuid-accounts",
            ),
            (
                Method::GET,
                "http://api.facebook.com/1234567890/parent-bsuid-accounts",
            ),
            (
                Method::GET,
                "https://api.facebook.com:8443/1234567890/parent-bsuid-accounts",
            ),
            (Method::POST, ALLOWED),
            (
                Method::GET,
                "https://evil.example/1234567890/parent-bsuid-accounts",
            ),
        ];
        let mut seen = String::new();
        for (method, url) in refused {
            for auth in ["client", "bearer", "oauth"] {
                let req = c
                    .request_url(method.clone(), Url::parse(url).unwrap())
                    .query("access_token", SENTINEL);
                let req = match auth {
                    "bearer" => req.bearer(&SENTINEL.into()),
                    "oauth" => req.oauth(&SENTINEL.into()),
                    _ => req,
                };
                writeln!(seen, "{req:?} {req:#?}").unwrap();
                let err = req.send_raw().await.unwrap_err();
                assert!(
                    matches!(&err, Error::Validation(v) if v.field == "url"),
                    "{method} {url} ({auth}): {err}"
                );
                writeln!(seen, "{err} {err:?} {err:#?}").unwrap();
            }
        }
        assert!(t.requests().is_empty());
        writeln!(seen, "{c:?}").unwrap();

        // Accepted and retried once: the retry is logged, the token is not.
        t.push_json(
            500,
            json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
        );
        t.push_json(200, json!({}));
        c.request_url(Method::GET, Url::parse(ALLOWED).unwrap())
            .send_raw()
            .await
            .unwrap();
        assert_eq!(t.requests().len(), 2);
        assert_eq!(t.remaining(), 0);

        let logs = capture.0.lock().unwrap().clone();
        assert!(logs.contains("retrying graph request"), "{logs}");
        for (what, text) in [("errors and Debug", &seen), ("logs", &logs)] {
            assert!(!text.contains(SENTINEL), "the token in {what}: {text}");
            assert!(
                !text.contains("SENTINEL"),
                "part of the token in {what}: {text}"
            );
        }
    }
}
