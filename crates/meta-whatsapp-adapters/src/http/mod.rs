//! [`HttpTransport`] over `reqwest` (feature `reqwest`): rustls with the
//! aws-lc-rs provider, HTTP/2, streaming in both directions.
//!
//! ```no_run
//! # fn demo() -> Result<(), meta_whatsapp_core::error::ConfigError> {
//! use std::time::Duration;
//! use meta_whatsapp_adapters::http::ReqwestTransport;
//!
//! let transport = ReqwestTransport::builder()
//!     .connect_timeout(Duration::from_secs(5))
//!     .pool_idle_timeout(Some(Duration::from_secs(60)))
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! Behaviour the port leaves to the adapter:
//!
//! - **Timeouts.** [`HttpRequest::timeout`] is a total deadline, from
//!   connecting until the body is fully read — for [`send_streaming`] that
//!   includes reading the stream, so give large downloads a generous one.
//!   `None` falls back to the builder's [`timeout`](ReqwestTransportBuilder::timeout)
//!   (unset by default: no deadline). Exceeding either is
//!   [`TransportError::Timeout`], also when it fires mid-stream.
//! - **Errors.** Timeout → `Timeout`; DNS/TCP/TLS failure → `Connect`; an
//!   unusable request (bad header value, bad MIME type) → `Build`; anything
//!   else → `Backend`. Non-2xx responses are returned, not errors.
//! - **No URL in errors.** reqwest errors carry the request URL, and Graph
//!   URLs can carry secrets in the query (`client_secret`, `code` in the
//!   token exchange). The URL is stripped before an error leaves this
//!   module.
//! - **Redirects** are followed up to 10 hops, with no `Referer` (reqwest's
//!   default would hand the previous URL, query string included, to the
//!   redirect target), except where they would carry a credential. reqwest
//!   rebuilds every hop from the request's original headers and drops
//!   `Authorization`, `Proxy-Authorization` and `Cookie` only on a hop that
//!   changes scheme, host or port from the URL that answered it; so a
//!   request that carries one of them (a user name or password in its URL
//!   included: reqwest sends those as `Authorization: Basic`) does not
//!   follow a hop that keeps scheme, host and port: that 3xx is the
//!   response (a non-2xx, so an error in `meta-whatsapp-client`). The
//!   client checked only the URL it asked for (its credential rules allow
//!   one path on `api.facebook.com`, not the host), and a hop to another
//!   origin followed by one within it would hand that origin the token.
//!   Hops that change origin are followed, without the credentials. That
//!   policy is [`credential_redirect_policy`]. A request without
//!   credentials follows reqwest's default. Clients you build yourself go
//!   in through [`ReqwestTransport::with_clients`]; the one client given to
//!   [`ReqwestTransport::with_client`] keeps its own policy, see there.
//! - **Proxies.** `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY`
//!   (or their lower-case forms) are honoured, read once when the transport
//!   is built. Operating-system proxy settings on macOS and Windows are not
//!   (reqwest's `system-proxy` feature is off: a server rarely has them and
//!   it pulls platform crates in). For a proxy configured in code, build
//!   the `reqwest::Client`s with `.proxy(…)` and use
//!   [`ReqwestTransport::with_clients`].
//!
//! [`send_streaming`]: HttpTransport::send_streaming

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, PROXY_AUTHORIZATION};
use http::{HeaderMap, HeaderName, HeaderValue};
use meta_whatsapp_core::error::{ConfigError, TransportError};
use meta_whatsapp_core::transport::{
    HttpRequest, HttpResponse, HttpTransport, Multipart, RequestBody, StreamingResponse,
};

/// `HttpTransport` backed by a `reqwest::Client`. Cheap to clone (the
/// connection pools are shared).
#[derive(Clone)]
pub struct ReqwestTransport {
    /// Requests without credentials (reqwest's default redirect policy,
    /// in the stock transport); it also builds every request.
    client: reqwest::Client,
    /// Requests with credentials ([`carries_credentials`]): the same
    /// settings, and [`credential_redirect_policy`] (the same client as
    /// `client` when it came from [`ReqwestTransport::with_client`]).
    credentialed: reqwest::Client,
}

impl fmt::Debug for ReqwestTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // reqwest's own Debug lists default headers, which may be
        // credentials an integrator configured.
        f.debug_struct("ReqwestTransport").finish_non_exhaustive()
    }
}

