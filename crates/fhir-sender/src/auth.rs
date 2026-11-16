//! Authentication for Promptly requests.
//!
//! Implements `spec/10-senders.md` §8.3. Upstream: `fhir_sender/auth.py`.
//!
//! The IG allows exactly one mechanism per environment: an API key in
//! `X-API-Key` (active today), or an `OAuth2` client-credentials bearer token
//! (the target), which replaces the key.

use std::fmt::Write as _;
use std::time::Duration;

use ihb_config::Secret;

use crate::client::Clock;
use crate::error::DeliveryError;
use crate::http::{Method, Request, Transport};

/// How long a token is assumed to live when the endpoint does not say.
pub const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;

/// A token is refreshed this long before it expires.
pub const TOKEN_REFRESH_MARGIN_SECONDS: f64 = 60.0;

/// The credentials in use.
#[derive(Debug)]
pub enum Auth {
    /// `X-API-Key`.
    ApiKey(Secret),
    /// A bearer token fetched with the client-credentials grant.
    OAuth2(OAuth2),
}

/// An `OAuth2` client-credentials source with a cached token.
#[derive(Debug)]
pub struct OAuth2 {
    /// The token endpoint, `https` only.
    token_url: String,
    /// `FHIR_OAUTH_CLIENT_ID`.
    client_id: String,
    /// `FHIR_OAUTH_CLIENT_SECRET`; never printed.
    client_secret: Secret,
    /// `FHIR_OAUTH_SCOPE`.
    scope: String,
    /// Bounds the token request.
    timeout: Duration,
    /// The cached token, discarded on a `401`.
    token: Option<Secret>,
    /// When the cached token expires, on the [`Clock`]'s scale.
    expires_at: f64,
}

impl Auth {
    /// An API key.
    ///
    /// # Errors
    ///
    /// A message when the key is blank.
    pub fn api_key(key: Secret) -> Result<Self, String> {
        if key.expose().trim().is_empty() {
            return Err("API key must not be empty".to_string());
        }
        Ok(Self::ApiKey(key))
    }

    /// An `OAuth2` client-credentials source.
    ///
    /// # Errors
    ///
    /// A message when `token_url` is not `https`.
    pub fn oauth2(
        token_url: &str,
        client_id: &str,
        client_secret: Secret,
        scope: &str,
        timeout: Duration,
    ) -> Result<Self, String> {
        if !token_url.to_ascii_lowercase().starts_with("https://") {
            return Err("OAuth2 token endpoint must use HTTPS".to_string());
        }
        Ok(Self::OAuth2(OAuth2 {
            token_url: token_url.to_string(),
            client_id: client_id.to_string(),
            client_secret,
            scope: scope.to_string(),
            timeout,
            token: None,
            expires_at: 0.0,
        }))
    }

    /// The headers for the next request, fetching a token first when the
    /// cached one is missing or about to expire.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::Authorization`] when the token endpoint rejects the
    /// client; [`DeliveryError::Transient`] when it cannot be reached or
    /// answers unreadably.
    pub async fn headers<T: Transport, C: Clock>(
        &mut self,
        transport: &mut T,
        clock: &C,
    ) -> Result<Vec<(String, String)>, DeliveryError> {
        match self {
            Self::ApiKey(key) => Ok(vec![("X-API-Key".to_string(), key.expose().to_string())]),
            Self::OAuth2(oauth) => {
                if oauth.token.is_none()
                    || clock.now() >= oauth.expires_at - TOKEN_REFRESH_MARGIN_SECONDS
                {
                    oauth.fetch(transport, clock).await?;
                }
                let token = oauth.token.as_ref().map_or("", Secret::expose);
                Ok(vec![(
                    "Authorization".to_string(),
                    format!("Bearer {token}"),
                )])
            }
        }
    }

    /// Discard cached credentials after a `401`. `true` when a retry with
    /// fresh credentials is worthwhile (only a token can be refreshed).
    pub fn invalidate(&mut self) -> bool {
        match self {
            Self::ApiKey(_) => false,
            Self::OAuth2(oauth) => {
                oauth.token = None;
                true
            }
        }
    }
}

impl OAuth2 {
    /// Request a token and cache it with its expiry.
    async fn fetch<T: Transport, C: Clock>(
        &mut self,
        transport: &mut T,
        clock: &C,
    ) -> Result<(), DeliveryError> {
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.expose()),
            ("scope", self.scope.as_str()),
        ]
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&");

        let response = transport
            .send(Request {
                method: Method::Post,
                url: self.token_url.clone(),
                headers: vec![(
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                )],
                body: Some(form.into_bytes()),
                timeout: self.timeout,
            })
            .await
            .map_err(|error| DeliveryError::Transient(format!("Token request failed: {error}")))?;

        if matches!(response.status, 400 | 401 | 403) {
            return Err(DeliveryError::Authorization {
                status: response.status,
                detail: "Token endpoint rejected the client credentials".to_string(),
            });
        }
        if response.status != 200 {
            return Err(DeliveryError::Transient(format!(
                "Token endpoint returned HTTP {}",
                response.status
            )));
        }

        let unreadable = || {
            DeliveryError::Transient("Token endpoint returned an unreadable response".to_string())
        };
        let payload = response.json().ok_or_else(unreadable)?;
        let token = payload
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(unreadable)?;
        let lifetime = match payload.get("expires_in") {
            None => DEFAULT_TOKEN_LIFETIME_SECONDS,
            Some(serde_json::Value::Number(number)) => number.as_f64().ok_or_else(unreadable)?,
            Some(serde_json::Value::String(text)) => {
                text.trim().parse::<f64>().map_err(|_| unreadable())?
            }
            Some(_) => return Err(unreadable()),
        };

        self.token = Some(Secret::new(token));
        self.expires_at = clock.now() + lifetime;
        tracing::info!(
            lifetime_seconds = lifetime,
            "obtained an OAuth2 access token"
        );
        Ok(())
    }
}

