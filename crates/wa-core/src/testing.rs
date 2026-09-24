//! Deterministic doubles for the ports. Enabled with the `testing` feature.
//!
//! [`ScriptedTransport`] answers requests from a queue and records every
//! request it saw, so a unit test can assert the exact method, path, query,
//! headers and JSON body the client produced — without a network, a mock
//! server, or a runtime beyond `#[tokio::test]`.
//!
//! ```
//! # async fn demo() {
//! use wa_core::testing::ScriptedTransport;
//! let t = ScriptedTransport::new();
//! t.push_json(200, serde_json::json!({"success": true}));
//! // ... build a client on `t.clone()`, call it ...
//! // let req = t.last_request().unwrap();
//! // assert_eq!(req.path(), "/v25.0/123/messages");
//! # }
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use url::Url;

use crate::error::TransportError;
use crate::transport::{HttpRequest, HttpResponse, HttpTransport, RequestBody};

type ErrorFactory = Box<dyn Fn() -> TransportError + Send + Sync>;

enum Scripted {
    Response(HttpResponse),
    Error(ErrorFactory),
}

#[derive(Default)]
struct State {
    queue: VecDeque<Scripted>,
    requests: Vec<RecordedRequest>,
}

/// A transport that replays scripted responses in order.
#[derive(Clone, Default)]
pub struct ScriptedTransport {
    state: Arc<Mutex<State>>,
}

impl fmt::Debug for ScriptedTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("ScriptedTransport")
            .field("queued", &s.queue.len())
            .field("recorded", &s.requests.len())
            .finish()
    }
}

impl ScriptedTransport {
    /// Empty script.
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, item: Scripted) -> &Self {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queue
            .push_back(item);
        self
    }

    /// Queue a JSON response.
    #[allow(clippy::needless_pass_by_value)] // `push_json(200, json!(..))` reads better
    pub fn push_json(&self, status: u16, body: serde_json::Value) -> &Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        self.push(Scripted::Response(HttpResponse {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            headers,
            body: Bytes::from(body.to_string()),
        }))
    }

    /// Queue a raw response.
    pub fn push_bytes(&self, status: u16, content_type: &str, body: impl Into<Bytes>) -> &Self {
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(content_type) {
            headers.insert(http::header::CONTENT_TYPE, v);
        }
        self.push(Scripted::Response(HttpResponse {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            headers,
            body: body.into(),
        }))
    }

    /// Queue a transport failure.
    pub fn push_error(&self, make: impl Fn() -> TransportError + Send + Sync + 'static) -> &Self {
        self.push(Scripted::Error(Box::new(make)))
    }

    /// Every request seen so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requests
            .clone()
    }

    /// The most recent request.
    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requests
            .last()
            .cloned()
    }

    /// Responses queued but not consumed. Assert `0` at the end of a test to
    /// prove the code under test made every request you scripted.
    pub fn remaining(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queue
            .len()
    }
}

#[async_trait]
impl HttpTransport for ScriptedTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let recorded = RecordedRequest::capture(request).await?;
        let (method, url) = (recorded.method.clone(), recorded.url.clone());
        let next = {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            s.requests.push(recorded);
            s.queue.pop_front()
        };
        match next {
            Some(Scripted::Response(r)) => Ok(r),
            Some(Scripted::Error(make)) => Err(make()),
            None => Err(TransportError::Backend(anyhow::anyhow!(
                "ScriptedTransport: no response scripted for {method} {url}"
            ))),
        }
    }
}

/// A captured request.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Method.
    pub method: Method,
    /// Full URL.
    pub url: Url,
    /// Headers.
    pub headers: HeaderMap,
    /// Body, buffered.
    pub body: RecordedBody,
}

/// A captured body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedBody {
    /// No body.
    Empty,
    /// Buffered or streamed bytes.
    Bytes {
        /// Content-Type.
        content_type: String,
        /// Bytes.
        data: Bytes,
    },
    /// Multipart form fields as `(name, filename, content_type, data)`.
    Multipart(Vec<(String, Option<String>, Option<String>, Bytes)>),
}

impl RecordedRequest {
    async fn capture(req: HttpRequest) -> Result<Self, TransportError> {
        let body = match req.body {
            RequestBody::Empty => RecordedBody::Empty,
            RequestBody::Bytes { content_type, data } => RecordedBody::Bytes { content_type, data },
            RequestBody::Multipart(m) => RecordedBody::Multipart(
                m.parts
                    .into_iter()
                    .map(|p| (p.name, p.filename, p.content_type, p.data))
                    .collect(),
            ),
            RequestBody::Stream {
                content_type,
                mut stream,
                ..
            } => {
                let mut buf = Vec::new();
                while let Some(chunk) = stream.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                RecordedBody::Bytes {
                    content_type,
                    data: Bytes::from(buf),
                }
            }
        };
        Ok(Self {
            method: req.method,
            url: req.url,
            headers: req.headers,
            body,
        })
    }

    /// URL path, e.g. `/v25.0/123/messages`.
    pub fn path(&self) -> &str {
        self.url.path()
    }

    /// First value of a query parameter.
    pub fn query(&self, key: &str) -> Option<String> {
        self.url
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    /// Header value as a string.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Token from `Authorization: Bearer …`.
    pub fn bearer(&self) -> Option<&str> {
        self.header("authorization")?.strip_prefix("Bearer ")
    }

    /// Body parsed as JSON, when it is a buffered body.
    pub fn json(&self) -> Option<serde_json::Value> {
        match &self.body {
            RecordedBody::Bytes { data, .. } => serde_json::from_slice(data).ok(),
            _ => None,
        }
    }

    /// Multipart field by name, as `(filename, content_type, data)`.
    pub fn multipart_field(&self, name: &str) -> Option<(Option<&str>, Option<&str>, &Bytes)> {
        match &self.body {
            RecordedBody::Multipart(parts) => parts
                .iter()
                .find(|p| p.0 == name)
                .map(|p| (p.1.as_deref(), p.2.as_deref(), &p.3)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Multipart;

    #[tokio::test]
    async fn replays_in_order_and_records() {
        let t = ScriptedTransport::new();
        t.push_json(200, serde_json::json!({"a": 1}));
        t.push_error(|| TransportError::Timeout);

        let mut req = HttpRequest::new(
            Method::POST,
            Url::parse("https://graph.facebook.com/v25.0/1/media?x=y").unwrap(),
        );
        req.body = RequestBody::Multipart(Multipart::new().text("messaging_product", "whatsapp"));
        let r = t.send(req).await.unwrap();
        assert_eq!(r.status, 200);
        let err = t
            .send(HttpRequest::new(
                Method::GET,
                Url::parse("https://graph.facebook.com/v25.0/2").unwrap(),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::Timeout));
        assert!(
            t.send(HttpRequest::new(
                Method::GET,
                Url::parse("https://graph.facebook.com/v25.0/3").unwrap()
            ))
            .await
            .is_err()
        );

        let reqs = t.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].path(), "/v25.0/1/media");
        assert_eq!(reqs[0].query("x").as_deref(), Some("y"));
        let (_, _, data) = reqs[0].multipart_field("messaging_product").unwrap();
        assert_eq!(&data[..], b"whatsapp");
        assert_eq!(t.remaining(), 0);
    }
}
