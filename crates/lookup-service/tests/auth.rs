//! Authentication, roles and CSRF.
//!
//! Implements `spec/20-lookup-service.md` §2.7, §2.8, §10.6 and §12; ported
//! from `test_security.py` and the auth cases of `test_config.py`,
//! `test_api.py` and `test_gui.py` (upstream `2a220e3`).
//!
//! # The key
//!
//! `TEST_KEY_PKCS8` is a 2048-bit RSA private key generated once for these
//! tests. It signs tokens the verifier is then asked to accept, which is the
//! only way to test signature checking honestly. It protects nothing: no
//! service trusts its public half.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::{codes_definition, make_row, send};
use lookup_service::config::{AUTH_DISABLED_ACK_VALUE, Config, SeedLocation};
use lookup_service::jwks::JwksKeys;
use lookup_service::memory_store::MemoryTableStore;
use lookup_service::security::{
    KeysError, Principal, ROLE_ADMIN, ROLE_EDITOR, ROLE_READER, SigningKeys, TokenVerifier,
    constant_time_eq, parse_easy_auth_principal, rsa_public_key_der,
};
use lookup_service::web::AppState;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

/// See the module documentation. Not a secret.
const TEST_KEY_PKCS8: &str = "\
     MIIEugIBADANBgkqhkiG9w0BAQEFAASCBKQwggSgAgEAAoIBAQC2H/pwDIIOX/yeDFayTc1eu76UK4NR8sr63MqyHb\
     KPd73Le8QGvGxi1vNq5mYXdOObssDLb1qopsDlStAvHbIXOu81LGyweHU4eHRI4o5TNLvqSyyCcJn5qYIRPTe7uENU\
     5fqLDNFhsO53Ump08+g4J15KPLUgF5vK5zIYJVE6HQFboaLyJKp7nyQiEItSJvIs9WQxrraS4e5dfAkf3CLd1awcxP\
     123r9KSfrtlQatbLMeXYnTsfu+DqQukLj4TqHv1zLeyTHzppzZ60SiBJggB/eIugmDVLEOFS14HabxZOi9ixoM3bWf\
     pQ60Ooovta8MM6KTT6pT4ONKTDUkkxSTAgMBAAECgf8vqi7hJGTlXjm/I3uNLs+Q+5HlNX4o9GIrRZ3G//67jZsepk\
     WnfajgNWF/mM0fVng7R40tsPcJLJtPZ0UVD7CMY+A3Cmlnxh7nqJ7GdITLQgHsT8BWYyHpNXPVDzCG8LKuhYIM4k3q\
     Q2Ql1mwGL8+W4rbKtxiwTX14nQl/SET58CxMBlH5QglhDrGMe/MaGpgONSG0Bu3M+55+4C2Kf1W+l3HpY41/3n9pO1\
     xGGXTruS2GNecuGNedEll1OU6w6fEjZ5CXoI/D30mBCvsEicmue1DXqt1ILtOpHOZ2skdk+r97p4uexaUTgPm1u9cY\
     QbAeY6QcfSSzZ7+Opb23YPECgYEA4QmduKiKk8mcfEQ556YHQK78Fh9mczIheoJVx992F8RSo2tJNLvebhFI0Aj4/5\
     nkjkCCbi8CN1Wyh9KlFCZIwaQW4pHbnJuXOdz7Lt14LYVtJTcjsar6ERiIK1wu9GZeOkOPKIUY+wgui/sSwHunTc8v\
     pwBDEij4OCpH/dR3W0kCgYEAzy7fKz/AQhyi2wsClfhWWQCkh7nLlEZXryl/Ygwlj+ge93ygN/aGDzJlYkYyEnYv4v\
     ceQFwW2J4uZsDk5hnGUrk7t3oLkAYp7uxkrl85GYZ4cY7JIovBX5oUauQo/p0ktFnhFrCV3Y9A0iVAbqg5PWqSRhR2\
     m0OPEm6+TNkz9PsCgYAXrPpSBjFmMQzsqgTpkTcsH/zXmwAdWIXbLME3oRP8liQXm179VFKng9wEPx1rTtaT/Md8Li\
     fuiG6bCZdiklCPRuM9fRj81gXICa7HlHdjWqlMBAwWUiWzDMCuxykNf4OkX509JxuktYyoH4cuhxPoBMc/q7Bixbqv\
     tgsS8nqIOQKBgCcXwwhYGSDgYmJOa4h8uV5F3sW1axJpS3ZDsEOB4zyKIf20yE6G97i6W6L1d9E/970tv8mJFs582m\
     iNESPRzTgit3/hSXk9kIACnuTAFvgYBXX36T/Zev1dB2k77/hLEnftVamAXGbX9vUwDj5TA6nxB/StO8wELWTe/I1n\
     loQtAoGATy5XKSbgiqaXLbHAKDo97qw92/Mqc4kcRYNrXazFEp5AliLGKevDyJN3v9D6IfEd9ZOEQv+eBWoqiXyY6I\
     f53yUOWzq+SdVblxRnNsz5QItht6updkq+nyGfkgd4dn7Kb8D6jEVkWaCiz589DUhLwLbHbxrzur1CaMOULyhtzPM=";

const TENANT: &str = "11111111-2222-3333-4444-555555555555";
const AUDIENCE: &str = "api://lookup-service-test";
const NOW: u64 = 1_800_000_000;

