//! [`TablePrefix`], shared by the SQL adapters (features `postgres` and
//! `sqlite`).

use std::borrow::Cow;
use std::fmt;

use meta_whatsapp_core::error::ConfigError;

/// Longest accepted prefix. Postgres truncates identifiers at 63 bytes; the
/// longest name we derive (`…messages_conversation_idx`) adds 25. A test
/// checks every identifier in the migrations against this. SQLite has no
/// such limit; the same bound keeps one prefix valid on both.
pub(crate) const MAX_PREFIX_LEN: usize = 38;

/// A validated prefix for every table, index and sequence the SQL adapters
/// (Postgres and SQLite) create (`wa_` by default).
///
/// Validation (`[a-z_][a-z0-9_]*`, at most 38 bytes) is what makes it safe to
/// splice into SQL text: identifiers cannot be bound as parameters.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TablePrefix(Cow<'static, str>);

impl TablePrefix {
    /// `wa_`.
    ///
    /// Stable: predates the rename to meta-whatsapp-rs, never change it
    /// (docs/architecture.md § "Stable identifiers").
    pub const DEFAULT: Self = Self(Cow::Borrowed("wa_"));

    /// Validate a prefix. Lower-case ASCII letters, digits and `_`, not
    /// starting with a digit, 1 to 38 bytes. End it with `_` for readable
    /// table names (`tenant1_kv`).
    pub fn new(prefix: impl Into<String>) -> Result<Self, ConfigError> {
        let prefix = prefix.into();
        let valid_start = prefix
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
        let valid_rest = prefix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid_start || !valid_rest || prefix.len() > MAX_PREFIX_LEN {
            return Err(ConfigError::new(format!(
                "invalid table prefix `{prefix}`: expected [a-z_][a-z0-9_]*, at most {MAX_PREFIX_LEN} bytes"
            )));
        }
        Ok(Self(Cow::Owned(prefix)))
    }

    /// The prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `prefix` + `name`, for SQL text. Safe because the prefix is validated
    /// and `name` is always a literal in this crate.
    pub(crate) fn table(&self, name: &str) -> String {
        format!("{}{name}", self.0)
    }
}

impl Default for TablePrefix {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Debug for TablePrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TablePrefix({:?})", self.0)
    }
}

impl fmt::Display for TablePrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_validation() {
        assert_eq!(TablePrefix::default().as_str(), "wa_");
        assert_eq!(TablePrefix::new("tenant_1_").unwrap().as_str(), "tenant_1_");
        assert_eq!(TablePrefix::new("_x").unwrap().as_str(), "_x");
        for bad in [
            "",
            "1abc",
            "Wa_",
            "wa-",
            "wa_; drop table x; --",
            "wa\"",
            "wä_",
            &"a".repeat(MAX_PREFIX_LEN + 1),
        ] {
            assert!(TablePrefix::new(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(TablePrefix::new("a".repeat(MAX_PREFIX_LEN)).is_ok());
    }
}
