//! The [`Client`] and its builder.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use http::Method;
use wa_core::config::{ApiVersion, GraphEndpoint};
use wa_core::error::ConfigError;
use wa_core::secret::AccessToken;
use wa_core::transport::HttpTransport;
use wa_core::{Error, Result};

use crate::request::GraphRequest;
use crate::retry::RetryPolicy;

/// Default per-request timeout when neither the builder nor the request set
/// one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct Shared {
    pub(crate) transport: Arc<dyn HttpTransport>,
    pub(crate) endpoint: GraphEndpoint,
    pub(crate) retry: RetryPolicy,
    pub(crate) timeout: Duration,
    pub(crate) user_agent: String,
}

/// A WhatsApp Business Platform client.
///
/// Cheap to clone (one `Arc` and an optional token). One client can serve
/// many tenants: [`Client::with_token`] returns a copy that authenticates as
/// a different business — use it with the business token each merchant's
/// Embedded Signup produced.
///
/// Every endpoint family hangs off an accessor:
///
/// | Accessor | Scope |
/// | --- | --- |
/// | [`Client::messages`] | send, mark read, typing indicators |
/// | [`Client::media`] | upload, download (streaming), delete |
/// | [`Client::templates`] | create, list, edit, delete, library |
/// | [`Client::authentication`] | authentication templates + OTP |
/// | [`Client::embedded_signup`] | onboarding businesses |
/// | [`Client::signups`] | In-App Signup deep links |
/// | [`Client::waba`] | WABA settings, webhook subscriptions |
/// | [`Client::phone_number`] | registration, verification, settings |
/// | [`Client::business_profile`] | about, address, websites, picture |
/// | [`Client::commerce`] | catalogs, commerce settings |
/// | [`Client::flows`] | WhatsApp Flows |
/// | [`Client::analytics`] | messaging, pricing, template analytics |
/// | [`Client::marketing`] | Marketing Messages API for WhatsApp |
/// | [`Client::qr_codes`] | QR codes / prefilled messages |
/// | [`Client::block_users`] | block list |
/// | [`Client::groups`] | Groups API |
/// | [`Client::calling`] | Calling API signalling |
///
/// Anything not wrapped yet is reachable through [`Client::get`],
/// [`Client::post`] and [`Client::delete`].
#[derive(Clone)]
pub struct Client {
    pub(crate) shared: Arc<Shared>,
    pub(crate) token: Option<AccessToken>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("endpoint", &self.shared.endpoint)
            .field("transport", &self.shared.transport)
            .field("has_token", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Start building a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// A copy of this client that authenticates with `token`.
    #[must_use]
    pub fn with_token(&self, token: AccessToken) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            token: Some(token),
        }
    }

    /// The token this client sends, if any.
    pub fn token(&self) -> Option<&AccessToken> {
        self.token.as_ref()
    }

    /// Graph endpoint (base URL + version).
    pub fn endpoint(&self) -> &GraphEndpoint {
        &self.shared.endpoint
    }

    /// Start a request to an arbitrary versioned Graph path.
    pub fn request(&self, method: Method, path: &str) -> GraphRequest {
        GraphRequest::new(self.clone(), method, self.shared.endpoint.url(path))
    }

    /// `GET {version}/{path}`.
    pub fn get(&self, path: &str) -> GraphRequest {
        self.request(Method::GET, path)
    }

    /// `POST {version}/{path}`.
    pub fn post(&self, path: &str) -> GraphRequest {
        self.request(Method::POST, path)
    }

    /// `DELETE {version}/{path}`.
    pub fn delete(&self, path: &str) -> GraphRequest {
        self.request(Method::DELETE, path)
    }

    /// A request to an absolute URL (media download links). The token is
    /// only attached for the configured Graph endpoint and Meta's media CDN
    /// (`https://*.fbsbx.com`, `*.facebook.com`, `*.whatsapp.net`); any other
    /// host fails with a validation error before a byte is sent.
    pub fn request_url(&self, method: Method, url: url::Url) -> GraphRequest {
        GraphRequest::new(self.clone(), method, url)
    }
}

/// Builds a [`Client`].
#[derive(Default)]
pub struct ClientBuilder {
    transport: Option<Arc<dyn HttpTransport>>,
    token: Option<AccessToken>,
    endpoint: Option<GraphEndpoint>,
    version: Option<ApiVersion>,
    retry: Option<RetryPolicy>,
    timeout: Option<Duration>,
    user_agent: Option<String>,
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder").finish_non_exhaustive()
    }
}

impl ClientBuilder {
    /// HTTP transport. Required. `wa_adapters::http::ReqwestTransport` is the
    /// stock one.
    #[must_use]
    pub fn transport(mut self, transport: impl HttpTransport) -> Self {
        self.transport = Some(Arc::new(transport));
        self
    }

    /// HTTP transport, already shared.
    #[must_use]
    pub fn shared_transport(mut self, transport: Arc<dyn HttpTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Default access token. Optional: multi-tenant callers often set it per
    /// tenant with [`Client::with_token`] instead.
    #[must_use]
    pub fn access_token(mut self, token: impl Into<AccessToken>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Graph API version. Defaults to [`ApiVersion::DEFAULT`].
    #[must_use]
    pub fn api_version(mut self, version: ApiVersion) -> Self {
        self.version = Some(version);
        self
    }

    /// Custom endpoint (proxy, mock server). Overrides [`Self::api_version`]
    /// only if that was not set.
    #[must_use]
    pub fn endpoint(mut self, endpoint: GraphEndpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    /// Retry policy. Defaults to [`RetryPolicy::default`].
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    /// Default per-request timeout. Defaults to [`DEFAULT_TIMEOUT`].
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// `User-Agent` header.
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Build the client.
    pub fn build(self) -> Result<Client> {
        let transport = self.transport.ok_or_else(|| {
            Error::from(ConfigError::new(
                "a transport is required (e.g. wa_adapters::http::ReqwestTransport)",
            ))
        })?;
        let endpoint = match (self.endpoint, self.version) {
            (Some(ep), Some(v)) if ep.version() != v => {
                GraphEndpoint::custom(ep.base().as_str(), v)?
            }
            (Some(ep), _) => ep,
            (None, v) => GraphEndpoint::production(v.unwrap_or_default()),
        };
        Ok(Client {
            shared: Arc::new(Shared {
                transport,
                endpoint,
                retry: self.retry.unwrap_or_default(),
                timeout: self.timeout.unwrap_or(DEFAULT_TIMEOUT),
                user_agent: self
                    .user_agent
                    .unwrap_or_else(|| concat!("wa-rs/", env!("CARGO_PKG_VERSION")).to_owned()),
            }),
            token: self.token,
        })
    }
}