fn key_pair() -> RsaKeyPair {
    let der = STANDARD.decode(TEST_KEY_PKCS8).unwrap();
    RsaKeyPair::from_pkcs8(&der).unwrap()
}

/// Hands out the test key's public half for `kid` `k1`.
struct TestKeys;

impl SigningKeys for TestKeys {
    fn signing_key(&self, kid: &str) -> Result<Vec<u8>, KeysError> {
        match kid {
            "k1" => Ok(key_pair().public().as_ref().to_vec()),
            "down" => Err(KeysError::Unavailable),
            _ => Err(KeysError::Unknown),
        }
    }
}

fn b64(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(value.to_string())
}

/// Sign `claims` with `header`.
fn sign(header: &Value, claims: &Value) -> String {
    let input = format!("{}.{}", b64(header), b64(claims));
    let mut signature = vec![0_u8; key_pair().public().modulus_len()];
    key_pair()
        .sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut signature,
        )
        .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn claims() -> Value {
    json!({
        "iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"),
        "aud": AUDIENCE,
        "exp": NOW + 3600,
        "roles": [ROLE_READER],
        "azp": "transformer-app",
    })
}

fn token(claims: &Value) -> String {
    sign(&json!({"alg": "RS256", "kid": "k1"}), claims)
}

fn verifier() -> TokenVerifier {
    TokenVerifier::new(TENANT, AUDIENCE, Arc::new(TestKeys))
}

/// Replace one claim.
fn with(key: &str, value: Value) -> Value {
    let mut c = claims();
    c[key] = value;
    c
}

/// Remove one claim.
fn without(key: &str) -> Value {
    let mut c = claims();
    c.as_object_mut().unwrap().remove(key);
    c
}

mod configuration {
    //! §2.7 and §2.8 (`AuthSettingsTests`).

    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Result<Config, ihb_config::ConfigErrors> {
        Config::from_pairs(list.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
    }

    const AUTH: [(&str, &str); 2] = [
        ("LOOKUP_AUTH_TENANT_ID", "tenant"),
        ("LOOKUP_AUTH_AUDIENCE", "api://lookup"),
    ];

    fn with_auth(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        AUTH.iter().chain(extra).copied().collect()
    }

    #[test]
    fn missing_auth_configuration_refuses_to_start() {
        for env in [
            vec![],
            vec![("ENVIRONMENT", "PRD")],
            vec![("LOOKUP_AUTH_TENANT_ID", "t")],
            vec![("LOOKUP_AUTH_AUDIENCE", "a")],
            vec![("ENVIRONMENT", "LOCAL")],
        ] {
            let errors = pairs(&env).unwrap_err().to_string();
            assert!(
                errors.contains("Authentication is not configured"),
                "{env:?}: {errors}"
            );
        }
        let local = pairs(&[("ENVIRONMENT", "LOCAL")]).unwrap_err().to_string();
        assert!(
            local.contains("LOOKUP_AUTH_DISABLED=true for local development"),
            "{local}"
        );
        let prd = pairs(&[("ENVIRONMENT", "PRD")]).unwrap_err().to_string();
        assert!(!prd.contains("for local development"), "{prd}");
    }

    #[test]
    fn auth_cannot_be_disabled_outside_local_dev_and_tst() {
        for environment in ["PRD", "SIT", "PREPROD"] {
            let errors = pairs(&with_auth(&[
                ("ENVIRONMENT", environment),
                ("LOOKUP_AUTH_DISABLED", "true"),
                ("LOOKUP_AUTH_DISABLED_ACK", AUTH_DISABLED_ACK_VALUE),
            ]))
            .unwrap_err()
            .to_string();
            assert!(
                errors.contains("only allowed when ENVIRONMENT is one of LOCAL, DEV, TST"),
                "{environment}: {errors}"
            );
        }
    }

    #[test]
    fn dev_and_tst_need_the_acknowledgement_to_disable_auth() {
        // A missing or mistyped acknowledgement must refuse to start, never
        // fall back to open access.
        let lower = AUTH_DISABLED_ACK_VALUE.to_lowercase();
        for environment in ["DEV", "TST"] {
            for ack in [None, Some(""), Some("true"), Some(lower.as_str())] {
                let mut env = vec![
                    ("ENVIRONMENT", environment),
                    ("LOOKUP_AUTH_DISABLED", "true"),
                ];
                if let Some(ack) = ack {
                    env.push(("LOOKUP_AUTH_DISABLED_ACK", ack));
                }
                let errors = pairs(&env).unwrap_err().to_string();
                assert!(
                    errors.contains("also needs LOOKUP_AUTH_DISABLED_ACK="),
                    "{environment} {ack:?}: {errors}"
                );
            }
        }
    }

    #[test]
    fn dev_and_tst_can_disable_auth_with_the_acknowledgement() {
        for environment in ["dev", "TST"] {
            let config = pairs(&[
                ("ENVIRONMENT", environment),
                ("LOOKUP_AUTH_DISABLED", "true"),
                ("LOOKUP_AUTH_DISABLED_ACK", AUTH_DISABLED_ACK_VALUE),
            ])
            .unwrap();
            assert!(!config.auth_enabled());
            assert!(config.unauthenticated_deployment());
        }
    }

    #[test]
    fn the_acknowledgement_alone_does_not_disable_auth() {
        let config = pairs(&with_auth(&[
            ("ENVIRONMENT", "DEV"),
            ("LOOKUP_AUTH_DISABLED_ACK", AUTH_DISABLED_ACK_VALUE),
        ]))
        .unwrap();
        assert!(config.auth_enabled());
        assert!(!config.unauthenticated_deployment());
    }

    #[test]
    fn disabled_auth_is_accepted_locally_without_configuration() {
        let config = pairs(&[("ENVIRONMENT", "local"), ("LOOKUP_AUTH_DISABLED", "true")]).unwrap();
        assert!(!config.auth_enabled());
        assert!(!config.unauthenticated_deployment());
        assert!(config.is_local());
    }

    #[test]
    fn auth_disabled_must_be_a_boolean() {
        let errors = pairs(&[("ENVIRONMENT", "LOCAL"), ("LOOKUP_AUTH_DISABLED", "maybe")])
            .unwrap_err()
            .to_string();
        assert!(errors.contains("LOOKUP_AUTH_DISABLED"), "{errors}");
        for yes in ["TRUE", "1"] {
            assert!(pairs(&[("ENVIRONMENT", "LOCAL"), ("LOOKUP_AUTH_DISABLED", yes)]).is_ok());
        }
        for no in ["false", "0"] {
            assert!(pairs(&with_auth(&[("LOOKUP_AUTH_DISABLED", no)])).is_ok());
        }
    }

    #[test]
    fn an_unset_seed_directory_means_no_seed() {
        // §2.9.
        let config = pairs(&AUTH).unwrap();
        assert_eq!(config.seed, SeedLocation::None);
    }

    #[test]
    fn the_hand_built_default_is_closed() {
        // `Config::default()` must not open the service by omission.
        assert!(Config::default().auth_enabled());
    }
}

mod verifier {
    //! §12.2 (`TokenVerifierTests`).

