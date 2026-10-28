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
use meta_whatsapp_core::error::{GraphErrorEnvelope, TransportError, ValidationError};
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
    ///
    /// A response that does not decode gives an [`Error::Decode`] with the
    /// first bytes of the body, unless the body carries Meta's `paging`
    /// object or names `access_token`: paging links can embed the access
    /// token, so then the error keeps only serde's error category and
    /// position.
    pub async fn send<T: DeserializeOwned>(self) -> Result<T> {
        let context = self.context;
        let resp = self.send_raw().await?;
        decode_json(context, &resp.body)
    }

    /// [`Self::send`] for a response that names people (the send
    /// responses): a decode failure carries neither the body nor serde's
    /// message, see [`decode_json_private`].
    pub(crate) async fn send_private<T: DeserializeOwned>(self) -> Result<T> {
        self.send_withheld(RECIPIENT_WITHHELD).await
    }

    /// [`Self::send`] for a response that carries something else that must
    /// stay out of errors (key material): as [`Self::send_private`], with
    /// `placeholder` as the body snippet, see [`decode_json_withheld`].
    pub(crate) async fn send_withheld<T: DeserializeOwned>(
        self,
        placeholder: &'static [u8],
    ) -> Result<T> {
        let context = self.context;
        let resp = self.send_raw().await?;
        decode_json_withheld(context, &resp.body, placeholder)
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
                body_snippet: safe_snippet(&resp.body),
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
    /// (anything else yields one validation error). A page that does not
    /// decode is reported without its body or serde's message: Meta's
    /// paging links can carry the access token.
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
                    // serde's message quotes the item's values: withheld as
                    // the page is.
                    let item = serde_json::from_value::<T>(raw)
                        .map_err(|e| withheld_serde_error(st.base.context, &e, PAGING_WITHHELD));
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
                match req
                    .send_withheld::<Page<serde_json::Value>>(PAGING_WITHHELD)
                    .await
                {
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
            body_snippet: safe_snippet(&resp.body),
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
            body_snippet: safe_snippet(&resp.body),
        },
    }
}

/// Decode a JSON body, attaching context and a snippet on failure.
///
/// Every decode of a 2xx answer goes through here or through
/// [`decode_json_withheld`] (`GraphRequest::send`, `send_success`,
/// `send_checked` and the modules that read `send_raw` themselves). When
/// the body [may carry the token](may_carry_token), the snippet and
/// serde's message (which quotes values) are withheld, as
/// [`decode_json_withheld`] with [`PAGING_WITHHELD`] does: Meta's
/// `paging.next` and `paging.previous` links can embed the
/// `access_token` the request was sent with. The rule reads the body, not
/// the target type, so it covers every paged answer — a `Page<T>`, a
/// struct of a module's own with a `paging` field, a page nested in an
/// object — without the call site opting in. Any other answer keeps its
/// snippet.
pub(crate) fn decode_json<T: DeserializeOwned>(context: &'static str, body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| {
        if may_carry_token(body) {
            withheld_serde_error(context, &e, PAGING_WITHHELD)
        } else {
            Error::decode(context, e, body)
        }
    })
}

/// Whether a body may carry the access token: it names a `paging` key
/// (whose `next`/`previous` links can embed it) or `access_token`
/// anywhere (a link outside `paging`, or a token). A byte scan of the
/// whole body rather than a parse, since the body is one that just failed
/// to parse; and the whole of it, since the links sit at the end of a
/// page, past the snippet's 512 bytes, where serde's message can still
/// quote them.
fn may_carry_token(body: &[u8]) -> bool {
    const MARKERS: [&[u8]; 2] = [b"\"paging\"", b"access_token"];
    MARKERS
        .iter()
        .any(|marker| body.windows(marker.len()).any(|w| w == *marker))
}

/// The `body_snippet` of an [`Error::Http`] built from a response body:
/// [`snippet`](meta_whatsapp_core::error::snippet)'s first bytes, or the
/// [`PAGING_WITHHELD`] text when the body
/// [may carry the token](may_carry_token). Every `Error::Http` of the
/// crate takes its snippet from here, so that `Error::Http::body_snippet`
/// never holds a paging link's token.
pub(crate) fn safe_snippet(body: &[u8]) -> String {
    if may_carry_token(body) {
        String::from_utf8_lossy(PAGING_WITHHELD).into_owned()
    } else {
        meta_whatsapp_core::error::snippet(body)
    }
}

/// A decode error for a response that names people: `why` replaces serde's
/// message, and the body snippet is a placeholder.
pub(crate) fn withheld_decode_error(context: &'static str, why: impl fmt::Display) -> Error {
    withheld_decode_error_as(context, why, RECIPIENT_WITHHELD)
}

/// The body snippet of a decode error for a response that names people.
const RECIPIENT_WITHHELD: &[u8] = b"(withheld: the response names the recipient)";

/// The body snippet of a decode error for a response that can carry
/// `paging` links: Meta's `next` and `previous` URLs can embed the access
/// token the request was sent with. [`decode_json`] uses it on its own for
/// any body that carries them.
pub(crate) const PAGING_WITHHELD: &[u8] = b"(withheld: paging links can carry the access token)";

fn withheld_decode_error_as(
    context: &'static str,
    why: impl fmt::Display,
    placeholder: &'static [u8],
) -> Error {
    Error::decode(
        context,
        <serde_json::Error as serde::de::Error>::custom(why),
        placeholder,
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
    decode_json_withheld(context, body, RECIPIENT_WITHHELD)
}

/// [`decode_json_private`] with `placeholder` as the body snippet, for a
/// body that carries something else that must stay out of errors (the
/// number's settings carry Meta's `cloud_encryption_key`).
pub(crate) fn decode_json_withheld<T: DeserializeOwned>(
    context: &'static str,
    body: &[u8],
    placeholder: &'static [u8],
) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| withheld_serde_error(context, &e, placeholder))
}

