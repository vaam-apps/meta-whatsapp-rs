//! A Flow's uploaded media, downloaded and decrypted (roadmap M5d2, design
//! D454 to D457). A user who fills a `PhotoPicker` or `DocumentPicker`
//! sends the file to WhatsApp's CDN, encrypted; the data exchange request
//! the integrating app receives carries, for each file, a `cdn_url` and
//! the keys and hashes to open it (`flows/guides/media_upload`, "Handling
//! media"). [`FlowMediaDownload::download`] does what that page tells the
//! endpoint to do, for an app that is not written in Rust:
//!
//! 1. the `cdn_url` is checked **by its text, before any network I/O**
//!    ([`FlowMediaDeployment`]): `https`, the default port, no user name
//!    or password, and a **host name listed** (default `*.whatsapp.net`,
//!    `WA_SERVER_FLOWS_MEDIA_HOSTS`); an address in the URL, listed or
//!    not, is refused (the list holds names only, no CIDR);
//! 2. the host is looked up once and **every address it answers must be
//!    public** (the webhooks-out check, [`crate::webhooks_out::destination`]);
//!    the download connects to the first of them and to no other (a
//!    rebinding host cannot be pointed elsewhere between the check and
//!    the connection);
//! 3. the file is fetched with a plain `GET`: no credentials of any kind (not
//!    the WABA's token, no cookie), **no redirect followed** (a `3xx` is a
//!    failure), at most [`MAX_DOWNLOAD_BYTES`] read, within
//!    [`FETCH_TIMEOUT`]; at most [`CONCURRENT_DOWNLOADS`] at once per
//!    replica;
//! 4. the library's `decrypt_media` checks the SHA-256 of the file, the
//!    HMAC, decrypts, and checks the SHA-256 of the result, in that order
//!    and in constant time: **nothing is returned unless every check
//!    passed**.
//!
//! Meta's text warns that uploaded files may be malicious: the bytes are
//! returned as `application/octet-stream` and never interpreted here.
//!
//! **The tenant.** The operation is on a WABA the caller's tenant owns
//! ([`OwnedWaba`]): another tenant's is `404` before anything else. The
//! service cannot tell which Flow a file belongs to: the CDN URL and the
//! keys come from Meta, and the caller holds them. What it guarantees is
//! that its network position serves nobody else: the host check, the
//! public-address check and the bounds above.
//!
//! **What is kept out of every record.** The URL (its path may carry a
//! token), the keys, the hashes, the file name and the bytes are in no
//! log, error, audit event, metric or idempotency record; the audit event
//! (`flow_media_downloaded`) names the tenant, the WABA and the size.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use meta_whatsapp_rs::client::flows::endpoint::{MediaEncryptionMetadata, decrypt_media};
use tokio::sync::Semaphore;

use crate::audit::{Subject, flow_media_audit};
use crate::authz::{Authorizer, Caller, OwnedWaba};
use crate::error::{ErrorCode, ServiceError};
use crate::webhooks_out::destination::{
    DestinationPolicy, InvalidAllowList, Pinned, Refusal, Resolver, pin,
};

/// The log target of this module's lines.
const LOG_TARGET: &str = "meta_whatsapp_server::api::flow_media";

/// The audit action of [`FlowMediaDownload::download`].
pub const FLOW_MEDIA_DOWNLOADED: &str = "flow_media_downloaded";

/// The hosts a download may connect to unless the deployment lists
/// others: the CDN of Meta's example (`https://mmg.whatsapp.net/v/…`).
/// Meta publishes no list of CDN hosts: this is the library's rule
/// (`FlowMedia` in `meta_whatsapp_rs::client::flows::endpoint`), not
/// Meta's.
pub const DEFAULT_HOSTS: &str = "*.whatsapp.net";

/// The largest file a Flow accepts from a user: `max-file-size-kb`'s
/// ceiling, 25,600 KiB (`flows/guides/media_upload`).
pub const MAX_FILE_BYTES: usize = 25_600 * 1024;

/// The most of the CDN's answer read: a file at [`MAX_FILE_BYTES`] plus
/// the padding (at most 16 bytes) and the 10 bytes of HMAC, with room.
pub const MAX_DOWNLOAD_BYTES: usize = MAX_FILE_BYTES + 64;

/// Downloads in flight at once on one replica: each holds up to twice
/// [`MAX_DOWNLOAD_BYTES`] (the file, then its plaintext). The service's
/// own; Meta states none.
pub const CONCURRENT_DOWNLOADS: usize = 4;

