//! Authentication, role checks and CSRF protection.
//!
//! Implements `spec/20-lookup-service.md` §12.
//!
//! Reference implementation: `lookup_service/lookup_service/security.py`
//! (upstream `2a220e3`, #426, "Phase 1.5").
//!
//! # Two callers, two mechanisms
//!
//! - The **lookup API** (`GET /api/v1/lookup/{table}`) takes an Entra
//!   **bearer token**, validated here against the tenant's signing keys
//!   (§12.2). A transformer is a service, not a person, and presents the
//!   token of its managed identity.
//! - The **GUI and the admin API** sit behind Container Apps *Easy Auth*,
//!   which authenticates the person and overwrites the
//!   `X-MS-CLIENT-PRINCIPAL*` headers. They are trusted **only when
//!   authentication is enabled**, i.e. when the service is reached through
//!   Easy Auth (§12.3); a deployment that left Easy Auth off would let any
//!   caller invent a principal, which is why the switch that disables
//!   authentication (§2.8) also disables the trust in those headers rather
//!   than leaving them half-honoured.
//!
//! # What is never logged
//!
//! A token, a key, a claim value or a header's contents (§12.7). A rejected
//! token logs the *kind* of failure and nothing it carried; upstream is the
//! same (`logger.warning("Rejected bearer token: %s", type(exc).__name__)`)
//! because a library's exception text can quote claim values.
//!
//! # Examples
//!
//! The role hierarchy: each role includes the ones below it.
//!
//! ```
//! use lookup_service::security::{Principal, READER_ROLES, EDITOR_ROLES, ROLE_EDITOR};
//!
//! let editor = Principal::new("a.person@example", [ROLE_EDITOR]);
//! assert!(editor.has_any(&READER_ROLES), "an editor can read");
//! assert!(editor.has_any(&EDITOR_ROLES));
//! ```

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, HeaderValue, header};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SecureRandom as _;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use serde_json::Value;

use crate::config::Config;
use crate::error::{Kind, LookupError};

/// May read tables and look keys up.
pub const ROLE_READER: &str = "Lookup.Reader";
/// May also upload and change settings.
pub const ROLE_EDITOR: &str = "Lookup.Editor";
/// Everything an editor may.
pub const ROLE_ADMIN: &str = "Lookup.Admin";

/// Roles that may read: each role includes the ones below it (§12.1).
pub const READER_ROLES: [&str; 3] = [ROLE_READER, ROLE_EDITOR, ROLE_ADMIN];
/// Roles that may write (§12.1).
pub const EDITOR_ROLES: [&str; 2] = [ROLE_EDITOR, ROLE_ADMIN];

/// Seconds of clock skew tolerated on `exp` and `nbf` (§12.2).
///
/// Upstream `CLOCK_SKEW_SECONDS`: Entra and a container's clock are not
/// perfectly aligned, and a token rejected for a second of drift is an
/// outage that looks like an authorisation failure.
pub const CLOCK_SKEW_SECONDS: u64 = 60;

/// The Easy Auth principal header (§12.3).
pub const EASY_AUTH_PRINCIPAL_HEADER: &str = "x-ms-client-principal";
/// The Easy Auth display-name header (§12.3).
pub const EASY_AUTH_NAME_HEADER: &str = "x-ms-client-principal-name";

/// Easy Auth emits role claims under either type, depending on the identity
/// provider's mapping (upstream `ROLE_CLAIM_TYPES`).
const ROLE_CLAIM_TYPES: [&str; 2] = [
    "roles",
    "http://schemas.microsoft.com/ws/2008/06/identity/claims/role",
];
/// Where a person's name may be, in order of preference.
const NAME_CLAIM_TYPES: [&str; 3] = [
    "preferred_username",
    "name",
    "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name",
];
/// The largest accepted `X-MS-CLIENT-PRINCIPAL`. A real one is a few
/// hundred bytes to a few kilobytes; the cap stops a header being used to
/// make the service decode and parse megabytes.
pub const MAX_PRINCIPAL_HEADER_LENGTH: usize = 32 * 1024;
/// The longest recorded principal name.
pub const MAX_PRINCIPAL_NAME_LENGTH: usize = 200;
/// The name when none can be read.
pub const UNKNOWN_PRINCIPAL_NAME: &str = "unknown";

