//! QR codes and short links that open a chat with a prefilled message.
//!
//! Docs:
//! - `qr-codes` — create, list, get, update, delete; limits.
//! - `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-management-api`
//!   — `GET`/`POST /{phone-number-id}/message_qrdls`.
//! - `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-api`
//!   — `GET`/`DELETE /{phone-number-id}/message_qrdls/{qr-code-id}`.
//!
//! Limits enforced locally (from `qr-codes` and the reference): a prefilled
//! message is 1–140 characters, a QR code id is 14 alphanumeric characters,
//! and a list page holds 1–25 codes. The 2,000-codes-per-number cap is a
//! server-side count and is left to Meta.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use futures::{Stream, StreamExt, future, stream};
use serde::{Deserialize, Serialize};
use wa_core::error::ValidationError;
use wa_core::ids::{PhoneNumberId, QrCodeId};
use wa_core::paging::Page;
use wa_core::{Error, Result};

use crate::{Client, GraphRequest};

/// Maximum length of a prefilled message, in characters (`qr-codes`,
/// "Limitations").
pub const PREFILLED_MESSAGE_MAX_CHARS: usize = 140;

/// Length of a QR code id (`...qr-code-api`: "unique 14-character
/// identifier").
pub const QR_CODE_ID_LEN: usize = 14;

/// Largest `limit` the list endpoint accepts (`...qr-code-management-api`:
/// `limit` is `integer [min: 1, max: 25]`).
pub const LIST_LIMIT_MAX: u32 = 25;

