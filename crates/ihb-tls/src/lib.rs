//! Client trust roots, and the startup check that a configured CA bundle is
//! usable.
//!
//! Implements `spec/18-deployment.md` §4 and the `SSL_CERT_FILE` row of
//! `spec/02-configuration.md` §1.
//!
//! # This crate is a facade
//!
//! The implementation lives in [`rustls_extra_roots`], which was extracted
//! from this workspace and published: nothing about "the Mozilla roots plus
//! one PEM bundle" is specific to the Integration Hub, and every
//! corporate-CA-behind-a-proxy deployment needs it. What stays here is the
//! part that *is* ours — which clauses these functions implement, and the
//! decision to spell the variable `SSL_CERT_FILE`.
//!
//! # Two rules worth knowing first
//!
//! **The bundle is additional, not a replacement.** [`CERT_FILE_VARIABLE`]
//! is spelled `SSL_CERT_FILE`, and OpenSSL gives that name the opposite
//! meaning: setting it there *replaces* the default trust store. Here it
//! **adds** to it, because `spec/02-configuration.md` §1 says "PEM bundle
//! of additional trusted roots" and because a corporate CA is issued
//! alongside the public ones rather than instead of them. A deployment that
//! set this variable expecting OpenSSL's meaning would get a *larger* trust
//! set than it intended, never a smaller one — the safe direction to be
//! wrong in, but still worth knowing. See `spec/18-deployment.md` §4.4.
//!
//! **A configured bundle that cannot be used is fatal.** Not a warning.
//! `spec/18-deployment.md` §4.3: silently falling back to the default roots
//! is how a certificate problem becomes a TLS handshake failure three hops
//! away, in a different service, hours later. If the operator said to load
//! a file, and the file cannot be loaded, the service does not start.
//!
//! Blank is absent, as everywhere else in this system
//! (`spec/02-configuration.md` §1): `SSL_CERT_FILE=""` means the variable
//! was not set, because a templated container definition whose source was
//! empty produces exactly that and it must not be read as "load the file
//! named empty string".
//!
//! # Examples
//!
//! The usual call. With no bundle configured this is the Mozilla root set:
//!
//! ```
//! let config = ihb_tls::client_config().expect("no bundle is configured here");
//! # let _ = config;
//! ```
//!
//! Checking a specific file, which is what the startup check does:
//!
//! ```
//! use std::path::Path;
//!
//! let error = ihb_tls::root_store_from(Some(Path::new("/nonexistent.pem")))
//!     .expect_err("the file is not there");
//!
//! // The message names the file, because "invalid certificate" without a
//! // path is the least useful message a deployment can fail with.
//! assert!(error.to_string().contains("/nonexistent.pem"));
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]

/// The environment variable naming an additional PEM bundle.
///
/// See the module documentation for why this name does not carry its
/// OpenSSL meaning here. `rustls-extra-roots` makes the name a parameter;
/// this workspace fixes it, because `spec/02-configuration.md` §1 does.
pub const CERT_FILE_VARIABLE: &str = rustls_extra_roots::DEFAULT_VARIABLE;

pub use rustls_extra_roots::{
    TrustError, configured_bundle, root_store, root_store_from, startup_check, validate_at_startup,
};

use std::sync::Arc;

use tokio_rustls::rustls::crypto::{CryptoProvider, ring};
use tokio_rustls::rustls::{ClientConfig, ConfigBuilder, ServerConfig, WantsVerifier};

/// The crypto provider every TLS configuration here is built with: `ring`.
///
/// **Named, never defaulted.** `tiberius` 0.13 depends on `tokio-rustls`
/// with its default features, which adds `aws-lc-rs` next to `ring`. With two
/// providers compiled in, `rustls` cannot choose one and `ClientConfig::builder()`
/// panics at runtime — which is how the dashboard's tests failed under
/// `cargo test --workspace` and passed under `-p dashboard`
/// (`spec/12-message-store.md` §5.8.5). Naming the provider makes the choice
/// independent of which crates a binary happens to link.
#[must_use]
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

/// A client configuration builder on [`provider`].
///
/// # Panics
///
/// Never in practice: `ring` supports the default protocol versions.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "ring supports the default versions; a failure here is a build defect"
)]
pub fn client_builder() -> ConfigBuilder<ClientConfig, WantsVerifier> {
    ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports rustls's default protocol versions")
}

/// A client configuration trusting the Mozilla roots plus any configured bundle.
///
/// Built on [`provider`]. It replaces `rustls_extra_roots::client_config`,
/// which consults the process-default provider and so panics once two are
/// linked.
///
/// # Errors
///
/// [`TrustError`], as [`root_store`].
pub fn client_config() -> Result<ClientConfig, TrustError> {
    Ok(client_builder()
        .with_root_certificates(root_store()?)
        .with_no_client_auth())
}

/// A server configuration builder on [`provider`].
///
/// # Panics
///
/// Never in practice: `ring` supports the default protocol versions.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "ring supports the default versions; a failure here is a build defect"
)]
pub fn server_builder() -> ConfigBuilder<ServerConfig, WantsVerifier> {
    ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports rustls's default protocol versions")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn the_builders_do_not_depend_on_a_process_default_provider() {
        // `spec/12-message-store.md` §5.8.5: this must hold however many
        // providers the binary links, and no default is installed here.
        assert!(
            tokio_rustls::rustls::crypto::CryptoProvider::get_default().is_none(),
            "this test is only meaningful with no process default"
        );
        let _client = super::client_builder();
        let _server = super::server_builder();
    }

    use super::*;

    #[test]
    fn the_variable_is_the_one_the_specification_names() {
        // spec/02-configuration.md §1. `rustls-extra-roots` lets a caller
        // choose the name; this workspace does not, and this test is what
        // notices if the upstream default ever moves.
        assert_eq!(CERT_FILE_VARIABLE, "SSL_CERT_FILE");
    }

    #[test]
    fn no_bundle_configured_is_not_an_error() {
        // spec/18-deployment.md §4.3 only bites when a bundle *is* named.
        assert!(root_store_from(None).is_ok());
    }

    #[test]
    fn a_missing_file_is_a_startup_failure_naming_the_file() {
        // spec/18-deployment.md §4.3, asserted here rather than only
        // upstream: it is this workspace's clause, and a facade that
        // re-exported the wrong function would still compile.
        let error = root_store_from(Some(Path::new("/no/such/bundle.pem")))
            .expect_err("a configured bundle that cannot be read is fatal");

        assert!(
            error.to_string().contains("/no/such/bundle.pem"),
            "the message must name the file: {error}"
        );
    }
}