/// The CSRF cookie's name on a developer machine.
pub const CSRF_COOKIE_NAME: &str = "lookup_csrf";
/// The CSRF cookie's name everywhere else.
///
/// `__Host-` pins the cookie to this exact host over HTTPS, so a sibling
/// subdomain cannot plant its own value (§12.5).
pub const CSRF_SECURE_COOKIE_NAME: &str = "__Host-lookup_csrf";
/// The header an API caller echoes the token in.
pub const CSRF_HEADER_NAME: &str = "x-csrf-token";
/// The form field a page echoes the token in.
pub const CSRF_FIELD_NAME: &str = "csrf_token";

/// Who is calling.
///
/// # Examples
///
/// ```
/// use lookup_service::security::{Principal, ROLE_READER, EDITOR_ROLES};
///
/// let reader = Principal::new("svc", [ROLE_READER]);
/// assert!(!reader.has_any(&EDITOR_ROLES));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// A name for the audit trail: recorded against uploads and settings
    /// changes. Never a token.
    pub name: String,
    /// The roles the caller holds.
    pub roles: BTreeSet<String>,
}

impl Principal {
    /// A principal with `roles`.
    #[must_use]
    pub fn new<'a>(name: impl Into<String>, roles: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            name: name.into(),
            roles: roles.into_iter().map(str::to_owned).collect(),
        }
    }

    /// Whether the caller holds at least one of `allowed`.
    #[must_use]
    pub fn has_any(&self, allowed: &[&str]) -> bool {
        allowed.iter().any(|role| self.roles.contains(*role))
    }

    /// Every caller's identity when authentication is off on a developer
    /// machine (§12.4).
    #[must_use]
    pub fn local_dev() -> Self {
        Self::new("local-dev", [ROLE_ADMIN])
    }

    /// Every caller's identity when authentication is off in a deployed
    /// environment (§12.4). The name says plainly that nobody signed in,
    /// because it is recorded against uploads and settings changes.
    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::new("unauthenticated", [ROLE_ADMIN])
    }
}

/// The principal when authentication is disabled (§12.4).
#[must_use]
pub fn auth_disabled_principal(config: &Config) -> Principal {
    if config.is_local() {
        Principal::local_dev()
    } else {
        Principal::unauthenticated()
    }
}

/// Why a signing key could not be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysError {
    /// The key set could not be fetched: `503 backend_unavailable`, because
    /// the caller did nothing wrong (§12.2).
    Unavailable,
    /// The key set has no such key id: `401`.
    Unknown,
}

/// Resolves the key that signed a token.
///
/// Synchronous because the production implementation does network I/O on a
/// miss and is called from `spawn_blocking`; a trait so the tests supply a
/// key they hold the private half of.
pub trait SigningKeys: Send + Sync {
    /// The DER `RSAPublicKey` for `kid`.
    ///
    /// # Errors
    ///
    /// [`KeysError`].
    fn signing_key(&self, kid: &str) -> Result<Vec<u8>, KeysError>;
}