    use super::*;

    fn verify(token: &str) -> Result<Principal, lookup_service::error::LookupError> {
        verifier().verify_at(token, NOW)
    }

    #[test]
    fn a_valid_token_yields_its_name_and_roles() {
        let principal = verify(&token(&claims())).unwrap();
        assert_eq!(principal.name, "transformer-app");
        assert!(principal.roles.contains(ROLE_READER));
    }

    #[test]
    fn the_name_prefers_the_username_and_is_bounded() {
        let named = verify(&token(&with(
            "preferred_username",
            json!("a.person@example"),
        )))
        .unwrap();
        assert_eq!(named.name, "a.person@example");
        let long = verify(&token(&with("preferred_username", json!("x".repeat(500))))).unwrap();
        assert_eq!(long.name.len(), 200);
        let mut bare = claims();
        bare.as_object_mut().unwrap().remove("azp");
        assert_eq!(verify(&token(&bare)).unwrap().name, "unknown");
    }

    #[test]
    fn the_v1_issuer_is_accepted() {
        let c = with("iss", json!(format!("https://sts.windows.net/{TENANT}/")));
        assert!(verify(&token(&c)).is_ok());
    }

    #[test]
    fn an_audience_list_containing_ours_is_accepted() {
        assert!(verify(&token(&with("aud", json!(["other", AUDIENCE])))).is_ok());
        assert!(verify(&token(&with("aud", json!(["other"])))).is_err());
    }

    #[test]
    fn a_single_role_string_is_a_role() {
        let principal = verify(&token(&with("roles", json!(ROLE_EDITOR)))).unwrap();
        assert!(principal.roles.contains(ROLE_EDITOR));
        let none = verify(&token(&with("roles", json!(7)))).unwrap();
        assert!(none.roles.is_empty());
    }

    #[test]
    fn expiry_has_a_minute_of_skew() {
        // §12.2.
        let c = with("exp", json!(NOW - 30));
        assert!(verify(&token(&c)).is_ok(), "30 s past is within the skew");
        let error = verify(&token(&with("exp", json!(NOW - 61)))).unwrap_err();
        assert_eq!(error.detail, "Token has expired");
        assert!(error.bearer_challenge);
    }

    #[test]
    fn a_token_not_yet_valid_is_refused() {
        assert!(verify(&token(&with("nbf", json!(NOW + 30)))).is_ok());
        assert!(verify(&token(&with("nbf", json!(NOW + 120)))).is_err());
    }

    #[test]
    fn wrong_audience_issuer_and_missing_claims_are_refused() {
        for bad in [
            with("aud", json!("api://other")),
            with("iss", json!("https://evil.example/")),
            without("exp"),
            without("iss"),
            without("aud"),
        ] {
            let error = verify(&token(&bad)).unwrap_err();
            assert_eq!(error.kind.status(), 401, "{bad}");
            assert_eq!(error.detail, "Invalid token");
            assert!(error.bearer_challenge);
        }
    }

    #[test]
    fn only_rs256_is_accepted() {
        for alg in ["none", "HS256", "RS512", "ES256"] {
            let t = sign(&json!({"alg": alg, "kid": "k1"}), &claims());
            assert_eq!(verify(&t).unwrap_err().kind.status(), 401, "{alg}");
        }
        let unsigned = format!("{}.{}.", b64(&json!({"alg": "none"})), b64(&claims()));
        assert!(verify(&unsigned).is_err());
    }

    #[test]
    fn a_tampered_token_is_refused() {
        let good = token(&claims());
        let mut parts: Vec<&str> = good.split('.').collect();
        let forged = b64(&with("roles", json!([ROLE_ADMIN])));
        parts[1] = &forged;
        assert!(
            verify(&parts.join(".")).is_err(),
            "payload changed, signature kept"
        );

        let mut flipped = good.clone().into_bytes();
        let last = flipped.len() - 2;
        flipped[last] = if flipped[last] == b'A' { b'B' } else { b'A' };
        assert!(verify(&String::from_utf8(flipped).unwrap()).is_err());
    }