/// The whole fetch's time limit. The service's own; Meta states none.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest `cdn_url` accepted, in bytes.
pub const MAX_URL_BYTES: usize = 2048;

/// Why a fetch failed. Nothing of the URL, the host or the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The connection could not be made.
    #[error("could not connect")]
    Connect,
    /// The TLS handshake failed (the certificate, the name).
    #[error("TLS failed")]
    Tls,
    /// The time limit passed.
    #[error("timed out")]
    Timeout,
    /// The CDN answered something other than `200`: a redirect included,
    /// which is never followed.
    #[error("the CDN answered {0}")]
    Status(u16),
    /// The file is longer than the bound.
    #[error("the file is longer than allowed")]
    TooLarge,
    /// Not HTTP, or a head or a framing the client refuses.
    #[error("the CDN's answer was not valid HTTP")]
    Protocol,
}

/// Fetches a file from a CDN (`meta-whatsapp-server-http`'s `HyperFetcher`
/// in the service, a stub in tests).
///
/// An implementation **connects to `target.address` and to no other**
/// (the core resolved and checked it: resolving again would undo the
/// check), uses the host name for TLS and `Host` only, sends **no
/// credential**, follows **no redirect** (a `3xx` is [`FetchError::Status`])
/// and reads at most `max_bytes` (past it, [`FetchError::TooLarge`], and
/// the rest is never read), within `timeout`.
#[async_trait]
pub trait CdnFetcher: Send + Sync + 'static {
    /// `GET` `target`'s URL: the body of a `200`.
    ///
    /// # Errors
    ///
    /// A [`FetchError`].
    async fn get(
        &self,
        target: &Pinned,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError>;
}

/// `list` as the hosts a download may connect to: comma-separated host
/// names, `*.`-prefixed ones allowed, and nothing else (see
/// [`FlowMediaDeployment::with_hosts`]); what `WA_SERVER_FLOWS_MEDIA_HOSTS`
/// is checked with at start-up.
///
/// # Errors
///
/// [`InvalidAllowList`]: an entry is not a host name, or is an address or
/// a CIDR (its position, never its text).
pub fn media_hosts(list: &str) -> Result<DestinationPolicy, InvalidAllowList> {
    for (i, entry) in list
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .enumerate()
    {
        if entry.contains('/') || entry.parse::<std::net::IpAddr>().is_ok() {
            return Err(InvalidAllowList(i + 1));
        }
    }
    DestinationPolicy::parse(list, false)
}

/// What a deployment gives the download: the host list, the lookup, the
/// fetcher and the slots. `Debug` shows nothing but the slots.
#[derive(Clone)]
pub struct FlowMediaDeployment {
    hosts: DestinationPolicy,
    resolver: Arc<dyn Resolver>,
    fetcher: Arc<dyn CdnFetcher>,
    slots: Arc<Semaphore>,
}

impl fmt::Debug for FlowMediaDeployment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlowMediaDeployment")
            .field("free_slots", &self.slots.available_permits())
            .finish_non_exhaustive()
    }
}

impl FlowMediaDeployment {
    /// The default hosts ([`DEFAULT_HOSTS`]), [`CONCURRENT_DOWNLOADS`]
    /// slots.
    #[must_use]
    pub fn new(resolver: Arc<dyn Resolver>, fetcher: Arc<dyn CdnFetcher>) -> Self {
        Self {
            // The constant is a valid list; if it ever were not, an empty
            // list refuses every URL: closed, not open.
            hosts: DestinationPolicy::parse(DEFAULT_HOSTS, false).unwrap_or_default(),
            resolver,
            fetcher,
            slots: Arc::new(Semaphore::new(CONCURRENT_DOWNLOADS)),
        }
    }

    /// The hosts a download may connect to: `list` (comma-separated host
    /// names, `*.`-prefixed ones allowed) replaces [`DEFAULT_HOSTS`]. Names
    /// only: a media URL is a host name (an address in a URL is refused
    /// whatever the list holds), and a CIDR in the list would admit a
    /// listed host's addresses inside it, private ones too, so it is
    /// refused here.
    ///
    /// # Errors
    ///
    /// [`InvalidAllowList`]: an entry is not a host name, or is an address
    /// or a CIDR (its position, never its text).
    pub fn with_hosts(mut self, list: &str) -> Result<Self, InvalidAllowList> {
        self.hosts = media_hosts(list)?;
        Ok(self)
    }

