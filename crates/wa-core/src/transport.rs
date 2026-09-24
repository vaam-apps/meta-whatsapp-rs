//! The HTTP transport port.
//!
//! `wa-client` builds [`HttpRequest`]s and hands them to an
//! [`HttpTransport`]. The shipped adapter is `wa_adapters::http::ReqwestTransport`;
//! write your own to route through a corporate proxy, add mTLS, record
//! traffic, or use a different client. Tests use
//! [`crate::testing::ScriptedTransport`].
//!
//! The port is deliberately dumb: no retries, no auth, no JSON. Those live in
//! the client, once, instead of in every adapter.

use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use http::{HeaderMap, Method, StatusCode};
use url::Url;

use crate::error::TransportError;

/// A stream of body chunks.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send + 'static>>;

/// An outgoing request.
#[derive(Debug)]
pub struct HttpRequest {
    /// Method.
    pub method: Method,
    /// Absolute URL, query included.
    pub url: Url,
    /// Headers, `Authorization` included when the client adds one.
    pub headers: HeaderMap,
    /// Body.
    pub body: RequestBody,
    /// Per-request deadline; `None` means the adapter's default.
    pub timeout: Option<Duration>,
}

impl HttpRequest {
    /// A request with no headers and no body.
    pub fn new(method: Method, url: Url) -> Self {
        Self {
            method,
            url,
            headers: HeaderMap::new(),
            body: RequestBody::Empty,
            timeout: None,
        }
    }
}

/// Request body.
#[non_exhaustive]
pub enum RequestBody {
    /// No body.
    Empty,
    /// A buffered body with its content type (`application/json`, …).
    Bytes {
        /// Content-Type header value.
        content_type: String,
        /// Body bytes.
        data: Bytes,
    },
    /// `multipart/form-data` (media upload).
    Multipart(Multipart),
    /// A streamed body (large uploads). Adapters that cannot stream may
    /// buffer it.
    Stream {
        /// Content-Type header value.
        content_type: String,
        /// Length, when known; some servers require it.
        content_length: Option<u64>,
        /// The chunks.
        stream: ByteStream,
    },
}

impl RequestBody {
    /// A JSON body.
    pub fn json(data: impl Into<Bytes>) -> Self {
        Self::Bytes {
            content_type: "application/json".to_owned(),
            data: data.into(),
        }
    }

    /// Buffered body bytes, when buffered. Test doubles use this.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes { data, .. } => Some(data),
            _ => None,
        }
    }
}

impl fmt::Debug for RequestBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes { content_type, data } => f
                .debug_struct("Bytes")
                .field("content_type", content_type)
                .field("len", &data.len())
                .finish(),
            Self::Multipart(m) => f.debug_tuple("Multipart").field(m).finish(),
            Self::Stream {
                content_type,
                content_length,
                ..
            } => f
                .debug_struct("Stream")
                .field("content_type", content_type)
                .field("content_length", content_length)
                .finish_non_exhaustive(),
        }
    }
}

/// A `multipart/form-data` body.
#[derive(Debug, Default)]
pub struct Multipart {
    /// Parts, in order.
    pub parts: Vec<Part>,
}

impl Multipart {
    /// Empty form.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a text field.
    #[must_use]
    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.parts.push(Part {
            name: name.into(),
            filename: None,
            content_type: None,
            data: Bytes::from(value.into()),
        });
        self
    }

    /// Add a file field.
    #[must_use]
    pub fn file(
        mut self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> Self {
        self.parts.push(Part {
            name: name.into(),
            filename: Some(filename.into()),
            content_type: Some(content_type.into()),
            data: data.into(),
        });
        self
    }

    /// Find a part by name. Test doubles use this.
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }
}

/// One multipart field.
pub struct Part {
    /// Field name.
    pub name: String,
    /// File name, for file fields.
    pub filename: Option<String>,
    /// Content type, for file fields.
    pub content_type: Option<String>,
    /// Field contents.
    pub data: Bytes,
}

impl fmt::Debug for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Part")
            .field("name", &self.name)
            .field("filename", &self.filename)
            .field("content_type", &self.content_type)
            .field("len", &self.data.len())
            .finish()
    }
}

/// A fully buffered response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// Status.
    pub status: StatusCode,
    /// Headers.
    pub headers: HeaderMap,
    /// Body.
    pub body: Bytes,
}

/// A response whose body is still streaming (media download).
pub struct StreamingResponse {
    /// Status.
    pub status: StatusCode,
    /// Headers.
    pub headers: HeaderMap,
    /// Body chunks.
    pub body: ByteStream,
}

impl fmt::Debug for StreamingResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamingResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl StreamingResponse {
    /// Buffer the whole body.
    pub async fn collect(self) -> Result<HttpResponse, TransportError> {
        let mut body = Vec::new();
        let mut stream = self.body;
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk?);
        }
        Ok(HttpResponse {
            status: self.status,
            headers: self.headers,
            body: Bytes::from(body),
        })
    }
}

/// Sends HTTP requests. Implementations must be cheap to share (`Arc`).
#[async_trait]
pub trait HttpTransport: Send + Sync + fmt::Debug + 'static {
    /// Send `request` and buffer the response.
    ///
    /// Non-2xx statuses are **not** errors at this layer; return them as a
    /// normal [`HttpResponse`] so the client can decode Graph error bodies.
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;

    /// Send `request` and stream the response body.
    ///
    /// The default buffers via [`HttpTransport::send`]; adapters that can
    /// stream should override it.
    async fn send_streaming(
        &self,
        request: HttpRequest,
    ) -> Result<StreamingResponse, TransportError> {
        let resp = self.send(request).await?;
        let body = resp.body;
        Ok(StreamingResponse {
            status: resp.status,
            headers: resp.headers,
            body: Box::pin(futures::stream::once(async move { Ok(body) })),
        })
    }
}

#[async_trait]
impl<T: HttpTransport + ?Sized> HttpTransport for std::sync::Arc<T> {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        (**self).send(request).await
    }

    async fn send_streaming(
        &self,
        request: HttpRequest,
    ) -> Result<StreamingResponse, TransportError> {
        (**self).send_streaming(request).await
    }
}