/// Entry point, see [`Client::qr_codes`].
#[derive(Debug, Clone)]
pub struct QrCodes {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`QrCodes`] API for `phone_number_id`.
    pub fn qr_codes(&self, phone_number_id: impl Into<PhoneNumberId>) -> QrCodes {
        QrCodes {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl QrCodes {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn collection(&self) -> String {
        format!("{}/message_qrdls", self.phone_number_id)
    }

    fn item(&self, code: &QrCodeId) -> Result<String> {
        validate_code("code", code)?;
        Ok(format!("{}/message_qrdls/{code}", self.phone_number_id))
    }

    /// Create a QR code and short link:
    /// `POST /{phone-number-id}/message_qrdls` (`qr-codes#create-qr-code`).
    ///
    /// The response carries `qr_image_url` only when
    /// [`CreateQrCode::generate_qr_image`] is set. Not idempotent: a replay
    /// would create a second code.
    pub async fn create(&self, request: &CreateQrCode) -> Result<QrCode> {
        validate_message(&request.prefilled_message)?;
        self.client
            .post(&self.collection())
            .json(request)
            .context("create QR code response")
            .send()
            .await
    }

    /// Change the prefilled message of an existing code: the same
    /// `POST /{phone-number-id}/message_qrdls`, with `code` in the body
    /// (`qr-codes#update-a-qr-code`).
    ///
    /// Marked idempotent: it sets the message of a given code to a value.
    pub async fn update(&self, request: &UpdateQrCode) -> Result<QrCode> {
        validate_code("code", &request.code)?;
        validate_message(&request.prefilled_message)?;
        self.client
            .post(&self.collection())
            .json(request)
            .idempotent(true)
            .context("update QR code response")
            .send()
            .await
    }

    /// One code: `GET /{phone-number-id}/message_qrdls/{qr-code-id}`
    /// (`qr-codes#get-a-qr-code`).
    ///
    /// Meta wraps the code in a one-element `data` array; this unwraps it
    /// and fails with a decode error if the array is empty.
    pub async fn get(&self, code: &QrCodeId, fields: &QrCodeFields) -> Result<QrCode> {
        let path = self.item(code)?;
        let resp: DataList<QrCode> = self
            .client
            .get(&path)
            .query_opt("fields", fields.to_param())
            .context("get QR code response")
            .send()
            .await?;
        resp.data.into_iter().next().ok_or_else(|| {
            Error::decode(
                "get QR code response",
                serde::de::Error::custom("`data` is empty"),
                b"",
            )
        })
    }

    /// One page of codes, newest first:
    /// `GET /{phone-number-id}/message_qrdls` (`qr-codes#get-list`).
    pub async fn list(&self, query: &ListQrCodes) -> Result<Page<QrCode>> {
        self.list_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every code, following `paging.cursors.after` until the last page.
    ///
    /// The stream manages cursors itself, so `query.after`/`query.before`
    /// must be `None`; otherwise (or on any other validation failure) the
    /// stream yields that single error.
    pub fn list_stream(
        &self,
        query: &ListQrCodes,
    ) -> impl Stream<Item = Result<QrCode>> + Send + 'static {
        let request = reject_cursors(query.after.as_deref(), query.before.as_deref())
            .and_then(|()| self.list_request(query));
        match request {
            Ok(req) => req.paginate::<QrCode>().left_stream(),
            Err(e) => stream::once(future::ready(Err(e))).right_stream(),
        }
    }

    fn list_request(&self, query: &ListQrCodes) -> Result<GraphRequest> {
        if let Some(limit) = query.limit
            && !(1..=LIST_LIMIT_MAX).contains(&limit)
        {
            return Err(ValidationError::new(
                "limit",
                format!("must be between 1 and {LIST_LIMIT_MAX}"),
            )
            .into());
        }
        if let Some(code) = &query.code {
            validate_code("code", code)?;
        }
        Ok(self
            .client
            .get(&self.collection())
            .query_opt("fields", query.fields.to_param())
            .query_opt("code", query.code.as_ref())
            .query_opt("limit", query.limit)
            .context("list QR codes response"))
    }

    /// Delete a code: `DELETE /{phone-number-id}/message_qrdls/{qr-code-id}`
    /// (`qr-codes#delete-qr-code`). Users who scan it afterwards are told it
    /// expired.
    pub async fn delete(&self, code: &QrCodeId) -> Result<()> {
        let path = self.item(code)?;
        self.client
            .delete(&path)
            .context("delete QR code response")
            .send_success()
            .await
    }
}

fn validate_message(message: &str) -> Result<()> {
    let len = message.chars().count();
    if len == 0 {
        return Err(ValidationError::new("prefilled_message", "must not be empty").into());
    }
    if len > PREFILLED_MESSAGE_MAX_CHARS {
        return Err(ValidationError::new(
            "prefilled_message",
            format!("must be at most {PREFILLED_MESSAGE_MAX_CHARS} characters, got {len}"),
        )
        .into());
    }
    Ok(())
}

/// Meta rejects anything but a 14-character alphanumeric id. Checking it
/// locally also keeps a stray `/` from retargeting the request path.
fn validate_code(field: &str, code: &QrCodeId) -> Result<()> {
    let s = code.as_str();
    if s.len() != QR_CODE_ID_LEN || !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(ValidationError::new(
            field,
            format!("must be {QR_CODE_ID_LEN} ASCII letters or digits"),
        )
        .into());
    }
    Ok(())
}

fn reject_cursors(after: Option<&str>, before: Option<&str>) -> Result<()> {
    if after.is_some() {
        return Err(ValidationError::new("after", "streams manage cursors; leave it unset").into());
    }
    if before.is_some() {
        return Err(
            ValidationError::new("before", "streams manage cursors; leave it unset").into(),
        );
    }
    Ok(())
}

#[derive(Deserialize)]
struct DataList<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

/// Image format for a QR code (`generate_qr_image`, and the
/// `qr_image_url.format(…)` field selector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum QrImageFormat {
    /// `PNG`.
    Png,
    /// `SVG` — Meta's recommendation for print.
    Svg,
}

impl QrImageFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Svg => "SVG",
        }
    }
}

/// Body of [`QrCodes::create`] (`CreateQrCodeRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateQrCode {
    /// Text prefilled in the user's composer, 1–140 characters.
    pub prefilled_message: String,
    /// Ask Meta to render an image and return its `qr_image_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate_qr_image: Option<QrImageFormat>,
}

impl CreateQrCode {
    /// A code with `prefilled_message` and no image.
    pub fn new(prefilled_message: impl Into<String>) -> Self {
        Self {
            prefilled_message: prefilled_message.into(),
            generate_qr_image: None,
        }
    }

    /// Also render an image in `format`.
    #[must_use]
    pub fn with_image(mut self, format: QrImageFormat) -> Self {
        self.generate_qr_image = Some(format);
        self
    }
}

/// Body of [`QrCodes::update`] (`UpdateQrCodeRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateQrCode {
    /// Code to change.
    pub code: QrCodeId,
    /// New prefilled message, 1–140 characters.
    pub prefilled_message: String,
}