    /// Whether `url` is, by its text, one a download may fetch: `https`,
    /// the default port, no user name or password, a listed host name.
    /// Nothing is looked up.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `cdn_url`: never says which rule.
    pub fn admits_url(&self, url: &str) -> Result<(), ServiceError> {
        let invalid = || ServiceError::invalid("cdn_url");
        if url.len() > MAX_URL_BYTES {
            return Err(invalid());
        }
        let parsed = url::Url::parse(url).map_err(|_| invalid())?;
        // The parser drops a default port: what remains is another one.
        if parsed.scheme() != "https"
            || parsed.port().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || !matches!(parsed.host(), Some(url::Host::Domain(_)))
        {
            return Err(invalid());
        }
        self.hosts.check_url(url).map(|_| ()).map_err(|_| invalid())
    }
}

/// The keys and hashes that open one file, as Meta's request gives them:
/// base64 text. `Debug` shows nothing of them.
#[derive(Clone)]
pub struct MediaKeys {
    /// SHA-256 of the downloaded file.
    pub encrypted_hash: String,
    /// The AES-CBC IV.
    pub iv: String,
    /// The AES-256 key. Secret.
    pub encryption_key: String,
    /// The HMAC-SHA256 key. Secret.
    pub hmac_key: String,
    /// SHA-256 of the decrypted file.
    pub plaintext_hash: String,
}

impl fmt::Debug for MediaKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaKeys").finish_non_exhaustive()
    }
}

impl MediaKeys {
    /// The library's metadata, once every value is base64 of the length
    /// Meta's page shows (hashes 32 bytes, IV 16, AES key 32, HMAC key 16
    /// to 64): checked here so that a request that cannot open its file
    /// downloads nothing.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on the value's field
    /// (`encryption_metadata.iv`, …), never quoting it.
    pub fn metadata(&self) -> Result<MediaEncryptionMetadata, ServiceError> {
        let check = |field: &'static str, value: &str, lengths: std::ops::RangeInclusive<usize>| {
            match base64::engine::general_purpose::STANDARD.decode(value) {
                Ok(bytes) if lengths.contains(&bytes.len()) => Ok(()),
                _ => Err(ServiceError::invalid(field)),
            }
        };
        check(
            "encryption_metadata.encrypted_hash",
            &self.encrypted_hash,
            32..=32,
        )?;
        check("encryption_metadata.iv", &self.iv, 16..=16)?;
        check(
            "encryption_metadata.encryption_key",
            &self.encryption_key,
            32..=32,
        )?;
        check("encryption_metadata.hmac_key", &self.hmac_key, 16..=64)?;
        check(
            "encryption_metadata.plaintext_hash",
            &self.plaintext_hash,
            32..=32,
        )?;
        // The library's type is built by its own deserializer (it is
        // non-exhaustive); the values are the ones just checked.
        serde_json::from_value(serde_json::json!({
            "encrypted_hash": self.encrypted_hash,
            "iv": self.iv,
            "encryption_key": self.encryption_key,
            "hmac_key": self.hmac_key,
            "plaintext_hash": self.plaintext_hash,
        }))
        .map_err(|_| ServiceError::invalid("encryption_metadata"))
    }
}

/// The download on a WABA the caller's tenant owns: an [`OwnedWaba`] its
/// [`Authorizer`] made, and admits.
pub struct FlowMediaDownload<'a> {
    caller: &'a Caller,
    waba: &'a OwnedWaba,
    deployment: &'a FlowMediaDeployment,
}