impl ReqwestTransport {
    /// A transport with the [builder](ReqwestTransportBuilder)'s defaults.
    /// Fails only if the TLS backend cannot be initialised.
    pub fn new() -> Result<Self, ConfigError> {
        Self::builder().build()
    }

    /// Configure a transport.
    pub fn builder() -> ReqwestTransportBuilder {
        ReqwestTransportBuilder::default()
    }

    /// Use your own clients (proxies, mTLS, custom roots…): `credentialed`
    /// sends every request that carries a credential (an `Authorization`,
    /// `Proxy-Authorization` or `Cookie` header, or a user name or password
    /// in its URL), `plain` every other. Their timeouts apply when a
    /// request has none. The choice reads the request, not the headers a
    /// client adds to every request (`default_headers`): set no credential
    /// there, `plain` would follow redirects with it.
    ///
    /// Their settings are yours, and two of them are security settings
    /// that [`ReqwestTransport::builder`] makes for you:
    ///
    /// - **`credentialed` must be built with
    ///   `.redirect(`[`credential_redirect_policy()`]`)`**, or with
    ///   `.redirect(reqwest::redirect::Policy::none())`. reqwest's default
    ///   policy sends the credentials again on a redirect that keeps
    ///   scheme, host and port, and after a hop to another origin, on
    ///   every hop within that origin: to URLs the Graph client never
    ///   checked (see the [module docs](crate::http)). The transport cannot
    ///   check or change a built client's policy.
    /// - reqwest sends a `Referer` on redirects unless you build them with
    ///   `.referer(false)`: it would hand the previous URL, query included
    ///   (`client_secret`, `code`), to the redirect target.
    ///
    /// ```no_run
    /// # fn demo() -> reqwest::Result<()> {
    /// use meta_whatsapp_adapters::http::{ReqwestTransport, credential_redirect_policy};
    ///
    /// let proxy = || reqwest::Proxy::all("http://egress.internal:3128");
    /// let plain = reqwest::Client::builder()
    ///     .proxy(proxy()?)
    ///     .referer(false)
    ///     .build()?;
    /// let credentialed = reqwest::Client::builder()
    ///     .proxy(proxy()?)
    ///     .referer(false)
    ///     .redirect(credential_redirect_policy())
    ///     .build()?;
    /// let transport = ReqwestTransport::with_clients(plain, credentialed);
    /// # Ok(()) }
    /// ```
    pub fn with_clients(plain: reqwest::Client, credentialed: reqwest::Client) -> Self {
        Self {
            client: plain,
            credentialed,
        }
    }

    /// Use one existing client as is, for every request: prefer
    /// [`ReqwestTransport::with_clients`], which keeps redirects for the
    /// requests without credentials.
    ///
    /// **This client's redirect policy is also the one requests with
    /// credentials get**, and the transport cannot check or change it. So
    /// build it with `.redirect(`[`credential_redirect_policy()`]`)` or
    /// `.redirect(reqwest::redirect::Policy::none())`. With reqwest's
    /// default policy (a plain `reqwest::Client::new()` has it), a redirect
    /// that keeps scheme, host and port carries the token to a URL the
    /// Graph client never checked, and so does every hop within another
    /// origin after a hop there (see the [module docs](crate::http)).
    /// Build it with `.referer(false)` too, see
    /// [`ReqwestTransport::with_clients`].
    pub fn with_client(client: reqwest::Client) -> Self {
        Self::with_clients(client.clone(), client)
    }

    /// `request` as reqwest's, and the client that must send it: the
    /// credentialed one when the request carries a credential. That is
    /// read from the request reqwest built, not from ours: reqwest turns a
    /// user name or password in the URL into `Authorization: Basic`.
    fn prepare(
        &self,
        request: HttpRequest,
    ) -> Result<(&reqwest::Client, reqwest::Request), TransportError> {
        let request = self.build(request)?;
        let client = if carries_credentials(request.headers()) {
            &self.credentialed
        } else {
            &self.client
        };
        Ok((client, request))
    }

