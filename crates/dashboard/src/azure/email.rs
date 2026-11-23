//! Escalation e-mail through Azure Communication Services.
//!
//! Implements `spec/15-dashboard.md` §5.2 and §5.8.
//!
//! # Sending can never fail an evaluation
//!
//! §5.8.3. An alarm that could not be e-mailed is still an alarm on the
//! page, and an ACS outage must not stop the dashboard evaluating the other
//! rules. So [`Mailer::send`] returns whether it worked and nothing above
//! it treats that as fatal.

use std::time::Duration;

use ihb_config::Secret;

use crate::azure::http::{HttpError, HttpsClient};

/// The ACS e-mail API version (§5.8.1).
///
/// Pinned here for the reason the ARM versions are: the SDK owned it in the
/// Python and this workspace owns it now.
pub const ACS_API_VERSION: &str = "2023-03-31";

/// Where an ACS connection string's parts come from.
///
/// The string is `endpoint=https://x.communication.azure.com/;accesskey=...`.
#[derive(Debug, Clone)]
pub struct AcsEndpoint {
    /// The `endpoint=` part.
    pub endpoint: String,
    /// The `accesskey=` part.
    pub access_key: Secret,
}

impl AcsEndpoint {
    /// Parse a connection string.
    ///
    /// # Errors
    ///
    /// A message naming what is missing — never the string itself (§5.8.6).
    ///
    /// # Examples
    ///
    /// ```
    /// use dashboard::azure::email::AcsEndpoint;
    ///
    /// let parsed = AcsEndpoint::parse(
    ///     "endpoint=https://x.communication.azure.com/;accesskey=c2VjcmV0",
    /// )
    /// .expect("a well-formed connection string");
    ///
    /// assert_eq!(parsed.endpoint, "https://x.communication.azure.com");
    /// ```
    pub fn parse(connection_string: &str) -> Result<Self, String> {
        let mut endpoint = None;
        let mut access_key = None;

        for part in connection_string.split(';') {
            let Some((name, value)) = part.split_once('=') else {
                continue;
            };
            // `split_once` stops at the first `=`, so a base64 key keeping
            // its padding survives intact.
            match name.trim().to_ascii_lowercase().as_str() {
                "endpoint" => endpoint = Some(value.trim().trim_end_matches('/').to_string()),
                "accesskey" => access_key = Some(Secret::new(value.trim())),
                _ => {}
            }
        }

        match (endpoint, access_key) {
            (Some(endpoint), Some(access_key)) => Ok(Self {
                endpoint,
                access_key,
            }),
            // §5.8.6 — the string is not echoed back.
            (None, _) => Err("the ACS connection string has no endpoint= part".to_string()),
            (_, None) => Err("the ACS connection string has no accesskey= part".to_string()),
        }
    }
}

/// Everything needed to send.
///
/// Built only when §5.8.5 is satisfied: a partially configured deployment
/// has no [`Mailer`] at all, rather than one that fails on every send.
#[derive(Debug)]
pub struct Mailer {
    /// Where to send.
    acs: AcsEndpoint,
    /// `ALERT_EMAIL_FROM`.
    from: String,
    /// `ALERT_EMAIL_TO`.
    to: String,
    /// `ALERT_EMAIL_MAX_RETRIES`.
    max_retries: u32,
    /// `ALERT_EMAIL_RETRY_BACKOFF_SECONDS`.
    backoff: Duration,
    /// The HTTPS client.
    client: HttpsClient,
}

impl Mailer {
    /// A mailer, when every part of §5.8.5 is present.
    ///
    /// Returns `None` when it is not — which is not an error: most
    /// deployments do not send e-mail, and §1.2 makes that normal.
    #[must_use]
    pub fn new(
        connection_string: Option<&Secret>,
        from: Option<&str>,
        to: Option<&str>,
        max_retries: u32,
        backoff: Duration,
        client: HttpsClient,
    ) -> Option<Self> {
        // §5.8.5 — every part, or none. A half-configured mailer is worse
        // than none: it makes the page offer a control that does nothing.
        let acs = AcsEndpoint::parse(connection_string?.expose()).ok()?;
        let from = from.filter(|value| !value.trim().is_empty())?;
        let to = to.filter(|value| !value.trim().is_empty())?;

        Some(Self {
            acs,
            from: from.to_string(),
            to: to.to_string(),
            max_retries,
            backoff,
            client,
        })
    }

