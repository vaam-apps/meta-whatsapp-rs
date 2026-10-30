//! [`GraphRequest::send_streaming`]'s bound on a non-2xx body: at most
//! [`STREAMED_ERROR_BODY_CAP`] bytes are read, no chunk is pulled after
//! them, and the stream is dropped rather than drained; a 2xx body is the
//! caller's, unread.
//!
//! `ScriptedTransport` answers a streaming request with its whole body as
//! one chunk (the port's default), so these tests script the chunks
//! themselves: a body that counts its pulls, panics on one it was told
//! must never come, and records being dropped.

use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::ids::{MediaId, PhoneNumberId};
use meta_whatsapp_core::transport::{HttpRequest, HttpResponse, HttpTransport, StreamingResponse};
use meta_whatsapp_core::{Error, ErrorKind};
use pretty_assertions::assert_eq;

use super::{STREAMED_ERROR_BODY_CAP, decode_error};
use crate::media::{MediaDownload, MediaInfo};
use crate::{Client, RetryPolicy};

/// A media URL as `GET /{media-id}` answers it (`business-phone-numbers/media`).
const LOOKASIDE: &str = "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636&ext=1&hash=abc";

/// The example error response of `support/error-codes`.
const DOCUMENTED_ERROR: &str = r#"{
  "error": {
    "message": "(#130429) Rate limit hit",
    "type": "OAuthException",
    "code": 130429,
    "error_data": {
        "messaging_product": "whatsapp",
        "details": "Cloud API message throughput has been reached."
    },
    "error_subcode": 2494055,
    "fbtrace_id": "Az8or2yhqkZfEZ-_4Qn_Bam"
  }
}"#;

/// What a scripted body saw.
#[derive(Debug, Default)]
struct Probe {
    pulls: AtomicUsize,
    dropped: AtomicBool,
}

impl Probe {
    fn pulls(&self) -> usize {
        self.pulls.load(Ordering::SeqCst)
    }

    fn dropped(&self) -> bool {
        self.dropped.load(Ordering::SeqCst)
    }
}

/// Pull `n` (from 0) gives `chunk(n)`, `None` ending the body; pull
/// `never_from` and later panic: the code under test must not ask.
struct Body {
    probe: Arc<Probe>,
    chunk: Box<dyn FnMut(usize) -> Option<Bytes> + Send>,
    never_from: usize,
}

impl Stream for Body {
    type Item = Result<Bytes, TransportError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let n = this.probe.pulls.fetch_add(1, Ordering::SeqCst);
        assert!(
            n < this.never_from,
            "chunk {n} was pulled: the body was read past the cap"
        );
        Poll::Ready((this.chunk)(n).map(Ok))
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        self.probe.dropped.store(true, Ordering::SeqCst);
    }
}

/// A body of `chunks`, then its end; pulls from `never_from` on panic.
fn script(chunks: Vec<Bytes>, never_from: usize) -> (Body, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    let body = Body {
        probe: probe.clone(),
        chunk: Box::new(move |n| chunks.get(n).cloned()),
        never_from,
    };
    (body, probe)
}

/// A body that never ends: `chunk` on every pull, panicking from
/// `never_from` on.
fn endless(chunk: Bytes, never_from: usize) -> (Body, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    let body = Body {
        probe: probe.clone(),
        chunk: Box::new(move |_| Some(chunk.clone())),
        never_from,
    };
    (body, probe)
}

/// The method, URL and bearer token of each request.
type Seen = Arc<Mutex<Vec<(Method, String, Option<String>)>>>;

/// Answers the one streaming request it gets with `status` and a scripted
/// body; records the request's method, URL and bearer token.
struct StreamingTransport {
    status: StatusCode,
    body: Mutex<Option<Body>>,
    seen: Seen,
}

impl fmt::Debug for StreamingTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamingTransport")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpTransport for StreamingTransport {
    async fn send(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
        panic!("only `send_streaming` is scripted");
    }

    async fn send_streaming(
        &self,
        request: HttpRequest,
    ) -> Result<StreamingResponse, TransportError> {
        let bearer = request
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned);
        self.seen
            .lock()
            .unwrap()
            .push((request.method, request.url.to_string(), bearer));
        let body = self
            .body
            .lock()
            .unwrap()
            .take()
            .expect("one streaming request");
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(StreamingResponse {
            status: self.status,
            headers,
            body: Box::pin(body),
        })
    }
}