/// Validates Entra access tokens: signature, issuer, audience, expiry (§12.2).
///
/// # Examples
///
/// ```
/// use lookup_service::security::{KeysError, SigningKeys, TokenVerifier};
///
/// struct NoKeys;
/// impl SigningKeys for NoKeys {
///     fn signing_key(&self, _: &str) -> Result<Vec<u8>, KeysError> {
///         Err(KeysError::Unknown)
///     }
/// }
///
/// let verifier = TokenVerifier::new("tenant", "api://lookup", std::sync::Arc::new(NoKeys));
/// let error = verifier.verify("not-a-token").unwrap_err();
/// assert_eq!(error.kind.status(), 401);
/// assert!(error.bearer_challenge);
/// ```
pub struct TokenVerifier {
    /// The `aud` a token must carry.
    audience: String,
    /// Both issuer forms of this tenant (see [`TokenVerifier::new`]).
    issuers: [String; 2],
    /// Where keys come from.
    keys: Arc<dyn SigningKeys>,
}

impl std::fmt::Debug for TokenVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenVerifier").finish_non_exhaustive()
    }
}

impl TokenVerifier {
    /// A verifier for `tenant_id`'s tokens addressed to `audience`.
    ///
    /// Two issuers are accepted: v2.0 tokens use
    /// `https://login.microsoftonline.com/{tenant}/v2.0`, but an app
    /// registration defaults to v1 tokens, issued by
    /// `https://sts.windows.net/{tenant}/`. Both are this tenant's
    /// (upstream comment in `TokenVerifier.__init__`).
    #[must_use]
    pub fn new(tenant_id: &str, audience: &str, keys: Arc<dyn SigningKeys>) -> Self {
        Self {
            audience: audience.to_owned(),
            issuers: [
                format!("https://login.microsoftonline.com/{tenant_id}/v2.0"),
                format!("https://sts.windows.net/{tenant_id}/"),
            ],
            keys,
        }
    }

    /// Verify `token` now.
    ///
    /// # Errors
    ///
    /// `401 unauthorized` with the `Bearer` challenge for a token that is
    /// malformed, forged, expired, for another audience or issuer, or signed
    /// with an unknown key; `503 backend_unavailable` when the tenant's keys
    /// cannot be fetched.
    pub fn verify(&self, token: &str) -> Result<Principal, LookupError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.verify_at(token, now)
    }

    /// As [`TokenVerifier::verify`] with the clock supplied, so a test can
    /// expire a token without sleeping.
    ///
    /// # Errors
    ///
    /// As [`TokenVerifier::verify`].
    pub fn verify_at(&self, token: &str, now: u64) -> Result<Principal, LookupError> {
        let invalid = |kind: &str| {
            // The kind only: an error's own text can quote claim values (§12.7).
            tracing::warn!(reason = kind, "Rejected bearer token");
            LookupError::bearer_unauthorized("Invalid token")
        };

        let mut parts = token.split('.');
        let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid("malformed"));
        };
        let decode = |part: &str| URL_SAFE_NO_PAD.decode(part.trim_end_matches('='));
        let (Ok(header), Ok(payload), Ok(signature)) = (
            decode(header_b64),
            decode(payload_b64),
            decode(signature_b64),
        ) else {
            return Err(invalid("malformed"));
        };
        let (Ok(header), Ok(claims)) = (
            serde_json::from_slice::<Value>(&header),
            serde_json::from_slice::<Value>(&payload),
        ) else {
            return Err(invalid("malformed"));
        };

        // Only RS256 (upstream `algorithms=["RS256"]`). Refusing the rest,
        // `none` above all, is what stops a token choosing its own check.
        if header.get("alg").and_then(Value::as_str) != Some("RS256") {
            return Err(invalid("algorithm"));
        }
        let Some(kid) = header.get("kid").and_then(Value::as_str) else {
            return Err(invalid("no key id"));
        };

        let key = match self.keys.signing_key(kid) {
            Ok(key) => key,
            Err(KeysError::Unknown) => return Err(invalid("unknown key")),
            Err(KeysError::Unavailable) => {
                tracing::error!("Could not fetch the token signing keys");
                return Err(LookupError::backend_unavailable(
                    "Token signing keys are unavailable",
                    None,
                ));
            }
        };
        let signing_input = format!("{header_b64}.{payload_b64}");
        if UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key)
            .verify(signing_input.as_bytes(), &signature)
            .is_err()
        {
            return Err(invalid("signature"));
        }

        // Required claims: exp, iss, aud (upstream `options={"require": ...}`).
        let Some(exp) = claims.get("exp").and_then(Value::as_f64) else {
            return Err(invalid("missing exp"));
        };
        let (Some(now_f), Some(skew)) = (exact_f64(now), exact_f64(CLOCK_SKEW_SECONDS)) else {
            return Err(invalid("clock out of range"));
        };
        if now_f > exp + skew {
            tracing::warn!(reason = "expired", "Rejected bearer token");
            return Err(LookupError::bearer_unauthorized("Token has expired"));
        }
        if let Some(nbf) = claims.get("nbf").and_then(Value::as_f64)
            && nbf > now_f + skew
        {
            return Err(invalid("not yet valid"));
        }
        let issuer_ok = claims
            .get("iss")
            .and_then(Value::as_str)
            .is_some_and(|iss| self.issuers.iter().any(|expected| expected == iss));
        if !issuer_ok {
            return Err(invalid("issuer"));
        }
        let audience_ok = match claims.get("aud") {
            Some(Value::String(aud)) => *aud == self.audience,
            Some(Value::Array(list)) => list.iter().any(|a| a.as_str() == Some(&self.audience)),
            _ => false,
        };
        if !audience_ok {
            return Err(invalid("audience"));
        }

        Ok(Principal {
            name: bearer_name(&claims),
            roles: string_set(claims.get("roles")),
        })
    }
}

