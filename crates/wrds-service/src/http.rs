//! One `POST`, over plain TCP or TLS.
//!
//! Implements `spec/09-transformers.md` §9.2 and §9.5.
//!
//! A fresh connection per call: the transformer makes at most one call per
//! message and the Python's `requests` session gave no guarantee worth
//! preserving across them.

// `rustc`'s `unreachable_pub` wants `pub(crate)` for items a private module
// shares with its parent; clippy's `redundant_pub_crate` wants `pub` for the
// same items. They cannot both be satisfied, and the first is the one that
// says what is meant.
#![allow(clippy::redundant_pub_crate)]

use std::path::Path;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::Error;

/// The content type §9.2 requires.
pub(crate) const CONTENT_TYPE_VALUE: &str = "text/xml; charset=utf-8";

/// A TLS connector over the workspace trust roots, presenting a client
/// certificate when one is configured.
///
/// `client_cert` is one PEM file holding the chain and the private key, the
/// shape `requests`' `cert=` took.
///
/// # Errors
///
/// [`Error::Tls`], naming the file but never its contents, when the trust
/// roots cannot be built or the PEM has no certificate or no usable key.
pub(crate) fn connector(client_cert: Option<&Path>) -> Result<TlsConnector, Error> {
    let roots = ihb_tls::root_store().map_err(|error| Error::Tls(error.to_string()))?;
    let builder = ihb_tls::client_builder().with_root_certificates(roots);

    let config = if let Some(path) = client_cert {
        let chain = CertificateDer::pem_file_iter(path)
            .map_err(|error| Error::Tls(format!("{}: {error}", path.display())))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| Error::Tls(format!("{}: {error}", path.display())))?;
        if chain.is_empty() {
            return Err(Error::Tls(format!(
                "{}: no certificate in the file",
                path.display()
            )));
        }
        let key = PrivateKeyDer::from_pem_file(path)
            .map_err(|error| Error::Tls(format!("{}: {error}", path.display())))?;
        builder
            .with_client_auth_cert(chain, key)
            .map_err(|error| Error::Tls(error.to_string()))?
    } else {
        builder.with_no_client_auth()
    };

    Ok(TlsConnector::from(Arc::new(config)))
}

/// `POST` `body` and return the response text for a `200`.
///
/// The caller applies the timeout around this whole call (connect, TLS and
/// read), so nothing here sets its own. Any non-`200` is [`Error::Status`]
/// without reading the body, so a SOAP fault sent with a `500` (the SOAP 1.1
/// convention) surfaces as a status, not as [`Error::Fault`]
/// (`spec/09-transformers.md` §9.5).
///
/// # Errors
///
/// [`Error::Connection`] for a bad URL, refused connection, TLS or HTTP
/// failure; [`Error::Tls`] for an `https` endpoint given no connector;
/// [`Error::Status`] for any status but `200`.
pub(crate) async fn post(
    endpoint: &str,
    tls: Option<&TlsConnector>,
    body: String,
) -> Result<String, Error> {
    let uri: Uri = endpoint
        .parse()
        .map_err(|error| Error::Connection(format!("the endpoint URL is invalid: {error}")))?;
    let host = uri
        .host()
        .ok_or_else(|| Error::Connection("the endpoint URL names no host".to_string()))?
        .to_string();
    let secure = uri.scheme_str() == Some("https");
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });

    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|error| Error::Connection(error.to_string()))?;

    let request = Request::builder()
        .method("POST")
        .uri(uri.path_and_query().map_or("/", |p| p.as_str()))
        .header(CONTENT_TYPE, CONTENT_TYPE_VALUE)
        .header(HOST, &host)
        .body(Full::new(Bytes::from(body)))
        .map_err(|error| Error::Connection(error.to_string()))?;

    if secure {
        let connector =
            tls.ok_or_else(|| Error::Tls("an https endpoint needs a TLS connector".to_string()))?;
        let name = ServerName::try_from(host)
            .map_err(|error| Error::Connection(format!("invalid server name: {error}")))?;
        let stream = connector
            .connect(name, tcp)
            .await
            .map_err(|error| Error::Connection(error.to_string()))?;
        exchange(stream, request).await
    } else {
        exchange(tcp, request).await
    }
}

/// Send `request` over `io` and read the whole response.
async fn exchange<I>(io: I, request: Request<Full<Bytes>>) -> Result<String, Error>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|error| Error::Connection(error.to_string()))?;
    // The connection drives itself; the response below is what is awaited.
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let response = sender
        .send_request(request)
        .await
        .map_err(|error| Error::Connection(error.to_string()))?;

    let status = response.status().as_u16();
    if status != 200 {
        return Err(Error::Status(status));
    }
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|error| Error::Connection(error.to_string()))?
        .to_bytes();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch file that is removed afterwards.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str, content: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("wrds-service-{}-{name}", std::process::id()));
            std::fs::write(&path, content).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn tls_message(result: Result<TlsConnector, Error>) -> String {
        match result {
            Err(Error::Tls(message)) => message,
            Err(other) => panic!("expected a TLS error, got {other:?}"),
            Ok(_) => panic!("expected a TLS error, got a connector"),
        }
    }

    #[test]
    fn no_client_certificate_is_a_usable_connector() {
        assert!(connector(None).is_ok());
    }

    #[test]
    fn a_client_certificate_file_that_is_missing_is_named_in_the_refusal() {
        let path = std::env::temp_dir().join("wrds-service-no-such-cert.pem");
        let message = tls_message(connector(Some(&path)));
        assert!(
            message.contains("wrds-service-no-such-cert.pem"),
            "{message}"
        );
    }

    #[test]
    fn a_pem_with_no_certificate_is_refused_and_its_contents_are_not_echoed() {
        let file = Scratch::new("empty.pem", "SECRET-LOOKING-TEXT\n");
        let message = tls_message(connector(Some(&file.0)));
        assert!(message.contains("no certificate in the file"), "{message}");
        assert!(!message.contains("SECRET-LOOKING-TEXT"), "{message}");
    }

    #[tokio::test]
    async fn a_url_that_is_not_a_url_is_a_connection_error() {
        let error = post("http://exa mple/", None, String::new())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, Error::Connection(m) if m.contains("invalid")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_url_with_no_host_is_a_connection_error() {
        let error = post("/just/a/path", None, String::new()).await.unwrap_err();
        assert!(
            matches!(&error, Error::Connection(m) if m.contains("no host")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn an_https_endpoint_without_a_connector_is_a_tls_error() {
        // The connection is made first, so give it something to connect to.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = post(&format!("https://127.0.0.1:{port}/"), None, String::new())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, Error::Tls(m) if m.contains("needs a TLS connector")),
            "{error:?}"
        );
    }
}
