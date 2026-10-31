//! Secret-bearing strings. `Debug` never prints the value; reading it takes
//! an explicit `expose_secret()`.
//!
//! Links whose query is a credential (Meta's signed media and CDN URLs)
//! stay plain `String`s in the public types that carry them; their `Debug`
//! prints them through [`RedactedUrl`] (or [`redacted_url`]), which keeps
//! the scheme, host, port and path and hides every query value; a link
//! whose path is the credential (a group invite link) prints through
//! [`RedactedPath`], which hides the path too.

use std::fmt::{self, Write as _};

use secrecy::zeroize::Zeroize;
use secrecy::{ExposeSecret, SecretSlice, SecretString};

macro_rules! secret_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone)]
        pub struct $name(SecretString);

        impl $name {
            /// Wrap a secret value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(SecretString::from(value.into()))
            }

            /// Read the secret. Keep the borrow short; never log it.
            pub fn expose_secret(&self) -> &str {
                self.0.expose_secret()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self::new(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self::new(s)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                String::deserialize(d).map(Self::new)
            }
        }
    };
}

secret_type!(
    /// A Graph API access token: system user, business integration system
    /// user (from Embedded Signup), user, or app token (`app_id|app_secret`).
    /// Opaque; never parse it.
    AccessToken
);
secret_type!(
    /// The Meta app secret. Signs webhooks (`X-Hub-Signature-256`) and is
    /// needed to exchange Embedded Signup codes for business tokens.
    AppSecret
);
secret_type!(
    /// The string you typed into the dashboard's **Verify token** field.
    VerifyToken
);

/// Secret key material as bytes (OTP pepper, vault keys). Zeroized on
/// drop; `Debug` is redacted; reading takes `expose_secret()`.
#[derive(Clone)]
pub struct SecretBytes(SecretSlice<u8>);

impl SecretBytes {
    /// Wrap secret bytes. The caller's buffer is wiped: the bytes are
    /// copied into an exactly-sized allocation first, so no reallocation
    /// can leave an unwiped copy behind.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        let mut bytes: Vec<u8> = bytes.into();
        let boxed: Box<[u8]> = bytes.as_slice().into();
        bytes.zeroize();
        Self(SecretSlice::from(boxed))
    }

    /// Read the secret. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &[u8] {
        self.0.expose_secret()
    }

    /// Length in bytes (not secret).
    pub fn len(&self) -> usize {
        self.0.expose_secret().len()
    }

    /// Whether there are no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([REDACTED])")
    }
}

impl AccessToken {
    /// An app access token, `app_id|app_secret`, for endpoints that require
    /// one (e.g. `debug_token`, app subscriptions).
    pub fn app_token(app_id: &crate::ids::AppId, secret: &AppSecret) -> Self {
        Self::new(format!("{app_id}|{}", secret.expose_secret()))
    }
}

/// A link shown without its credentials, for a `Debug` implementation: a
/// field that stays a `String` in a public type but holds a signed URL
/// (`MediaInfo::url` in `meta-whatsapp-client`, the media `url` of an
/// inbound webhook in `meta-whatsapp-webhooks`).
///
/// `Debug` prints what [`redacted_url`] returns, quoted as a string's
/// `Debug` is, or `<redacted: not a URL>` (unquoted) when the link does
/// not parse as an absolute URL:
///
/// ```
/// use meta_whatsapp_core::secret::RedactedUrl;
///
/// let url = "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1&hash=SENTINEL";
/// assert_eq!(
///     format!("{:?}", RedactedUrl(url)),
///     r#""https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=<redacted>&hash=<redacted>""#,
/// );
/// assert_eq!(format!("{:?}", RedactedUrl("SENTINEL")), "<redacted: not a URL>");
/// ```
#[derive(Clone, Copy)]
pub struct RedactedUrl<'a>(pub &'a str);

impl fmt::Debug for RedactedUrl<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match redacted_url(self.0) {
            Some(shown) => fmt::Debug::fmt(&shown, f),
            None => f.write_str("<redacted: not a URL>"),
        }
    }
}

