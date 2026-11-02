//! A Server-Sent Events reader over the router's answer (roadmap M2b):
//! the frames a live event stream sends, as a client parses them, one at a
//! time, without waiting for the body's end (it has none).

use std::time::Duration;

use http_body_util::BodyExt;
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::{Call, Reply};

/// A frame: its `event`, its `id`, its `data` (lines joined), or a
/// comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
    pub comment: Option<String>,
}

impl Frame {
    /// `data` as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.data).unwrap_or_else(|e| panic!("not JSON ({e}): {self:?}"))
    }

    /// `id` as a sequence.
    pub fn sequence(&self) -> i64 {
        self.id
            .as_deref()
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("no sequence: {self:?}"))
    }
}

/// An open stream.
pub struct Sse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    body: Body,
    buffer: String,
}

/// Open `call` on `router`: the stream when it answered `200`, else the
/// answer whole.
pub async fn open(router: &Router, call: Call) -> Result<Sse, Box<Reply>> {
    let response = router.clone().oneshot(call.build()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    if status != StatusCode::OK {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        return Err(Box::new(Reply {
            status,
            headers,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        }));
    }
    Ok(Sse {
        status,
        headers,
        body: response.into_body(),
        buffer: String::new(),
    })
}

impl Sse {
    /// The next frame within `wait`, comments included; `None` when none
    /// came, or the stream ended.
    pub async fn frame(&mut self, wait: Duration) -> Option<Frame> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let raw: String = self.buffer.drain(..end + 2).collect();
                return Some(parse(&raw));
            }
            let next = tokio::time::timeout_at(deadline, self.body.frame()).await;
            match next {
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.buffer.push_str(&String::from_utf8_lossy(&data));
                    }
                }
                _ => return None,
            }
        }
    }

    /// The next frame that is not a comment within `wait`.
    pub async fn next(&mut self, wait: Duration) -> Option<Frame> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let frame = self.frame(left).await?;
            if frame.comment.is_none() {
                return Some(frame);
            }
        }
    }

    /// The next `whatsapp` frame within `wait`; panics on anything else.
    pub async fn event(&mut self, wait: Duration) -> Frame {
        let frame = self
            .next(wait)
            .await
            .unwrap_or_else(|| panic!("no event within {wait:?}"));
        assert_eq!(frame.event.as_deref(), Some("whatsapp"), "{frame:?}");
        frame
    }

    /// Whether the stream ended (its body is done) within `wait`.
    pub async fn ended(&mut self, wait: Duration) -> bool {
        matches!(
            tokio::time::timeout(wait, self.body.frame()).await,
            Ok(None | Some(Err(_)))
        )
    }
}

fn parse(raw: &str) -> Frame {
    let mut frame = Frame {
        event: None,
        id: None,
        data: String::new(),
        comment: None,
    };
    let mut data = Vec::new();
    for line in raw.lines() {
        if let Some(comment) = line.strip_prefix(':') {
            frame.comment = Some(comment.trim().to_owned());
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => frame.event = Some(value.to_owned()),
            "id" => frame.id = Some(value.to_owned()),
            "data" => data.push(value.to_owned()),
            _ => {}
        }
    }
    frame.data = data.join("\n");
    frame
}