    /// `request` as reqwest's. A built request does not depend on the
    /// client that built it: either client can send it.
    fn build(&self, request: HttpRequest) -> Result<reqwest::Request, TransportError> {
        let HttpRequest {
            method,
            url,
            mut headers,
            body,
            timeout,
        } = request;
        let mut builder = self.client.request(method, url);
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        builder = match body {
            RequestBody::Empty => builder.headers(headers),
            RequestBody::Bytes { content_type, data } => {
                // The body decides these; a stale caller value would lie.
                headers.remove(CONTENT_LENGTH);
                headers.insert(CONTENT_TYPE, header_value(&content_type)?);
                builder.headers(headers).body(data)
            }
            RequestBody::Multipart(form) => {
                // `multipart` sets the boundary content type and the length.
                headers.remove(CONTENT_TYPE);
                headers.remove(CONTENT_LENGTH);
                builder.headers(headers).multipart(multipart(form)?)
            }
            RequestBody::Stream {
                content_type,
                content_length,
                stream,
            } => {
                headers.remove(CONTENT_LENGTH);
                headers.insert(CONTENT_TYPE, header_value(&content_type)?);
                // With a length the body goes out with Content-Length, not
                // chunked: some upload endpoints require it.
                if let Some(len) = content_length {
                    headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
                }
                builder
                    .headers(headers)
                    .body(reqwest::Body::wrap_stream(stream))
            }
            _ => {
                return Err(TransportError::Build(
                    "request body kind not supported by ReqwestTransport".to_owned(),
                ));
            }
        };
        builder.build().map_err(map_error)
    }
}

/// The headers reqwest drops on a redirect that changes origin, and keeps
/// on one that does not: the credentials (`Cookie2` and `WWW-Authenticate`,
/// which it drops too, carry none of ours).
const CREDENTIAL_HEADERS: [HeaderName; 3] = [AUTHORIZATION, PROXY_AUTHORIZATION, COOKIE];

/// Whether a request carries a credential, so must not follow a redirect
/// that keeps it ([`credential_redirect_policy`]).
fn carries_credentials(headers: &HeaderMap) -> bool {
    CREDENTIAL_HEADERS
        .iter()
        .any(|name| headers.contains_key(name))
}

/// The redirect policy the stock transport gives requests that carry a
/// credential; build the `credentialed` client of
/// [`ReqwestTransport::with_clients`] with it.
///
/// reqwest (with tower-http's redirect service) builds every hop from the
/// request's original headers, and drops `Authorization`,
/// `Proxy-Authorization` and `Cookie` only when the hop changes scheme,
/// host or port from the URL that answered it. So a hop that keeps all
/// three, compared as reqwest compares them (parsed URLs, the default port
/// filled in), would carry the credentials: this policy does not follow
/// it, and the 3xx is the response. Any other hop is followed, without
/// them, up to reqwest's default limit of 10 (then an error). When it
/// cannot see the URL that answered, it does not follow either.
///
/// It stops such hops whether or not the request carries a credential: a
/// client with this policy does not follow a redirect within an origin at
/// all.
pub fn credential_redirect_policy() -> reqwest::redirect::Policy {
    let default = reqwest::redirect::Policy::default();
    reqwest::redirect::Policy::custom(move |attempt| {
        let next = attempt.url();
        // reqwest always lists the URL that answered; without it, stop.
        let keeps_credentials = attempt.previous().last().is_none_or(|previous| {
            next.scheme() == previous.scheme()
                && next.host_str() == previous.host_str()
                && next.port_or_known_default() == previous.port_or_known_default()
        });
        if keeps_credentials {
            attempt.stop()
        } else {
            default.redirect(attempt)
        }
    })
}

/// Builder for [`ReqwestTransport`].
#[derive(Debug, Clone)]
pub struct ReqwestTransportBuilder {
    connect_timeout: Option<Duration>,
    timeout: Option<Duration>,
    pool_idle_timeout: Option<Duration>,
    pool_max_idle_per_host: usize,
    tcp_keepalive: Option<Duration>,
}

impl Default for ReqwestTransportBuilder {
    fn default() -> Self {
        Self {
            // The OS default can be minutes; a Graph call that cannot even
            // connect in 10s is better retried.
            connect_timeout: Some(Duration::from_secs(10)),
            timeout: None,
            pool_idle_timeout: Some(Duration::from_secs(90)),
            pool_max_idle_per_host: usize::MAX,
            tcp_keepalive: Some(Duration::from_secs(60)),
        }
    }
}

