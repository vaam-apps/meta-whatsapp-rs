//! Signed links in webhook bodies never reach `Debug`.
//!
//! Meta signs the media download URL (`lookaside.fbsbx.com/…?mid=…&hash=…`)
//! and its CDN links (`scontent.xx.fbcdn.net/…?oh=…&oe=…`) in their query.
//! Every fixture is re-read with a sentinel signature appended to each
//! such link: the payload and its events must not show it in `{:?}` or
//! `{:#?}`, and must still serialize it whole (the data is the
//! integrator's, only `Debug` hides it).

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use meta_whatsapp_webhooks::WebhookPayload;
use meta_whatsapp_webhooks::fields::{MediaContent, MessageContent, Referral};
use serde_json::Value;

/// A signature no fixture carries otherwise.
const SENTINEL: &str = "S1GN4TUR3-s3nt1nel";

/// Objects whose `url` is a signed media link ([`MediaContent`]): the five
/// media message types, and the call recording's `audio` and transcript's
/// `document`.
const MEDIA_OBJECTS: [&str; 5] = ["image", "audio", "video", "document", "sticker"];

/// [`Referral`]'s CDN links.
const CDN_LINKS: [&str; 3] = ["image_url", "video_url", "thumbnail_url"];

fn sign(link: &str) -> String {
    let joint = if link.contains('?') { '&' } else { '?' };
    format!("{link}{joint}oh=00_{SENTINEL}&oe=6A1B2C3D")
}

/// Append the sentinel signature to every signed link in `value`; return
/// the keys it was found under (`image.url` → `image`).
fn sign_links(value: &mut Value, parent: Option<&str>, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let signed = (key == "url" && parent.is_some_and(|p| MEDIA_OBJECTS.contains(&p)))
                    || CDN_LINKS.contains(&key.as_str());
                if signed && let Value::String(link) = child {
                    *link = sign(link);
                    found.push(if key == "url" {
                        parent.unwrap_or_default().to_owned()
                    } else {
                        key.clone()
                    });
                } else {
                    sign_links(child, Some(key), found);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                sign_links(item, parent, found);
            }
        }
        _ => {}
    }
}

#[test]
fn no_signed_link_reaches_debug_of_a_payload_or_its_events() {
    let mut covered = BTreeSet::new();
    let mut signed_fixtures = 0;
    for name in common::all_fixtures() {
        let mut json: Value = serde_json::from_slice(&common::fixture_bytes(&name)).unwrap();
        let mut found = Vec::new();
        sign_links(&mut json, None, &mut found);
        if found.is_empty() {
            continue;
        }
        signed_fixtures += 1;
        let body = serde_json::to_vec(&json).unwrap();
        let payload = WebhookPayload::from_slice(&body).unwrap();

        for shown in [format!("{payload:?}"), format!("{payload:#?}")] {
            assert!(!shown.contains(SENTINEL), "{name}: {shown}");
            assert!(shown.contains("oh=<redacted>"), "{name}: {shown}");
        }
        // The links are kept whole: serializing writes every one back.
        let written = serde_json::to_string(&payload).unwrap();
        assert_eq!(written.matches(SENTINEL).count(), found.len(), "{name}");

        let events = payload.into_events();
        let mut in_events = 0;
        for event in &events {
            for shown in [format!("{event:?}"), format!("{event:#?}")] {
                assert!(!shown.contains(SENTINEL), "{name}: {shown}");
            }
            in_events += serde_json::to_string(event)
                .unwrap()
                .matches(SENTINEL)
                .count();
        }
        assert!(in_events >= 1, "{name}: no event carries the link");
        covered.extend(found);
    }
    // Every kind of signed link is exercised by at least one fixture.
    let expected: BTreeSet<String> = MEDIA_OBJECTS
        .iter()
        .chain(&CDN_LINKS)
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(covered, expected);
    assert!(signed_fixtures >= 10, "only {signed_fixtures} fixtures");
}

#[test]
fn media_content_debug_shows_the_link_redacted_and_keeps_it_whole() {
    let url = sign("https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=133");
    let media = MediaContent {
        url: Some(url.clone()),
        caption: Some("Taj Mahal".into()),
        ..MediaContent::default()
    };
    let shown = format!("{media:?}");
    assert!(
        shown.contains(
            r#"url: Some("https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=<redacted>&oh=<redacted>&oe=<redacted>")"#
        ),
        "{shown}"
    );
    // Every other field is shown as the derived `Debug` showed it.
    assert!(
        shown.starts_with("MediaContent { id: None, mime_type: None, sha256: None, url: Some(")
    );
    assert!(shown.ends_with(
        r#", caption: Some("Taj Mahal"), filename: None, voice: None, animated: None }"#
    ));
    for content in [
        MessageContent::Image(media.clone()),
        MessageContent::Audio(media.clone()),
        MessageContent::Video(media.clone()),
        MessageContent::Document(media.clone()),
        MessageContent::Sticker(media.clone()),
    ] {
        for shown in [format!("{content:?}"), format!("{content:#?}")] {
            assert!(!shown.contains(SENTINEL), "{shown}");
        }
    }
    assert_eq!(media.url.as_deref(), Some(url.as_str()));
    assert_eq!(serde_json::to_value(&media).unwrap()["url"], url);

    // A link that does not parse is hidden whole; no link shows `None`.
    let odd = MediaContent {
        url: Some(format!("?hash={SENTINEL}")),
        ..MediaContent::default()
    };
    assert!(format!("{odd:?}").contains("url: Some(<redacted: not a URL>)"));
    assert!(format!("{:?}", MediaContent::default()).contains("url: None"));
}

#[test]
fn referral_debug_redacts_the_cdn_links_only() {
    let referral = Referral {
        source_url: Some("https://fb.me/3cr4Wqqkv".into()),
        image_url: Some(sign("https://scontent.xx.fbcdn.net/v/t45.1/img.jpg")),
        video_url: Some(sign("https://scontent.xx.fbcdn.net/v/t42.1/vid.mp4")),
        thumbnail_url: Some(sign("https://scontent.xx.fbcdn.net/v/t15.1/thumb.jpg")),
        ctwa_clid: Some("ARAkLkA8rmlFeiCktEJQ".into()),
        ..Referral::default()
    };
    for shown in [format!("{referral:?}"), format!("{referral:#?}")] {
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert!(shown.contains("https://fb.me/3cr4Wqqkv"), "{shown}");
        assert!(shown.contains("ARAkLkA8rmlFeiCktEJQ"), "{shown}");
        assert!(
            shown.contains(
                "https://scontent.xx.fbcdn.net/v/t45.1/img.jpg?oh=<redacted>&oe=<redacted>"
            ),
            "{shown}"
        );
    }
    let written = serde_json::to_string(&referral).unwrap();
    assert_eq!(written.matches(SENTINEL).count(), 3);
}