/// A decode error that keeps serde's error category and position, and
/// neither its message nor the body.
fn withheld_serde_error(
    context: &'static str,
    e: &serde_json::Error,
    placeholder: &'static [u8],
) -> Error {
    withheld_decode_error_as(
        context,
        format_args!(
            "{:?} error at line {} column {} (details withheld)",
            e.classify(),
            e.line(),
            e.column()
        ),
        placeholder,
    )
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

/// Roadmap L20c: ids checked against their documented shape
/// (`meta_whatsapp_core::ids`). A numeric id is refused when it is built;
/// an opaque one stays one percent-encoded path segment.
#[cfg(test)]
mod id_shape_tests {
    use meta_whatsapp_core::ids::{
        AppId, BusinessId, GroupId, MediaId, PhoneNumberId, TemplateId, WabaId,
    };
    use meta_whatsapp_core::recipient::Recipient;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;
    use crate::messages::OutboundMessage;
    use crate::retry::RetryPolicy;

    /// An id that would turn a call on its object into a call on the
    /// WABA's subscriptions, if it became two path segments.
    const HOSTILE: &str = "123/subscribed_apps";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    /// Decisive: a numeric id holding `/` is refused before any request.
    /// Each numeric id type is built from [`HOSTILE`] with its checked
    /// constructor, and only if that let it through, used in a call whose
    /// path it names. Every response is scripted, so an id the check let
    /// through would be sent, and recorded: none is.
    #[tokio::test]
    async fn a_numeric_id_holding_a_slash_is_refused_before_any_request() {
        let t = ScriptedTransport::new();
        for _ in 0..6 {
            t.push_json(200, json!({"success": true, "id": "1", "name": "n"}));
        }
        let c = client(&t);
        let pn = || PhoneNumberId::new("106540352242922");
        let text = OutboundMessage::text(Recipient::phone("+16505551234"), "hi");
        let outcomes: [(&str, Result<()>); 6] = [
            (
                "phone_number_id",
                async {
                    c.messages(PhoneNumberId::new(HOSTILE)?).send(&text).await?;
                    Ok(())
                }
                .await,
            ),
            (
                "waba_id",
                async { c.waba(WabaId::new(HOSTILE)?).subscribe_app(None).await }.await,
            ),
            (
                "business_id",
                async {
                    c.business(BusinessId::new(HOSTILE)?).get(&["id"]).await?;
                    Ok(())
                }
                .await,
            ),
            (
                "media_id",
                async { c.media(pn()?).delete(&MediaId::new(HOSTILE)?).await }.await,
            ),
            (
                "template_id",
                async {
                    c.templates(WabaId::new("102290129340398")?)
                        .get_fields(&TemplateId::new(HOSTILE)?, &["name"])
                        .await?;
                    Ok(())
                }
                .await,
            ),
            (
                "app_id",
                async {
                    c.media(pn()?)
                        .start_upload_session(&AppId::new(HOSTILE)?, "a.png", 1, "image/png")
                        .await?;
                    Ok(())
                }
                .await,
            ),
        ];
        let sent: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
        assert!(
            sent.is_empty(),
            "a numeric id holding `/` was sent: {sent:?}"
        );
        for (field, outcome) in outcomes {
            let err = outcome.unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == field),
                "{field}: {err}"
            );
        }
    }

    /// Decisive: a group id holding `/` (an opaque id: a standard-base64
    /// one may) is built as it is, and stays one percent-encoded segment:
    /// this `DELETE` cannot land on the group's participants, nor on any
    /// other object.
    #[tokio::test]
    async fn a_group_id_holding_a_slash_stays_one_encoded_segment() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"success": true}));
        let c = client(&t);
        let group = GroupId::new("Y2FwaV9ncm91cDox/participants");
        c.group(group.clone()).delete().await.unwrap();
        c.groups(PhoneNumberId::new("106540352242922").unwrap())
            .group(group)
            .delete()
            .await
            .unwrap();
        for req in t.requests() {
            assert_eq!(req.method, Method::DELETE);
            assert_eq!(req.path(), "/v25.0/Y2FwaV9ncm91cDox%2Fparticipants");
            assert_eq!(req.url.query(), None);
        }
        assert_eq!(t.remaining(), 0);
    }

    /// A numeric id from Meta keeps its shape, whatever it is: parsing never
    /// fails on it, and used in a path it is still one encoded segment.
    #[tokio::test]
    async fn a_numeric_id_meta_sent_is_kept_and_stays_one_segment() {
        let id: WabaId = serde_json::from_value(json!(HOSTILE)).unwrap();
        assert_eq!(id.as_str(), HOSTILE);
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t).waba(id).subscribe_app(None).await.unwrap();
        assert_eq!(
            t.last_request().unwrap().path(),
            "/v25.0/123%2Fsubscribed_apps/subscribed_apps"
        );
        assert_eq!(t.remaining(), 0);
    }
}

/// A paged answer that fails to decode keeps its body, and serde's
/// message, out of the error: Meta's `paging.next` and `paging.previous`
/// links can embed the access token, and `Error`'s `Debug` is what callers
/// log. The rule is [`decode_json`]'s, so it holds for every one-page read
/// without the call site opting in, and `body_guard_tests` keeps the
/// modules from building an error from a body themselves.
#[cfg(test)]
mod paging_withheld_tests {
    use futures::StreamExt;
    use meta_whatsapp_core::ids::{
        BusinessId, FlowId, GroupId, PhoneNumberId, QrCodeId, TemplateGroupId, TemplateId, UserId,
        WabaId,
    };
    use meta_whatsapp_core::paging::Page;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use time::OffsetDateTime;

