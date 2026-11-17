//! Accepting connections, with or without TLS.
//!
//! Implements `spec/19-rest-servers.md` §1 and the shutdown discipline of
//! `spec/07-hl7-server.md` §8: a request that has been answered `201` must
//! have been forwarded, so in-flight requests are awaited, not cancelled.
//!
//! The same accept loop as `hl7-soap-server`'s, generic over what answers the
//! request so that `rest-server` can serve either of its pipelines through
//! it. TLS lives here because both services need it and neither should have
//! its own copy of how a PEM file becomes a `ServerConfig`.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use ihb_runtime::shutdown::Watcher;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::Tls;

/// Something that answers a request.
pub trait Handler: Send + Sync + 'static {
    /// Answer one request. Never fails: every failure is a response.
    fn handle(
        &self,
        request: Request<Incoming>,
        peer: SocketAddr,
    ) -> impl Future<Output = Response<Full<Bytes>>> + Send;
}

/// Why TLS could not be set up. Each variant names the file.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A file could not be read.
    #[error("could not read {path}: {source}")]
    Unreadable {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: io::Error,
    },

    /// A file held no certificate.
    #[error("{path} contains no certificate")]
    Empty {
        /// Which file.
        path: String,
    },

    /// rustls refused the pair.
    #[error("the certificate and key were not accepted: {0}")]
    Rejected(String),
}

/// Build a rustls configuration from the PEM files.
///
/// # Errors
///
/// [`TlsError`] naming the file at fault.
pub fn tls_config(tls: &Tls) -> Result<ServerConfig, TlsError> {
    let certificates = read_certificates(&tls.certificate)?;
    let key = PrivateKeyDer::from_pem_file(&tls.key).map_err(|error| TlsError::Unreadable {
        path: tls.key.display().to_string(),
        source: io::Error::other(error.to_string()),
    })?;

    ihb_tls::server_builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|error| TlsError::Rejected(error.to_string()))
}

/// The certificate chain, leaf first.
fn read_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let display = path.display().to_string();
    let unreadable = |error: tokio_rustls::rustls::pki_types::pem::Error| TlsError::Unreadable {
        path: display.clone(),
        source: io::Error::other(error.to_string()),
    };

    let certificates: Vec<_> = CertificateDer::pem_file_iter(path)
        .map_err(unreadable)?
        .collect::<Result<_, _>>()
        .map_err(unreadable)?;

    if certificates.is_empty() {
        return Err(TlsError::Empty { path: display });
    }
    Ok(certificates)
}

/// Serve until the watcher says stop.
///
/// # Errors
///
/// [`io::Error`] if the address cannot be bound.
pub async fn serve<H: Handler>(
    address: SocketAddr,
    handler: Arc<H>,
    tls: Option<ServerConfig>,
    watcher: Watcher,
) -> io::Result<()> {
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr().unwrap_or(address);
    let acceptor = tls.map(|config| TlsAcceptor::from(Arc::new(config)));

    tracing::info!(%bound, tls = acceptor.is_some(), "REST listener started");

    let mut connections = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            // A pending shutdown wins over a ready connection, so a busy
            // endpoint cannot keep the process alive indefinitely.
            biased;

            () = watcher.wait() => break,

            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let handler = Arc::clone(&handler);
                    let acceptor = acceptor.clone();
                    connections.spawn(async move {
                        if let Err(error) = connection(stream, peer, acceptor, handler).await {
                            tracing::warn!(%peer, %error, "connection ended with an error");
                        }
                    });
                }
                Err(error) => tracing::warn!(%error, "could not accept a connection"),
            },
        }
    }

    tracing::info!("REST listener stopped accepting; draining in-flight requests");
    while connections.join_next().await.is_some() {}
    tracing::info!("REST listener drained");

    Ok(())
}

/// One connection, over TLS or not.
async fn connection<H: Handler>(
    stream: TcpStream,
    peer: SocketAddr,
    acceptor: Option<TlsAcceptor>,
    handler: Arc<H>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = service_fn(move |request| {
        let handler = Arc::clone(&handler);
        async move { Ok::<_, Infallible>(handler.handle(request, peer).await) }
    });

    match acceptor {
        Some(acceptor) => {
            let stream = acceptor.accept(stream).await?;
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await?;
        }
        None => {
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await?;
        }
    }
    Ok(())
}
