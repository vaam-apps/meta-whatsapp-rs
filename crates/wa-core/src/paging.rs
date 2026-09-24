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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Absolute URL of the previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
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
}