/// `application/x-www-form-urlencoded` for one key or value.
fn form_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{FakeClock, FakeTransport};
    use crate::http::Response;

    fn token(body: &str) -> Response {
        Response {
            status: 200,
            headers: vec![],
            body: body.as_bytes().to_vec(),
        }
    }

    fn oauth() -> Auth {
        Auth::oauth2(
            "https://auth.example/token",
            "id",
            Secret::new("s3cret/+"),
            "target-entity:7",
            Duration::from_secs(5),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn an_api_key_goes_in_x_api_key_and_cannot_be_refreshed() {
        // spec/10-senders.md §8.3.
        let mut auth = Auth::api_key(Secret::new("k")).unwrap();
        let headers = auth
            .headers(&mut FakeTransport::new(vec![]), &FakeClock::new())
            .await
            .unwrap();
        assert_eq!(headers, [("X-API-Key".to_string(), "k".to_string())]);
        assert!(!auth.invalidate());
        assert!(Auth::api_key(Secret::new("  ")).is_err());
    }

    #[tokio::test]
    async fn a_token_is_fetched_once_and_reused_until_near_expiry() {
        let mut transport = FakeTransport::new(vec![
            Ok(token(r#"{"access_token":"t1","expires_in":300}"#)),
            Ok(token(r#"{"access_token":"t2","expires_in":300}"#)),
        ]);
        let clock = FakeClock::new();
        let mut auth = oauth();

        let first = auth.headers(&mut transport, &clock).await.unwrap();
        assert_eq!(first[0].1, "Bearer t1");
        let again = auth.headers(&mut transport, &clock).await.unwrap();
        assert_eq!(again[0].1, "Bearer t1");
        assert_eq!(transport.sent.len(), 1, "reused");

        // Inside the 60 s refresh margin of a 300 s token.
        clock.advance(250.0);
        let refreshed = auth.headers(&mut transport, &clock).await.unwrap();
        assert_eq!(refreshed[0].1, "Bearer t2");
        assert_eq!(transport.sent.len(), 2);
    }

    #[tokio::test]
    async fn the_token_request_is_a_form_with_the_secret_encoded() {
        let mut transport = FakeTransport::new(vec![Ok(token(r#"{"access_token":"t"}"#))]);
        oauth()
            .headers(&mut transport, &FakeClock::new())
            .await
            .unwrap();
        let body = String::from_utf8(transport.sent[0].body.clone().unwrap()).unwrap();
        assert_eq!(
            body,
            "grant_type=client_credentials&client_id=id&client_secret=s3cret%2F%2B&scope=target-entity%3A7"
        );
    }

    #[tokio::test]
    async fn a_rejected_client_is_an_authorization_failure_and_a_server_error_is_transient() {
        for status in [400, 401, 403] {
            let mut transport = FakeTransport::new(vec![Ok(Response {
                status,
                headers: vec![],
                body: vec![],
            })]);
            let error = oauth()
                .headers(&mut transport, &FakeClock::new())
                .await
                .unwrap_err();
            assert!(
                matches!(error, DeliveryError::Authorization { .. }),
                "{status}"
            );
        }
        let mut transport = FakeTransport::new(vec![Ok(Response {
            status: 503,
            headers: vec![],
            body: vec![],
        })]);
        let error = oauth()
            .headers(&mut transport, &FakeClock::new())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            DeliveryError::Transient("Token endpoint returned HTTP 503".to_string())
        );
    }

    #[tokio::test]
    async fn an_unreadable_token_response_is_transient_and_never_echoes_it() {
        for body in [
            "not json",
            r#"{"no_token":1}"#,
            r#"{"access_token":"t","expires_in":[]}"#,
        ] {
            let mut transport = FakeTransport::new(vec![Ok(token(body))]);
            let error = oauth()
                .headers(&mut transport, &FakeClock::new())
                .await
                .unwrap_err();
            assert_eq!(
                error,
                DeliveryError::Transient(
                    "Token endpoint returned an unreadable response".to_string()
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn the_token_endpoint_must_be_https_and_invalidate_forces_a_refresh() {
        assert!(
            Auth::oauth2(
                "http://auth/t",
                "i",
                Secret::new("s"),
                "x",
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(oauth().invalidate());
    }

    #[test]
    fn debug_output_does_not_contain_the_secrets() {
        let text = format!(
            "{:?} {:?}",
            oauth(),
            Auth::api_key(Secret::new("k-9")).unwrap()
        );
        assert!(!text.contains("s3cret"), "{text}");
        assert!(!text.contains("k-9"), "{text}");
    }
}