/// A name for the audit trail from a token's claims, in upstream's order.
fn bearer_name(claims: &Value) -> String {
    for claim in ["preferred_username", "azp", "appid", "oid", "sub"] {
        if let Some(value) = claims.get(claim).and_then(Value::as_str)
            && !value.is_empty()
        {
            return truncate(value);
        }
    }
    UNKNOWN_PRINCIPAL_NAME.to_owned()
}

/// At most [`MAX_PRINCIPAL_NAME_LENGTH`] characters.
fn truncate(value: &str) -> String {
    value.chars().take(MAX_PRINCIPAL_NAME_LENGTH).collect()
}

/// A string or a list of strings as a set; anything else is empty.
fn string_set(value: Option<&Value>) -> BTreeSet<String> {
    match value {
        Some(Value::String(one)) => BTreeSet::from([one.clone()]),
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// Decode the base64 JSON `X-MS-CLIENT-PRINCIPAL` header (§12.3).
///
/// # Errors
///
/// `401 unauthorized` when the header is absent, over
/// [`MAX_PRINCIPAL_HEADER_LENGTH`], not valid base64, not JSON, or has no
/// `claims` list.
///
/// # Examples
///
/// ```
/// use base64::Engine as _;
/// use lookup_service::security::parse_easy_auth_principal;
///
/// let body = br#"{"claims":[{"typ":"roles","val":"Lookup.Reader"},
///                            {"typ":"name","val":"Ada"}]}"#;
/// let encoded = base64::engine::general_purpose::STANDARD.encode(body);
///
/// let principal = parse_easy_auth_principal(Some(&encoded), None)?;
/// assert_eq!(principal.name, "Ada");
/// assert!(principal.roles.contains("Lookup.Reader"));
/// # Ok::<(), lookup_service::error::LookupError>(())
/// ```
pub fn parse_easy_auth_principal(
    encoded: Option<&str>,
    name_header: Option<&str>,
) -> Result<Principal, LookupError> {
    let unauthorized = |detail: &str| LookupError::new(Kind::Unauthorized, detail);
    let Some(encoded) = encoded.filter(|e| !e.is_empty()) else {
        return Err(unauthorized("Authentication required"));
    };
    let invalid = || unauthorized("Invalid authentication principal");
    if encoded.len() > MAX_PRINCIPAL_HEADER_LENGTH {
        return Err(invalid());
    }
    let decoded = STANDARD.decode(encoded).map_err(|_| invalid())?;
    let payload: Value = serde_json::from_slice(&decoded).map_err(|_| invalid())?;
    let Some(claims) = payload.get("claims").and_then(Value::as_array) else {
        return Err(invalid());
    };

    // `role_typ` lets the provider name its own role claim type.
    let mut role_types: BTreeSet<String> =
        ROLE_CLAIM_TYPES.iter().map(|t| (*t).to_owned()).collect();
    role_types.extend(string_set(payload.get("role_typ")));

    let mut roles = BTreeSet::new();
    let mut claim_names: Vec<(String, String)> = Vec::new();
    for claim in claims {
        let (Some(claim_type), Some(value)) = (
            claim.get("typ").and_then(Value::as_str),
            claim.get("val").and_then(Value::as_str),
        ) else {
            continue;
        };
        if role_types.contains(claim_type) {
            roles.insert(value.to_owned());
        } else if !claim_names.iter().any(|(t, _)| t == claim_type) {
            // First of each type, as upstream's `setdefault`.
            claim_names.push((claim_type.to_owned(), value.to_owned()));
        }
    }

    let from_header = name_header.map(str::trim).filter(|n| !n.is_empty());
    let name = from_header.map(str::to_owned).or_else(|| {
        NAME_CLAIM_TYPES.iter().find_map(|t| {
            claim_names
                .iter()
                .find(|(c, _)| c == t)
                .map(|(_, v)| v.clone())
        })
    });
    Ok(Principal {
        name: truncate(&name.unwrap_or_else(|| UNKNOWN_PRINCIPAL_NAME.to_owned())),
        roles,
    })
}

/// The Easy Auth principal, or the stand-in when authentication is off, and
/// the role check (§12.3, §12.4).
///
/// # Errors
///
/// `401` without a usable principal; `403 forbidden` without one of
/// `allowed`.
pub fn easy_auth_principal(
    config: &Config,
    headers: &HeaderMap,
    allowed: &[&str],
) -> Result<Principal, LookupError> {
    if !config.auth_enabled() {
        return Ok(auth_disabled_principal(config));
    }
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let principal = parse_easy_auth_principal(
        text(EASY_AUTH_PRINCIPAL_HEADER),
        text(EASY_AUTH_NAME_HEADER),
    )?;
    require_role(principal, allowed)
}

/// `403 forbidden` unless the principal holds one of `allowed`.
fn require_role(principal: Principal, allowed: &[&str]) -> Result<Principal, LookupError> {
    if principal.has_any(allowed) {
        Ok(principal)
    } else {
        Err(LookupError::new(
            Kind::Forbidden,
            format!("Requires one of the roles: {}", {
                let mut sorted = allowed.to_vec();
                sorted.sort_unstable();
                sorted.join(", ")
            }),
        ))
    }
}

/// GUI and admin API, read access (Easy Auth).
///
/// # Errors
///
/// As [`easy_auth_principal`].
pub fn require_reader(config: &Config, headers: &HeaderMap) -> Result<Principal, LookupError> {
    easy_auth_principal(config, headers, &READER_ROLES)
}

/// GUI and admin API, upload and settings changes (Easy Auth).
///
/// # Errors
///
/// As [`easy_auth_principal`].
pub fn require_editor(config: &Config, headers: &HeaderMap) -> Result<Principal, LookupError> {
    easy_auth_principal(config, headers, &EDITOR_ROLES)
}

/// Admin API writes (§12.1, §12.5).
///
/// An editor, plus the CSRF header for a session-cookie
/// caller. A caller that sends a bearer token is exempt (§12.5), because a
/// browser never attaches one by itself, which is the whole of what CSRF
/// protection defends against.
///
/// # Errors
///
/// As [`require_editor`], then `403 csrf_failed`.
pub fn require_api_editor(config: &Config, headers: &HeaderMap) -> Result<Principal, LookupError> {
    let principal = require_editor(config, headers)?;
    if config.auth_enabled() && !has_bearer_header(headers) {
        verify_csrf(config, headers, None)?;
    }
    Ok(principal)
}

/// Whether an `Authorization: Bearer` header is present.
fn has_bearer_header(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(' ').next())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
}

/// The bearer token from the request.
///
/// # Errors
///
/// `401` with the `Bearer` challenge when it is absent or blank.
pub fn bearer_token(headers: &HeaderMap) -> Result<String, LookupError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
    if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
        return Err(LookupError::bearer_unauthorized(
            "A bearer token is required",
        ));
    }
    Ok(token.trim().to_owned())
}