    #[test]
    fn an_unknown_key_id_and_a_missing_one_are_refused() {
        let unknown = sign(&json!({"alg": "RS256", "kid": "nope"}), &claims());
        assert_eq!(verify(&unknown).unwrap_err().kind.status(), 401);
        let none = sign(&json!({"alg": "RS256"}), &claims());
        assert_eq!(verify(&none).unwrap_err().kind.status(), 401);
    }

    #[test]
    fn unreachable_signing_keys_are_a_503_not_a_401() {
        // The caller did nothing wrong (§12.2).
        let t = sign(&json!({"alg": "RS256", "kid": "down"}), &claims());
        let error = verify(&t).unwrap_err();
        assert_eq!(error.kind.status(), 503);
        assert!(!error.bearer_challenge);
    }

    #[test]
    fn garbage_is_refused() {
        for t in ["", "a", "a.b", "a.b.c", "a.b.c.d", "....", "!!.!!.!!"] {
            assert_eq!(verify(t).unwrap_err().kind.status(), 401, "{t:?}");
        }
    }

    #[test]
    fn a_refusal_carries_no_claim() {
        // §12.7: the message never quotes what the token said.
        let c = with("aud", json!("DISTINCTIVE-AUDIENCE"));
        let error = verify(&token(&c)).unwrap_err();
        assert!(!format!("{error:?}").contains("DISTINCTIVE"));
    }

    #[test]
    fn the_der_encoding_pads_a_high_bit() {
        assert_eq!(
            rsa_public_key_der(&[0x80], &[0x01, 0x00, 0x01]),
            [
                0x30, 0x09, 0x02, 0x02, 0x00, 0x80, 0x02, 0x03, 0x01, 0x00, 0x01
            ]
        );
        assert_eq!(
            rsa_public_key_der(&[0, 0, 0x7f], &[3]),
            [0x30, 0x06, 0x02, 0x01, 0x7f, 0x02, 0x01, 0x03],
            "leading zeros are dropped"
        );
    }

    #[test]
    fn the_der_of_the_real_key_round_trips_through_a_jwk() {
        let pair = key_pair();
        let components: ring::rsa::PublicKeyComponents<Vec<u8>> = pair.public().into();
        assert_eq!(
            rsa_public_key_der(&components.n, &components.e),
            pair.public().as_ref()
        );
    }

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"sama"));
    }
}

mod easy_auth {
    //! §12.3 (`EasyAuthPrincipalTests`).

    use super::*;

    fn header(value: &Value) -> String {
        STANDARD.encode(value.to_string())
    }

    #[test]
    fn roles_and_the_name_are_read() {
        let encoded = header(&json!({"claims": [
            {"typ": "roles", "val": "Lookup.Reader"},
            {"typ": "http://schemas.microsoft.com/ws/2008/06/identity/claims/role", "val": "Lookup.Editor"},
            {"typ": "name", "val": "Ada"},
            {"typ": "preferred_username", "val": "ada@example"},
        ]}));
        let principal = parse_easy_auth_principal(Some(&encoded), None).unwrap();
        assert_eq!(
            principal.name, "ada@example",
            "preferred_username outranks name"
        );
        assert!(principal.roles.contains("Lookup.Reader"));
        assert!(principal.roles.contains("Lookup.Editor"));
    }

    #[test]
    fn the_name_header_wins_and_role_typ_is_honoured() {
        let encoded = header(&json!({"role_typ": "grp", "claims": [
            {"typ": "grp", "val": "Lookup.Admin"}, {"typ": "name", "val": "Ada"}]}));
        let principal = parse_easy_auth_principal(Some(&encoded), Some("  Header Name ")).unwrap();
        assert_eq!(principal.name, "Header Name");
        assert!(principal.roles.contains("Lookup.Admin"));
    }

    #[test]
    fn no_name_is_unknown_and_junk_claims_are_skipped() {
        let encoded = header(&json!({"claims": [1, {"typ": 2}, {"val": "x"}]}));
        let principal = parse_easy_auth_principal(Some(&encoded), None).unwrap();
        assert_eq!(principal.name, "unknown");
        assert!(principal.roles.is_empty());
    }

    #[test]
    fn absent_oversized_and_malformed_headers_are_401() {
        let error = parse_easy_auth_principal(None, None).unwrap_err();
        assert_eq!(error.detail, "Authentication required");
        assert!(
            !error.bearer_challenge,
            "Easy Auth is not a bearer challenge"
        );
        for bad in [
            "!!notbase64!!".to_owned(),
            STANDARD.encode("not json"),
            header(&json!({"claims": "no"})),
            header(&json!([])),
            "A".repeat(33 * 1024),
        ] {
            let error = parse_easy_auth_principal(Some(&bad), None).unwrap_err();
            assert_eq!(error.kind.status(), 401);
            assert_eq!(error.detail, "Invalid authentication principal");
        }
    }
}

/// A store with one table, behind a configuration.
async fn secured(config: Config) -> (common::App<MemoryTableStore>, Router) {
    let store = MemoryTableStore::default();
    store.put_table(
        codes_definition(),
        vec![make_row(&["A"], &[("label", "Alpha")])],
    );
    let state = Arc::new(AppState::with_signing_keys(
        config,
        store,
        Some(Arc::new(TestKeys)),
    ));
    state.start().await.unwrap();
    let router = lookup_service::web::router(Arc::clone(&state));
    (
        common::App {
            state,
            router: router.clone(),
        },
        router,
    )
}

