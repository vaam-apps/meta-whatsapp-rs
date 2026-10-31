//! [`SecretUrl`]: a capability link, one that opens something for whoever
//! holds it.

use std::fmt;

use meta_whatsapp_core::secret::redacted_url as redacted;
use serde::{Deserialize, Serialize};

/// A link Meta answers with that is its own credential: whoever holds it
/// can open what it points to, without a token or a login. The client
/// uses it for:
///
/// - [`FlowPreview::preview_url`](crate::flows::FlowPreview::preview_url)
///   (and so [`FlowDetails::preview`](crate::flows::FlowDetails::preview)):
///   the Flow's web preview, whose `token` query parameter opens it for 30
///   days or until invalidated (`flows/guides/flowsapi`);
/// - [`FlowAsset::download_url`](crate::flows::FlowAsset::download_url):
///   a signed CDN link to the Flow JSON, fetched with a plain GET.
///
/// The link is kept whole, for the caller to open, embed, fetch or share,
/// and treated as a secret:
///
/// - `Debug` prints the scheme, host, port and path, then each query
///   parameter's name with `<redacted>` as its value; user info becomes
///   `<redacted>@` and a fragment `#<redacted>`:
///   `SecretUrl("https://business.facebook.com/wa/manage/flows/550/preview/?token=<redacted>")`.
///   Every value is hidden, not only `token`'s, so a parameter Meta adds
///   later is hidden too, and a query without `name=` is hidden whole. The
///   path is shown: Meta's links carry their credential in the query (the
///   preview's `token`, the CDN's signature). A link that does not parse
///   as an absolute URL prints as `SecretUrl(<redacted: not a URL>)`,
///   never as itself.
/// - There is no `Display`, `Deref` or `AsRef<str>`: reading the link
///   takes an explicit [`expose_secret`](Self::expose_secret).
/// - `Serialize` writes the whole link as a JSON string, and
///   `Deserialize` reads one (`#[serde(transparent)]`): serializing is how
///   a caller stores the link or hands it to the page that embeds it, and
///   a link without its credential opens nothing. Do not log a serialized
///   response that holds one.
///
/// ```
/// use meta_whatsapp_client::flows::SecretUrl; // also `common::SecretUrl`
///
/// let url = SecretUrl::new("https://business.facebook.com/wa/manage/flows/550/preview/?token=SENTINEL");
/// assert!(!format!("{url:?}").contains("SENTINEL"));
/// assert!(url.expose_secret().ends_with("?token=SENTINEL"));
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretUrl(String);

impl SecretUrl {
    /// Wrap a link (in a test, or one read back from storage).
    pub fn new(url: impl Into<String>) -> Self {
        Self(url.into())
    }