    /// The message body ACS expects (§5.8.2).
    #[must_use]
    pub fn message(&self, subject: &str, html: &str) -> String {
        serde_json::json!({
            "senderAddress": self.from,
            "recipients": { "to": [{ "address": self.to }] },
            "content": { "subject": subject, "html": html },
        })
        .to_string()
    }

    /// Send one message.
    ///
    /// Returns whether it was sent. **Never** an error the caller must
    /// handle: §5.8.3 makes an ACS failure a log line, not something that
    /// can stop an evaluation cycle.
    pub async fn send(&self, subject: &str, html: &str) -> bool {
        let body = self.message(subject, html);
        let url = format!(
            "{}/emails:send?api-version={ACS_API_VERSION}",
            self.acs.endpoint
        );

        for attempt in 0..=self.max_retries {
            match self.attempt(&url, &body).await {
                Ok(()) => return true,
                Err(error) => {
                    let retryable = matches!(error, HttpError::Status { code: 429, .. });
                    // §5.8.4 — the sandbox quota is low and several alarms
                    // firing in one cycle reach it easily. The wait doubles
                    // each time; one that cannot be computed is not
                    // invented (no ceiling is declared), so the send gives
                    // up as it does when the retries run out (RCA-INT-4).
                    let wait = 2_u32
                        .checked_pow(attempt)
                        .and_then(|factor| self.backoff.checked_mul(factor));
                    if retryable
                        && attempt < self.max_retries
                        && let (Some(wait), Some(next)) = (wait, attempt.checked_add(1))
                    {
                        tracing::warn!(
                            %error,
                            attempt = next,
                            wait_seconds = wait.as_secs(),
                            "ACS throttled the alert e-mail; retrying"
                        );
                        tokio::time::sleep(wait).await;
                        continue;
                    }

                    tracing::error!(%error, %subject, "could not send the alert e-mail");
                    return false;
                }
            }
        }

        false
    }