/// Download [`LOOKASIDE`] through a transport answering `status` and `body`,
/// asserting the one request the download made.
async fn download(status: u16, body: Body) -> crate::Result<MediaDownload> {
    let seen: Seen = Arc::default();
    let client = Client::builder()
        .transport(StreamingTransport {
            status: StatusCode::from_u16(status).unwrap(),
            body: Mutex::new(Some(body)),
            seen: seen.clone(),
        })
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap();
    let info = MediaInfo {
        messaging_product: "whatsapp".into(),
        url: LOOKASIDE.into(),
        mime_type: "image/jpeg".into(),
        sha256: String::new(),
        file_size: None,
        id: MediaId::new("1037543291543636").unwrap(),
    };
    let result = client
        .media(PhoneNumberId::new("106540352242922").unwrap())
        .download_with_info(info)
        .await;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(Method::GET, LOOKASIDE.to_owned(), Some("TOKEN".to_owned()))],
        "one GET of the media URL, with the token"
    );
    result
}

/// The `Error::Http` a non-2xx download failed with.
fn http_error(result: crate::Result<MediaDownload>) -> (u16, String) {
    match result {
        Err(Error::Http {
            status,
            body_snippet,
        }) => (status, body_snippet),
        Err(other) => panic!("expected Error::Http, got {other:?}"),
        Ok(_) => panic!("expected Error::Http, got a download"),
    }
}

/// [`DOCUMENTED_ERROR`] followed by spaces up to `len` bytes, still valid
/// JSON when read whole: `{"error":{…}` + spaces + `}`.
fn padded_envelope(len: usize) -> Vec<u8> {
    let envelope = DOCUMENTED_ERROR.trim_end();
    let open = envelope.strip_suffix('}').unwrap();
    let mut out = open.as_bytes().to_vec();
    out.resize(len - 1, b' ');
    out.push(b'}');
    assert_eq!(out.len(), len);
    out
}

#[test]
fn the_cap_is_64_kib() {
    assert_eq!(STREAMED_ERROR_BODY_CAP, 65_536);
}

/// An endless error body in 16 KiB chunks: four reach the cap, a fifth is
/// never pulled, and the stream is dropped when the call returns.
#[tokio::test]
async fn an_endless_error_body_is_read_to_the_cap_and_no_further() {
    let chunk = Bytes::from(vec![b'x'; 16 * 1024]);
    let (body, probe) = endless(chunk, 4);
    let (status, shown) = http_error(download(502, body).await);
    assert_eq!(status, 502);
    assert_eq!(
        shown,
        format!("{}…", "x".repeat(512)),
        "the first bytes only"
    );
    assert_eq!(probe.pulls(), 4, "the cap, in 16 KiB chunks");
    assert!(probe.dropped(), "the rest is dropped, not drained");
}

/// Chunks that do not divide the cap: the one crossing it is cut, and the
/// next is never pulled, whatever it holds.
#[tokio::test]
async fn the_chunk_crossing_the_cap_is_cut_and_the_next_never_pulled() {
    let (body, probe) = endless(Bytes::from(vec![b'y'; 10_000]), 7);
    let (status, shown) = http_error(download(500, body).await);
    assert_eq!(status, 500);
    assert!(shown.starts_with("yyyy") && shown.ends_with('…'), "{shown}");
    assert_eq!(
        probe.pulls(),
        7,
        "7 × 10 000 bytes is the first to reach 65 536"
    );
    assert!(probe.dropped());

    // One chunk far larger than the cap (a transport that buffers): read
    // once, cut, and nothing more asked for.
    let (body, probe) = endless(Bytes::from(vec![b'z'; 4 << 20]), 1);
    let (status, _) = http_error(download(500, body).await);
    assert_eq!((status, probe.pulls(), probe.dropped()), (500, 1, true));
}

/// Meta's documented error envelope, in chunks, decodes to the same
/// `Error::Api` the buffered path gives, with the HTTP status.
#[tokio::test]
async fn a_documented_error_envelope_still_decodes_with_its_status() {
    let bytes = DOCUMENTED_ERROR.as_bytes();
    let chunks = bytes
        .chunks(37)
        .map(Bytes::copy_from_slice)
        .collect::<Vec<_>>();
    let pulls = chunks.len() + 1; // the chunks, then the end
    let (body, probe) = script(chunks, pulls);
    let err = download(429, body).await.unwrap_err();
    let graph = err.graph().expect("an Error::Api");
    assert_eq!(graph.code, 130429);
    assert_eq!(graph.http_status, Some(429));
    assert_eq!(err.kind(), ErrorKind::RateLimited);
    assert_eq!(
        graph.details(),
        Some("Cloud API message throughput has been reached.")
    );
    let buffered = decode_error(&HttpResponse {
        status: StatusCode::TOO_MANY_REQUESTS,
        headers: HeaderMap::new(),
        body: Bytes::from_static(DOCUMENTED_ERROR.as_bytes()),
    });
    assert_eq!(err.graph(), buffered.graph(), "as from the buffered path");
    assert_eq!(probe.pulls(), pulls, "a small body is read to its end");
    assert!(probe.dropped());
}