fn dev_config() -> Config {
    Config {
        environment: "DEV".to_owned(),
        auth_tenant_id: Some(TENANT.to_owned()),
        auth_audience: Some(AUDIENCE.to_owned()),
        ..Config::default()
    }
}

/// The `X-MS-CLIENT-PRINCIPAL` header value for a person holding `roles`.
fn principal_header(name: &str, roles: &[&str]) -> String {
    let mut claims: Vec<Value> = roles
        .iter()
        .map(|r| json!({"typ": "roles", "val": r}))
        .collect();
    claims.push(json!({"typ": "name", "val": name}));
    STANDARD.encode(json!({ "claims": claims }).to_string())
}

fn request(method: &str, path: &str, headers: &[(&str, String)], body: Body) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    builder.body(body).unwrap()
}

mod bearer_api {
    //! §12.2 on the route (`test_api.py` auth cases).

    use super::*;

    async fn lookup(headers: &[(&str, String)]) -> common::Reply {
        let (_, router) = secured(dev_config()).await;
        send(
            &router,
            request("GET", "/api/v1/lookup/codes?k=A", headers, Body::empty()),
        )
        .await
    }

    fn bearer(claims: &Value) -> (&'static str, String) {
        ("authorization", format!("Bearer {}", token(claims)))
    }

    #[tokio::test]
    async fn a_reader_token_is_served() {
        // `exp` is far in the future of the real clock too.
        let c = with("exp", json!(NOW * 2));
        let reply = lookup(&[bearer(&c)]).await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.json()["value"], "Alpha");
    }

    #[tokio::test]
    async fn no_token_is_401_with_the_challenge() {
        let reply = lookup(&[]).await;
        assert_eq!(reply.status, 401);
        assert_eq!(reply.json()["error"], "unauthorized");
        assert_eq!(reply.header("www-authenticate"), "Bearer");
    }

    #[tokio::test]
    async fn a_non_bearer_scheme_or_blank_token_is_401() {
        for value in ["Basic abc", "Bearer ", "Bearer", "token"] {
            let reply = lookup(&[("authorization", value.to_owned())]).await;
            assert_eq!(reply.status, 401, "{value}");
            assert_eq!(reply.header("www-authenticate"), "Bearer");
        }
    }

    #[tokio::test]
    async fn an_expired_token_says_so() {
        let reply = lookup(&[bearer(&with("exp", json!(1)))]).await;
        assert_eq!(reply.status, 401);
        assert_eq!(reply.json()["detail"], "Token has expired");
    }

    #[tokio::test]
    async fn a_token_without_a_lookup_role_is_403() {
        let mut c = with("exp", json!(NOW * 2));
        c["roles"] = json!(["Something.Else"]);
        let reply = lookup(&[bearer(&c)]).await;
        assert_eq!(reply.status, 403);
        assert_eq!(reply.json()["error"], "forbidden");
        assert_eq!(
            reply.header("www-authenticate"),
            "",
            "403 is not a challenge"
        );
    }

    #[tokio::test]
    async fn every_lookup_role_may_read() {
        for role in [ROLE_READER, ROLE_EDITOR, ROLE_ADMIN] {
            let mut c = with("exp", json!(NOW * 2));
            c["roles"] = json!([role]);
            assert_eq!(lookup(&[bearer(&c)]).await.status, 200, "{role}");
        }
    }

    #[tokio::test]
    async fn authentication_comes_before_readiness_and_validation() {
        // §8.1: a caller with no credential learns nothing about the service.
        let store = MemoryTableStore::default();
        let state = Arc::new(AppState::with_signing_keys(
            dev_config(),
            store,
            Some(Arc::new(TestKeys)),
        ));
        let router = lookup_service::web::router(state);
        let unstarted = send(
            &router,
            request("GET", "/api/v1/lookup/codes?k=A", &[], Body::empty()),
        )
        .await;
        assert_eq!(unstarted.status, 401, "not 503 not_ready");
        let bad_key = lookup(&[]).await;
        assert_eq!(bad_key.status, 401);
    }

    #[tokio::test]
    async fn health_and_static_need_no_credential() {
        let (_, router) = secured(dev_config()).await;
        for path in ["/health/live", "/health/ready", "/static/lookup.css"] {
            let reply = send(&router, request("GET", path, &[], Body::empty())).await;
            assert_eq!(reply.status, 200, "{path}");
        }
    }
}

mod roles {
    //! §12.1 across the Easy Auth routes.

    use super::*;

    async fn status(
        method: &str,
        path: &str,
        roles: Option<&[&str]>,
        extra: &[(&str, String)],
        body: Body,
    ) -> u16 {
        let (_, router) = secured(dev_config()).await;
        let mut headers: Vec<(&str, String)> = extra.to_vec();
        if let Some(roles) = roles {
            headers.push(("x-ms-client-principal", principal_header("Ada", roles)));
        }
        send(&router, request(method, path, &headers, body))
            .await
            .status
            .as_u16()
    }

