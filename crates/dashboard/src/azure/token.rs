//! Getting and holding an AAD token.
//!
//! Implements `spec/15-dashboard.md` §4.1 and §4.6.1–§4.6.4.
//!
//! # Two ways in, and only two
//!
//! §4.1: a service principal when all three variables are configured,
//! otherwise the ambient credential — which inside Container Apps means
//! `IDENTITY_ENDPOINT`, and **not** IMDS (§4.6.2).
//!
//! Leaving IMDS out is deliberate. A request to `169.254.169.254` from a
//! machine that is not in Azure does not fail fast; it hangs until the
//! socket times out, once per panel per refresh. On a developer's laptop
//! that turns §1.2's "works with no Azure" into "works with no Azure, after
//! a minute".

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::azure::http::{HttpError, HttpsClient};
use crate::config::AzureAuth;

/// The ARM scope (§4.6.1).
pub const ARM_SCOPE: &str = "https://management.azure.com/.default";

/// The Log Analytics scope (§4.6.1).
///
/// A **different** token from [`ARM_SCOPE`]. Sending the ARM one to Log
/// Analytics returns a `401` that looks exactly like a missing role
/// assignment, which is a long afternoon.
pub const LOGS_SCOPE: &str = "https://api.loganalytics.io/.default";

/// The managed-identity API version (§4.6.2).
pub const IDENTITY_API_VERSION: &str = "2019-08-01";

/// How long before a token's stated expiry it is treated as expired (§4.6.3).
///
/// Five minutes: long enough that a token is never used in the last moments
/// of its life, short enough that the cache still does its job across an
/// hour-long token.
pub const EXPIRY_MARGIN: Duration = Duration::from_secs(300);

/// A token and when it stops being usable.
#[derive(Debug, Clone)]
struct Held {
    /// The bearer token.
    value: String,
    /// When it may no longer be used — already reduced by [`EXPIRY_MARGIN`].
    usable_until: Instant,
}

/// Where tokens come from.
///
/// One per process, shared by every integration. §4.6.3: an AAD token lasts
/// about an hour and every panel needs one, so fetching per request would
/// turn one page load into a dozen token requests and meet AAD's own
/// throttle.
pub struct TokenSource {
    /// How to authenticate (§4.1).
    auth: AzureAuth,
    /// The client used to fetch one.
    client: HttpsClient,
    /// Held tokens, by scope. Different scopes are different tokens
    /// (§4.6.1), so this cannot be a single slot.
    held: Mutex<BTreeMap<String, Held>>,
}

impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the tokens, and never the client secret.
        f.debug_struct("TokenSource")
            .field("auth", &self.auth.describe())
            .finish_non_exhaustive()
    }
}

impl TokenSource {
    /// A source using this credential.
    #[must_use]
    pub fn new(auth: AzureAuth, client: HttpsClient) -> Self {
        Self {
            auth,
            client,
            held: Mutex::new(BTreeMap::new()),
        }
    }

    /// A usable token for `scope`, from the cache when there is one.
    ///
    /// # Errors
    ///
    /// [`HttpError`] when one could not be obtained. §4.6.4 makes that an
    /// empty result for the caller rather than a failed page — but the
    /// reason is returned so it can be logged, because otherwise nobody
    /// ever finds out why every panel is blank.
    pub async fn token(&self, scope: &str) -> Result<String, HttpError> {
        // The lock is held across the fetch, so a page fanning out to four
        // integrations at once makes **one** token request rather than
        // four. That is the whole point of §4.6.3.
        let mut held = self.held.lock().await;

        if let Some(token) = held.get(scope)
            && Instant::now() < token.usable_until
        {
            return Ok(token.value.clone());
        }

        let fresh = self.fetch(scope).await?;
        held.insert(scope.to_string(), fresh.clone());

        Ok(fresh.value)
    }

