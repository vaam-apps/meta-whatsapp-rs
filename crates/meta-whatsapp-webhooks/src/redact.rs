//! Log-safe renderings of things derived from webhook bodies.
//!
//! Webhook bodies carry personal data (message text, names, phone numbers,
//! addresses), and serde's error messages quote the value they choke on:
//! `invalid type: string "Hi, I'm Jane, call me on +49 151…", expected u64`.
//! Everything this crate logs about a body goes through here, so logs say
//! *where* and *what kind* of thing went wrong, never *what the user wrote*.
//! The full, unredacted text stays available to the integrator on the event
//! itself ([`crate::Change::parse_error`], [`crate::WebhookEvent::Unparsed`]),
//! which is their data to handle.

use sha2::{Digest, Sha256};

/// Longest redacted message kept; serde's "expected one of …" lists can be
/// long and are not worth a multi-kilobyte log line.
const MAX_LEN: usize = 300;

/// Runs of this many digits outside quotes are treated as possible phone
/// numbers or ids and dropped. Line and column numbers stay readable.
const MAX_DIGITS: usize = 7;

/// `serde` / `serde_json` error text with every quoted value removed.
///
/// Serde quotes payload values in `"…"` (Rust `Debug` escaping, for
/// strings) or backticks (numbers, characters, unknown variants, and
/// `meta_whatsapp_core::timestamp`'s custom errors). Both are replaced by `…`, except
/// the identifier after `missing field ` / `duplicate field `, which names a
/// field of *our* schema and is the most useful part of the message. Long
/// digit runs outside quotes are dropped too.
pub(crate) fn serde_message(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(MAX_LEN));
    let mut rest = message;
    while let Some(c) = rest.chars().next() {
        match c {
            '"' => {
                rest = skip_debug_string(&rest[1..]);
                out.push_str("\"…\"");
            }
            '`' => {
                let inner_end = rest[1..].find('`').map_or(rest.len(), |i| i + 1);
                let inner = &rest[1..inner_end];
                let schema_name = (out.ends_with("missing field ")
                    || out.ends_with("duplicate field "))
                    && !inner.is_empty()
                    && inner
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
                if schema_name {
                    out.push('`');
                    out.push_str(inner);
                    out.push('`');
                } else {
                    out.push_str("`…`");
                }
                rest = rest.get(inner_end + 1..).unwrap_or("");
            }
            '0'..='9' => {
                let run = rest.bytes().take_while(u8::is_ascii_digit).count();
                if run >= MAX_DIGITS {
                    out.push('…');
                } else {
                    out.push_str(&rest[..run]);
                }
                rest = &rest[run..];
            }
            c => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        if out.len() >= MAX_LEN {
            out.push_str(" [truncated]");
            break;
        }
    }
    out
}

/// Skip a `Debug`-escaped string body up to and including its closing
/// quote; an unterminated one runs to the end.
fn skip_debug_string(s: &str) -> &str {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return &s[i + 1..],
            _ => {}
        }
    }
    ""
}

/// Hex SHA-256 of a body: lets an operator match a log line to a stored
/// body (or to Meta's retry of it) without the log holding the content.
pub(crate) fn body_digest(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_values_and_long_digit_runs_are_removed() {
        let cases = [
            (
                r#"invalid type: string "Jane \"JD\" Doe, +4915112345678", expected u64 at line 3 column 17"#,
                r#"invalid type: string "…", expected u64 at line 3 column 17"#,
            ),
            (
                "invalid type: integer `4915112345678`, expected a string",
                "invalid type: integer `…`, expected a string",
            ),
            (
                "invalid unix timestamp `Jane Doe`",
                "invalid unix timestamp `…`",
            ),
            (
                "unknown variant `jane@example.com`, expected `a` or `b`",
                "unknown variant `…`, expected `…` or `…`",
            ),
            (
                "missing field `display_phone_number`",
                "missing field `display_phone_number`",
            ),
            (
                "duplicate field `id` at line 1 column 1",
                "duplicate field `id` at line 1 column 1",
            ),
            // A field name that is not an identifier is not ours.
            ("missing field `Jane Doe`", "missing field `…`"),
            ("found 4915112345678 here", "found … here"),
            ("unterminated \"Jane Doe", "unterminated \"…\""),
            ("unterminated `Jane Doe", "unterminated `…`"),
            ("ünïcödé \"x\" ok", "ünïcödé \"…\" ok"),
        ];
        for (input, expected) in cases {
            assert_eq!(serde_message(input), expected, "{input}");
        }
    }

    #[test]
    fn long_messages_are_capped() {
        let long = "a".repeat(10_000);
        let out = serde_message(&long);
        assert!(out.len() < MAX_LEN + 20, "{}", out.len());
        assert!(out.ends_with("[truncated]"));
    }

    #[test]
    fn digest_is_sha256_hex() {
        assert_eq!(
            body_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