    #[tokio::test]
    async fn reads_need_any_role_and_a_principal() {
        for path in [
            "/api/v1/tables",
            "/api/v1/tables/codes",
            "/api/v1/tables/codes/rows",
            "/",
            "/tables/codes",
            "/upload",
        ] {
            assert_eq!(
                status("GET", path, None, &[], Body::empty()).await,
                401,
                "{path} anonymous"
            );
            assert_eq!(
                status("GET", path, Some(&[]), &[], Body::empty()).await,
                403,
                "{path} no role"
            );
            for role in [ROLE_READER, ROLE_EDITOR, ROLE_ADMIN] {
                assert_eq!(
                    status("GET", path, Some(&[role]), &[], Body::empty()).await,
                    200,
                    "{path} {role}"
                );
            }
        }
    }

    #[tokio::test]
    async fn writes_need_an_editor_before_anything_else() {
        let patch = || Body::from(json!({"preload": true}).to_string());
        let headers = [("content-type", "application/json".to_owned())];
        assert_eq!(
            status("PATCH", "/api/v1/tables/codes", None, &headers, patch()).await,
            401
        );
        assert_eq!(
            status(
                "PATCH",
                "/api/v1/tables/codes",
                Some(&[ROLE_READER]),
                &headers,
                patch()
            )
            .await,
            403
        );
        // An editor passes the role check and then meets the CSRF check.
        let csrf = status(
            "PATCH",
            "/api/v1/tables/codes",
            Some(&[ROLE_EDITOR]),
            &headers,
            patch(),
        )
        .await;
        assert_eq!(csrf, 403, "csrf_failed");
        assert_eq!(
            status("POST", "/upload", Some(&[ROLE_READER]), &[], Body::empty()).await,
            403
        );
        assert_eq!(
            status(
                "POST",
                "/tables/codes/settings",
                Some(&[ROLE_READER]),
                &[],
                Body::empty()
            )
            .await,
            403
        );
    }

    #[tokio::test]
    async fn the_forbidden_message_lists_the_roles_sorted() {
        let (_, router) = secured(dev_config()).await;
        let reply = send(
            &router,
            request(
                "GET",
                "/api/v1/tables",
                &[("x-ms-client-principal", principal_header("A", &[]))],
                Body::empty(),
            ),
        )
        .await;
        assert_eq!(
            reply.json()["detail"],
            "Requires one of the roles: Lookup.Admin, Lookup.Editor, Lookup.Reader"
        );
    }

    #[tokio::test]
    async fn the_easy_auth_headers_are_ignored_when_auth_is_off() {
        // §12.3/§12.4: no Easy Auth means anyone could write the header.
        let config = Config {
            auth_disabled: true,
            environment: "LOCAL".into(),
            ..Config::default()
        };
        let (_, router) = secured(config).await;
        let reply = send(
            &router,
            request("GET", "/api/v1/tables", &[], Body::empty()),
        )
        .await;
        assert_eq!(reply.status, 200);
    }
}

mod csrf {
    //! §12.5 (`CsrfTests`).

    use super::*;

    fn editor() -> (&'static str, String) {
        (
            "x-ms-client-principal",
            principal_header("Ada", &[ROLE_EDITOR]),
        )
    }

    fn set_cookie(reply: &common::Reply) -> String {
        reply.header("set-cookie").to_owned()
    }

    fn token_of(cookie: &str) -> String {
        cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_owned()
    }

    #[tokio::test]
    async fn a_page_issues_a_hardened_cookie_and_embeds_the_token() {
        let (_, router) = secured(dev_config()).await;
        let reply = send(
            &router,
            request("GET", "/upload", &[editor()], Body::empty()),
        )
        .await;
        let cookie = set_cookie(&reply);
        assert!(cookie.starts_with("__Host-lookup_csrf="), "{cookie}");
        for attribute in ["HttpOnly", "SameSite=Lax", "Path=/", "Secure"] {
            assert!(cookie.contains(attribute), "{attribute}: {cookie}");
        }
        let token = token_of(&cookie);
        assert_eq!(token.len(), 43);
        assert!(
            reply
                .body
                .contains(&format!("name=\"csrf_token\" value=\"{token}\"")),
            "{}",
            reply.body
        );
    }

    #[tokio::test]
    async fn locally_the_cookie_is_plain() {
        let config = Config {
            auth_disabled: true,
            environment: "LOCAL".into(),
            ..Config::default()
        };
        let (_, router) = secured(config).await;
        let reply = send(&router, request("GET", "/upload", &[], Body::empty())).await;
        let cookie = set_cookie(&reply);
        assert!(cookie.starts_with("lookup_csrf="), "{cookie}");
        assert!(!cookie.contains("Secure"), "{cookie}");
    }

    #[tokio::test]
    async fn an_existing_valid_cookie_is_kept_not_reissued() {
        let (_, router) = secured(dev_config()).await;
        let token = "a".repeat(43);
        let cookie = ("cookie", format!("__Host-lookup_csrf={token}"));
        let reply = send(
            &router,
            request("GET", "/upload", &[editor(), cookie], Body::empty()),
        )
        .await;
        assert_eq!(reply.header("set-cookie"), "");
        assert!(reply.body.contains(&token));
        let bad = ("cookie", "__Host-lookup_csrf=short".to_owned());
        let reply = send(
            &router,
            request("GET", "/upload", &[editor(), bad], Body::empty()),
        )
        .await;
        assert!(
            reply
                .header("set-cookie")
                .starts_with("__Host-lookup_csrf="),
            "replaced"
        );
    }

