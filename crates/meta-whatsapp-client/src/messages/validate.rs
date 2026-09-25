//! Counting helpers for the hard limits Meta documents.
//!
//! This module only counts; every call site names the doc page the limit
//! comes from, so a reviewer can check the number against the source.

use meta_whatsapp_core::error::ValidationError;

/// Result of a single local check.
pub(crate) type Check = Result<(), ValidationError>;

/// Meta states text limits in "characters" without saying which unit. We
/// count Unicode scalar values (`char`s): it never under-counts ASCII, and a
/// byte count would reject valid non-Latin text far too early.
pub(crate) fn char_len(value: &str) -> usize {
    value.chars().count()
}

/// A documented "Required" string: present and not empty.
pub(crate) fn non_empty(field: &str, value: &str) -> Check {
    if value.is_empty() {
        return Err(ValidationError::new(
            field,
            "is required and must not be empty",
        ));
    }
    Ok(())
}

/// At most `max` characters.
pub(crate) fn max_chars(field: &str, value: &str, max: usize) -> Check {
    let n = char_len(value);
    if n > max {
        return Err(ValidationError::new(
            field,
            format!("has {n} characters; the documented maximum is {max}"),
        ));
    }
    Ok(())
}

/// Required text with a maximum length.
pub(crate) fn text(field: &str, value: &str, max: usize) -> Check {
    non_empty(field, value)?;
    max_chars(field, value, max)
}

/// Optional text: when present it must satisfy [`text`] ("required if using
/// a footer" etc.).
pub(crate) fn opt_text(field: &str, value: Option<&str>, max: usize) -> Check {
    value.map_or(Ok(()), |v| text(field, v, max))
}

/// Between `min` and `max` entries, inclusive.
pub(crate) fn count(field: &str, n: usize, min: usize, max: usize) -> Check {
    if n < min || n > max {
        return Err(ValidationError::new(
            field,
            format!("has {n} entries; the documented range is {min}..={max}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_characters_not_bytes() {
        // 4 characters, 8 bytes.
        assert!(max_chars("f", "éééé", 4).is_ok());
        assert!(max_chars("f", "ééééé", 4).is_err());
    }

    #[test]
    fn boundaries_are_inclusive() {
        assert!(count("f", 1, 1, 3).is_ok());
        assert!(count("f", 3, 1, 3).is_ok());
        assert!(count("f", 0, 1, 3).is_err());
        assert!(count("f", 4, 1, 3).is_err());
        assert_eq!(text("a.b", "", 3).unwrap_err().field, "a.b");
        assert!(opt_text("f", None, 1).is_ok());
        assert!(opt_text("f", Some(""), 1).is_err());
    }
}
