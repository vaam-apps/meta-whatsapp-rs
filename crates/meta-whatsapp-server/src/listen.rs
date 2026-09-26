//! The accept loop both listeners run (docs/design/server.md, section 6,
//! "Exhaustion": timeouts, bounded concurrency).
//!
//! `axum::serve` sets no timer on hyper, which turns hyper's header read
//! timeout off, and accepts connections without limit: a client sending
//! its headers a byte at a time (slowloris) would hold a connection, a
//! task and a file descriptor for ever, and enough of them would exhaust
//! the process. This loop serves the same router with:
//!
//! - a **header read timeout** ([`Limits::header_read_timeout`]): a
//!   connection whose request head is not complete in time is closed;
//! - a **connection cap** ([`Limits::max_connections`]): past it, new
//!   connections wait in the kernel's backlog until one closes;
//! - **graceful shutdown**: once `shutdown` completes, no connection is
//!   accepted and open ones finish their request (the caller bounds the
//!   wait with `WA_SERVER_SHUTDOWN_GRACE`).
//!
//! The request deadline is a layer of the routers ([`crate::api`]).

use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use meta_whatsapp_rs::webhooks::axum::Router;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

/// What one listener allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long a client has to send a request's head.
    pub header_read_timeout: Duration,
    /// Connections served at once.
    pub max_connections: usize,
}

/// The public listener's limits: Meta and whoever else reaches the
/// ingress. Only 64 deliveries are read at once per replica
/// ([`crate::events::MAX_DELIVERIES_IN_FLIGHT`]); the rest are answered
/// `503` at once, so the cap only bounds idle and refused connections.
pub const PUBLIC_LIMITS: Limits = Limits {
    header_read_timeout: Duration::from_secs(10),
    max_connections: 256,
};

/// The internal listener's limits: the integrators' backends and
/// operators (and, from M2, up to 1,000 event streams).
pub const INTERNAL_LIMITS: Limits = Limits {
    header_read_timeout: Duration::from_secs(10),
    max_connections: 4096,
};

/// Serve `router` on `listener` within `limits` until `shutdown`
/// completes, then wait for the open connections to finish.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    limits: Limits,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let permits = Arc::new(Semaphore::new(limits.max_connections));
    let graceful = GracefulShutdown::new();
    let mut http = http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout);
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let permit = tokio::select! {
            () = &mut shutdown => break,
            permit = permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                // The semaphore is never closed.
                Err(_) => break,
            },
        };
        let stream = tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) => {
                    // As axum::serve: a connection that failed while being
                    // accepted is the client's problem; anything else (out
                    // of file descriptors) is waited out.
                    if !is_connection_error(&error) {
                        tracing::warn!(error = %error, "accepting a connection failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    continue;
                }
            },
        };
        let connection = http.serve_connection(
            TokioIo::new(stream),
            TowerToHyperService::new(router.clone()),
        );
        let connection = graceful.watch(connection);
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::trace!(error = %error, "connection ended with an error");
            }
            drop(permit);
        });
    }
    drop(listener);
    graceful.shutdown().await;
    Ok(())
}

fn is_connection_error(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        error.kind(),
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset
    )
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use meta_whatsapp_rs::webhooks::axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;

    use super::*;

    async fn start(limits: Limits) -> (SocketAddr, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().route("/", get(|| async { "ok" }));
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve(listener, router, limits, async {
            let _ = stopped.await;
        }));
        (addr, stop)
    }

    /// Everything the server sends until it closes, or `None` if it is
    /// still open after `wait`.
    async fn read_until_closed(stream: &mut TcpStream, wait: Duration) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        tokio::time::timeout(wait, stream.read_to_end(&mut out))
            .await
            .ok()
            .map(|_| out)
    }

    const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

    /// A client that never finishes its request head is cut off after the
    /// header read timeout (slowloris). Decisive: the timer and the
    /// timeout on the connection builder.
    #[tokio::test]
    async fn a_slow_request_head_is_cut_off() {
        let (addr, _stop) = start(Limits {
            header_read_timeout: Duration::from_millis(300),
            max_connections: 8,
        })
        .await;
        let mut slow = TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nX-Slow: ")
            .await
            .unwrap();
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            // A byte now and then does not keep it open.
            let _ = slow.write_all(b"a").await;
        }
        let answer = read_until_closed(&mut slow, Duration::from_secs(5)).await;
        let answer = answer.expect("the connection is still open 5 s later");
        assert!(
            answer.is_empty() || answer.starts_with(b"HTTP/1.1 408"),
            "{}",
            String::from_utf8_lossy(&answer)
        );
        // A prompt request is served.
        let mut prompt = TcpStream::connect(addr).await.unwrap();
        prompt.write_all(REQUEST).await.unwrap();
        let answer = read_until_closed(&mut prompt, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(answer.starts_with(b"HTTP/1.1 200"), "{answer:?}");
    }

    /// Past the cap, a connection waits until one closes. Decisive: the
    /// permit taken before each accept.
    #[tokio::test]
    async fn connections_past_the_cap_wait() {
        let (addr, _stop) = start(Limits {
            header_read_timeout: Duration::from_secs(30),
            max_connections: 2,
        })
        .await;
        // Two connections that hold their slot (an unfinished head).
        let mut holders = Vec::new();
        for _ in 0..2 {
            let mut holder = TcpStream::connect(addr).await.unwrap();
            holder.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
            holders.push(holder);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut third = TcpStream::connect(addr).await.unwrap();
        third.write_all(REQUEST).await.unwrap();
        assert!(
            read_until_closed(&mut third, Duration::from_millis(500))
                .await
                .is_none(),
            "a third connection was served past a cap of two"
        );
        drop(holders.pop());
        let answer = read_until_closed(&mut third, Duration::from_secs(5))
            .await
            .expect("served once a slot was free");
        assert!(answer.starts_with(b"HTTP/1.1 200"), "{answer:?}");
    }

    /// After the shutdown signal, no connection is accepted.
    #[tokio::test]
    async fn nothing_is_accepted_after_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().route("/", get(|| async { "ok" }));
        let server = tokio::spawn(serve(listener, router, PUBLIC_LIMITS, async {}));
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("stopped")
            .unwrap()
            .unwrap();
        assert!(TcpStream::connect(addr).await.is_err());
    }
}