impl ReqwestTransportBuilder {
    /// Deadline for establishing a connection (TCP + TLS). Default 10s.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Total deadline for requests that do not set
    /// [`HttpRequest::timeout`]. Default: none. `meta-whatsapp-client` sets one on
    /// every request it makes.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// How long an idle pooled connection is kept; `None` keeps it until
    /// the server closes it. Default 90s.
    #[must_use]
    pub fn pool_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.pool_idle_timeout = timeout;
        self
    }

    /// Maximum idle connections kept per host. Default: unlimited.
    #[must_use]
    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.pool_max_idle_per_host = max;
        self
    }

    /// TCP keep-alive interval; `None` disables it. Default 60s.
    #[must_use]
    pub fn tcp_keepalive(mut self, interval: Option<Duration>) -> Self {
        self.tcp_keepalive = interval;
        self
    }

    /// Build the transport. Fails only if the TLS backend cannot be
    /// initialised (e.g. no usable root certificates).
    pub fn build(self) -> Result<ReqwestTransport, ConfigError> {
        Ok(ReqwestTransport {
            client: self.client(reqwest::redirect::Policy::default())?,
            credentialed: self.client(credential_redirect_policy())?,
        })
    }

    fn client(&self, redirects: reqwest::redirect::Policy) -> Result<reqwest::Client, ConfigError> {
        let mut builder = reqwest::Client::builder()
            // A Referer on a redirect would carry the previous URL's query
            // (`client_secret`, `code`) to whatever host it points at.
            .referer(false)
            .redirect(redirects)
            .pool_idle_timeout(self.pool_idle_timeout)
            .pool_max_idle_per_host(self.pool_max_idle_per_host)
            .tcp_keepalive(self.tcp_keepalive);
        if let Some(t) = self.connect_timeout {
            builder = builder.connect_timeout(t);
        }
        if let Some(t) = self.timeout {
            builder = builder.timeout(t);
        }
        builder
            .build()
            .map_err(|e| ConfigError::new(format!("could not build the HTTP client: {e}")))
    }
}

fn header_value(content_type: &str) -> Result<HeaderValue, TransportError> {
    HeaderValue::from_str(content_type)
        .map_err(|_| TransportError::Build(format!("invalid content type `{content_type}`")))
}

/// Our multipart form as reqwest's. Parts keep their order, file name and
/// MIME type; each has a known length, so the whole form is sent with a
/// Content-Length (Meta's upload endpoint wants one).
fn multipart(form: Multipart) -> Result<reqwest::multipart::Form, TransportError> {
    let mut out = reqwest::multipart::Form::new();
    for part in form.parts {
        let len = u64::try_from(part.data.len()).unwrap_or(u64::MAX);
        let mut p = reqwest::multipart::Part::stream_with_length(part.data, len);
        if let Some(filename) = part.filename {
            p = p.file_name(filename);
        }
        if let Some(mime) = part.content_type {
            p = p.mime_str(&mime).map_err(|_| {
                TransportError::Build(format!(
                    "invalid content type `{mime}` for multipart part `{}`",
                    part.name
                ))
            })?;
        }
        out = out.part(part.name, p);
    }
    Ok(out)
}

/// reqwest error → port error. Always strips the URL first (see the module
/// docs). A timeout wins over "connect": a connect timeout is a timeout.
fn map_error(error: reqwest::Error) -> TransportError {
    let error = error.without_url();
    if error.is_timeout() {
        TransportError::Timeout
    } else if error.is_connect() {
        TransportError::Connect(error.into())
    } else if error.is_builder() {
        TransportError::Build(error.to_string())
    } else {
        TransportError::Backend(error.into())
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let (client, request) = self.prepare(request)?;
        let mut response = client.execute(request).await.map_err(map_error)?;
        let status = response.status();
        let headers = std::mem::take(response.headers_mut());
        let body = response.bytes().await.map_err(map_error)?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    async fn send_streaming(
        &self,
        request: HttpRequest,
    ) -> Result<StreamingResponse, TransportError> {
        let (client, request) = self.prepare(request)?;
        let mut response = client.execute(request).await.map_err(map_error)?;
        let status = response.status();
        let headers = std::mem::take(response.headers_mut());
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(map_error));
        Ok(StreamingResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}