/// The boundary: an envelope of exactly the cap is read whole and decodes,
/// without asking for more; one byte longer, its closing brace is past the
/// cap, so what was read is no envelope and the error is `Error::Http`.
#[tokio::test]
async fn an_envelope_of_exactly_the_cap_decodes_and_one_byte_more_does_not() {
    let at_cap = Bytes::from(padded_envelope(STREAMED_ERROR_BODY_CAP));
    let (body, probe) = script(vec![at_cap], 1);
    let err = download(400, body).await.unwrap_err();
    assert_eq!(
        err.graph().map(|g| (g.code, g.http_status)),
        Some((130429, Some(400)))
    );
    assert_eq!(probe.pulls(), 1, "the cap reached, the end is not awaited");

    let over = Bytes::from(padded_envelope(STREAMED_ERROR_BODY_CAP + 1));
    let (body, probe) = script(vec![over], 1);
    let (status, shown) = http_error(download(400, body).await);
    assert_eq!(status, 400);
    assert!(
        shown.starts_with("{\n  \"error\": {") && shown.ends_with('…'),
        "{shown}"
    );
    assert_eq!(probe.pulls(), 1);

    // The same bodies split across chunks at the cap.
    let whole = padded_envelope(STREAMED_ERROR_BODY_CAP + 1);
    let (head, tail) = whole.split_at(STREAMED_ERROR_BODY_CAP);
    let (body, probe) = script(
        vec![Bytes::copy_from_slice(head), Bytes::copy_from_slice(tail)],
        1,
    );
    let (status, _) = http_error(download(400, body).await);
    assert_eq!(
        (status, probe.pulls()),
        (400, 1),
        "the tail is never pulled"
    );
}

/// The token rule still holds on what was read: an error body naming
/// `access_token` keeps the withheld text, not its bytes.
#[tokio::test]
async fn a_cut_error_body_that_names_the_token_is_withheld() {
    let mut text =
        br#"{"paging":{"next":"https://graph.facebook.com/x?access_token=LEAKED"},"#.to_vec();
    text.resize(STREAMED_ERROR_BODY_CAP * 2, b' ');
    let (body, _) = script(vec![Bytes::from(text)], 1);
    let (_, shown) = http_error(download(500, body).await);
    assert!(!shown.contains("LEAKED"), "{shown}");
    assert_eq!(shown.as_bytes(), super::PAGING_WITHHELD);
}

/// A transport error while reading an error body is that error, as before
/// the cap.
#[tokio::test]
async fn a_transport_error_while_reading_an_error_body_surfaces() {
    struct Failing(Arc<Probe>);
    impl Stream for Failing {
        type Item = Result<Bytes, TransportError>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let n = self.0.pulls.fetch_add(1, Ordering::SeqCst);
            assert!(n < 2, "pulled after the error");
            Poll::Ready(Some(if n == 0 {
                Ok(Bytes::from_static(b"{\"error\":"))
            } else {
                Err(TransportError::Timeout)
            }))
        }
    }
    let probe = Arc::new(Probe::default());
    let failing = Failing(probe.clone());
    let read = super::read_error_body(StreamingResponse {
        status: StatusCode::BAD_GATEWAY,
        headers: HeaderMap::new(),
        body: Box::pin(failing),
    })
    .await;
    assert!(matches!(
        read,
        Err(Error::Transport(TransportError::Timeout))
    ));
    assert_eq!(probe.pulls(), 2);
}

/// A 2xx body is the caller's: returned unread, and readable to its end
/// however far past the error cap it goes.
#[tokio::test]
async fn a_2xx_stream_is_not_capped() {
    let chunks = vec![Bytes::from(vec![b'm'; 16 * 1024]); 8];
    let (body, probe) = script(chunks, 9);
    let download = download(200, body).await.unwrap();
    assert_eq!(probe.pulls(), 0, "nothing read before the caller reads");
    assert!(!probe.dropped());
    let read: Vec<Bytes> = download.body.map(|chunk| chunk.unwrap()).collect().await;
    assert_eq!(read.iter().map(Bytes::len).sum::<usize>(), 8 * 16 * 1024);
    const { assert!(8 * 16 * 1024 > STREAMED_ERROR_BODY_CAP) };
    assert_eq!(probe.pulls(), 9, "eight chunks, then the end");
    assert!(probe.dropped());
}