    #[tokio::test]
    async fn the_form_post_needs_the_token() {
        let (state, router) = secured(dev_config()).await;
        let token = "t".repeat(43);
        let cookie = ("cookie", format!("__Host-lookup_csrf={token}"));
        let form = |t: &str| Body::from(format!("csrf_token={t}&ttl_value=60&ttl_unit=seconds"));
        let post = |t: &str| {
            request(
                "POST",
                "/tables/codes/settings",
                &[
                    editor(),
                    cookie.clone(),
                    ("content-type", "application/x-www-form-urlencoded".into()),
                ],
                form(t),
            )
        };
        let missing = send(&router, post("")).await;
        assert_eq!(missing.status, 403);
        assert!(missing.body.contains("csrf_failed"), "{}", missing.body);
        let wrong = send(&router, post("other")).await;
        assert_eq!(wrong.status, 403);
        // The memory store is writable, so the right token saves.
        let ok = send(&router, post(&token)).await;
        assert_eq!(ok.status, 303, "{}", ok.body);
        let record = state.state.resolver().unwrap().definition("codes").unwrap();
        assert_eq!(record.ttl_seconds, Some(60));
    }

    #[tokio::test]
    async fn the_admin_api_wants_the_header_unless_a_bearer_is_sent() {
        let (_, router) = secured(dev_config()).await;
        let token = "t".repeat(43);
        let cookie = ("cookie", format!("__Host-lookup_csrf={token}"));
        let json = ("content-type", "application/json".to_owned());
        let body = || Body::from(json!({"preload": true}).to_string());

        let no_header = send(
            &router,
            request(
                "PATCH",
                "/api/v1/tables/codes",
                &[editor(), cookie.clone(), json.clone()],
                body(),
            ),
        )
        .await;
        assert_eq!(no_header.status, 403);
        assert_eq!(no_header.json()["error"], "csrf_failed");

        let with_header = send(
            &router,
            request(
                "PATCH",
                "/api/v1/tables/codes",
                &[
                    editor(),
                    cookie.clone(),
                    json.clone(),
                    ("x-csrf-token", token.clone()),
                ],
                body(),
            ),
        )
        .await;
        assert_eq!(with_header.status, 200, "{}", with_header.body);

        // A bearer header exempts the call (a browser never sends one itself).
        let bearer = send(
            &router,
            request(
                "PATCH",
                "/api/v1/tables/codes",
                &[editor(), json, ("authorization", "Bearer x".into())],
                body(),
            ),
        )
        .await;
        assert_eq!(bearer.status, 200, "{}", bearer.body);
    }

    #[tokio::test]
    async fn the_uploader_is_the_principal() {
        let (state, router) = secured(dev_config()).await;
        let token = "t".repeat(43);
        let boundary = "BOUND";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"c.csv\"\r\n\r\ncode,label\nB,Beta\n\r\n--{boundary}--\r\n"
        );
        let reply = send(
            &router,
            request(
                "POST",
                "/api/v1/tables/codes/uploads",
                &[
                    editor(),
                    ("cookie", format!("__Host-lookup_csrf={token}")),
                    ("x-csrf-token", token.clone()),
                    (
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    ),
                ],
                Body::from(body),
            ),
        )
        .await;
        assert_eq!(reply.status, 201, "{}", reply.body);
        let by = state
            .state
            .store
            .record("codes")
            .unwrap()
            .stats
            .last_upload_by;
        assert_eq!(by.as_deref(), Some("Ada"));
    }
}

mod pages {
    //! §12.4, §9.3 and §10.6 (`test_gui.py` auth cases).

    use super::*;

