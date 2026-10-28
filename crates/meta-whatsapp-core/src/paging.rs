//! Graph API cursor pagination.

use serde::{Deserialize, Serialize};

/// A page of results: `{"data": [...], "paging": {...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items on this page.
    #[serde(default = "Vec::new")]
    pub data: Vec<T>,
    /// Cursors and links, absent on single-page results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paging: Option<Paging>,
    /// Some edges (e.g. `message_templates` with `summary=total_count`)
    /// return a summary object next to `data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<serde_json::Value>,
}

impl<T> Page<T> {
    /// The `after` cursor, if there is a next page.
    pub fn next_cursor(&self) -> Option<&str> {
        let paging = self.paging.as_ref()?;
        paging.next.as_ref()?;
        paging.cursors.as_ref()?.after.as_deref()
    }
}

/// `paging` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paging {
    /// Cursors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursors: Option<Cursors>,
    /// Absolute URL of the next page; absent on the last page.
    ///
    /// Meta's link can embed the request's credentials; deserializing
    /// removes its `access_token` and `appsecret_proof` query parameters
    /// (the rest of the URL, `after` and `limit` among it, is kept), so
    /// neither `Debug` nor `Serialize` shows them. Nothing in the library
    /// follows the link: pagination re-issues the request with `after`.
    #[serde(
        default,
        deserialize_with = "without_credentials",
        skip_serializing_if = "Option::is_none"
    )]
    pub next: Option<String>,
    /// Absolute URL of the previous page, its `access_token` and
    /// `appsecret_proof` query parameters removed on deserialize, as for
    /// [`next`](Self::next).
    #[serde(
        default,
        deserialize_with = "without_credentials",
        skip_serializing_if = "Option::is_none"
    )]
    pub previous: Option<String>,
}

/// The query parameters [`Paging`]'s links lose on deserialize.
const CREDENTIAL_PARAMS: [&str; 2] = ["access_token", "appsecret_proof"];

fn without_credentials<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.map(|link| strip_credentials(&link)))
}

/// `link` without its credential query parameters. A string scan rather
/// than a URL parse, so a link that is not an absolute URL is cleaned too;
/// a parameter's name is compared percent-decoded and ignoring ASCII case.
fn strip_credentials(link: &str) -> String {
    let Some((base, rest)) = link.split_once('?') else {
        return link.to_owned();
    };
    let (query, fragment) = match rest.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (rest, None),
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split_once('=').map_or(*pair, |(name, _)| name);
            let name: String = url::form_urlencoded::parse(name.as_bytes())
                .map(|(decoded, _)| decoded.into_owned())
                .collect();
            !CREDENTIAL_PARAMS
                .iter()
                .any(|credential| name.eq_ignore_ascii_case(credential))
        })
        .collect();
    let mut out = base.to_owned();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// `paging.cursors` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursors {
    /// Cursor to the page before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// Cursor to the page after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_cursor_requires_a_next_link() {
        let last: Page<u8> =
            serde_json::from_str(r#"{"data":[1],"paging":{"cursors":{"before":"a","after":"b"}}}"#)
                .unwrap();
        assert_eq!(last.next_cursor(), None);
        let more: Page<u8> = serde_json::from_str(
            r#"{"data":[1],"paging":{"cursors":{"after":"b"},"next":"https://graph.facebook.com/x"}}"#,
        )
        .unwrap();
        assert_eq!(more.next_cursor(), Some("b"));
        let empty: Page<u8> = serde_json::from_str("{}").unwrap();
        assert!(empty.data.is_empty());
    }

    #[test]
    fn links_lose_their_credentials_on_deserialize() {
        let page: Page<u8> = serde_json::from_str(
            r#"{"data":[1],"paging":{"cursors":{"after":"c1"},
              "next":"https://graph.facebook.com/v25.0/1/x?access_token=LEAKED&appsecret_proof=X&after=c1&limit=2",
              "previous":"https://graph.facebook.com/v25.0/1/x?before=c0&ACCESS%5FTOKEN=LEAKED#top"}}"#,
        )
        .unwrap();
        let paging = page.paging.as_ref().unwrap();
        assert_eq!(
            paging.next.as_deref(),
            Some("https://graph.facebook.com/v25.0/1/x?after=c1&limit=2")
        );
        assert_eq!(
            paging.previous.as_deref(),
            Some("https://graph.facebook.com/v25.0/1/x?before=c0#top")
        );
        assert_eq!(page.next_cursor(), Some("c1"));
        let shown = format!("{page:?} {}", serde_json::to_string(&page).unwrap());
        assert!(!shown.contains("LEAKED"), "{shown}");
        assert!(!shown.contains("appsecret_proof"), "{shown}");
        assert!(shown.contains("after=c1"), "{shown}");
    }

    #[test]
    fn a_link_with_only_credentials_keeps_its_path() {
        assert_eq!(
            strip_credentials("https://graph.facebook.com/v25.0/1/x?access_token=LEAKED"),
            "https://graph.facebook.com/v25.0/1/x"
        );
        assert_eq!(
            strip_credentials("https://h/x?after=a"),
            "https://h/x?after=a"
        );
        assert_eq!(strip_credentials("https://h/x"), "https://h/x");
        // Not a credential: only exact names are removed.
        assert_eq!(
            strip_credentials("https://h/x?access_token_type=a&my_access_token=b"),
            "https://h/x?access_token_type=a&my_access_token=b"
        );
    }
}