impl fmt::Debug for FlowMediaDownload<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlowMediaDownload")
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> FlowMediaDownload<'a> {
    /// The download on `waba` for `caller`.
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`.
    /// `waba` another tenant's: `404 not_found`. Either way before
    /// anything is read, resolved or fetched.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
        deployment: &'a FlowMediaDeployment,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!(
                target: LOG_TARGET,
                "refused a Flow media download on another tenant's WABA"
            );
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            caller,
            waba,
            deployment,
        })
    }

    /// Download `cdn_url`, verify and decrypt it with `keys`: the file's
    /// bytes, only when every check passed. Audited
    /// ([`FLOW_MEDIA_DOWNLOADED`]) with the size.
    ///
    /// # Errors
    ///
    /// - `422 invalid_request` on `cdn_url` (not a listed host name over
    ///   `https`, an address, a port, user info, too long, or a host that
    ///   resolves to a non-public address) or on a value of
    ///   `encryption_metadata`: nothing was fetched (for an address, after
    ///   a lookup and before a connection).
    /// - `429 too_many_requests`: [`CONCURRENT_DOWNLOADS`] are in flight.
    /// - `413 media_too_large`: the file is past [`MAX_DOWNLOAD_BYTES`].
    /// - `502 media_download_failed`: the lookup, the connection, the TLS
    ///   handshake or the time limit failed, or the CDN answered other
    ///   than `200` (a redirect included, never followed).
    /// - `502 integrity`: a hash or the HMAC did not match, or the padding
    ///   was wrong: nothing is returned.
    pub async fn download(&self, cdn_url: &str, keys: &MediaKeys) -> Result<Vec<u8>, ServiceError> {
        // Everything that needs no network, first.
        self.deployment.admits_url(cdn_url)?;
        let metadata = keys.metadata()?;
        let _slot = self
            .deployment
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ServiceError::too_many_requests(1))?;
        let target = pin(
            &self.deployment.hosts,
            self.deployment.resolver.as_ref(),
            cdn_url,
        )
        .await
        .map_err(|refusal| match refusal {
            Refusal::Unresolved => {
                tracing::info!(target: LOG_TARGET, "a Flow media host did not resolve");
                ServiceError::new(ErrorCode::MEDIA_DOWNLOAD_FAILED).retryable(true)
            }
            Refusal::Url | Refusal::NotListed | Refusal::Address => {
                tracing::warn!(
                    target: LOG_TARGET,
                    "refused a Flow media URL: its host resolves to a non-public address"
                );
                ServiceError::invalid("cdn_url")
            }
        })?;
        let file = self
            .deployment
            .fetcher
            .get(&target, MAX_DOWNLOAD_BYTES, FETCH_TIMEOUT)
            .await
            .map_err(|error| {
                tracing::info!(target: LOG_TARGET, %error, "a Flow media download failed");
                match error {
                    FetchError::TooLarge => ServiceError::new(ErrorCode::MEDIA_TOO_LARGE),
                    FetchError::Timeout
                    | FetchError::Connect
                    | FetchError::Tls
                    | FetchError::Status(_)
                    | FetchError::Protocol => {
                        ServiceError::new(ErrorCode::MEDIA_DOWNLOAD_FAILED).retryable(true)
                    }
                }
            })?;
        let bytes = decrypt_media(&file, &metadata).map_err(|_| {
            tracing::warn!(
                target: LOG_TARGET,
                "a Flow media file failed its integrity check: nothing is returned"
            );
            ServiceError::new(ErrorCode::INTEGRITY)
        })?;
        flow_media_audit(
            FLOW_MEDIA_DOWNLOADED,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.waba.waba_id().as_str()),
                ..Subject::default()
            },
            bytes.len(),
        );
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment() -> FlowMediaDeployment {
        struct Never;
        #[async_trait]
        impl Resolver for Never {
            async fn resolve(
                &self,
                _: &str,
                _: u16,
            ) -> Result<Vec<std::net::IpAddr>, crate::webhooks_out::destination::ResolveError>
            {
                Err(crate::webhooks_out::destination::ResolveError)
            }
        }
        #[async_trait]
        impl CdnFetcher for Never {
            async fn get(&self, _: &Pinned, _: usize, _: Duration) -> Result<Vec<u8>, FetchError> {
                Err(FetchError::Connect)
            }
        }
        FlowMediaDeployment::new(Arc::new(Never), Arc::new(Never))
    }

    /// The URL's own rules, by its text. Decisive: the scheme, the port,
    /// the user info, the address-literal and the host-suffix checks.
    #[test]
    fn a_url_is_https_with_a_listed_host_name_and_nothing_else() {
        let d = deployment();
        for ok in [
            "https://mmg.whatsapp.net/v/t62.7117-24/1?ccb=11&oh=00_x&oe=6A",
            "https://MMG.WhatsApp.net./v/x",
            "https://a.b.whatsapp.net/v/x",
            "https://mmg.whatsapp.net:443/v/x",
            // Spellings of the same name, judged as what they parse to.
            "https://mmg%2ewhatsapp.net/v/x",
            "https://mmg\u{3002}whatsapp.net/v/x",
            "https://\u{ff4d}mg.whatsapp.net/v/x",
            "https://mmg.whatsapp.net:/v/x:444",
            // Userinfo-looking text in the path or query is only that.
            "https://mmg.whatsapp.net/@evil.example/x?u=a@b",
        ] {
            assert!(d.admits_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://mmg.whatsapp.net/v/x",
            "ftp://mmg.whatsapp.net/v/x",
            "https://mmg.whatsapp.net:8443/v/x",
            "https://mmg.whatsapp.net@169.254.169.254/v/x",
            "https://user:pass@mmg.whatsapp.net/v/x",
            "https://mmg.whatsapp.net.evil.example/v/x",
            "https://evilwhatsapp.net/v/x",
            "https://whatsapp.net/v/x",
            "https://example.com/v/x",
            "https://169.254.169.254/latest/meta-data/",
            "https://127.0.0.1/v/x",
            "https://10.0.0.5/v/x",
            "https://[::1]/v/x",
            "https://[::ffff:10.0.0.5]/v/x",
            "https://0x7f000001/v/x",
            "https://2130706433/v/x",
            "https://localhost/v/x",
            "//mmg.whatsapp.net/v/x",
            "mmg.whatsapp.net/v/x",
            // The apex has no label before it; nor does an empty one.
            "https://.whatsapp.net/v/x",
            // Where the host ends, as the URL parser reads it.
            "https://evil.example%2f.whatsapp.net/v/x",
            "https://evil.example\\.whatsapp.net/v/x",
            "https://evil.example#.whatsapp.net/v/x",
            "https://evil.example?.whatsapp.net/v/x",
            "https://mmg.whatsapp.net%2eevil.example/v/x",
            "https://mmg.whatsapp.net%00.evil.example/v/x",
            "https://mmg.whatsapp.net.:8443/v/x",
            "https://xn--whatsapp-.net/v/x",
            "https://mmg.whatsapp.net\u{a0}.evil.example/v/x",
            "",
        ] {
            let Err(error) = d.admits_url(bad) else {
                panic!("admitted: {bad}")
            };
            assert_eq!(error.code(), "invalid_request", "{bad}");
            assert_eq!(error.field(), Some("cdn_url"), "{bad}");
        }
        let long = format!("https://mmg.whatsapp.net/{}", "a".repeat(MAX_URL_BYTES));
        assert!(d.admits_url(&long).is_err());
    }

    #[test]
    fn the_hosts_are_swappable_and_names_only() {
        let d = deployment()
            .with_hosts("*.cdn.example, media.example")
            .unwrap();
        assert!(d.admits_url("https://x.cdn.example/f").is_ok());
        assert!(d.admits_url("https://media.example/f").is_ok());
        assert!(d.admits_url("https://mmg.whatsapp.net/f").is_err());
        // A CIDR or an address would make the host check lax: refused.
        for list in [
            "10.0.0.0/8",
            "*.cdn.example, 10.1.2.3",
            "::1",
            "x.example/8",
        ] {
            assert!(deployment().with_hosts(list).is_err(), "{list}");
        }
        // An empty list admits nothing.
        assert!(
            deployment()
                .with_hosts("")
                .unwrap()
                .admits_url("https://mmg.whatsapp.net/f")
                .is_err()
        );
    }

    #[test]
    fn keys_are_base64_of_the_documented_lengths_and_show_nothing() {
        let b64 = |n: usize| base64::engine::general_purpose::STANDARD.encode(vec![7u8; n]);
        let keys = |hash: usize, iv: usize, key: usize, hmac: usize| MediaKeys {
            encrypted_hash: b64(hash),
            iv: b64(iv),
            encryption_key: b64(key),
            hmac_key: b64(hmac),
            plaintext_hash: b64(hash),
        };
        assert!(keys(32, 16, 32, 32).metadata().is_ok());
        for (bad, field) in [
            (keys(31, 16, 32, 32), "encryption_metadata.encrypted_hash"),
            (keys(32, 12, 32, 32), "encryption_metadata.iv"),
            (keys(32, 16, 16, 32), "encryption_metadata.encryption_key"),
            (keys(32, 16, 32, 8), "encryption_metadata.hmac_key"),
        ] {
            assert_eq!(bad.metadata().unwrap_err().field(), Some(field));
        }
        let mut garbage = keys(32, 16, 32, 32);
        garbage.iv = "not base64!".into();
        assert_eq!(
            garbage.metadata().unwrap_err().field(),
            Some("encryption_metadata.iv")
        );
        let shown = format!("{:?}", keys(32, 16, 32, 32));
        assert!(!shown.contains(&b64(32)), "{shown}");
    }
}
