//! A Flow's uploaded media over the network (roadmap M5d2, design D454):
//! the core's [`CdnFetcher`] port, as the service runs it.
//!
//! [`HyperFetcher`] connects to the address the core's check pinned
//! ([`Pinned::address`]) and nowhere else: it resolves nothing itself, so
//! the host's name is used only for TLS (SNI and the certificate's check,
//! on the platform's trust store and an explicit aws-lc-rs provider) and
//! the `Host` header. One `GET`, HTTP/1.1, **no credential of any kind**
//! (no `Authorization`, no cookie: the request has the headers below and
//! no more), **a redirect never followed** (any status but `200` is a
//! failure), the whole fetch within its time limit, the body read up to
//! the bound and no further (a declared length past it is refused before a
//! byte of the body is read), the connection closed afterwards (nothing is
//! pooled: a file is rare and its host is the CDN's).
//!
//! It logs nothing: the core logs a failure's kind, never the URL.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper_util::rt::TokioIo;
use meta_whatsapp_rs::webhooks::axum::http::{Request, header};
use meta_whatsapp_server_core::flow_media::{CdnFetcher, FetchError};
use meta_whatsapp_server_core::webhooks_out::destination::{Host, Pinned};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt as _;
use tokio::net::TcpStream;

/// Why a [`HyperFetcher`] could not be built.
#[derive(Debug, thiserror::Error)]
#[error("the TLS configuration of Flow media downloads could not be built: {0}")]
pub struct FetcherError(&'static str);

/// The most of an answer's head hyper buffers (its default is about 400
/// KiB); hyper's floor is 8 KiB.
const MAX_HEAD_BUFFER: usize = 16 * 1024;

/// Fetches files from a CDN over TCP and TLS to a pinned address.
#[derive(Clone)]
pub struct HyperFetcher {
    tls: Arc<ClientConfig>,
}

impl std::fmt::Debug for HyperFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperFetcher").finish_non_exhaustive()
    }
}

impl HyperFetcher {
    /// Trusting the platform's certificate store.
    ///
    /// # Errors
    ///
    /// [`FetcherError`]: the protocol versions, or the platform's store.
    pub fn new() -> Result<Self, FetcherError> {
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| FetcherError("the protocol versions"))?
        .with_platform_verifier()
        .map_err(|_| FetcherError("the platform's trust store"))?
        .with_no_client_auth();
        Ok(Self {
            tls: Arc::new(config),
        })
    }

    /// Trusting `roots` only (tests, a private CA).
    ///
    /// # Errors
    ///
    /// [`FetcherError`]: the protocol versions.
    pub fn with_roots(roots: RootCertStore) -> Result<Self, FetcherError> {
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| FetcherError("the protocol versions"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            tls: Arc::new(config),
        })
    }

    async fn fetch(&self, target: &Pinned, max_bytes: usize) -> Result<Vec<u8>, FetchError> {
        let tcp = TcpStream::connect(target.address)
            .await
            .map_err(|_| FetchError::Connect)?;
        if target.destination.https {
            let name = match &target.destination.host {
                Host::Name(name) => {
                    ServerName::try_from(name.clone()).map_err(|_| FetchError::Tls)?
                }
                Host::Ip(ip) => ServerName::IpAddress((*ip).into()),
            };
            let tls = tokio_rustls::TlsConnector::from(self.tls.clone())
                .connect(name, tcp)
                .await
                .map_err(|_| FetchError::Tls)?;
            exchange(tls, target, max_bytes).await
        } else {
            exchange(tcp, target, max_bytes).await
        }
    }
}

async fn exchange<I>(io: I, target: &Pinned, max_bytes: usize) -> Result<Vec<u8>, FetchError>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .max_buf_size(MAX_HEAD_BUFFER)
        .handshake(TokioIo::new(io))
        .await
        .map_err(|_| FetchError::Protocol)?;
    // Driven here, not spawned: when this future is dropped (the time
    // limit, the caller going away) the connection goes with it.
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = async {
        let request = Request::get(target.destination.path_and_query.as_str())
            .header(header::HOST, target.destination.authority.as_str())
            .header(header::USER_AGENT, "meta-whatsapp-rs")
            .header(header::ACCEPT, "*/*")
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::CONNECTION, "close")
            .body(Empty::<Bytes>::new())
            .map_err(|_| FetchError::Protocol)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| FetchError::Protocol)?;
        let status = response.status().as_u16();
        // A redirect, or anything but the file: the answer is the failure,
        // and its body is never read.
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        let declared = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|n| n > max_bytes as u64) {
            return Err(FetchError::TooLarge);
        }
        let mut body = response.into_body();
        let mut file = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| FetchError::Protocol)?;
            if let Some(data) = frame.data_ref() {
                if file.len().saturating_add(data.len()) > max_bytes {
                    return Err(FetchError::TooLarge);
                }
                file.extend_from_slice(data);
            }
        }
        // A body shorter than it declared is a truncated one.
        if declared.is_some_and(|n| n != file.len() as u64) {
            return Err(FetchError::Protocol);
        }
        Ok(file)
    }
    .await;
    driver.abort();
    result
}

#[async_trait]
impl CdnFetcher for HyperFetcher {
    async fn get(
        &self,
        target: &Pinned,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError> {
        tokio::time::timeout(timeout, self.fetch(target, max_bytes))
            .await
            .map_err(|_| FetchError::Timeout)?
    }
}