    /// One send attempt.
    async fn attempt(&self, url: &str, body: &str) -> Result<(), HttpError> {
        // §5.8.1 — the date, the host, the path *with* its query string and
        // a hash of the body are all signed, and the two extra headers are
        // sent because the service recomputes the signature from them. See
        // `acs_signature` for what each of those gets wrong on its own.
        let host = url
            .split_once("://")
            .and_then(|(_, rest)| rest.split('/').next())
            .unwrap_or_default()
            .to_string();
        let path = url.split_once(&host).map_or("/", |(_, rest)| rest);

        let signer = acs_signature::Signer::new(self.acs.access_key.expose()).map_err(|error| {
            // §5.8.6 — the key never appears.
            HttpError::Url(match error {
                acs_signature::KeyError::NotBase64 => {
                    "the ACS access key is not valid base64".to_string()
                }
                acs_signature::KeyError::Unusable => "the ACS access key is not usable".to_string(),
            })
        })?;

        let request = signer.sign("POST", path, &host, &acs_signature::now(), body.as_bytes());

        self.client
            .post_json_with(url, &request.headers(), body.to_string())
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> HttpsClient {
        HttpsClient::new().expect("no CA bundle is configured in tests")
    }

    fn mailer() -> Mailer {
        Mailer::new(
            Some(&Secret::new(
                "endpoint=https://x.communication.azure.com/;accesskey=c2VjcmV0LWtleQ==",
            )),
            Some("noreply@example.nhs.wales"),
            Some("noc@example.nhs.wales"),
            3,
            Duration::from_secs(2),
            client(),
        )
        .expect("every part is configured")
    }

    #[test]
    fn a_connection_string_is_split_into_its_parts() {
        // spec/15-dashboard.md §5.8.1.
        let parsed =
            AcsEndpoint::parse("endpoint=https://x.communication.azure.com/;accesskey=c2VjcmV0")
                .unwrap();

        assert_eq!(parsed.endpoint, "https://x.communication.azure.com");
        assert_eq!(parsed.access_key.expose(), "c2VjcmV0");
    }

    #[test]
    fn a_base64_key_keeps_its_padding() {
        // THE subtle one. An access key is base64 and routinely ends in
        // `=`; splitting on every `=` rather than the first would truncate
        // it, and the signature would be wrong in a way that looks like a
        // permissions problem.
        let parsed = AcsEndpoint::parse(
            "endpoint=https://x.communication.azure.com/;accesskey=YWJjZGVmZ2g=",
        )
        .unwrap();

        assert_eq!(parsed.access_key.expose(), "YWJjZGVmZ2g=");
    }

    #[test]
    fn a_malformed_connection_string_is_refused_without_echoing_itself() {
        // §5.8.6 — it carries the account access key.
        for (input, missing) in [
            ("accesskey=c2VjcmV0", "endpoint"),
            ("endpoint=https://x/", "accesskey"),
            ("", "endpoint"),
        ] {
            let error = AcsEndpoint::parse(input).unwrap_err();
            assert!(error.contains(missing), "{input:?} -> {error}");
            assert!(!error.contains("c2VjcmV0"), "the key leaked: {error}");
        }
    }

    #[test]
    fn a_partially_configured_deployment_has_no_mailer() {
        // §5.8.5 — a half-configured mailer makes the page offer a control
        // that silently does nothing, which is worse than offering none.
        let full = Some(Secret::new("endpoint=https://x/;accesskey=c2VjcmV0"));

        assert!(Mailer::new(None, Some("a@b"), Some("c@d"), 3, Duration::ZERO, client()).is_none());
        assert!(
            Mailer::new(
                full.as_ref(),
                None,
                Some("c@d"),
                3,
                Duration::ZERO,
                client()
            )
            .is_none()
        );
        assert!(
            Mailer::new(
                full.as_ref(),
                Some("a@b"),
                None,
                3,
                Duration::ZERO,
                client()
            )
            .is_none()
        );
        assert!(
            Mailer::new(
                full.as_ref(),
                Some("a@b"),
                Some("  "),
                3,
                Duration::ZERO,
                client()
            )
            .is_none(),
            "a blank recipient is not a recipient"
        );

        assert!(
            Mailer::new(
                full.as_ref(),
                Some("a@b"),
                Some("c@d"),
                3,
                Duration::ZERO,
                client()
            )
            .is_some()
        );
    }

    #[test]
    fn the_message_is_the_shape_acs_expects() {
        // §5.8.2 — the Python's shape, so a working sender configuration
        // keeps working.
        let body: serde_json::Value =
            serde_json::from_str(&mailer().message("Alarm 1", "<b>x</b>")).unwrap();

        assert_eq!(body["senderAddress"], "noreply@example.nhs.wales");
        assert_eq!(
            body["recipients"]["to"][0]["address"],
            "noc@example.nhs.wales"
        );
        assert_eq!(body["content"]["subject"], "Alarm 1");
        assert_eq!(body["content"]["html"], "<b>x</b>");
    }

    #[test]
    fn the_subject_and_body_are_json_escaped() {
        // An alarm subject carries a rule's display name, which comes from
        // a form. Building the JSON by hand would let a quote break it.
        let body = mailer().message(r#"a "quoted" subject"#, "line\nbreak");

        let parsed: serde_json::Value = serde_json::from_str(&body).expect("still valid JSON");
        assert_eq!(parsed["content"]["subject"], r#"a "quoted" subject"#);
        assert_eq!(parsed["content"]["html"], "line\nbreak");
    }

    #[tokio::test]
    async fn a_send_failure_is_false_rather_than_an_error() {
        // §5.8.3 — an ACS outage must not stop the dashboard evaluating
        // the other rules. `.invalid` is reserved by RFC 2606 and never
        // resolves, so this is a real failure rather than a mocked one.
        let mailer = Mailer::new(
            Some(&Secret::new(
                "endpoint=https://dashboard-test.invalid;accesskey=c2VjcmV0",
            )),
            Some("a@b"),
            Some("c@d"),
            0,
            Duration::ZERO,
            client(),
        )
        .unwrap();

        assert!(!mailer.send("subject", "<p>body</p>").await);
    }

    #[test]
    fn the_api_version_is_pinned() {
        // §5.8.1 — ours now, as with §4.6's.
        assert_eq!(ACS_API_VERSION, "2023-03-31");
    }

    #[test]
    fn a_key_never_reaches_a_debug_line() {
        // §5.8.6.
        let rendered = format!("{:?}", mailer());

        assert!(!rendered.contains("c2VjcmV0LWtleQ"), "{rendered}");
    }
}
