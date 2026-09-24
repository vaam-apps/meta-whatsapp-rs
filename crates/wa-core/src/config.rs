//! Graph API version and endpoint configuration.

use std::fmt;
use std::str::FromStr;

use url::Url;

use crate::error::ConfigError;

/// A Graph API version, `vMAJOR.MINOR`.
///
/// Meta supports each version for roughly two years; pin one explicitly in
/// production and move deliberately. [`ApiVersion::DEFAULT`] is the version
/// Meta's WhatsApp docs used when this crate was last verified against them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiVersion {
    /// Major version.
    pub major: u16,
    /// Minor version.
    pub minor: u16,
}

impl ApiVersion {
    /// `v25.0` — the version used throughout Meta's WhatsApp docs as of
    /// 2026-09-24.
    pub const DEFAULT: Self = Self::new(25, 0);

    /// Build a version.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl Default for ApiVersion {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}.{}", self.major, self.minor)
    }
}

impl FromStr for ApiVersion {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ConfigError::new(format!("invalid Graph API version `{s}`, expected vNN.N"));
        let rest = s.strip_prefix('v').ok_or_else(bad)?;
        let (major, minor) = rest.split_once('.').ok_or_else(bad)?;
        Ok(Self::new(
            major.parse().map_err(|_| bad())?,
            minor.parse().map_err(|_| bad())?,
        ))
    }
}

/// Where Graph API requests go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEndpoint {
    base: Url,
    version: ApiVersion,
}

impl GraphEndpoint {
    /// Production host.
    pub const PRODUCTION: &'static str = "https://graph.facebook.com/";

    /// `https://graph.facebook.com/` at `version`.
    pub fn production(version: ApiVersion) -> Self {
        Self {
            // Infallible for this literal; unit-tested below.
            base: Url::parse(Self::PRODUCTION).unwrap_or_else(|_| unreachable!()),
            version,
        }
    }

    /// A custom base URL (proxy, mock server). Must be absolute `http(s)`.
    pub fn custom(base: &str, version: ApiVersion) -> Result<Self, ConfigError> {
        let mut base =
            Url::parse(base).map_err(|e| ConfigError::new(format!("invalid base url: {e}")))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(ConfigError::new("base url must be http or https"));
        }
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        Ok(Self { base, version })
    }

    /// API version in use.
    pub fn version(&self) -> ApiVersion {
        self.version
    }

    /// Base URL (without version).
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Absolute URL for a versioned Graph path such as `"123/messages"`.
    ///
    /// Each `/`-separated segment is percent-encoded, so an id containing
    /// `/`, `?` or `#` cannot escape its segment.
    pub fn url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        {
            // A base with a path always yields segments; `url()` constructors
            // above guarantee an http(s) base, which is never cannot-be-a-base.
            if let Ok(mut segs) = url.path_segments_mut() {
                segs.pop_if_empty();
                segs.push(&self.version.to_string());
                for seg in path.split('/').filter(|s| !s.is_empty()) {
                    segs.push(seg);
                }
            }
        }
        url
    }

    /// Unversioned URL, for the few endpoints that take none (media
    /// download URLs are absolute and handled separately).
    pub fn unversioned_url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut segs) = url.path_segments_mut() {
            segs.pop_if_empty();
            for seg in path.split('/').filter(|s| !s.is_empty()) {
                segs.push(seg);
            }
        }
        url
    }

    /// Whether `url` points at this endpoint's host (scheme, host and port).
    /// Used before following a `paging.next` link with credentials attached.
    pub fn same_origin(&self, url: &Url) -> bool {
        self.base.scheme() == url.scheme()
            && self.base.host_str() == url.host_str()
            && self.base.port_or_known_default() == url.port_or_known_default()
    }
}

impl Default for GraphEndpoint {
    fn default() -> Self {
        Self::production(ApiVersion::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_round_trips() {
        let v: ApiVersion = "v25.0".parse().unwrap();
        assert_eq!(v, ApiVersion::DEFAULT);
        assert_eq!(v.to_string(), "v25.0");
        assert!("25.0".parse::<ApiVersion>().is_err());
        assert!("vx.0".parse::<ApiVersion>().is_err());
    }

    #[test]
    fn builds_versioned_urls_and_encodes_segments() {
        let ep = GraphEndpoint::default();
        assert_eq!(
            ep.url("123/messages").as_str(),
            "https://graph.facebook.com/v25.0/123/messages"
        );
        // An id smuggling a query string stays inside its segment.
        assert_eq!(
            ep.url("12?x=1/messages").as_str(),
            "https://graph.facebook.com/v25.0/12%3Fx=1/messages"
        );
    }

    #[test]
    fn custom_base_keeps_its_path() {
        let ep =
            GraphEndpoint::custom("http://localhost:8080/graph", ApiVersion::new(24, 0)).unwrap();
        assert_eq!(
            ep.url("me").as_str(),
            "http://localhost:8080/graph/v24.0/me"
        );
        assert!(GraphEndpoint::custom("ftp://x", ApiVersion::DEFAULT).is_err());
    }

    #[test]
    fn same_origin_rejects_other_hosts() {
        let ep = GraphEndpoint::default();
        assert!(ep.same_origin(&Url::parse("https://graph.facebook.com/v25.0/x?after=a").unwrap()));
        assert!(!ep.same_origin(&Url::parse("https://evil.example/v25.0/x").unwrap()));
        assert!(!ep.same_origin(&Url::parse("http://graph.facebook.com/v25.0/x").unwrap()));
    }
}