/// A link whose path is its credential, for a `Debug` implementation: a
/// field that stays a `String` but opens something for whoever holds it
/// by its path (a WhatsApp group invite link,
/// `https://chat.whatsapp.com/<code>`).
///
/// `Debug` keeps the scheme, host and port and hides the rest: a path
/// other than `/` becomes `/<redacted>`, a query `?<redacted>`, a fragment
/// `#<redacted>`, user info `<redacted>@`; a link that cannot be a base
/// shows its scheme then `:<redacted>`. Quoted as a string's `Debug` is,
/// or `<redacted: not a URL>` (unquoted) when the link does not parse as
/// an absolute URL:
///
/// ```
/// use meta_whatsapp_core::secret::RedactedPath;
///
/// assert_eq!(
///     format!("{:?}", RedactedPath("https://chat.whatsapp.com/SENTINEL")),
///     r#""https://chat.whatsapp.com/<redacted>""#,
/// );
/// assert_eq!(format!("{:?}", RedactedPath("SENTINEL")), "<redacted: not a URL>");
/// ```
#[derive(Clone, Copy)]
pub struct RedactedPath<'a>(pub &'a str);

impl fmt::Debug for RedactedPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match redacted_path(self.0) {
            Some(shown) => fmt::Debug::fmt(&shown, f),
            None => f.write_str("<redacted: not a URL>"),
        }
    }
}

/// [`RedactedPath`]'s rendering, or `None` when `link` does not parse.
fn redacted_path(link: &str) -> Option<String> {
    let url = url::Url::parse(link).ok()?;
    if url.cannot_be_a_base() {
        return Some(format!("{}:{REDACTED}", url.scheme()));
    }
    let mut out = format!("{}://", url.scheme());
    if !url.username().is_empty() || url.password().is_some() {
        out.push_str(REDACTED);
        out.push('@');
    }
    if let Some(host) = url.host_str() {
        out.push_str(host);
    }
    if let Some(port) = url.port() {
        let _ = write!(out, ":{port}");
    }
    if url.path() == "/" {
        out.push('/');
    } else {
        let _ = write!(out, "/{REDACTED}");
    }
    if url.query().is_some() {
        let _ = write!(out, "?{REDACTED}");
    }
    if url.fragment().is_some() {
        let _ = write!(out, "#{REDACTED}");
    }
    Some(out)
}

const REDACTED: &str = "<redacted>";