    /// Fetch a token, by whichever route §4.1 selected.
    ///
    /// The request shapes, the scope-to-resource conversion and the two
    /// spellings of `expires_in` all live in [`azure_token_lite`]; what
    /// stays here is the transport, because this workspace has exactly one
    /// HTTPS client and §4.6's whole point is not acquiring a second one.
    async fn fetch(&self, scope: &str) -> Result<Held, HttpError> {
        let credential = match &self.auth {
            AzureAuth::ServicePrincipal {
                tenant_id,
                client_id,
                client_secret,
            } => azure_token_lite::Credential::client_secret(
                tenant_id,
                client_id,
                client_secret.expose(),
            ),
            // §4.6.2 — Container Apps, not IMDS. `azure-token-lite` leaves
            // IMDS out for the same reason: a request to 169.254.169.254
            // from a machine that is not in Azure hangs rather than fails.
            AzureAuth::Ambient { client_id } => azure_token_lite::Credential::ManagedIdentity {
                client_id: client_id.clone(),
            },
        };

        let request = credential
            .request(scope)
            .map_err(|error| HttpError::Url(format!("{error} (spec/15-dashboard.md §4.6.2)")))?;

        let headers: Vec<(&str, &str)> = request
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let document = match request.body {
            Some(body) => self.client.post_form(&request.url, body).await?,
            None => self.client.get_json_with(&request.url, &headers).await?,
        };

        parse(&document)
    }
}

/// Read a token response (§4.6.3).
///
/// Delegates to [`azure_token_lite::parse_response`], which accepts
/// `expires_in` as a number (AAD) and as a **string** (the identity
/// endpoint), and treats a response with neither as already needing a
/// refresh rather than as valid forever.
fn parse(document: &serde_json::Value) -> Result<Held, HttpError> {
    let token = azure_token_lite::parse_response(&document.to_string())
        .map_err(|error| HttpError::Body(error.to_string()))?;

    Ok(Held {
        value: token.access_token,
        usable_until: Instant::now() + token.expires_in.saturating_sub(EXPIRY_MARGIN),
    })
}

/// The `resource` an identity endpoint wants, from a `.default` scope.
///
/// The two token endpoints disagree about spelling: AAD v2 takes a
/// **scope** ending in `/.default`, the identity endpoint takes a
/// **resource** without it. Sending one form to the other is a `400` that
/// says nothing useful.
///
/// # Examples
///
/// ```
/// use dashboard::azure::token::{resource_of, ARM_SCOPE};
///
/// assert_eq!(resource_of(ARM_SCOPE), "https://management.azure.com/");
/// assert_eq!(resource_of("https://x/"), "https://x/", "already a resource");
/// ```
#[must_use]
pub fn resource_of(scope: &str) -> &str {
    azure_token_lite::resource_of(scope)
}

