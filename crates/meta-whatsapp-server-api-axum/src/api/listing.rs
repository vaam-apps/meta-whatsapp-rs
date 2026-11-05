//! What the roadmap M5g routes share: an opaque cursor read back to
//! Meta's, and a comma-separated list in a query.

use super::common::decode_cursor;
use crate::error::ApiError;

/// Meta's cursor behind the caller's opaque `cursor` (the hex of what
/// Meta gave, as every list of the service writes it), else `422
/// invalid_request` on `cursor`.
pub(super) fn after(cursor: Option<&str>) -> Result<Option<String>, ApiError> {
    match cursor {
        None => Ok(None),
        Some(cursor) => decode_cursor(cursor)
            .filter(|after| !after.is_empty())
            .map(Some)
            .ok_or_else(|| ApiError::invalid("cursor")),
    }
}

/// The values of a comma-separated query parameter: none for an absent or
/// empty one. A value is not trimmed: the core checks each against the
/// values Meta documents.
pub(super) fn csv(value: Option<&str>) -> Vec<String> {
    match value {
        None | Some("") => Vec::new(),
        Some(list) => list.split(',').map(str::to_owned).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cursor is the hex of Meta's; a list is split on commas, an empty
    /// one is none, a hole is kept for the core to refuse.
    #[test]
    fn a_cursor_is_hex_and_a_list_is_split_on_commas() {
        assert_eq!(after(None).unwrap(), None);
        assert_eq!(after(Some("4d6574")).unwrap().as_deref(), Some("Met"));
        for bad in ["", "zz", "4d65740"] {
            assert_eq!(
                after(Some(bad)).unwrap_err().field(),
                Some("cursor"),
                "{bad:?}"
            );
        }
        assert_eq!(csv(None), Vec::<String>::new());
        assert_eq!(csv(Some("")), Vec::<String>::new());
        assert_eq!(csv(Some("a,b")), ["a", "b"]);
        assert_eq!(csv(Some("a,,b")), ["a", "", "b"]);
    }
}