/// Which optional fields [`QrCodes::get`] and [`QrCodes::list`] request.
///
/// `code`, `prefilled_message` and `deep_link_url` are always returned. With
/// the default (nothing optional) no `fields` parameter is sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QrCodeFields {
    /// Request `qr_image_url.format(<FORMAT>)`.
    pub image_format: Option<QrImageFormat>,
    /// Request `creation_time` (Meta returns it to first-party apps only).
    pub creation_time: bool,
}

impl QrCodeFields {
    fn to_param(self) -> Option<String> {
        if self.image_format.is_none() && !self.creation_time {
            return None;
        }
        let mut fields = String::from("code,prefilled_message,deep_link_url");
        if self.creation_time {
            fields.push_str(",creation_time");
        }
        if let Some(format) = self.image_format {
            fields.push_str(",qr_image_url.format(");
            fields.push_str(format.as_str());
            fields.push(')');
        }
        Some(fields)
    }
}

/// Query of [`QrCodes::list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQrCodes {
    /// Optional fields to include.
    pub fields: QrCodeFields,
    /// Only return this code (if it exists).
    pub code: Option<QrCodeId>,
    /// Page size, 1–25.
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

/// A QR code and short link (`QrCodeDetails` / `QrCodeResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QrCode {
    /// The 14-character id; use it to update or delete the code.
    pub code: QrCodeId,
    /// Text prefilled in the user's composer.
    pub prefilled_message: String,
    /// `https://wa.me/message/<code>` short link.
    pub deep_link_url: String,
    /// Image download URL, when an image format was requested.
    #[serde(default)]
    pub qr_image_url: Option<String>,
    /// Creation time as Meta sends it (unix seconds per the reference, typed
    /// `string` there); first-party apps only.
    #[serde(default)]
    pub creation_time: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::StreamExt;
    use http::Method;
    use serde_json::json;
    use wa_core::ErrorKind;
    use wa_core::error::TransportError;
    use wa_core::testing::ScriptedTransport;

    use super::*;
    use crate::RetryPolicy;

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn code() -> QrCodeId {
        QrCodeId::new("4O4YGZEG3RIVE1")
    }

    fn validation_field(err: &Error) -> &str {
        match err {
            Error::Validation(v) => &v.field,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_matches_docs_example() {
        let t = ScriptedTransport::new();
        // qr-codes "Create QR code" example response.
        t.push_json(
            200,
            json!({
                "code": "4O4YGZEG3RIVE1",
                "prefilled_message": "Cyber Monday 1",
                "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1",
                "qr_image_url": "https://scontent-iad3-2.xx.fbcdn.net/..."
            }),
        );
        let qr = client(&t)
            .qr_codes("106540352242922")
            .create(&CreateQrCode::new("Cyber Monday").with_image(QrImageFormat::Svg))
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/106540352242922/message_qrdls");
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(req.url.query(), None);
        assert_eq!(
            req.json(),
            Some(json!({"prefilled_message": "Cyber Monday", "generate_qr_image": "SVG"}))
        );
        assert_eq!(qr.code, code());
        assert_eq!(qr.prefilled_message, "Cyber Monday 1");
        assert_eq!(qr.deep_link_url, "https://wa.me/message/4O4YGZEG3RIVE1");
        assert_eq!(
            qr.qr_image_url.as_deref(),
            Some("https://scontent-iad3-2.xx.fbcdn.net/...")
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn create_without_image_omits_generate_qr_image() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"code": "4O4YGZEG3RIVE1", "prefilled_message": "Hi", "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1"}),
        );
        client(&t)
            .qr_codes("1")
            .create(&CreateQrCode::new("Hi").with_image(QrImageFormat::Png))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({"prefilled_message": "Hi", "generate_qr_image": "PNG"}))
        );
        t.push_json(
            200,
            json!({"code": "4O4YGZEG3RIVE1", "prefilled_message": "Hi", "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1"}),
        );
        let qr = client(&t)
            .qr_codes("1")
            .create(&CreateQrCode::new("Hi"))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({"prefilled_message": "Hi"}))
        );
        assert_eq!(qr.qr_image_url, None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_matches_docs_example_without_image_url() {
        let t = ScriptedTransport::new();
        // qr-codes "Update a QR code" example response: no qr_image_url.
        t.push_json(
            200,
            json!({
                "code": "4O4YGZEG3RIVE1",
                "prefilled_message": "Cyber Tuesday",
                "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1"
            }),
        );
        let qr = client(&t)
            .qr_codes("106540352242922")
            .update(&UpdateQrCode {
                code: code(),
                prefilled_message: "Cyber Tuesday".into(),
            })
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/106540352242922/message_qrdls");
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({"code": "4O4YGZEG3RIVE1", "prefilled_message": "Cyber Tuesday"}))
        );
        assert_eq!(qr.prefilled_message, "Cyber Tuesday");
        assert_eq!(qr.qr_image_url, None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_is_replayed_but_create_is_not() {
        let retrying = |t: &ScriptedTransport| {
            Client::builder()
                .transport(t.clone())
                .access_token("TOKEN")
                .retry(RetryPolicy {
                    max_retries: 1,
                    base_delay: Duration::ZERO,
                    max_delay: Duration::ZERO,
                })
                .build()
                .unwrap()
        };
        let body =
            json!({"code": "4O4YGZEG3RIVE1", "prefilled_message": "x", "deep_link_url": "u"});

        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        t.push_json(200, body.clone());
        retrying(&t)
            .qr_codes("1")
            .update(&UpdateQrCode {
                code: code(),
                prefilled_message: "x".into(),
            })
            .await
            .unwrap();
        assert_eq!(t.requests().len(), 2);
        assert_eq!(t.remaining(), 0);

        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let err = retrying(&t)
            .qr_codes("1")
            .create(&CreateQrCode::new("x"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Transport(TransportError::Timeout)));
        assert_eq!(t.requests().len(), 1, "a create is never replayed");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn get_unwraps_data_and_requests_image_field() {
        let t = ScriptedTransport::new();
        // qr-codes "Get a QR code" example response (+ the requested image).
        t.push_json(
            200,
            json!({
                "data": [{
                    "code": "4O4YGZEG3RIVE1",
                    "prefilled_message": "Cyber Monday",
                    "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1",
                    "qr_image_url": "https://scontent.xx.fbcdn.net/q.png"
                }]
            }),
        );
        let qr = client(&t)
            .qr_codes("106540352242922")
            .get(
                &code(),
                &QrCodeFields {
                    image_format: Some(QrImageFormat::Png),
                    creation_time: true,
                },
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/106540352242922/message_qrdls/4O4YGZEG3RIVE1"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.query("fields").as_deref(),
            Some("code,prefilled_message,deep_link_url,creation_time,qr_image_url.format(PNG)")
        );
        assert_eq!(qr.code, code());
        assert_eq!(qr.prefilled_message, "Cyber Monday");
        assert_eq!(
            qr.qr_image_url.as_deref(),
            Some("https://scontent.xx.fbcdn.net/q.png")
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn get_with_default_fields_sends_no_query_and_empty_data_fails() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": []}));
        let err = client(&t)
            .qr_codes("1")
            .get(&code(), &QrCodeFields::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Decode { .. }), "{err:?}");
        assert_eq!(t.last_request().unwrap().url.query(), None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_matches_docs_example() {
        let t = ScriptedTransport::new();
        // qr-codes "Get a list of QR codes" example response.
        t.push_json(
            200,
            json!({
                "data": [
                    {"code": "4O4YGZEG3RIVE1", "prefilled_message": "Cyber Monday", "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1"},
                    {"code": "WOMVT6TJ2BP7A1", "prefilled_message": "Tell me more about your production workshop", "deep_link_url": "https://wa.me/message/WOMVT6TJ2BP7A1"}
                ]
            }),
        );
        let page = client(&t)
            .qr_codes("106540352242922")
            .list(&ListQrCodes {
                code: Some(code()),
                limit: Some(25),
                after: Some("A".into()),
                before: Some("B".into()),
                ..ListQrCodes::default()
            })
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922/message_qrdls");
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.url.query(),
            Some("code=4O4YGZEG3RIVE1&limit=25&after=A&before=B")
        );
        assert_eq!(page.data.len(), 2);
        assert_eq!(page.data[1].code, QrCodeId::new("WOMVT6TJ2BP7A1"));
        assert_eq!(
            page.data[1].prefilled_message,
            "Tell me more about your production workshop"
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_stream_follows_cursors() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [{"code": "4O4YGZEG3RIVE1", "prefilled_message": "a", "deep_link_url": "u1"}],
                "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/v25.0/1/message_qrdls?after=c1"}
            }),
        );
        t.push_json(
            200,
            json!({"data": [{"code": "WOMVT6TJ2BP7A1", "prefilled_message": "b", "deep_link_url": "u2"}]}),
        );
        let codes: Vec<QrCode> = client(&t)
            .qr_codes("1")
            .list_stream(&ListQrCodes {
                limit: Some(1),
                ..ListQrCodes::default()
            })
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(codes.len(), 2);
        let reqs = t.requests();
        assert_eq!(reqs[0].url.query(), Some("limit=1"));
        assert_eq!(reqs[1].url.query(), Some("limit=1&after=c1"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn delete_matches_docs_example() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .qr_codes("106540352242922")
            .delete(&code())
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::DELETE);
        assert_eq!(
            req.path(),
            "/v25.0/106540352242922/message_qrdls/4O4YGZEG3RIVE1"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(req.json(), None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn not_found_error_is_decoded() {
        let t = ScriptedTransport::new();
        // ...qr-code-api 404 example.
        t.push_json(
            404,
            json!({"error": {"message": "QR code not found", "type": "GraphMethodException", "code": 803}}),
        );
        let err = client(&t).qr_codes("1").delete(&code()).await.unwrap_err();
        assert_eq!(err.graph().map(|g| g.code), Some(803));
        assert_eq!(err.kind(), ErrorKind::Unknown);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn prefilled_message_over_140_chars_is_rejected_locally() {
        let t = ScriptedTransport::new();
        let qr = client(&t).qr_codes("1");
        // 140 multi-byte characters are fine: the limit is characters.
        let ok = "é".repeat(140);
        assert!(validate_message(&ok).is_ok());
        let err = qr
            .create(&CreateQrCode::new("a".repeat(141)))
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "prefilled_message");
        let err = qr
            .update(&UpdateQrCode {
                code: code(),
                prefilled_message: "a".repeat(141),
            })
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "prefilled_message");
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn empty_prefilled_message_is_rejected_locally() {
        let t = ScriptedTransport::new();
        let err = client(&t)
            .qr_codes("1")
            .create(&CreateQrCode::new(""))
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "prefilled_message");
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn malformed_code_is_rejected_before_it_reaches_the_path() {
        let t = ScriptedTransport::new();
        let qr = client(&t).qr_codes("1");
        for bad in ["4O4YGZEG3RIVE", "4O4YGZEG3RIVE12", "4O4YGZEG3RIV/1", ""] {
            let err = qr.delete(&QrCodeId::new(bad)).await.unwrap_err();
            assert_eq!(validation_field(&err), "code", "{bad:?}");
            let err = qr
                .get(&QrCodeId::new(bad), &QrCodeFields::default())
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "code", "{bad:?}");
        }
        let err = qr
            .update(&UpdateQrCode {
                code: QrCodeId::new("short"),
                prefilled_message: "x".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "code");
        let err = qr
            .list(&ListQrCodes {
                code: Some(QrCodeId::new("short")),
                ..ListQrCodes::default()
            })
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "code");
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn list_limit_outside_1_to_25_is_rejected_locally() {
        let t = ScriptedTransport::new();
        let qr = client(&t).qr_codes("1");
        for bad in [0, 26] {
            let err = qr
                .list(&ListQrCodes {
                    limit: Some(bad),
                    ..ListQrCodes::default()
                })
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "limit", "{bad}");
        }
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn list_stream_refuses_caller_cursors() {
        let t = ScriptedTransport::new();
        let qr = client(&t).qr_codes("1");
        let items: Vec<Result<QrCode>> = qr
            .list_stream(&ListQrCodes {
                after: Some("c".into()),
                ..ListQrCodes::default()
            })
            .collect()
            .await;
        assert_eq!(items.len(), 1);
        assert_eq!(validation_field(items[0].as_ref().unwrap_err()), "after");
        let items: Vec<Result<QrCode>> = qr
            .list_stream(&ListQrCodes {
                before: Some("c".into()),
                ..ListQrCodes::default()
            })
            .collect()
            .await;
        assert_eq!(validation_field(items[0].as_ref().unwrap_err()), "before");
        assert!(t.requests().is_empty());
    }
}