    /// The whole link, credential included. Keep it out of logs.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match redacted(&self.0) {
            Some(shown) => f.debug_tuple("SecretUrl").field(&shown).finish(),
            None => f.write_str("SecretUrl(<redacted: not a URL>)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A credential no test link uses otherwise.
    const SENTINEL: &str = "S3NT1NEL-c4p4b1l1ty";

    fn shown(link: &str) -> String {
        format!("{:?}", SecretUrl::new(link))
    }

    #[test]
    fn debug_hides_every_query_value_and_the_fragment() {
        for (link, expected) in [
            (
                format!("https://h/p/?a=1&token={SENTINEL}&{SENTINEL}#{SENTINEL}"),
                "https://h/p/?a=<redacted>&token=<redacted>&<redacted>#<redacted>",
            ),
            (
                format!("https://h/p/#{SENTINEL}"),
                "https://h/p/#<redacted>",
            ),
            // Percent-encoded names are names: the value goes all the same.
            (
                format!("https://h/p?%74oken={SENTINEL}"),
                "https://h/p?%74oken=<redacted>",
            ),
            // `;` is not a separator here: it stays inside the value.
            (
                format!("https://h/p?a=1;token={SENTINEL}"),
                "https://h/p?a=<redacted>",
            ),
            (
                format!("https://h/p?token={SENTINEL};x=1&b=2"),
                "https://h/p?token=<redacted>&b=<redacted>",
            ),
            // A value holding `=` is hidden whole, from its first `=`.
            (
                format!("https://h/p?oh=00_{SENTINEL}=="),
                "https://h/p?oh=<redacted>",
            ),
            // Empty pairs show nothing.
            ("https://h/p?&&".to_owned(), "https://h/p?&&"),
            ("https://h/p?".to_owned(), "https://h/p?"),
        ] {
            assert_eq!(shown(&link), format!("SecretUrl({expected:?})"), "{link}");
            assert!(!shown(&link).contains(SENTINEL), "{link}");
        }
    }

    #[test]
    fn debug_hides_user_info() {
        for link in [
            format!("https://u:{SENTINEL}@h/p"),
            format!("https://{SENTINEL}@h/p"),
            format!("https://{SENTINEL}:@h/p"),
            format!("https://:{SENTINEL}@h/p"),
        ] {
            assert_eq!(
                shown(&link),
                r#"SecretUrl("https://<redacted>@h/p")"#,
                "{link}"
            );
        }
    }

    #[test]
    fn debug_shows_the_path_host_and_port() {
        // Meta's links carry their credential in the query; the path is
        // shown, as is, so a log line still says what the link was.
        assert_eq!(
            shown("https://scontent.xx.fbcdn.net:8443/m1/v/t0.57323-24/An_Hq0jnfJ"),
            r#"SecretUrl("https://scontent.xx.fbcdn.net:8443/m1/v/t0.57323-24/An_Hq0jnfJ")"#
        );
        assert_eq!(
            shown("https://h/token/abc"),
            r#"SecretUrl("https://h/token/abc")"#
        );
        assert_eq!(shown("https://[::1]/p"), r#"SecretUrl("https://[::1]/p")"#);
    }

    #[test]
    fn a_link_that_does_not_parse_is_hidden_whole() {
        for link in [
            String::new(),
            SENTINEL.to_owned(),
            format!("?token={SENTINEL}"),
            format!("/wa/manage/flows/550/preview/?token={SENTINEL}"),
            format!("not a url {SENTINEL}"),
            format!("https://exa mple.com/?token={SENTINEL}"),
            format!("https://h:99999/{SENTINEL}"),
            format!("http://[{SENTINEL}]/"),
        ] {
            assert_eq!(shown(&link), "SecretUrl(<redacted: not a URL>)", "{link}");
        }
        // A link without a base hides everything after its scheme.
        assert_eq!(
            shown(&format!("data:text/plain,{SENTINEL}")),
            r#"SecretUrl("data:<redacted>")"#
        );
        assert_eq!(
            shown(&format!("mailto:{SENTINEL}@example.com")),
            r#"SecretUrl("mailto:<redacted>")"#
        );
    }

    #[test]
    fn alternate_debug_hides_it_too() {
        let url = SecretUrl::new(format!("https://u:p@h/p?token={SENTINEL}#{SENTINEL}"));
        let pretty = format!("{url:#?}");
        assert!(!pretty.contains(SENTINEL), "{pretty}");
        assert!(!pretty.contains("u:p"), "{pretty}");
        assert!(pretty.contains("https://<redacted>@h/p?token=<redacted>#<redacted>"));
    }

    #[test]
    fn the_link_is_kept_whole_for_the_caller_and_in_json() {
        let link = format!("https://u:p@h/p?token={SENTINEL}#{SENTINEL}");
        let url = SecretUrl::new(link.clone());
        assert_eq!(url.expose_secret(), link);
        assert_eq!(serde_json::to_value(&url).unwrap(), json!(link));
        assert_eq!(
            serde_json::from_value::<SecretUrl>(json!(link)).unwrap(),
            url
        );
        // A malformed link is kept as given too.
        let odd = SecretUrl::new(format!("?token={SENTINEL}"));
        assert_eq!(odd.expose_secret(), format!("?token={SENTINEL}"));
    }
}