    use super::*;
    use crate::analytics::{
        GroupAnalyticsQuery, GroupMetric, TemplateAnalyticsQuery, TemplateGroupAnalyticsQuery,
    };
    use crate::authentication::PreviewQuery;
    use crate::block_users::ListBlockedUsers;
    use crate::business_verification::ListVerificationSubmissions;
    use crate::flows::{ListFlowAssets, ListFlows};
    use crate::groups::{ListGroups, ListJoinRequests};
    use crate::marketing::{ListClientWabas, OnboardingStatus};
    use crate::qr_codes::{ListQrCodes, QrCodeFields};
    use crate::retry::RetryPolicy;
    use crate::signups::ListSignups;
    use crate::templates::{LibraryQuery, TemplateListQuery};
    use crate::waba::{ListAssignedUsers, PhoneNumbersQuery, WabaListQuery};

    /// A `next` link holding the token, as Meta can send it. Values ours.
    const NEXT: &str = "https://graph.facebook.com/v25.0/1/x?access_token=LEAKED&after=c1";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    /// A page whose `data` is the `next` link instead of a list: without the
    /// rule, serde's message would quote it and the snippet would hold it
    /// twice.
    fn leaky_page() -> String {
        format!(r#"{{"data":"{NEXT}","paging":{{"cursors":{{"after":"c1"}},"next":"{NEXT}"}}}}"#)
    }

    /// `body`'s error is a `Decode` that keeps serde's category, line and
    /// column, and shows neither the body nor the link. serde stops on the
    /// `data` string, so the column is that of its closing quote.
    fn assert_withheld(what: &str, err: &Error, body: &str) {
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("LEAKED"), "{what}: {shown}");
        let Error::Decode {
            context,
            source,
            body_snippet,
        } = err
        else {
            panic!("{what}: not a decode error: {err:?}")
        };
        assert!(!context.is_empty(), "{what}");
        assert_eq!(body_snippet.as_bytes(), PAGING_WITHHELD, "{what}");
        let column = body.find(r#"","paging""#).unwrap() + 1;
        assert_eq!(
            source.to_string(),
            format!("Data error at line 1 column {column} (details withheld)"),
            "{what}"
        );
    }

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).unwrap()
    }

    #[allow(clippy::too_many_lines)] // one list of call sites, read top to bottom
    #[tokio::test]
    async fn every_one_page_read_of_a_paged_answer_withholds_a_body_that_fails_to_decode() {
        let t = ScriptedTransport::new();
        let c = client(&t);
        let waba = || WabaId::new("1").unwrap();
        let phone = || PhoneNumberId::new("2").unwrap();
        let business = || BusinessId::new("3").unwrap();
        let body = leaky_page();
        let mut errors: Vec<(&str, Error)> = Vec::new();
        macro_rules! read {
            ($what:literal, $call:expr) => {{
                t.push_bytes(200, "application/json", body.clone());
                errors.push(($what, $call.await.unwrap_err()));
            }};
        }
        read!(
            "templates.list",
            c.templates(waba()).list(&TemplateListQuery::new())
        );
        read!(
            "templates.library",
            c.templates(waba()).library(&LibraryQuery::default())
        );
        read!(
            "waba.phone_numbers",
            c.waba(waba()).phone_numbers(&PhoneNumbersQuery::new())
        );
        read!("waba.subscribed_apps", c.waba(waba()).subscribed_apps());
        read!(
            "waba.assigned_users",
            c.waba(waba())
                .assigned_users(&ListAssignedUsers::new(business()))
        );
        read!(
            "business.client_whatsapp_business_accounts",
            c.business(business())
                .client_whatsapp_business_accounts(&WabaListQuery::new())
        );
        read!(
            "business.owned_whatsapp_business_accounts",
            c.business(business())
                .owned_whatsapp_business_accounts(&WabaListQuery::new())
        );
        read!(
            "analytics.template",
            c.analytics(waba()).template(&TemplateAnalyticsQuery::new(
                at(1),
                at(2),
                vec![TemplateId::new("4").unwrap()],
            ))
        );
        read!(
            "analytics.template_group",
            c.analytics(waba())
                .template_group(&TemplateGroupAnalyticsQuery::new(
                    at(1),
                    at(2),
                    vec![TemplateGroupId::new("5")],
                ))
        );
        read!(
            "analytics.groups",
            c.analytics(waba()).groups(&GroupAnalyticsQuery::new(
                at(1),
                at(2),
                vec![GroupId::new("6")],
                vec![GroupMetric::Sent],
            ))
        );
        // A struct of the module's own (`{"data": {"groups": …}, "paging"}`).
        read!(
            "groups.list",
            c.groups(phone()).list(&ListGroups::default())
        );
        read!(
            "groups.join_requests",
            c.group(GroupId::new("8"))
                .join_requests(&ListJoinRequests::default())
        );
        read!("flows.list", c.flows(waba()).list(&ListFlows::new()));
        read!(
            "flow.assets",
            c.flow(FlowId::new("7")).assets(&ListFlowAssets::new())
        );
        read!("signups.list", c.signups(waba()).list(&ListSignups::new()));
        read!(
            "block_users.list",
            c.block_users(phone()).list(&ListBlockedUsers::default())
        );
        read!(
            "qr_codes.list",
            c.qr_codes(phone()).list(&ListQrCodes::default())
        );
        read!(
            "marketing_business.client_wabas_with_status",
            c.marketing_business(business())
                .client_wabas_with_status(&ListClientWabas::new([OnboardingStatus::Eligible]))
        );
        read!("credit_lines.list", c.credit_lines().list(&business(), &[]));
        read!(
            "business_verification.submissions",
            c.business_verification()
                .submissions(&business(), &ListVerificationSubmissions::new())
        );
        // Documented as `{"data": [settings]}`, no paging; should Meta page
        // it, the rule holds there too.
        read!("commerce.settings", c.commerce(phone()).settings());
        // The same for the other `{"data": [...]}` answers.
        read!(
            "phone_number.username_suggestions",
            c.phone_number(phone()).username_suggestions()
        );
        read!(
            "phone_number.business_compliance_info",
            c.phone_number(phone()).business_compliance_info(&[])
        );
        read!(
            "templates.compare",
            c.templates(waba()).compare(
                &TemplateId::new("4").unwrap(),
                &TemplateId::new("9").unwrap(),
                1,
                2
            )
        );
        read!(
            "authentication.previews",
            c.authentication(waba()).previews(&PreviewQuery::default())
        );
        read!(
            "qr_codes.get",
            c.qr_codes(phone())
                .get(&QrCodeId::new("ANED2T5QRU7HG1"), &QrCodeFields::default())
        );
        read!("business_profile.get", c.business_profile(phone()).get(&[]));
        assert_eq!(t.remaining(), 0);
        assert_eq!(t.requests().len(), errors.len());
        for (what, err) in &errors {
            assert_withheld(what, err, &body);
        }
    }

    #[tokio::test]
    async fn a_page_item_that_does_not_decode_is_withheld_too() {
        // serde's message would quote the item, here the link itself.
        let t = ScriptedTransport::new();
        t.push_bytes(
            200,
            "application/json",
            format!(r#"{{"data":["{NEXT}"],"paging":{{"cursors":{{"after":"c1"}}}}}}"#),
        );
        let items: Vec<_> = client(&t)
            .qr_codes(PhoneNumberId::new("2").unwrap())
            .list_stream(&ListQrCodes::default())
            .collect()
            .await;
        let [Err(err)] = &items[..] else {
            panic!("{items:?}")
        };
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("LEAKED"), "{shown}");
        assert!(
            matches!(err, Error::Decode { body_snippet, .. } if body_snippet.as_bytes() == PAGING_WITHHELD),
            "{err:?}"
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn an_http_error_built_from_a_body_withholds_it_when_it_may_carry_the_token() {
        let withheld = String::from_utf8_lossy(PAGING_WITHHELD).into_owned();
        assert_eq!(safe_snippet(leaky_page().as_bytes()), withheld);
        assert_eq!(
            safe_snippet(br#"{"data":[],"paging":{}}"#),
            withheld,
            "a `paging` key alone"
        );
        assert_eq!(
            safe_snippet(br#"{"success":false}"#),
            r#"{"success":false}"#
        );

        let t = ScriptedTransport::new();
        let c = client(&t);
        let refused = format!(r#"{{"success":false,"next":"{NEXT}"}}"#);
        let html = format!("<html>Bad gateway for {NEXT}</html>");
        // `send_success`, `send_checked`, a non-2xx answer, and a module's own.
        t.push_bytes(200, "application/json", refused.clone());
        t.push_bytes(200, "application/json", refused.clone());
        t.push_bytes(502, "text/html", html);
        t.push_bytes(
            200,
            "application/json",
            format!(r#"{{"success":false,"deleted":false,"next":"{NEXT}"}}"#),
        );
        t.push_bytes(200, "application/json", r#"{"success":false}"#);
        let errors = [
            c.post_at(&["1", "x"]).send_success().await.unwrap_err(),
            send_checked::<serde_json::Value>(c.post_at(&["1", "y"]), "checked")
                .await
                .unwrap_err(),
            c.get_at(&["1"])
                .send::<serde_json::Value>()
                .await
                .unwrap_err(),
            c.phone_number(PhoneNumberId::new("2").unwrap())
                .delete_contact_book_entry(&UserId::new("US.13491208655302741918"))
                .await
                .unwrap_err(),
        ];
        for err in &errors {
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains("LEAKED"), "{shown}");
            assert!(
                matches!(err, Error::Http { body_snippet, .. } if *body_snippet == withheld),
                "{err:?}"
            );
        }
        // Without a marker the snippet stays, for debugging.
        let err = c.post_at(&["1", "z"]).send_success().await.unwrap_err();
        assert!(
            matches!(&err, Error::Http { body_snippet, .. } if body_snippet == r#"{"success":false}"#),
            "{err:?}"
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_page_nested_in_an_object_is_withheld_too() {
        // `GET /{TEMPLATE_GROUP_ID}` answers the group with the first page
        // of its templates.
        let body = format!(
            r#"{{"id":"5","whatsapp_business_templates":{{"data":"{NEXT}","paging":{{"next":"{NEXT}"}}}}}}"#
        );
        let t = ScriptedTransport::new();
        t.push_bytes(200, "application/json", body.clone());
        let err = client(&t)
            .templates(WabaId::new("1").unwrap())
            .get_group(&TemplateGroupId::new("5"))
            .await
            .unwrap_err();
        assert_withheld("templates.get_group", &err, &body);
        assert_eq!(t.remaining(), 0);
    }

    #[test]
    fn a_link_past_the_snippet_is_withheld_from_serdes_message() {
        // The snippet keeps 512 bytes; serde's message quotes the offending
        // value wherever it sits. Here `paging` is the link itself.
        let data = vec![1; 300];
        let body = serde_json::to_vec(&serde_json::json!({"data": data, "paging": NEXT})).unwrap();
        assert!(body.len() > 512);
        let err = decode_json::<Page<serde_json::Value>>("page", &body).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("LEAKED"), "{shown}");
        assert!(shown.contains("Data error at line 1 column"), "{shown}");
    }

    #[test]
    fn a_paged_answer_is_withheld_whatever_its_links_hold() {
        // No `access_token` in sight: the `paging` key alone decides, since
        // what Meta puts in a link is not ours to predict.
        let body = br#"{"data": {"id": "1"}, "paging": {"next": "https://graph.facebook.com/v25.0/1/x?after=c1"}}"#;
        let err = decode_json::<Page<serde_json::Value>>("page", body).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("after=c1"), "{shown}");
        assert!(
            matches!(&err, Error::Decode { body_snippet, .. } if body_snippet.as_bytes() == PAGING_WITHHELD),
            "{err:?}"
        );
    }

    #[test]
    fn a_body_naming_access_token_is_withheld_without_paging() {
        let body = format!(r#"{{"data":"{NEXT}","next":"{NEXT}"}}"#);
        let err = decode_json::<Page<serde_json::Value>>("page", body.as_bytes()).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("LEAKED"), "{shown}");
        assert!(
            matches!(&err, Error::Decode { body_snippet, .. } if body_snippet.as_bytes() == PAGING_WITHHELD),
            "{err:?}"
        );
    }

    #[test]
    fn a_single_object_answer_keeps_its_snippet_and_message() {
        let body = br#"{"id": 5, "name": "x"}"#;
        let err =
            decode_json::<std::collections::HashMap<String, String>>("node", body).unwrap_err();
        let Error::Decode {
            context,
            source,
            body_snippet,
        } = &err
        else {
            panic!("{err:?}")
        };
        assert_eq!(*context, "node");
        assert_eq!(body_snippet, r#"{"id": 5, "name": "x"}"#);
        assert_eq!(
            source.to_string(),
            "invalid type: integer `5`, expected a string at line 1 column 8"
        );
    }
}

/// The rules in [`decode_json`] and [`safe_snippet`] hold only while no
/// module builds an error from a response body some other way. This scan of
/// the crate's sources (outside `#[cfg(test)]` code, comments and string
/// literals) enforces it, one rule per way a body or serde's message can
/// reach an error:
///
/// - **decode**: a three-argument `…::decode(` (`Error::decode`, or an
///   alias of `Error`) passes a byte-string literal or a `SCREAMING_CASE`
///   placeholder as its body;
/// - **field**: a `body_snippet:` field is built with `safe_snippet(…)`, a
///   literal or a placeholder (`String::new()`, `X.to_owned()` …), never in
///   shorthand from a local;
/// - **snippet**: `meta_whatsapp_core::error::snippet` is named nowhere but
///   in `request.rs`'s `safe_snippet`;
/// - **parse**: `serde_json::from_slice`/`from_str`/`from_reader` never
///   read a `body` outside `decode_json*` (serde's message quotes values).
///
/// Each exception is listed in [`ALLOWED`] by file *and* enclosing
/// function, with why it may; an exception no longer used fails too.
#[cfg(test)]
mod body_guard_tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// `(file, enclosing fn, rule, why)`.
    const ALLOWED: [(&str, &str, &str, &str); 9] = [
        ("request.rs", "decode_json", "decode", "the rule itself"),
        ("request.rs", "decode_json", "parse", "the rule itself"),
        ("request.rs", "decode_json_withheld", "parse", "withholds"),
        (
            "request.rs",
            "withheld_decode_error_as",
            "decode",
            "`placeholder` is a `&'static` constant",
        ),
        ("request.rs", "safe_snippet", "snippet", "the rule itself"),
        (
            "request.rs",
            "decode_error",
            "parse",
            "a non-2xx body; serde's error is discarded",
        ),
        (
            "groups/mod.rs",
            "send_lenient",
            "parse",
            "serde's error is discarded (`unwrap_or(Null)`)",
        ),
        (
            "embedded_signup/mod.rs",
            "decode_business_token",
            "parse",
            "withholds the body and serde's message itself",
        ),
        (
            "embedded_signup/event.rs",
            "from_json",
            "decode",
            "the browser's message event, not a Graph answer",
        ),
    ];

    /// A file's code with comments blanked, and which characters sit in a
    /// string or char literal.
    struct Code {
        text: Vec<char>,
        literal: Vec<bool>,
    }

    fn is_ident(ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_'
    }

    fn lex(source: &str) -> Code {
        let s: Vec<char> = source.chars().collect();
        let mut text = Vec::with_capacity(s.len());
        let mut literal = Vec::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            if let Some(end) = comment_end(&s, i) {
                text.extend(
                    s[i..end]
                        .iter()
                        .map(|&c| if c == '\n' { '\n' } else { ' ' }),
                );
                literal.extend(std::iter::repeat_n(false, end - i));
                i = end;
            } else if let Some(end) = literal_end(&s, i) {
                text.extend_from_slice(&s[i..end]);
                literal.extend(std::iter::repeat_n(true, end - i));
                i = end;
            } else {
                text.push(s[i]);
                literal.push(false);
                i += 1;
            }
        }
        Code { text, literal }
    }

    /// The end of the comment starting at `i`, if one does (block comments
    /// nest).
    fn comment_end(s: &[char], i: usize) -> Option<usize> {
        let at = |k: usize| s.get(k).copied();
        match (at(i)?, at(i + 1)?) {
            ('/', '/') => Some((i..s.len()).find(|&k| s[k] == '\n').unwrap_or(s.len())),
            ('/', '*') => {
                let mut depth = 0u32;
                let mut k = i;
                while k < s.len() {
                    match (s[k], at(k + 1)) {
                        ('/', Some('*')) => {
                            depth += 1;
                            k += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            k += 2;
                            if depth == 0 {
                                return Some(k);
                            }
                        }
                        _ => k += 1,
                    }
                }
                Some(s.len())
            }
            _ => None,
        }
    }

    /// The end of the string, raw string (`r#"…"#`, after `b` or `c` too)
    /// or char literal starting at `i`, if one does; a lifetime is none.
    fn literal_end(s: &[char], i: usize) -> Option<usize> {
        let at = |k: usize| s.get(k).copied().unwrap_or('\0');
        let ident_before = |k: usize| k > 0 && is_ident(s[k - 1]);
        match s[i] {
            'r' if !ident_before(i) || (matches!(s[i - 1], 'b' | 'c') && !ident_before(i - 1)) => {
                let hashes = (i + 1..s.len()).take_while(|&k| s[k] == '#').count();
                let open = i + 1 + hashes;
                if at(open) != '"' {
                    return None; // `r#ident`, or a plain `r`
                }
                let close = (open + 1..s.len())
                    .find(|&k| s[k] == '"' && (1..=hashes).all(|h| at(k + h) == '#'))
                    .unwrap_or(s.len());
                Some((close + 1 + hashes).min(s.len()))
            }
            '"' => {
                let mut k = i + 1;
                while k < s.len() {
                    match s[k] {
                        '\\' => k += 2,
                        '"' => return Some(k + 1),
                        _ => k += 1,
                    }
                }
                Some(s.len())
            }
            '\'' => {
                let end = if at(i + 1) == '\\' {
                    (i + 3..s.len().min(i + 12)).find(|&k| s[k] == '\'')
                } else if at(i + 2) == '\'' {
                    Some(i + 2)
                } else {
                    None
                };
                end.map(|e| e + 1)
            }
            _ => None,
        }
    }

    impl Code {
        fn code(&self, i: usize) -> Option<char> {
            (!self.literal.get(i)?).then(|| self.text[i])
        }

        /// Where `word` stands as a whole identifier in code.
        fn words(&self, word: &str) -> Vec<usize> {
            let w: Vec<char> = word.chars().collect();
            (0..self.text.len().saturating_sub(w.len() - 1))
                .filter(|&i| {
                    self.text[i..i + w.len()] == w[..]
                        && (i..i + w.len()).all(|k| !self.literal[k])
                        && (i == 0 || !is_ident(self.text[i - 1]))
                        && self.text.get(i + w.len()).is_none_or(|&c| !is_ident(c))
                })
                .collect()
        }

        /// The next non-blank code character at or after `i`.
        fn next_code(&self, mut i: usize) -> Option<(usize, char)> {
            while i < self.text.len() {
                if self.literal[i] {
                    return Some((i, '"'));
                }
                if !self.text[i].is_whitespace() {
                    return Some((i, self.text[i]));
                }
                i += 1;
            }
            None
        }

        /// The index of the bracket closing the one at `open`.
        fn matching(&self, open: usize) -> Option<usize> {
            let mut depth = 0u32;
            for i in open..self.text.len() {
                match self.code(i) {
                    Some('(' | '[' | '{') => depth += 1,
                    Some(')' | ']' | '}') => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    _ => {}
                }
            }
            None
        }

        /// The comma-separated items from `start` to the first `,`, `)`,
        /// `]` or `}` at depth 0 (`until_comma`) or to the bracket that
        /// closes the list, trimmed.
        fn items(&self, start: usize, until_comma: bool) -> Vec<String> {
            let mut items = Vec::new();
            let mut current = String::new();
            let mut depth = 0u32;
            for i in start..self.text.len() {
                match self.code(i) {
                    Some('(' | '[' | '{') => depth += 1,
                    Some(')' | ']' | '}') if depth == 0 => break,
                    Some(')' | ']' | '}') => depth -= 1,
                    Some(',') if depth == 0 => {
                        if until_comma {
                            break;
                        }
                        items.push(std::mem::take(&mut current));
                        continue;
                    }
                    _ => {}
                }
                current.push(self.text[i]);
            }
            items.push(current);
            items
                .into_iter()
                .map(|a| a.trim().to_owned())
                .filter(|a| !a.is_empty())
                .collect()
        }

        /// `(name, start, end)` of every `fn` with a body.
        fn fns(&self) -> Vec<(String, usize, usize)> {
            let mut out = Vec::new();
            for at in self.words("fn") {
                let Some((name_at, first)) = self.next_code(at + 2) else {
                    continue;
                };
                if !is_ident(first) {
                    continue; // a `fn(…)` pointer type
                }
                let name: String = self.text[name_at..]
                    .iter()
                    .take_while(|&&c| is_ident(c))
                    .collect();
                let body =
                    (name_at..self.text.len()).find(|&i| matches!(self.code(i), Some('{' | ';')));
                if let Some(open) = body.filter(|&i| self.text[i] == '{')
                    && let Some(close) = self.matching(open)
                {
                    out.push((name, open, close));
                }
            }
            out
        }

        /// The spans of `#[cfg(test)]` items, and the names of the test
        /// modules declared out of line (`#[cfg(test)] mod tests;`).
        fn test_items(&self) -> (Vec<(usize, usize)>, Vec<String>) {
            let mut spans = Vec::new();
            let mut modules = Vec::new();
            let text: String = self.text.iter().collect();
            let starts: Vec<usize> = text
                .match_indices("#[cfg(test)]")
                .map(|(byte, _)| text[..byte].chars().count())
                .filter(|&i| !self.literal[i])
                .collect();
            for start in starts {
                let end = (start + 12..self.text.len())
                    .find(|&i| matches!(self.code(i), Some('{' | ';')))
                    .and_then(|i| {
                        if self.text[i] == ';' {
                            Some(i)
                        } else {
                            self.matching(i)
                        }
                    })
                    .unwrap_or(self.text.len() - 1);
                let item: String = self.text[start + 12..=end].iter().collect();
                let words: Vec<&str> = item.split_whitespace().collect();
                if let ["mod", name] = words[..] {
                    modules.push(name.trim_end_matches(';').to_owned());
                }
                spans.push((start, end));
            }
            (spans, modules)
        }
    }

    fn is_literal(arg: &str) -> bool {
        let inner = arg
            .strip_prefix("b\"")
            .or_else(|| arg.strip_prefix('"'))
            .and_then(|rest| rest.strip_suffix('"'));
        inner.is_some_and(|inner| {
            let mut escaped = false;
            inner.chars().all(|ch| {
                let ok = escaped || ch != '"';
                escaped = !escaped && ch == '\\';
                ok
            })
        })
    }

    fn is_placeholder(arg: &str) -> bool {
        !arg.is_empty()
            && arg.starts_with(|c: char| c.is_ascii_uppercase())
            && arg
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
    }

    /// A value that holds no body: a literal or a placeholder, possibly
    /// turned into a `String`.
    fn is_constant(value: &str) -> bool {
        let base = [".to_owned()", ".to_string()", ".into()"]
            .iter()
            .find_map(|suffix| value.strip_suffix(suffix))
            .unwrap_or(value);
        value == "String::new()" || is_literal(base) || is_placeholder(base)
    }

    /// `safe_snippet(…)`, under any path, and nothing after its `)`.
    fn is_safe_snippet_call(value: &str) -> bool {
        let Some(open) = value.find('(') else {
            return false;
        };
        let name = &value[..open];
        let code = lex(value);
        let open = value[..open].chars().count();
        (name == "safe_snippet" || name.ends_with("::safe_snippet"))
            && code.matching(open) == Some(code.text.len() - 1)
    }

    /// One file's scan.
    struct Scan {
        /// `(rule, enclosing fn, detail)`.
        found: Vec<(&'static str, String, String)>,
        /// How many sites each rule looked at.
        seen: [usize; 4],
        /// The test modules it declares out of line.
        test_modules: Vec<String>,
    }

    fn scan(source: &str) -> Scan {
        let code = lex(source);
        let (tests, test_modules) = code.test_items();
        let in_tests = |i: usize| tests.iter().any(|&(a, b)| a <= i && i <= b);
        let fns = code.fns();
        let enclosing = |i: usize| {
            fns.iter()
                .filter(|(_, a, b)| *a <= i && i <= *b)
                .min_by_key(|(_, a, b)| b - a)
                .map_or_else(String::new, |(name, _, _)| name.clone())
        };
        let text = |a: usize, b: usize| code.text[a..b].iter().collect::<String>();
        let mut found = Vec::new();
        let mut seen = [0; 4];

        // decode
        for at in code.words("decode") {
            let path = at >= 2 && text(at - 2, at) == "::";
            let Some((open, '(')) = code.next_code(at + 6) else {
                continue;
            };
            if !path || in_tests(at) {
                continue;
            }
            let args = code.items(open + 1, false);
            if args.len() != 3 {
                continue;
            }
            seen[0] += 1;
            let body = &args[2];
            if !(is_literal(body) || is_placeholder(body)) {
                found.push(("decode", enclosing(at), format!("body argument `{body}`")));
            }
        }
        // field
        for at in code.words("body_snippet") {
            if in_tests(at) || (at > 0 && code.text[at - 1] == '.') {
                continue;
            }
            let after = at + "body_snippet".len();
            match code.next_code(after) {
                Some((colon, ':')) if code.text.get(colon + 1) != Some(&':') => {
                    seen[1] += 1;
                    let value = code.items(colon + 1, true).concat();
                    if !(is_safe_snippet_call(&value) || is_constant(&value)) {
                        found.push(("field", enclosing(at), format!("`body_snippet: {value}`")));
                    }
                }
                Some((_, ',' | '}')) => {
                    seen[1] += 1;
                    found.push((
                        "field",
                        enclosing(at),
                        "`body_snippet` shorthand".to_owned(),
                    ));
                }
                _ => {}
            }
        }
        // snippet
        for at in code.words("snippet") {
            if in_tests(at) {
                continue;
            }
            seen[2] += 1;
            found.push(("snippet", enclosing(at), "names `snippet`".to_owned()));
        }
        // parse
        for word in ["from_slice", "from_str", "from_reader"] {
            for at in code.words(word) {
                if in_tests(at) {
                    continue;
                }
                let mut open = at + word.len();
                if text(open, (open + 2).min(code.text.len())) == "::" {
                    // A turbofish: skip its `<…>`.
                    let mut depth = 0u32;
                    for i in open + 2..code.text.len() {
                        match code.code(i) {
                            Some('<') => depth += 1,
                            Some('>') => {
                                depth -= 1;
                                if depth == 0 {
                                    open = i + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                let Some((open, '(')) = code.next_code(open) else {
                    continue;
                };
                seen[3] += 1;
                let args = code.items(open + 1, false).join(", ");
                if !args.contains("body") {
                    continue;
                }
                found.push(("parse", enclosing(at), format!("`{word}({args})`")));
            }
        }
        Scan {
            found,
            seen,
            test_modules,
        }
    }

    /// The [`ALLOWED`] entry for a finding of `rule` in `file`'s fn
    /// `function`.
    fn allowed(file: &str, function: &str, rule: &str) -> Option<usize> {
        ALLOWED
            .iter()
            .position(|&(f, func, r, _)| f == file && func == function && r == rule)
    }

    /// The crate's non-test sources, as `(name relative to src/, source)`.
    fn sources() -> Vec<(String, String)> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        let mut test_files = BTreeSet::new();
        let mut out = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file).unwrap();
            let modules = scan(&source).test_modules;
            // `a/mod.rs` and `lib.rs` hold `a/<m>.rs`; `a/b.rs` holds `a/b/<m>.rs`.
            let dir = match file.file_stem().and_then(|s| s.to_str()) {
                Some("mod" | "lib") => file.parent().unwrap().to_path_buf(),
                _ => file.with_extension(""),
            };
            for m in modules {
                test_files.insert(dir.join(format!("{m}.rs")));
                test_files.insert(dir.join(&m).join("mod.rs"));
            }
            out.push((file.clone(), source));
        }
        out.into_iter()
            .filter(|(file, _)| !test_files.contains(file))
            .map(|(file, source)| {
                let name = file
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                (name, source)
            })
            .collect()
    }

    #[test]
    fn no_module_builds_an_error_from_a_response_body_itself() {
        let files = sources();
        assert!(files.len() > 20, "{}", files.len());
        assert!(
            !files.iter().any(|(name, _)| name.ends_with("tests.rs")),
            "test modules are not scanned"
        );
        let mut seen = [0; 4];
        let mut used = BTreeSet::new();
        let mut offending = Vec::new();
        for (name, source) in &files {
            let Scan {
                found,
                seen: counts,
                ..
            } = scan(source);
            for (rule, count) in seen.iter_mut().zip(counts) {
                *rule += count;
            }
            for (rule, function, detail) in found {
                match allowed(name, &function, rule) {
                    Some(i) => {
                        used.insert(i);
                    }
                    None => offending.push(format!("{name} in fn {function}: {rule}: {detail}")),
                }
            }
        }
        // Each rule saw the sites it is about, so a broken scan cannot pass
        // by finding nothing.
        let [decode, field, snippet, parse] = seen;
        assert!(decode >= 9, "decode calls: {decode}");
        assert!(field >= 9, "body_snippet fields: {field}");
        assert_eq!(snippet, 1, "names of `snippet`");
        assert!(parse >= 15, "serde parses: {parse}");
        assert!(
            offending.is_empty(),
            "an error built from a response body bypasses decode_json/safe_snippet: {offending:#?}"
        );
        let stale: Vec<_> = (0..ALLOWED.len()).filter(|i| !used.contains(i)).collect();
        assert!(stale.is_empty(), "unused exceptions: {stale:?}");
    }

    fn rules(source: &str) -> Vec<(&'static str, String)> {
        scan(source)
            .found
            .into_iter()
            .map(|(rule, function, _)| (rule, function))
            .collect()
    }

    #[test]
    fn the_scan_finds_each_way_round_the_rule() {
        let fns = |rule: &'static str, f: &str| vec![(rule, f.to_owned())];
        // decode: through an alias, over several lines, and a literal that
        // only starts the argument.
        assert_eq!(
            rules("fn a() { E::decode(\n    C,\n    e,\n    &resp.body,\n) }"),
            fns("decode", "a")
        );
        assert_eq!(
            rules(r#"fn b() { Error::decode(C, e, b"x".iter().chain(&resp.body)) }"#),
            fns("decode", "b")
        );
        // field: a struct literal, and the shorthand from a local.
        assert_eq!(
            rules("fn c() { Error::Http { status: 1, body_snippet: format!(\"{b:?}\") } }"),
            fns("field", "c")
        );
        assert_eq!(
            rules("fn d() { Error::Decode { context, source, body_snippet } }"),
            fns("field", "d")
        );
        assert_eq!(
            rules(
                "fn h() { Error::Http { status, body_snippet: safe_snippet(&b).replace(P, &r) } }"
            ),
            fns("field", "h")
        );
        // snippet: called, or imported under another name.
        assert_eq!(
            rules("fn e() { Error::Http { status: 1, body_snippet: snippet(&r.body) } }"),
            vec![("field", "e".to_owned()), ("snippet", "e".to_owned())]
        );
        assert_eq!(
            rules("use meta_whatsapp_core::error::snippet as s;"),
            fns("snippet", "")
        );
        // parse: serde's message wrapped into another error.
        assert_eq!(
            rules(
                "fn f() { serde_json::from_slice::<Vec<u8>>(&resp.body).map_err(|e| Error::Other(e.into())) }"
            ),
            fns("parse", "f")
        );
        // What passes: placeholders, literals and the helpers.
        assert_eq!(
            rules(
                r#"fn g() {
                    Error::decode(C, e, REDACTED);
                    Error::decode(C, e, b"(page item)");
                    Error::Http { status, body_snippet: WITHHELD.to_owned() };
                    Error::Http { status, body_snippet: crate::request::safe_snippet(&r.body) };
                    let n = e.body_snippet.len();
                    hex::decode(s);
                    serde_json::from_slice::<V>(&plain);
                }"#
            ),
            vec![]
        );
    }

    #[test]
    fn an_exception_holds_in_its_own_function_only() {
        assert!(allowed("request.rs", "decode_json", "decode").is_some());
        assert_eq!(allowed("request.rs", "send", "decode"), None);
        assert_eq!(allowed("request.rs", "decode_json", "field"), None);
        assert_eq!(allowed("groups/mod.rs", "decode_json", "decode"), None);
    }

    #[test]
    fn the_scan_skips_comments_literals_and_test_code() {
        let source = r##"
            // Error::decode(C, e, &resp.body)
            /// Never `Error::decode(…, &resp.body)`: use `decode_json`.
            /* serde_json::from_slice(&resp.body) /* nested */ snippet(x) */
            fn a() {
                let s = "Error::decode(C, e, &resp.body) // not a comment";
                let r = r#"{"body_snippet": snippet(b)}"#;
                let q = '"';
                let ok = Error::decode(C, e, b"still \" literal");
            }
            #[cfg(test)]
            mod tests {
                fn t() { Error::decode(C, e, &resp.body); snippet(&b); }
            }
            #[cfg(test)]
            mod more;
            fn after_tests() { Error::decode(C, e, &resp.body) }
        "##;
        let scan = scan(source);
        let found: Vec<_> = scan.found.into_iter().map(|(r, f, _)| (r, f)).collect();
        assert_eq!(found, vec![("decode", "after_tests".to_owned())]);
        assert_eq!(scan.seen, [2, 0, 0, 0]);
        assert_eq!(scan.test_modules, vec!["more".to_owned()]);
    }
}