/// `link` as a log may show it, or `None` when it does not parse as an
/// absolute URL (then show nothing of it).
///
/// The scheme, host, port and path are kept; each query parameter keeps
/// its name and gets `<redacted>` as its value (a pair without `=` is
/// hidden whole, an empty pair shows nothing); user info becomes
/// `<redacted>@` and a fragment `#<redacted>`; a link that cannot be a
/// base (`data:`, `mailto:`) shows only its scheme, then `:<redacted>`.
/// Every query value is hidden, not only the ones Meta signs with today,
/// so a parameter added later is hidden too. Built from the parsed parts,
/// so nothing the parser did not place in the scheme, host, port or path
/// is shown as it was.
///
/// ```
/// use meta_whatsapp_core::secret::redacted_url;
///
/// assert_eq!(
///     redacted_url("https://u:p@h/p?token=SENTINEL#frag").as_deref(),
///     Some("https://<redacted>@h/p?token=<redacted>#<redacted>"),
/// );
/// assert_eq!(redacted_url("?token=SENTINEL"), None);
/// ```
pub fn redacted_url(link: &str) -> Option<String> {
    let url = url::Url::parse(link).ok()?;
    if url.cannot_be_a_base() {
        // `data:`, `mailto:`…: everything after the scheme is opaque.
        return Some(format!("{}:{REDACTED}", url.scheme()));
    }
    let mut out = format!("{}://", url.scheme());
    if !url.username().is_empty() || url.password().is_some() {
        out.push_str(REDACTED);
        out.push('@');
    }
    if let Some(host) = url.host_str() {
        out.push_str(host);
    }
    if let Some(port) = url.port() {
        let _ = write!(out, ":{port}");
    }
    out.push_str(url.path());
    if let Some(query) = url.query() {
        out.push('?');
        for (i, pair) in query.split('&').enumerate() {
            if i > 0 {
                out.push('&');
            }
            if pair.is_empty() {
                continue;
            }
            // A pair without `=` is all value, for all we know; a `;`
            // inside a pair stays in its value.
            if let Some((name, _)) = pair.split_once('=') {
                out.push_str(name);
                out.push('=');
            }
            out.push_str(REDACTED);
        }
    }
    if url.fragment().is_some() {
        out.push('#');
        out.push_str(REDACTED);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A credential no test link uses otherwise.
    const SENTINEL: &str = "S3NT1NEL-s1gn4tur3";

    #[test]
    fn redacted_url_hides_query_values_user_info_and_fragment() {
        for (link, expected) in [
            (
                format!("https://h/p/?a=1&token={SENTINEL}&{SENTINEL}#{SENTINEL}"),
                "https://h/p/?a=<redacted>&token=<redacted>&<redacted>#<redacted>",
            ),
            (
                format!("https://u:{SENTINEL}@h:8443/p?oh=00_{SENTINEL}=="),
                "https://<redacted>@h:8443/p?oh=<redacted>",
            ),
            (
                format!("https://h/p?a=1;token={SENTINEL}&&"),
                "https://h/p?a=<redacted>&&",
            ),
            (format!("data:text/plain,{SENTINEL}"), "data:<redacted>"),
        ] {
            let shown = redacted_url(&link).unwrap();
            assert_eq!(shown, expected, "{link}");
            assert!(!shown.contains(SENTINEL), "{link}");
        }
        for link in [
            String::new(),
            format!("?token={SENTINEL}"),
            format!("https://exa mple.com/?token={SENTINEL}"),
        ] {
            assert_eq!(redacted_url(&link), None, "{link}");
        }
    }

    #[test]
    fn redacted_path_keeps_only_scheme_host_and_port() {
        for (link, expected) in [
            (
                format!("https://chat.whatsapp.com/{SENTINEL}"),
                "https://chat.whatsapp.com/<redacted>",
            ),
            (
                format!("https://u:{SENTINEL}@h:8443/a/{SENTINEL}/b?x={SENTINEL}#{SENTINEL}"),
                "https://<redacted>@h:8443/<redacted>?<redacted>#<redacted>",
            ),
            (format!("https://h/?{SENTINEL}"), "https://h/?<redacted>"),
            ("https://h/".to_owned(), "https://h/"),
            (format!("mailto:{SENTINEL}@h"), "mailto:<redacted>"),
        ] {
            for shown in [
                format!("{:?}", RedactedPath(&link)),
                format!("{:#?}", RedactedPath(&link)),
            ] {
                assert_eq!(shown, format!("{expected:?}"), "{link}");
                assert!(!shown.contains(SENTINEL), "{link}");
            }
        }
        for link in [SENTINEL.to_owned(), format!("/{SENTINEL}")] {
            assert_eq!(
                format!("{:?}", RedactedPath(&link)),
                "<redacted: not a URL>"
            );
        }
    }

    #[test]
    fn redacted_url_debug_is_the_quoted_redaction_in_both_forms() {
        let link = format!("https://lookaside.fbsbx.com/a/?mid=1&hash={SENTINEL}");
        let expected = r#""https://lookaside.fbsbx.com/a/?mid=<redacted>&hash=<redacted>""#;
        assert_eq!(format!("{:?}", RedactedUrl(&link)), expected);
        assert_eq!(format!("{:#?}", RedactedUrl(&link)), expected);
        assert_eq!(
            format!("{:?}", Some(RedactedUrl(&link))),
            format!("Some({expected})")
        );
        assert_eq!(
            format!("{:?}", RedactedUrl(SENTINEL)),
            "<redacted: not a URL>"
        );
    }

    #[test]
    fn debug_is_redacted() {
        let t = AccessToken::new("EAAJBsecret");
        assert_eq!(format!("{t:?}"), "AccessToken([REDACTED])");
        assert_eq!(t.expose_secret(), "EAAJBsecret");
    }

    #[test]
    fn secret_bytes_redact_and_clone() {
        let b = SecretBytes::new(vec![1u8, 2, 3]);
        let c = b.clone();
        assert_eq!(format!("{c:?}"), "SecretBytes([REDACTED])");
        assert_eq!(c.expose_secret(), &[1, 2, 3]);
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn app_token_format() {
        let t = AccessToken::app_token(
            &crate::ids::AppId::new("123").unwrap(),
            &AppSecret::new("s3cr3t"),
        );
        assert_eq!(t.expose_secret(), "123|s3cr3t");
    }
}
