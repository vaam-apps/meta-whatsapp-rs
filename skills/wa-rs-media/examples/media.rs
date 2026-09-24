//! Reference code for the `wa-rs-media` skill: upload, SHA-256-verified
//! download (buffered or streamed), and Resumable Upload handles for
//! template headers.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`).

use futures::StreamExt;
use wa_rs::client::media::{DownloadedMedia, validate_upload};
use wa_rs::client::messages::{Image, OutboundMessage};
use wa_rs::core::ids::{AppId, MediaId, UploadHandle};
use wa_rs::prelude::*;

/// Upload a PNG and send it as an image message.
pub async fn upload_and_send(
    client: &Client,
    phone_number_id: PhoneNumberId,
    to: Recipient,
    png: Vec<u8>,
) -> wa_rs::Result<MediaId> {
    let media = client.media(phone_number_id.clone());
    let id = media.upload(png, "image/png", "voucher.png").await?; // type and size checked first
    let image = Image::new(id.clone()).caption("Your voucher");
    client
        .messages(phone_number_id)
        .send(&OutboundMessage::new(to, image))
        .await?;
    Ok(id) // lives 30 days
}

/// A customer's photo from a webhook, buffered, capped and verified.
pub async fn download_small(
    client: &Client,
    phone_number_id: PhoneNumberId,
    inbound_media: &MediaId, // MediaContent::id of the webhook message
) -> wa_rs::Result<DownloadedMedia> {
    let media = client.media(phone_number_id);
    media.download_bytes(inbound_media, 5 * 1024 * 1024).await // refused over 5 MiB
}

/// A large file, streamed: trust the bytes only once the stream ended
/// without an error (the last item is the hash check).
pub async fn download_large(
    client: &Client,
    phone_number_id: PhoneNumberId,
    inbound_media: &MediaId,
) -> wa_rs::Result<Vec<u8>> {
    let download = client
        .media(phone_number_id)
        .download(inbound_media)
        .await?;
    let mut verified = download.verified()?; // fails fast on a malformed digest
    let mut file = Vec::new(); // stand-in for a temporary file
    while let Some(chunk) = verified.body.next().await {
        file.extend_from_slice(&chunk?); // a mismatch: Error::Transport(Integrity)
    }
    Ok(file) // only now is it the file Meta described
}

/// A template header example is a Resumable Upload handle, not a media id.
pub async fn header_handle(
    client: &Client, // needs a token: the upload authenticates with `OAuth`
    phone_number_id: PhoneNumberId,
    app_id: &AppId,
    png: Vec<u8>,
) -> wa_rs::Result<UploadHandle> {
    let media = client.media(phone_number_id);
    media
        .resumable_upload(app_id, "header.png", "image/png", png)
        .await
}

/// What Meta accepts, before any request: the table in `validate_upload`.
pub fn acceptable(mime_type: &str, len: u64) -> bool {
    validate_upload(mime_type, len).is_ok() // image/webp only as a sticker
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;
    use sha2::{Digest, Sha256};
    use wa_rs::core::testing::ScriptedTransport;

    use super::*;

    const NUMBER: &str = "106540352242922";

    fn client(transport: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn upload_is_multipart_then_the_id_is_sent() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"id": "1037543291543636"}));
        transport.push_json(200, json!({"messages": [{"id": "wamid.OUT"}]}));
        let png = vec![0x89, b'P', b'N', b'G'];
        let id = upload_and_send(
            &client(&transport),
            NUMBER.into(),
            Recipient::phone("+16505551234"),
            png,
        )
        .await
        .unwrap();
        assert_eq!(id.as_str(), "1037543291543636");
        let requests = transport.requests();
        assert_eq!(requests[0].path(), "/v25.0/106540352242922/media");
        let (_, _, kind) = requests[0].multipart_field("type").unwrap();
        assert_eq!(&kind[..], b"image/png");
        assert_eq!(
            requests[1].json().unwrap()["image"]["id"],
            "1037543291543636"
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn download_is_verified_and_only_goes_to_the_media_host() {
        let body = b"photo bytes".to_vec();
        let sha = hex_sha256(&body);
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({
                "url": "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1",
                "mime_type": "image/jpeg", "sha256": sha, "file_size": body.len(), "id": "1"
            }),
        );
        transport.push_bytes(200, "image/jpeg", body.clone());
        let got = download_small(&client(&transport), NUMBER.into(), &"1".into())
            .await
            .unwrap();
        assert_eq!(got.data.as_ref(), body.as_slice());
        assert_eq!(transport.last_request().unwrap().bearer(), Some("TOKEN"));

        // A URL on any other host is refused before the token leaves.
        transport.push_json(
            200,
            json!({
                "url": "https://media.example.net/x", "mime_type": "image/jpeg",
                "sha256": sha, "id": "2"
            }),
        );
        let refused = download_small(&client(&transport), NUMBER.into(), &"2".into())
            .await
            .unwrap_err();
        assert!(
            matches!(&refused, Error::Validation(v) if v.field == "url"),
            "{refused}"
        );
        assert_eq!(transport.requests().len(), 3); // no request to media.example.net
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn a_tampered_stream_fails_at_the_end() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({
                "url": "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1",
                "mime_type": "image/jpeg", "sha256": hex_sha256(b"original"), "id": "1"
            }),
        );
        transport.push_bytes(200, "image/jpeg", b"tampered".to_vec());
        let err = download_large(&client(&transport), NUMBER.into(), &"1".into())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Transport(_)), "{err}");
        assert!(err.is_retryable()); // fetch a fresh URL and try again
    }

    #[tokio::test]
    async fn resumable_upload_returns_a_handle() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"id": "upload:MTphdHRhY2htZW50"}));
        transport.push_json(200, json!({"h": "4::aW1hZ2UvcG5n:ARb"}));
        let handle = header_handle(
            &client(&transport),
            NUMBER.into(),
            &"1234".into(),
            vec![1, 2, 3],
        )
        .await
        .unwrap();
        assert_eq!(handle.as_str(), "4::aW1hZ2UvcG5n:ARb");
        let requests = transport.requests();
        assert_eq!(requests[0].path(), "/v25.0/1234/uploads");
        assert_eq!(requests[1].header("authorization"), Some("OAuth TOKEN"));
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn the_supported_types_table() {
        assert!(acceptable("image/png", 1024));
        assert!(!acceptable("image/png", 6 * 1024 * 1024)); // images: 5 MB
        assert!(acceptable("application/pdf", 50 * 1024 * 1024)); // documents: 100 MB
        assert!(!acceptable("image/gif", 10));
    }

    fn hex_sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut hex, b| {
                write!(hex, "{b:02x}").unwrap();
                hex
            })
    }
}