/// The CSRF cookie's name for this environment.
#[must_use]
pub fn csrf_cookie_name(config: &Config) -> &'static str {
    if config.is_local() {
        CSRF_COOKIE_NAME
    } else {
        CSRF_SECURE_COOKIE_NAME
    }
}

/// A fresh token: 32 random bytes as 43 URL-safe characters.
///
/// Upstream `secrets.token_urlsafe(32)`.
///
/// # Panics
///
/// Never in practice: the operating system's random source failing is not
/// recoverable, and a CSRF token that was not random would be worse than
/// stopping.
#[must_use]
#[allow(clippy::expect_used)] // see # Panics
pub fn new_csrf_token() -> String {
    let mut bytes = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("the operating system's random source is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Whether `value` has the shape of a token we issued: 32 to 128 URL-safe
/// characters. A cookie that does not is replaced rather than trusted.
fn is_token_shaped(value: &str) -> bool {
    (32..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The value of cookie `name`, if present.
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
}

/// The request's CSRF token, and whether it is new (so the caller must set
/// the cookie).
#[must_use]
pub fn current_csrf_token(config: &Config, headers: &HeaderMap) -> (String, bool) {
    match cookie(headers, csrf_cookie_name(config)) {
        Some(existing) if is_token_shaped(existing) => (existing.to_owned(), false),
        _ => (new_csrf_token(), true),
    }
}

/// The `Set-Cookie` value for a CSRF token.
///
/// `HttpOnly` is fine because forms embed the token; no script reads the
/// cookie. `Secure` everywhere but a developer machine, where there is no
/// TLS; `SameSite=Lax` and `Path=/` as upstream.
#[must_use]
pub fn csrf_set_cookie(config: &Config, token: &str) -> HeaderValue {
    let secure = if config.is_local() { "" } else { "; Secure" };
    let line = format!(
        "{}={token}; HttpOnly; SameSite=Lax; Path=/{secure}",
        csrf_cookie_name(config)
    );
    // The token is URL-safe base64, so this cannot be an invalid header.
    HeaderValue::from_str(&line).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Double-submit check: the cookie must equal the `X-CSRF-Token` header, or
/// else the form field (§12.5).
///
/// # Errors
///
/// `403 csrf_failed`.
///
/// # Examples
///
/// ```
/// use axum::http::{HeaderMap, HeaderValue, header};
/// use lookup_service::config::Config;
/// use lookup_service::security::verify_csrf;
///
/// let config = Config { environment: "LOCAL".into(), ..Config::default() };
/// let token = "a".repeat(43);
/// let mut headers = HeaderMap::new();
/// headers.insert(header::COOKIE, HeaderValue::from_str(&format!("lookup_csrf={token}"))?);
///
/// assert!(verify_csrf(&config, &headers, Some(&token)).is_ok());
/// assert!(verify_csrf(&config, &headers, Some("other")).is_err());
/// assert!(verify_csrf(&config, &headers, None).is_err());
/// # Ok::<(), axum::http::header::InvalidHeaderValue>(())
/// ```
pub fn verify_csrf(
    config: &Config,
    headers: &HeaderMap,
    form_token: Option<&str>,
) -> Result<(), LookupError> {
    let submitted = headers
        .get(CSRF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .or_else(|| form_token.filter(|v| !v.is_empty()));
    let cookie = cookie(headers, csrf_cookie_name(config));
    match (cookie, submitted) {
        (Some(cookie), Some(submitted))
            if constant_time_eq(cookie.as_bytes(), submitted.as_bytes()) =>
        {
            Ok(())
        }
        _ => Err(LookupError::new(
            Kind::CsrfFailed,
            "CSRF token missing or invalid; reload the page and try again",
        )),
    }
}

/// Equality that does not stop at the first differing byte.
///
/// `ring::constant_time::verify_slices_are_equal` is deprecated ("internal
/// function not intended for external use"), and `hmac_sha256`-style
/// comparison would pull in a crate for ten lines. The length is compared
/// openly: both tokens are 43 characters, so it reveals nothing.
///
/// # Examples
///
/// ```
/// use lookup_service::security::constant_time_eq;
///
/// assert!(constant_time_eq(b"abc", b"abc"));
/// assert!(!constant_time_eq(b"abc", b"abd"));
/// assert!(!constant_time_eq(b"abc", b"ab"));
/// ```
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The DER `RSAPublicKey` (`SEQUENCE { n, e }`) for a JWK's big-endian
/// modulus and exponent, which is the form `ring` verifies against.
///
/// # Examples
///
/// ```
/// use lookup_service::security::rsa_public_key_der;
///
/// // n = 0x80 needs a leading zero so it is not read as negative.
/// assert_eq!(
///     rsa_public_key_der(&[0x80], &[0x01, 0x00, 0x01]).as_deref(),
///     Some(&[0x30, 0x09, 0x02, 0x02, 0x00, 0x80, 0x02, 0x03, 0x01, 0x00, 0x01][..])
/// );
/// ```
#[must_use]
pub fn rsa_public_key_der(modulus: &[u8], exponent: &[u8]) -> Option<Vec<u8>> {
    let mut body = der_integer(modulus)?;
    body.extend(der_integer(exponent)?);
    let mut out = vec![0x30];
    out.extend(der_length(body.len())?);
    out.extend(body);
    Some(out)
}

/// `seconds` as an `f64`, only if that is exact (at most 2^53, RCA-CONV-4).
const fn exact_f64(seconds: u64) -> Option<f64> {
    const EXACT_LIMIT: u64 = 1 << 53;
    if seconds > EXACT_LIMIT {
        return None;
    }
    #[expect(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "RCA-CONV-4: checked above, at most 2^53 is exact"
    )]
    Some(seconds as f64)
}

/// A DER INTEGER from unsigned big-endian bytes.
fn der_integer(bytes: &[u8]) -> Option<Vec<u8>> {
    let significant: Vec<u8> = bytes.iter().copied().skip_while(|b| *b == 0).collect();
    // Zero is still one byte in DER; an empty input stays empty.
    let trimmed = if significant.is_empty() && !bytes.is_empty() {
        vec![0]
    } else {
        significant
    };
    let mut content = Vec::new();
    if trimmed.first().is_none_or(|b| b & 0x80 != 0) {
        content.push(0);
    }
    content.extend_from_slice(&trimmed);
    let mut out = vec![0x02];
    out.extend(der_length(content.len())?);
    out.extend(content);
    Some(out)
}

/// A DER length, or `None` if it cannot be encoded.
fn der_length(length: usize) -> Option<Vec<u8>> {
    if let Ok(short) = u8::try_from(length)
        && short < 0x80
    {
        return Some(vec![short]);
    }
    let digits: Vec<u8> = length
        .to_be_bytes()
        .into_iter()
        .skip_while(|b| *b == 0)
        .collect();
    let mut out = vec![0x80 | u8::try_from(digits.len()).ok()?];
    out.extend(digits);
    Some(out)
}