/// Percent-encode a form value.
///
/// A client secret can contain any of `+ / = &`, and every one of them
/// changes the meaning of a form body.
#[must_use]
pub fn form_encode(value: &str) -> String {
    azure_token_lite::form_encode(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ihb_config::Secret;

    fn principal() -> AzureAuth {
        AzureAuth::ServicePrincipal {
            tenant_id: "tenant".to_string(),
            client_id: "client".to_string(),
            client_secret: Secret::new("s3cr3t"),
        }
    }

    #[test]
    fn the_two_scopes_are_different_tokens() {
        // spec/15-dashboard.md §4.6.1. Sending the ARM token to Log
        // Analytics returns a 401 that looks exactly like a missing role
        // assignment.
        assert_ne!(ARM_SCOPE, LOGS_SCOPE);
        assert!(ARM_SCOPE.starts_with("https://management.azure.com"));
        assert!(LOGS_SCOPE.starts_with("https://api.loganalytics.io"));
    }

    #[test]
    fn a_scope_becomes_a_resource_for_the_identity_endpoint() {
        // The two token endpoints disagree about spelling, and sending one
        // form to the other is a 400 that says nothing useful.
        assert_eq!(resource_of(ARM_SCOPE), "https://management.azure.com/");
        assert_eq!(resource_of(LOGS_SCOPE), "https://api.loganalytics.io/");
        assert_eq!(resource_of("https://x/"), "https://x/");
    }

    #[test]
    fn a_secret_is_form_encoded() {
        // THE subtle one. A client secret routinely contains `+`, `/` and
        // `=`; unencoded, `+` reads as a space and the token endpoint
        // answers `invalid_client` for a secret that is perfectly correct.
        assert_eq!(form_encode("a+b"), "a%2Bb");
        assert_eq!(form_encode("a/b="), "a%2Fb%3D");
        assert_eq!(form_encode("a&b"), "a%26b");
        assert_eq!(form_encode("plain-Value_1.0~"), "plain-Value_1.0~");
        assert_eq!(
            form_encode(ARM_SCOPE),
            "https%3A%2F%2Fmanagement.azure.com%2F.default"
        );
    }

    #[test]
    fn a_token_response_is_read_from_either_shape() {
        // AAD returns `expires_in` as a number; the identity endpoint
        // returns it as a string. Both are real, and rejecting either
        // would work in one environment and not the other.
        let numeric = serde_json::json!({"access_token": "abc", "expires_in": 3600});
        let text = serde_json::json!({"access_token": "abc", "expires_in": "3600"});

        for document in [numeric, text] {
            let held = parse(&document).expect("a token");
            assert_eq!(held.value, "abc");
            // 3600 seconds less the 300-second margin.
            let remaining = held.usable_until - Instant::now();
            assert!(
                remaining > Duration::from_secs(3290) && remaining <= Duration::from_secs(3300),
                "{remaining:?}"
            );
        }
    }

    #[test]
    fn a_response_with_no_token_is_an_error() {
        let error = parse(&serde_json::json!({"error": "invalid_client"})).unwrap_err();

        assert!(matches!(error, HttpError::Body(_)), "{error:?}");
    }

    #[test]
    fn a_token_shorter_than_the_margin_is_never_treated_as_usable() {
        // §4.6.3's margin, at its edge. A 60-second token is not cached as
        // usable for -240 seconds; it is simply already due for refresh.
        let held = parse(&serde_json::json!({"access_token": "abc", "expires_in": 60})).unwrap();

        assert!(
            held.usable_until <= Instant::now(),
            "a token shorter than the margin must not read as fresh"
        );
    }

    #[test]
    fn a_response_with_no_expiry_is_not_cached_forever() {
        // A malformed response must not pin a token that stopped working
        // an hour ago.
        let held = parse(&serde_json::json!({"access_token": "abc"})).unwrap();

        assert!(held.usable_until <= Instant::now());
    }

    #[tokio::test]
    async fn without_an_identity_endpoint_the_ambient_credential_says_so() {
        // §4.6.2 — and it says so *immediately*. The reason IMDS is not
        // attempted is that it would hang here, once per panel per
        // refresh, on every developer's laptop.
        //
        // `IDENTITY_ENDPOINT` is read from the process environment, which
        // this test does not set. Removing it is not safe under parallel
        // tests, so this asserts only on the behaviour when it is absent —
        // which is what CI and a laptop both have.
        if std::env::var("IDENTITY_ENDPOINT").is_ok() {
            return;
        }

        let source = TokenSource::new(
            AzureAuth::Ambient { client_id: None },
            HttpsClient::new().expect("no CA bundle is configured in tests"),
        );

        let started = Instant::now();
        let error = source.token(ARM_SCOPE).await.unwrap_err();

        assert!(error.to_string().contains("IDENTITY_ENDPOINT"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it must fail fast rather than hang: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_secret_never_reaches_a_debug_line() {
        // `spec/04-observability.md` §2 and §5.7's rule, applied to the
        // credential rather than to a payload.
        let source = TokenSource::new(
            principal(),
            HttpsClient::new().expect("no CA bundle is configured in tests"),
        );

        let rendered = format!("{source:?}");
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(rendered.contains("service principal"), "{rendered}");
    }
}