    fn unauthenticated(environment: &str) -> Config {
        Config {
            environment: environment.to_owned(),
            auth_disabled: true,
            auth_disabled_ack: Some(AUTH_DISABLED_ACK_VALUE.to_owned()),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn a_deployed_unauthenticated_service_shows_the_banner_everywhere() {
        let (_, router) = secured(unauthenticated("DEV")).await;
        for path in ["/", "/upload", "/tables/codes", "/tables/missing"] {
            let reply = send(&router, request("GET", path, &[], Body::empty())).await;
            assert!(
                reply.body.contains("Authentication is disabled in DEV"),
                "{path}: {}",
                reply.status
            );
        }
    }

    #[tokio::test]
    async fn local_and_authenticated_services_show_no_banner() {
        let (_, router) = secured(unauthenticated("LOCAL")).await;
        let reply = send(&router, request("GET", "/", &[], Body::empty())).await;
        assert!(!reply.body.contains("Authentication is disabled"));
        let (_, router) = secured(dev_config()).await;
        let reply = send(
            &router,
            request(
                "GET",
                "/",
                &[(
                    "x-ms-client-principal",
                    principal_header("A", &[ROLE_READER]),
                )],
                Body::empty(),
            ),
        )
        .await;
        assert!(!reply.body.contains("Authentication is disabled"));
    }

    #[tokio::test]
    async fn forms_still_need_the_token_with_authentication_off() {
        // Auth off removes the role check, not the CSRF check on a form
        // (upstream `require_editor` skips only the roles).
        let (_, router) = secured(unauthenticated("TST")).await;
        let reply = send(
            &router,
            request(
                "POST",
                "/tables/codes/settings",
                &[("content-type", "application/x-www-form-urlencoded".into())],
                Body::from("ttl_value=5&ttl_unit=seconds"),
            ),
        )
        .await;
        assert_eq!(reply.status, 403);
    }

    #[tokio::test]
    async fn unauthenticated_writes_are_recorded_under_that_name() {
        // §12.4/§12.6.
        let (state, router) = secured(unauthenticated("TST")).await;
        let token = "t".repeat(43);
        let boundary = "BOUND";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"c.csv\"\r\n\r\ncode,label\nB,Beta\n\r\n--{boundary}--\r\n"
        );
        let reply = send(
            &router,
            request(
                "POST",
                "/api/v1/tables/codes/uploads",
                &[
                    ("cookie", format!("__Host-lookup_csrf={token}")),
                    ("x-csrf-token", token.clone()),
                    (
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    ),
                ],
                Body::from(body),
            ),
        )
        .await;
        assert_eq!(reply.status, 201, "{}", reply.body);
        let by = state
            .state
            .store
            .record("codes")
            .unwrap()
            .stats
            .last_upload_by;
        assert_eq!(by.as_deref(), Some("unauthenticated"));
    }

    #[tokio::test]
    async fn a_reader_sees_why_the_upload_form_is_disabled() {
        let (_, router) = secured(dev_config()).await;
        let reply = send(
            &router,
            request(
                "GET",
                "/upload",
                &[(
                    "x-ms-client-principal",
                    principal_header("A", &[ROLE_READER]),
                )],
                Body::empty(),
            ),
        )
        .await;
        assert_eq!(reply.status, 200);
        assert!(
            reply
                .body
                .contains("Changing tables needs the Lookup.Editor role"),
            "{}",
            reply.body
        );
    }

    #[tokio::test]
    async fn no_page_links_an_http_url() {
        // §10.6: assets are linked by absolute path, so a TLS-terminating
        // ingress cannot turn them into mixed content.
        let (_, router) = secured(unauthenticated("DEV")).await;
        for path in ["/", "/upload", "/tables/codes", "/tables/missing"] {
            let reply = send(
                &router,
                request(
                    "GET",
                    path,
                    &[("x-forwarded-proto", "https".into())],
                    Body::empty(),
                ),
            )
            .await;
            assert!(!reply.body.contains("http://"), "{path}");
            assert!(reply.body.contains("href=\"/static/lookup.css"), "{path}");
        }
    }
}

mod jwks {
    //! §12.2's key cache.

    use super::*;

    /// Serve `answers` one per connection; returns the URL and a hit count.
    fn serve(answers: Vec<String>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/tenant/discovery/v2.0/keys",
            listener.local_addr().unwrap()
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0_u8; 2048];
                let _ = socket.read(&mut buffer);
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = socket.write_all(answer.as_bytes());
            }
        });
        (url, hits)
    }

    fn ok(body: &Value) -> String {
        let text = body.to_string();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        )
    }

    fn key_set() -> Value {
        let pair = key_pair();
        let components: ring::rsa::PublicKeyComponents<Vec<u8>> = pair.public().into();
        json!({"keys": [
            {"kty": "RSA", "kid": "k1", "use": "sig",
             "n": URL_SAFE_NO_PAD.encode(&components.n),
             "e": URL_SAFE_NO_PAD.encode(&components.e)},
            {"kty": "EC", "kid": "ec"},
            {"kty": "RSA", "kid": "broken", "n": "!!", "e": "AQAB"},
        ]})
    }

    #[test]
    fn a_fetched_key_verifies_a_token_and_is_cached() {
        let (url, hits) = serve(vec![ok(&key_set()), ok(&key_set())]);
        let keys = Arc::new(JwksKeys::at(url));
        let verifier = TokenVerifier::new(TENANT, AUDIENCE, keys);

        assert!(verifier.verify_at(&token(&claims()), NOW).is_ok());
        assert!(verifier.verify_at(&token(&claims()), NOW).is_ok());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the second token used the cache"
        );
    }

    #[test]
    fn an_unknown_key_id_refreshes_once_and_is_then_rate_limited() {
        let (url, hits) = serve(vec![ok(&key_set()), ok(&key_set()), ok(&key_set())]);
        let keys = JwksKeys::at(url);

        assert!(keys.signing_key("k1").is_ok());
        // Unknown id right after a fetch: no new request (deviation, §12.2).
        assert_eq!(
            keys.signing_key("never-heard-of-it"),
            Err(KeysError::Unknown)
        );
        assert_eq!(keys.signing_key("another"), Err(KeysError::Unknown));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            keys.signing_key("ec"),
            Err(KeysError::Unknown),
            "non-RSA keys are ignored"
        );
        assert_eq!(
            keys.signing_key("broken"),
            Err(KeysError::Unknown),
            "so are undecodable ones"
        );
    }

    #[test]
    fn an_unreachable_or_failing_key_host_is_unavailable_and_retried() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/keys", listener.local_addr().unwrap());
        drop(listener);
        let keys = JwksKeys::at(url);
        assert_eq!(keys.signing_key("k1"), Err(KeysError::Unavailable));

        let (url, hits) = serve(vec![
            "HTTP/1.1 500 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            ok(&key_set()),
        ]);
        let keys = JwksKeys::at(url);
        assert_eq!(keys.signing_key("k1"), Err(KeysError::Unavailable));
        // A failure is not stamped as a fetch, so the next token retries
        // straight away and succeeds.
        assert!(keys.signing_key("k1").is_ok());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_document_that_is_not_a_key_set_has_no_keys() {
        let (url, _) = serve(vec![ok(&json!("not an object"))]);
        assert_eq!(JwksKeys::at(url).signing_key("k1"), Err(KeysError::Unknown));
    }
}
