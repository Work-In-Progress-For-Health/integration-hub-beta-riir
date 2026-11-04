//! Accepting connections, with or without TLS.
//!
//! Implements `spec/08-hl7-soap-server.md` §7 and the shutdown discipline
//! of `spec/07-hl7-server.md` §8.3, which applies here for the same reason:
//! a request that has been answered `200` must have been forwarded.

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use ihb_bus::sink::MessageSink;
use ihb_runtime::shutdown::Watcher;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::Tls;
use crate::http::handle;
use crate::process::{Mint, Processor};

/// Why TLS could not be set up.
///
/// Each variant names the file, because "invalid certificate" without a
/// path is the least useful message a deployment can fail with.
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

    /// A file held no PEM item of the kind wanted.
    #[error("{path} contains no {wanted}")]
    Empty {
        /// Which file.
        path: String,
        /// What was being looked for.
        wanted: &'static str,
    },

    /// The certificate and key were read but rustls refused them.
    #[error("the certificate and key were not accepted: {0}")]
    Rejected(String),
}

/// Build a rustls configuration from the PEM files.
///
/// No client authentication is requested (`with_no_client_auth`): callers
/// are identified by the assigning-authority check of `spec/08` §4, not by
/// a client certificate. Called once at startup so a bad file stops the
/// service there, not on the first request.
///
/// # Errors
///
/// [`TlsError`] naming the file at fault.
pub fn tls_config(tls: &Tls) -> Result<ServerConfig, TlsError> {
    let certificates = read_certificates(&tls.certificate)?;
    let key = read_key(&tls.key)?;

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|error| TlsError::Rejected(error.to_string()))
}

/// The certificate chain, leaf first.
fn read_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let display = path.display().to_string();

    // `rustls-pki-types` rather than the archived `rustls-pemfile`
    // (RUSTSEC-2025-0134); its own advisory says this is the same code.
    let certificates: Vec<_> = CertificateDer::pem_file_iter(path)
        .map_err(|error| TlsError::Unreadable {
            path: display.clone(),
            source: io::Error::other(error.to_string()),
        })?
        .collect::<Result<_, _>>()
        .map_err(|error| TlsError::Unreadable {
            path: display.clone(),
            source: io::Error::other(error.to_string()),
        })?;

    if certificates.is_empty() {
        return Err(TlsError::Empty {
            path: display,
            wanted: "certificate",
        });
    }
    Ok(certificates)
}

/// The private key: the first PEM key in the file, in any of the formats
/// `rustls-pki-types` accepts (PKCS#1, PKCS#8, SEC1).
fn read_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    PrivateKeyDer::from_pem_file(path).map_err(|error| TlsError::Unreadable {
        path: path.display().to_string(),
        source: io::Error::other(error.to_string()),
    })
}

/// Serve until the watcher says stop.
///
/// `tls` of `None` serves plaintext, which is the normal shape behind Azure
/// Container Apps ingress (which terminates TLS itself; see
/// `spec/08-hl7-soap-server.md` §6.2). On stop the listener closes first and
/// in-flight requests are then awaited, never cancelled.
///
/// # Errors
///
/// [`io::Error`] if the address cannot be bound. Per-connection failures
/// are logged and do not end the loop.
pub async fn serve<E, S, M>(
    address: SocketAddr,
    processor: Arc<Processor<E, S, M>>,
    tls: Option<ServerConfig>,
    watcher: Watcher,
) -> io::Result<()>
where
    E: MessageSink + 'static,
    S: MessageSink + 'static,
    M: Mint + 'static,
{
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr().unwrap_or(address);
    let acceptor = tls.map(|config| TlsAcceptor::from(Arc::new(config)));

    tracing::info!(%bound, tls = acceptor.is_some(), "SOAP listener started");

    let mut connections = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            // A pending shutdown wins over a ready connection, so a busy
            // endpoint cannot keep the process alive indefinitely.
            biased;

            () = watcher.wait() => break,

            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let processor = Arc::clone(&processor);
                    let acceptor = acceptor.clone();
                    connections.spawn(async move {
                        if let Err(error) = connection(stream, acceptor, processor, bound).await {
                            tracing::warn!(%peer, %error, "connection ended with an error");
                        }
                    });
                }
                Err(error) => tracing::warn!(%error, "could not accept a connection"),
            },
        }
    }

    tracing::info!("SOAP listener stopped accepting; draining in-flight requests");
    // A request that has been answered `200` must have been forwarded, so
    // in-flight work is awaited rather than cancelled.
    while connections.join_next().await.is_some() {}
    tracing::info!("SOAP listener drained");

    Ok(())
}

/// One connection, over TLS or not.
///
/// The two arms are near-identical and deliberately not unified behind a
/// trait object: `TokioIo` is generic over the stream, and boxing it to
/// share one code path would add a virtual call to every read and write on
/// the hot path to save six lines here.
async fn connection<E, S, M>(
    stream: tokio::net::TcpStream,
    acceptor: Option<TlsAcceptor>,
    processor: Arc<Processor<E, S, M>>,
    bound: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    E: MessageSink + 'static,
    S: MessageSink + 'static,
    M: Mint + 'static,
{
    let service = service_fn(move |request| {
        let processor = Arc::clone(&processor);
        async move { handle(request, processor, bound).await }
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
