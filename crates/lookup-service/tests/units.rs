//! The ported unit suites: configuration, keys, TTL conversion, CSV import,
//! the TTL cache, the resolver, the file store and seed import.
//!
//! Implements `spec/20-lookup-service.md` §11.1. Each module is one of the
//! Python's `tests/test_*.py`.

// The extra allows are test-only style lints: asserting `is_empty()` reads
// better than `assert_eq!(.., [])` for the empty-error cases, and a helper
// that takes a `json!` value by value is the point of the helper.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty,
    clippy::needless_pass_by_value,
    clippy::missing_const_for_fn,
    clippy::format_collect
)]

mod common;

mod config {
    //! `test_config.py`; `spec/20-lookup-service.md` §2.

    use lookup_service::config::{Config, SeedLocation};
    use std::path::PathBuf;

    fn pairs(list: &[(&str, &str)]) -> Result<Config, ihb_config::ConfigErrors> {
        Config::from_pairs(list.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
    }

    /// Authentication configured, so the rest of a test is about its subject
    /// (§2.7); `pairs(&[])` is now a refusal.
    const AUTH: [(&str, &str); 2] = [
        ("LOOKUP_AUTH_TENANT_ID", "tenant"),
        ("LOOKUP_AUTH_AUDIENCE", "api://lookup"),
    ];

    /// `extra` and the authentication settings.
    fn with_auth(
        extra: &[(&'static str, &'static str)],
    ) -> Result<Config, ihb_config::ConfigErrors> {
        let all: Vec<(&str, &str)> = AUTH.iter().chain(extra).copied().collect();
        pairs(&all)
    }

    #[test]
    fn defaults() {
        let config = pairs(&AUTH).unwrap();
        assert_eq!(config.seed, SeedLocation::None);
        assert_eq!(config.environment, "DEV");
        assert_eq!(config.listen.port(), 8080);
        assert!(config.swagger_enabled());
        assert!(config.auth_enabled());
        assert_eq!(
            (
                config.auth_tenant_id.as_deref(),
                config.auth_audience.as_deref()
            ),
            (Some("tenant"), Some("api://lookup"))
        );
        assert!(config.cosmos_endpoint.is_none());
        assert_eq!(
            (
                config.rows_container.as_str(),
                config.config_container.as_str()
            ),
            ("lookup-rows", "lookup-config")
        );
        assert_eq!(config.default_ttl_seconds, 300);
        assert_eq!(config.max_upload_bytes(), 50 * 1024 * 1024);
    }

    #[test]
    fn reads_environment() {
        let config = pairs(&[
            ("LOOKUP_SEED_DIR", "/data/seed"),
            ("ENVIRONMENT", "local"),
            ("PORT", "9000"),
            ("COSMOS_ENDPOINT", "https://localhost:8081"),
            ("COSMOS_KEY", "k"),
            ("COSMOS_DATABASE", "db"),
            ("COSMOS_DISABLE_SSL_VERIFY", "TRUE"),
            ("LOOKUP_ROWS_CONTAINER", "r"),
            ("LOOKUP_CONFIG_CONTAINER", "c"),
            ("LOOKUP_DEFAULT_TTL_SECONDS", "60"),
            ("LOOKUP_MAX_UPLOAD_MB", "5"),
            ("LOOKUP_CACHE_MAX_ENTRIES", "7"),
            ("LOOKUP_UPLOAD_CONCURRENCY", "4"),
            ("LOOKUP_AUTH_DISABLED", "true"),
        ])
        .unwrap();
        assert_eq!(config.seed, SeedLocation::Dir(PathBuf::from("/data/seed")));
        assert_eq!(config.environment, "LOCAL");
        assert!(config.auth_disabled);
        assert!(!config.auth_enabled());
        assert_eq!(config.listen.port(), 9000);
        assert_eq!(
            config.cosmos_endpoint.as_deref(),
            Some("https://localhost:8081")
        );
        assert!(config.cosmos_disable_ssl_verify);
        assert_eq!(
            (
                config.cosmos_database.as_str(),
                config.rows_container.as_str(),
                config.config_container.as_str()
            ),
            ("db", "r", "c")
        );
        assert_eq!(
            (
                config.default_ttl_seconds,
                config.max_upload_mb,
                config.cache_max_entries,
                config.upload_concurrency
            ),
            (60, 5, 7, 4)
        );
    }

    #[test]
    fn production_has_no_swagger() {
        let config = with_auth(&[("ENVIRONMENT", "prd")]).unwrap();
        assert!(!config.swagger_enabled());
    }

    #[test]
    fn blank_values_fall_back_to_defaults() {
        let config = with_auth(&[("PORT", "  "), ("ENVIRONMENT", "")]).unwrap();
        assert_eq!(config.listen.port(), 8080);
        assert_eq!(config.environment, "DEV");
    }

    #[test]
    fn invalid_integers_are_all_reported() {
        let errors = with_auth(&[("PORT", "eighty"), ("LOOKUP_DEFAULT_TTL_SECONDS", "0")])
            .unwrap_err()
            .to_string();
        assert!(errors.contains("PORT"), "{errors}");
        assert!(errors.contains("LOOKUP_DEFAULT_TTL_SECONDS"), "{errors}");
        assert!(errors.contains(">= 1"), "{errors}");
    }

    /// §2.4.
    #[test]
    fn ssl_verification_can_only_be_disabled_for_the_emulator() {
        let errors = with_auth(&[
            (
                "COSMOS_ENDPOINT",
                "https://prod-account.documents.azure.com:443/",
            ),
            ("COSMOS_DISABLE_SSL_VERIFY", "true"),
        ])
        .unwrap_err()
        .to_string();
        assert!(
            errors.contains("only allowed for a local Cosmos emulator"),
            "{errors}"
        );
    }
}

mod keys {
    //! `test_keys.py`; `spec/20-lookup-service.md` §3.1 and §4.

    use std::collections::BTreeMap;

    use lookup_service::keys::{
        MAX_COSMOS_ID_BYTES, canonical_key, encode_row_id, key_text, normalise_part,
    };
    use lookup_service::model::{Case, KeyPartRule, TableDefinition};

    fn ward_definition() -> TableDefinition {
        TableDefinition {
            name: "ward_map".into(),
            description: String::new(),
            key_columns: vec!["sending_facility".into(), "ward_code".into()],
            value_columns: vec!["national_ward_code".into()],
            default_value_column: "national_ward_code".into(),
            key_normalisation: BTreeMap::from([(
                "ward_code".to_owned(),
                KeyPartRule {
                    trim: true,
                    case: Case::Upper,
                },
            )]),
            ttl_seconds: None,
            preload: false,
        }
    }

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    fn named(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn rule(trim: bool, case: Case) -> KeyPartRule {
        KeyPartRule { trim, case }
    }

    #[test]
    fn default_rule_trims_and_preserves_case() {
        assert_eq!(normalise_part("  Ab ", &KeyPartRule::default()), "Ab");
    }

    #[test]
    fn upper_and_lower() {
        assert_eq!(normalise_part(" ab ", &rule(true, Case::Upper)), "AB");
        assert_eq!(normalise_part(" AB ", &rule(true, Case::Lower)), "ab");
    }

    #[test]
    fn trim_disabled_keeps_whitespace() {
        assert_eq!(normalise_part(" a ", &rule(false, Case::Preserve)), " a ");
    }

    #[test]
    fn positional_composite_key() {
        let key = canonical_key(
            &ward_definition(),
            &strings(&["FAC1", " w2 "]),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(key, ["FAC1", "W2"]);
    }

    #[test]
    fn named_key_is_reordered_to_key_columns() {
        let key = canonical_key(
            &ward_definition(),
            &[],
            &named(&[("ward_code", "w2"), ("sending_facility", "FAC1")]),
        )
        .unwrap();
        assert_eq!(key, ["FAC1", "W2"]);
    }

    #[test]
    fn named_and_positional_give_the_same_key() {
        let definition = ward_definition();
        assert_eq!(
            canonical_key(&definition, &strings(&["FAC1", "W2"]), &BTreeMap::new()).unwrap(),
            canonical_key(
                &definition,
                &[],
                &named(&[("sending_facility", "FAC1"), ("ward_code", "W2")])
            )
            .unwrap()
        );
    }

    fn rejection(
        positional: &[&str],
        by_name: &[(&str, &str)],
    ) -> lookup_service::error::LookupError {
        canonical_key(&ward_definition(), &strings(positional), &named(by_name)).unwrap_err()
    }

    #[test]
    fn rejects_wrong_arity() {
        assert!(
            rejection(&["FAC1"], &[])
                .detail
                .contains("Expected 2 key part")
        );
    }

    #[test]
    fn rejects_mixed_styles() {
        assert!(
            rejection(&["FAC1", "W2"], &[("ward_code", "W2")])
                .detail
                .contains("not both")
        );
    }

    #[test]
    fn rejects_missing_key() {
        assert!(rejection(&[], &[]).detail.contains("No key supplied"));
    }

    #[test]
    fn rejects_unknown_part_name() {
        let error = rejection(
            &[],
            &[
                ("sending_facility", "FAC1"),
                ("ward_code", "W2"),
                ("bed", "1"),
            ],
        );
        assert!(
            error.detail.contains("Unknown key part"),
            "{}",
            error.detail
        );
        assert!(error.detail.contains("['bed']"), "{}", error.detail);
    }

    #[test]
    fn rejects_missing_part_name() {
        assert!(
            rejection(&[], &[("sending_facility", "FAC1")])
                .detail
                .contains("Missing key part")
        );
    }

    #[test]
    fn rejects_part_empty_after_normalisation() {
        assert!(
            rejection(&["FAC1", "   "], &[])
                .detail
                .contains("'ward_code' is empty")
        );
    }

    #[test]
    fn error_carries_the_table_name() {
        assert_eq!(rejection(&["FAC1"], &[]).table.as_deref(), Some("ward_map"));
    }

    /// §4.5.
    #[test]
    fn plain_parts_are_readable() {
        assert_eq!(encode_row_id(&strings(&["FAC1", "W2"])), "FAC1::W2");
        assert_eq!(key_text(&strings(&["FAC1", "W2"])), "FAC1 :: W2");
    }

    #[test]
    fn forbidden_and_separator_characters_are_escaped() {
        assert_eq!(
            encode_row_id(&strings(&["a/b", "c\\d?e#f", "g:h%i"])),
            "a%2Fb::c%5Cd%3Fe%23f::g%3Ah%25i"
        );
    }

    #[test]
    fn encoding_is_unambiguous() {
        // Without escaping ':' these two keys would both become "a::b::c".
        assert_ne!(
            encode_row_id(&strings(&["a::b", "c"])),
            encode_row_id(&strings(&["a", "b::c"]))
        );
        assert_ne!(
            encode_row_id(&strings(&["a", "b"])),
            encode_row_id(&strings(&["a::b"]))
        );
    }

    #[test]
    fn non_ascii_is_kept() {
        assert_eq!(
            encode_row_id(&strings(&["Ysbyty Gwynedd ŵ"])),
            "Ysbyty Gwynedd ŵ"
        );
    }

    #[test]
    fn long_keys_are_hashed_deterministically() {
        let key = strings(&[&"x".repeat(200), &"y".repeat(200)]);
        let encoded = encode_row_id(&key);
        assert!(encoded.starts_with("h:"));
        assert!(encoded.len() <= MAX_COSMOS_ID_BYTES);
        assert_eq!(encoded, encode_row_id(&key));
        assert_ne!(
            encoded,
            encode_row_id(&strings(&[&"x".repeat(200), &"y".repeat(201)]))
        );
    }

    /// §4.5: the digest is of the Python's `json.dumps(..., ensure_ascii=False)`
    /// form. These expected values were produced by running that on the Python
    /// side: `hashlib.sha256(json.dumps(key, ensure_ascii=False).encode())`.
    #[test]
    fn hashed_id_matches_the_pythons_json_form() {
        assert_eq!(
            encode_row_id(&strings(&[&"x".repeat(200), &"y".repeat(200)])),
            "h:79e96e1fc0bc351ce8fec8e01705f7db7597208c491664e2e8527700921060ba"
        );
        // Non-ASCII is left unescaped; a quote and a newline are escaped.
        assert_eq!(
            encode_row_id(&strings(&[&"é".repeat(200), "a\"b\n"])),
            "h:89ae12c964233b627df2a6825a6604713693887ee5fcbb81a2917e8082b4a7a7"
        );
    }

    fn definition(
        name: &str,
        key: &[&str],
        values: &[&str],
        default: &str,
        normalisation: BTreeMap<String, KeyPartRule>,
    ) -> TableDefinition {
        TableDefinition {
            name: name.into(),
            description: String::new(),
            key_columns: strings(key),
            value_columns: strings(values),
            default_value_column: default.into(),
            key_normalisation: normalisation,
            ttl_seconds: None,
            preload: false,
        }
    }

    #[test]
    fn default_value_column_must_be_a_value_column() {
        assert!(
            definition("t", &["a"], &["b"], "a", BTreeMap::new())
                .validate()
                .is_err()
        );
    }

    #[test]
    fn columns_must_be_unique() {
        assert!(
            definition("t", &["a"], &["a"], "a", BTreeMap::new())
                .validate()
                .is_err()
        );
    }

    #[test]
    fn rejects_unsafe_column_name() {
        assert!(
            definition("t", &["a b"], &["c"], "c", BTreeMap::new())
                .validate()
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_table_name() {
        assert!(
            definition("Bad Name", &["a"], &["b"], "b", BTreeMap::new())
                .validate()
                .is_err()
        );
    }

    #[test]
    fn normalisation_must_reference_key_columns() {
        let rules = BTreeMap::from([("b".to_owned(), rule(true, Case::Upper))]);
        assert!(
            definition("t", &["a"], &["b"], "b", rules)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn a_valid_definition_validates() {
        assert!(ward_definition().validate().is_ok());
    }

    #[test]
    fn ttl_range_is_enforced() {
        let mut d = ward_definition();
        d.ttl_seconds = Some(0);
        assert!(d.validate().is_err());
        d.ttl_seconds = Some(7 * 86_400 + 1);
        assert!(d.validate().is_err());
        d.ttl_seconds = Some(7 * 86_400);
        assert!(d.validate().is_ok());
    }
}

mod ttl {
    //! `test_ttl.py`; `spec/20-lookup-service.md` §9.4.

    use lookup_service::ttl::{format_ttl, split_ttl, ttl_to_seconds};

    #[test]
    fn conversion() {
        assert_eq!(ttl_to_seconds(90, "seconds", None).unwrap(), 90);
        assert_eq!(ttl_to_seconds(2, "minutes", None).unwrap(), 120);
        assert_eq!(ttl_to_seconds(3, "hours", None).unwrap(), 10_800);
        assert_eq!(ttl_to_seconds(7, "days", None).unwrap(), 604_800);
    }

    #[test]
    fn rejects_out_of_range_and_unknown_unit() {
        for (value, unit) in [
            (0, "seconds"),
            (-1, "hours"),
            (8, "days"),
            (10_081, "minutes"),
            (1, "weeks"),
        ] {
            let error = ttl_to_seconds(value, unit, None).unwrap_err();
            assert_eq!(error.kind.code(), "invalid_settings", "{value} {unit}");
        }
    }

    #[test]
    fn split_picks_the_largest_exact_unit() {
        assert_eq!(split_ttl(86_400), (1, "days"));
        assert_eq!(split_ttl(7200), (2, "hours"));
        assert_eq!(split_ttl(90), (90, "seconds"));
        assert_eq!(split_ttl(300), (5, "minutes"));
    }

    #[test]
    fn format() {
        assert_eq!(format_ttl(1), "1 second");
        assert_eq!(format_ttl(300), "5 minutes");
        assert_eq!(format_ttl(86_400), "1 day");
    }
}

mod csv_import {
    //! `test_csv_import.py`; `spec/20-lookup-service.md` §6.

    use super::common::codes_definition;
    use lookup_service::import::{
        build_definition, decode_csv_bytes, import_rows, read_csv_default,
    };
    use lookup_service::mapping::{FieldSpec, RecordMapping, Transform, apply_transforms};
    use lookup_service::model::{Case, KeyPartRule, TableDefinition};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn settings(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn skips_blank_lines_and_tracks_physical_line_numbers() {
        let (document, errors) = read_csv_default("code,label\n\nA,\"multi\nline\"\nB,Beta\n");
        assert!(errors.is_empty());
        assert_eq!(document.header, ["code", "label"]);
        // The quoted value spans two physical lines, so A ends on line 4 and
        // B is reported on line 5.
        let lines: Vec<usize> = document.records.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [4, 5]);
        assert_eq!(document.records[0].1[1], "multi\nline");
    }

    #[test]
    fn ragged_rows_are_errors() {
        let (_, errors) = read_csv_default("code,label\nA\nB,Beta,extra\n");
        let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "line 2: expected 2 columns, got 1",
                "line 3: expected 2 columns, got 3"
            ]
        );
    }

    #[test]
    fn empty_and_blank_header() {
        assert!(
            read_csv_default("").1[0]
                .to_string()
                .contains("header row is required")
        );
        assert!(
            read_csv_default("code,,label\nA,B,C\n").1[0]
                .to_string()
                .contains("empty column name")
        );
    }

    #[test]
    fn crlf_and_cr_terminators_and_a_missing_final_newline() {
        let (document, errors) = read_csv_default("code,label\r\nA,x\rB,y");
        assert!(errors.is_empty());
        let lines: Vec<usize> = document.records.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [2, 3]);
    }

    #[test]
    fn quote_handling_matches_pythons_defaults() {
        let (document, _) = read_csv_default("a,b\n\"x \"\"q\"\" y\",mid\"dle\n");
        assert_eq!(document.records[0].1, ["x \"q\" y", "mid\"dle"]);
    }

    #[test]
    fn decode_strips_bom_and_rejects_invalid_utf8() {
        assert_eq!(
            decode_csv_bytes("\u{feff}code\n".as_bytes()).unwrap(),
            "code\n"
        );
        assert_eq!(
            decode_csv_bytes(b"\xff\xfe").unwrap_err().kind.code(),
            "invalid_upload"
        );
    }

    #[test]
    fn identity_mapping() {
        let (document, _) = read_csv_default("code,label\nA,Alpha\n");
        let result = import_rows(&document, &codes_definition(), None);
        assert!(result.errors.is_empty());
        assert_eq!(result.rows[0].key, ["A"]);
        assert_eq!(result.rows[0].values.get("label"), Some("Alpha"));
    }

    #[test]
    fn mapping_renames_columns_and_applies_transforms() {
        let (document, _) = read_csv_default("Code,Description\n a ,  Alpha \n");
        let mapping = RecordMapping {
            key: BTreeMap::from([(
                "code".to_owned(),
                FieldSpec::with("Code", &[Transform::Trim, Transform::Upper]),
            )]),
            values: BTreeMap::from([(
                "label".to_owned(),
                FieldSpec::with("Description", &[Transform::Trim]),
            )]),
        };
        let result = import_rows(&document, &codes_definition(), Some(&mapping));
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.rows[0].key, ["A"]);
        assert_eq!(result.rows[0].values.get("label"), Some("Alpha"));
    }

    #[test]
    fn key_normalisation_applies_after_transforms() {
        let mut definition = codes_definition();
        definition.key_normalisation = BTreeMap::from([(
            "code".to_owned(),
            KeyPartRule {
                trim: true,
                case: Case::Lower,
            },
        )]);
        let (document, _) = read_csv_default("code,label\nABC,x\n");
        assert_eq!(
            import_rows(&document, &definition, None).rows[0].key,
            ["abc"]
        );
    }

    #[test]
    fn mapping_targets_must_match_table_columns() {
        let (document, _) = read_csv_default("code,label\nA,Alpha\n");
        let mapping = RecordMapping {
            key: BTreeMap::from([("code".to_owned(), FieldSpec::plain("code"))]),
            values: BTreeMap::from([("other".to_owned(), FieldSpec::plain("label"))]),
        };
        let errors = import_rows(&document, &codes_definition(), Some(&mapping)).errors;
        assert!(
            errors
                .iter()
                .any(|e| e.to_string().contains("value targets"))
        );
    }

    #[test]
    fn header_must_contain_mapped_columns_and_nothing_else() {
        let (document, _) = read_csv_default("code,notes\nA,n\n");
        let errors: Vec<String> = import_rows(&document, &codes_definition(), None)
            .errors
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            errors.contains(&"line 1: columns ['label'] are not in the header".to_owned()),
            "{errors:?}"
        );
        assert!(
            errors.contains(
                &"line 1: header columns ['notes'] are neither key nor value columns".to_owned()
            ),
            "{errors:?}"
        );
    }

    /// §6.4.
    #[test]
    fn collects_errors_without_values_and_truncates() {
        let body: String = (0..60).map(|i| format!("DUP{},x\n", i % 2)).collect();
        let (document, _) = read_csv_default(&format!("code,label\n{body}"));
        let result = import_rows(&document, &codes_definition(), None);
        let messages = result.error_messages();
        assert_eq!(result.errors.len(), 50);
        assert!(result.truncated);
        assert!(
            messages
                .last()
                .unwrap()
                .contains("further errors suppressed")
        );
        assert!(!messages.iter().any(|m| m.contains("DUP")));
    }

    #[test]
    fn composite_key() {
        let definition = TableDefinition {
            name: "ward_map".into(),
            description: String::new(),
            key_columns: vec!["fac".into(), "ward".into()],
            value_columns: vec!["code".into()],
            default_value_column: "code".into(),
            key_normalisation: BTreeMap::new(),
            ttl_seconds: None,
            preload: false,
        };
        let (document, _) = read_csv_default("fac,ward,code\nF1,W1,X\nF1,W2,Y\nF1,W1,Z\n");
        let result = import_rows(&document, &definition, None);
        let keys: Vec<&Vec<String>> = result.rows.iter().map(|r| &r.key).collect();
        assert_eq!(keys, [&vec!["F1", "W1"], &vec!["F1", "W2"]]);
        assert_eq!(
            result.errors[0].to_string(),
            "line 4: duplicate key (first seen on line 2)"
        );
    }

    #[test]
    fn build_definition_defaults_from_header() {
        let header: Vec<String> = ["code", "a", "b"].iter().map(ToString::to_string).collect();
        let definition = build_definition("codes", &header, &settings(json!({}))).unwrap();
        assert_eq!(definition.key_columns, ["code"]);
        assert_eq!(definition.value_columns, ["a", "b"]);
        assert_eq!(definition.default_value_column, "a");
        assert!(!definition.preload);
    }

    #[test]
    fn build_definition_overrides() {
        let header: Vec<String> = ["x", "y", "z"].iter().map(ToString::to_string).collect();
        let definition = build_definition(
            "codes",
            &header,
            &settings(json!({
                "key_columns": ["x", "y"], "default_value_column": "z", "ttl_seconds": 30,
                "preload": true, "key_normalisation": {"x": {"case": "upper"}}
            })),
        )
        .unwrap();
        assert_eq!(definition.key_columns, ["x", "y"]);
        assert_eq!(definition.ttl_seconds, Some(30));
        assert_eq!(definition.rule_for("x").case, Case::Upper);
    }

    #[test]
    fn build_definition_invalid() {
        let only_key: Vec<String> = vec!["only_key".into()];
        assert!(build_definition("codes", &only_key, &settings(json!({}))).is_err());
        let two: Vec<String> = vec!["a".into(), "b".into()];
        assert!(build_definition("codes", &two, &settings(json!({"ttl_seconds": 0}))).is_err());
    }

    #[test]
    fn transforms_apply_in_order() {
        assert_eq!(
            apply_transforms("  Ab ", &[Transform::Trim, Transform::Upper]),
            "AB"
        );
        assert_eq!(apply_transforms("Ab", &[Transform::Lower]), "ab");
        assert_eq!(apply_transforms(" x ", &[]), " x ");
    }
}

mod table_cache {
    //! `test_table_cache.py`; `spec/20-lookup-service.md` §5.3.

    use std::sync::{Arc, Mutex};

    use super::common::make_row;
    use lookup_service::cache::{TableCache, Timer};

    fn clock() -> (Arc<Mutex<f64>>, Timer) {
        let now = Arc::new(Mutex::new(1000.0));
        let reader = Arc::clone(&now);
        (now, Arc::new(move || *reader.lock().unwrap()))
    }

    fn key(part: &str) -> Vec<String> {
        vec![part.to_owned()]
    }

    #[test]
    fn hit_and_miss_counters() {
        let (_, timer) = clock();
        let mut cache = TableCache::new(60, 2, timer);
        assert!(cache.get(&key("A")).is_none());
        cache.put(key("A"), make_row(&["A"], &[("label", "x")]));
        assert!(cache.get(&key("A")).is_some());
        assert_eq!((cache.hits(), cache.misses()), (1, 1));
    }

    #[test]
    fn entries_expire_after_the_ttl() {
        let (now, timer) = clock();
        let mut cache = TableCache::new(60, 2, timer);
        cache.put(key("A"), make_row(&["A"], &[("label", "x")]));
        *now.lock().unwrap() += 59.0;
        assert!(cache.get(&key("A")).is_some());
        *now.lock().unwrap() += 2.0;
        assert!(cache.get(&key("A")).is_none());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn evicts_beyond_max_entries() {
        let (_, timer) = clock();
        let mut cache = TableCache::new(60, 2, timer);
        for part in ["A", "B", "C"] {
            cache.put(key(part), make_row(&[part], &[("label", "x")]));
        }
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn evicts_the_least_recently_used() {
        let (_, timer) = clock();
        let mut cache = TableCache::new(60, 2, timer);
        cache.put(key("A"), make_row(&["A"], &[("label", "x")]));
        cache.put(key("B"), make_row(&["B"], &[("label", "x")]));
        assert!(cache.get(&key("A")).is_some());
        cache.put(key("C"), make_row(&["C"], &[("label", "x")]));
        assert!(cache.get(&key("A")).is_some());
        assert!(cache.get(&key("B")).is_none());
    }

    #[test]
    fn clear() {
        let (_, timer) = clock();
        let mut cache = TableCache::new(60, 2, timer);
        cache.put(key("A"), make_row(&["A"], &[("label", "x")]));
        cache.clear();
        assert_eq!(cache.len(), 0);
    }
}

mod resolver {
    //! `test_resolver.py`; `spec/20-lookup-service.md` §5.

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::common::{codes_definition, make_row};
    use lookup_service::cache::Timer;
    use lookup_service::error::Kind;
    use lookup_service::memory_store::MemoryTableStore;
    use lookup_service::model::{CacheMode, UploadInfo};
    use lookup_service::resolver::LookupResolver;
    use lookup_service::store::TableStore;

    struct Fixture {
        store: Arc<MemoryTableStore>,
        now: Arc<Mutex<f64>>,
        resolver: LookupResolver<MemoryTableStore>,
    }

    async fn fixture() -> Fixture {
        let store = Arc::new(MemoryTableStore::default());
        store.put_table(
            codes_definition(),
            vec![make_row(&["A"], &[("label", "Alpha")])],
        );
        let mut hot = codes_definition();
        hot.name = "hot".into();
        hot.preload = true;
        store.put_table(hot, vec![make_row(&["H"], &[("label", "Hot")])]);
        let now = Arc::new(Mutex::new(1000.0));
        let reader = Arc::clone(&now);
        let timer: Timer = Arc::new(move || *reader.lock().unwrap());
        let resolver = LookupResolver::with_timer(Arc::clone(&store), 300, 100, timer);
        resolver.start().await.unwrap();
        Fixture {
            store,
            now,
            resolver,
        }
    }

    fn k(part: &str) -> Vec<String> {
        vec![part.to_owned()]
    }

    async fn look(
        f: &Fixture,
        table: &str,
        part: &str,
    ) -> Result<lookup_service::model::LookupResult, lookup_service::error::LookupError> {
        f.resolver.lookup(table, &k(part), &BTreeMap::new()).await
    }

    #[tokio::test]
    async fn ttl_table_reads_the_store_once_then_serves_from_cache() {
        let f = fixture().await;
        let first = look(&f, "codes", "A").await.unwrap();
        let second = look(&f, "codes", "A").await.unwrap();
        assert!(!first.cached);
        assert!(second.cached);
        assert_eq!(second.value, "Alpha");
        assert_eq!(f.store.get_row_calls(), 1);
    }

    #[tokio::test]
    async fn cache_entry_expires_after_the_table_ttl() {
        let f = fixture().await;
        look(&f, "codes", "A").await.unwrap();
        *f.now.lock().unwrap() += 301.0;
        look(&f, "codes", "A").await.unwrap();
        assert_eq!(f.store.get_row_calls(), 2);
    }

    #[tokio::test]
    async fn per_table_ttl_overrides_the_default() {
        let f = fixture().await;
        let mut definition = codes_definition();
        definition.ttl_seconds = Some(10);
        f.store
            .put_table(definition, vec![make_row(&["A"], &[("label", "Alpha")])]);
        f.resolver.refresh_table("codes").await.unwrap();
        look(&f, "codes", "A").await.unwrap();
        *f.now.lock().unwrap() += 11.0;
        look(&f, "codes", "A").await.unwrap();
        assert_eq!(f.store.get_row_calls(), 2);
        assert_eq!(f.resolver.detail("codes").unwrap().summary.ttl_seconds, 10);
    }

    #[tokio::test]
    async fn preloaded_table_never_reads_the_store_per_key() {
        let f = fixture().await;
        let result = look(&f, "hot", "H").await.unwrap();
        assert!(result.cached);
        assert_eq!(f.store.get_row_calls(), 0);
        assert_eq!(
            look(&f, "hot", "missing").await.unwrap_err().kind,
            Kind::KeyNotFound
        );
        assert_eq!(f.store.get_row_calls(), 0);
    }

    #[tokio::test]
    async fn misses_are_not_cached_yet() {
        let f = fixture().await;
        for _ in 0..2 {
            assert_eq!(
                look(&f, "codes", "nope").await.unwrap_err().kind,
                Kind::KeyNotFound
            );
        }
        assert_eq!(f.store.get_row_calls(), 2);
    }

    /// §4.4.
    #[tokio::test]
    async fn key_not_found_does_not_echo_the_key() {
        let f = fixture().await;
        let error = look(&f, "codes", "SECRET999").await.unwrap_err();
        assert!(!error.detail.contains("SECRET999"));
    }

    #[tokio::test]
    async fn unknown_table() {
        let f = fixture().await;
        assert_eq!(
            look(&f, "ghost", "A").await.unwrap_err().kind,
            Kind::TableNotFound
        );
    }

    #[tokio::test]
    async fn backend_failure_propagates() {
        let f = fixture().await;
        f.store.set_fail_reads(true);
        assert_eq!(
            look(&f, "codes", "A").await.unwrap_err().kind,
            Kind::BackendUnavailable
        );
    }

    #[tokio::test]
    async fn refresh_swaps_the_preloaded_snapshot_and_drops_the_cache() {
        let f = fixture().await;
        look(&f, "codes", "A").await.unwrap();
        let mut hot = codes_definition();
        hot.name = "hot".into();
        hot.preload = true;
        let upload = UploadInfo {
            actor: "me".into(),
            file_name: None,
        };
        f.store
            .replace_table(&hot, &[make_row(&["H"], &[("label", "Hotter")])], &upload)
            .await
            .unwrap();
        f.resolver.refresh_table("hot").await.unwrap();
        assert_eq!(look(&f, "hot", "H").await.unwrap().value, "Hotter");
        f.store
            .replace_table(
                &codes_definition(),
                &[make_row(&["A"], &[("label", "Changed")])],
                &upload,
            )
            .await
            .unwrap();
        f.resolver.refresh_table("codes").await.unwrap();
        assert_eq!(look(&f, "codes", "A").await.unwrap().value, "Changed");
    }

    #[tokio::test]
    async fn refresh_of_a_deleted_table_removes_it() {
        let f = fixture().await;
        f.store.remove_table("codes");
        f.resolver.refresh_table("codes").await.unwrap();
        assert!(!f.resolver.has_table("codes"));
    }

    #[tokio::test]
    async fn summaries_report_cache_stats() {
        let f = fixture().await;
        look(&f, "codes", "A").await.unwrap();
        look(&f, "codes", "A").await.unwrap();
        let summaries = f.resolver.summaries();
        let codes = summaries.iter().find(|s| s.name == "codes").unwrap();
        assert_eq!(codes.cache.mode, CacheMode::Ttl);
        assert_eq!((codes.cache.hits, codes.cache.misses), (1, 1));
        assert_eq!(codes.cache.entries, 1);
        let hot = summaries.iter().find(|s| s.name == "hot").unwrap();
        assert_eq!(hot.cache.mode, CacheMode::Preloaded);
        assert_eq!(hot.cache.entries, 1);
    }
}

mod file_store {
    //! `test_file_store.py`; `spec/20-lookup-service.md` §7.

    use super::common::{SeedDir, repo_seed_dir};
    use lookup_service::file_store::{FileTableStore, MAX_SEED_FILE_BYTES, SeedSource};
    use lookup_service::model::{TableRecord, UploadInfo};
    use lookup_service::store::TableStore;
    use serde_json::json;

    fn load(dir: &SeedDir) -> Result<FileTableStore, lookup_service::error::SeedDataError> {
        FileTableStore::load(&SeedSource::from_dir(&dir.path)?)
    }

    async fn table(store: &FileTableStore, name: &str) -> TableRecord {
        store.get_table(name).await.unwrap().unwrap()
    }

    fn key(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    fn assert_fails(dir: &SeedDir, message: &str) {
        let error = load(dir).unwrap_err().to_string();
        assert!(error.contains(message), "{message:?} not in {error:?}");
    }

    /// The seed data shipped in the image must always load (§7.4).
    #[tokio::test]
    async fn repo_seed_loads() {
        let store = FileTableStore::load(&SeedSource::from_dir(&repo_seed_dir()).unwrap()).unwrap();
        let hb = table(&store, "health_board_mapping").await;
        assert_eq!(hb.stats.row_count, 4);
        assert!(hb.definition.preload);
        assert_eq!(
            table(&store, "ward_map").await.definition.key_columns,
            ["sending_facility", "ward_code"]
        );
    }

    /// §7.5: `local/lookup-seed`, mounted by local Compose, must always
    /// load: a bad file there stops the local stack starting.
    #[tokio::test]
    async fn the_local_seed_holds_the_pims_to_mpi_codes() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("local")
            .join("lookup-seed");
        let store = FileTableStore::load(&SeedSource::from_dir(&dir).unwrap()).unwrap();
        let names: Vec<String> = store
            .list_tables()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.definition.name)
            .collect();
        assert_eq!(names, ["pims_to_mpi_codes"]);

        let table = table(&store, "pims_to_mpi_codes").await;
        assert_eq!(table.definition.key_columns, ["code_type", "pims_code"]);
        let value = |parts: &[&str]| {
            let store = &store;
            let key = key(parts);
            async move {
                store
                    .get_row("pims_to_mpi_codes", &key)
                    .await
                    .unwrap()
                    .unwrap()
                    .values
                    .get("mpi_code")
                    .map(str::to_owned)
            }
        };
        assert_eq!(value(&["gender", "I"]).await.as_deref(), Some("U"));
        assert_eq!(value(&["nhs-status", "NN"]).await.as_deref(), Some("1"));
        // The same pims_code under two code types must not collide.
        assert_eq!(value(&["marital-status", "M"]).await.as_deref(), Some("M"));
        assert_eq!(value(&["gender", "M"]).await.as_deref(), Some("M"));
        // A blank target is a row, meaning "leave the field unset".
        assert_eq!(value(&["marital-status", "A"]).await.as_deref(), Some(""));
    }

    /// §7.4: mirrors `HEALTH_BOARD_MAPPING` in the ChemoCare `pid_mapper`.
    #[tokio::test]
    async fn health_board_mapping_matches_chemo_pid_mapper() {
        let store = FileTableStore::load(&SeedSource::from_dir(&repo_seed_dir()).unwrap()).unwrap();
        for (code, board) in [
            ("224", "VCC"),
            ("212", "BCUCC"),
            ("192", "SWWCC"),
            ("245", "SEWCC"),
        ] {
            let row = store
                .get_row("health_board_mapping", &key(&[code]))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.values.get("health_board"), Some(board));
        }
    }

    #[tokio::test]
    async fn first_column_is_the_key_by_default() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label,extra\nA,Alpha,x\n");
        let record = table(&load(&dir).unwrap(), "codes").await;
        assert_eq!(record.definition.key_columns, ["code"]);
        assert_eq!(record.definition.value_columns, ["label", "extra"]);
        assert_eq!(record.definition.default_value_column, "label");
        assert_eq!(record.stats.last_upload_by.as_deref(), Some("seed"));
    }

    #[tokio::test]
    async fn manifest_declares_composite_key_normalisation_ttl_and_preload() {
        let dir = SeedDir::new();
        dir.write_csv("ward_map", "fac,ward,code\nfac1,w1,X\n");
        dir.write_manifest(&json!({"ward_map": {"key_columns": ["fac", "ward"], "ttl_seconds": 60,
            "preload": true, "key_normalisation": {"fac": {"case": "upper"}, "ward": {"case": "upper"}}}}));
        let store = load(&dir).unwrap();
        assert!(
            store
                .get_row("ward_map", &key(&["FAC1", "W1"]))
                .await
                .unwrap()
                .is_some()
        );
        let record = table(&store, "ward_map").await;
        assert_eq!(record.definition.ttl_seconds, Some(60));
        assert!(record.definition.preload);
    }

    #[tokio::test]
    async fn tolerates_bom_and_blank_lines_and_trims_the_header() {
        let dir = SeedDir::new();
        dir.write_bytes(
            "codes",
            b"\xEF\xBB\xBF code , label \n\nA,Alpha\n,\nB,Beta\n",
        );
        let records = load(&dir).unwrap().list_tables().await.unwrap();
        // `,` has only blank cells, so it is skipped like a blank line.
        assert_eq!(records[0].stats.row_count, 2);
        assert_eq!(records[0].definition.key_columns, ["code"]);
    }

    #[tokio::test]
    async fn quoted_values_with_commas() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,\"Alpha, the first\"\n");
        let row = load(&dir)
            .unwrap()
            .get_row("codes", &key(&["A"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.values.get("label"), Some("Alpha, the first"));
    }

    #[tokio::test]
    async fn empty_seed_dir_loads_no_tables() {
        let dir = SeedDir::new();
        assert!(load(&dir).unwrap().list_tables().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_table_reads_return_nothing() {
        let dir = SeedDir::new();
        let store = load(&dir).unwrap();
        assert!(
            store
                .get_row("missing", &key(&["A"]))
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.get_table("missing").await.unwrap().is_none());
        assert!(store.load_rows("missing").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn query_rows_searches_keys_and_pages() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA1,x\nB1,y\nA2,z\n");
        let store = load(&dir).unwrap();
        let (rows, total) = store.query_rows("codes", Some("a"), 0, 1).await.unwrap();
        assert_eq!(total, 2);
        assert_eq!(
            rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
            [["A1"]]
        );
        let (rows, _) = store.query_rows("codes", None, 1, 10).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
            [["A2"], ["B1"]]
        );
    }

    #[tokio::test]
    async fn is_read_only() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,Alpha\n");
        let store = load(&dir).unwrap();
        let record = table(&store, "codes").await;
        assert!(!store.writable());
        let error = store
            .replace_table(
                &record.definition,
                &[],
                &UploadInfo {
                    actor: "me".into(),
                    file_name: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind.code(), "read_only");
    }

    #[test]
    fn missing_seed_dir() {
        let dir = SeedDir::new();
        let error = SeedSource::from_dir(&dir.path.join("nope"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not exist"));
    }

    #[test]
    fn empty_file() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "");
        assert_fails(&dir, "header row is required");
    }

    #[test]
    fn duplicate_key_reports_lines_not_values() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nSECRET,Alpha\nSECRET,Beta\n");
        let error = load(&dir).unwrap_err().to_string();
        assert!(error.contains("line 3: duplicate key (first seen on line 2)"));
        assert!(!error.contains("SECRET"));
    }

    #[test]
    fn duplicate_after_normalisation() {
        // " a " and "A" collide once the key part is trimmed and upper-cased.
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\n a ,Alpha\nA,Beta\n");
        dir.write_manifest(&json!({"codes": {"key_normalisation": {"code": {"case": "upper"}}}}));
        assert_fails(&dir, "duplicate key");
    }

    #[test]
    fn wrong_column_count() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,Alpha,extra\n");
        assert_fails(&dir, "line 2: expected 2 columns, got 3");
    }

    #[test]
    fn empty_key_part() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\n  ,Alpha\n");
        assert_fails(&dir, "line 2: empty key part");
    }

    #[test]
    fn duplicate_header() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,code\nA,B\n");
        assert_fails(&dir, "duplicate column names");
    }

    #[test]
    fn key_only_file_has_no_value_columns() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code\nA\n");
        assert_fails(&dir, "invalid table definition");
    }

    #[test]
    fn invalid_table_name_from_file_name() {
        let dir = SeedDir::new();
        dir.write_csv("Bad Name", "code,label\nA,Alpha\n");
        assert_fails(&dir, "invalid table definition");
    }

    #[test]
    fn invalid_normalisation_rule() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,Alpha\n");
        dir.write_manifest(&json!({"codes": {"key_normalisation": {"code": {"case": "shouty"}}}}));
        assert_fails(&dir, "invalid table definition");
    }

    #[test]
    fn manifest_without_csv() {
        let dir = SeedDir::new();
        dir.write_manifest(&json!({"ghost": {"key_columns": ["a"]}}));
        assert_fails(&dir, "no CSV file");
    }

    #[test]
    fn manifest_unknown_field() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,Alpha\n");
        dir.write_manifest(&json!({"codes": {"key_colums": ["code"]}}));
        assert_fails(&dir, "unknown fields");
    }

    #[test]
    fn manifest_key_columns_must_be_a_list() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,Alpha\n");
        dir.write_manifest(&json!({"codes": {"key_columns": "code"}}));
        assert_fails(&dir, "must be a list of strings");
    }

    #[test]
    fn manifest_column_not_in_header() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,Alpha\n");
        dir.write_manifest(
            &json!({"codes": {"key_columns": ["code"], "value_columns": ["missing"]}}),
        );
        assert_fails(&dir, "['missing'] are not in the header");
    }

    #[test]
    fn unmapped_header_column() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label,notes\nA,Alpha,n\n");
        dir.write_manifest(&json!({"codes": {"value_columns": ["label"]}}));
        assert_fails(&dir, "['notes'] are neither key nor value columns");
    }

    #[test]
    fn invalid_manifest_json() {
        let dir = SeedDir::new();
        std::fs::write(dir.path.join("tables.json"), "{not json").unwrap();
        assert_fails(&dir, "could not be read");
    }

    #[test]
    fn manifest_wrong_shape() {
        let dir = SeedDir::new();
        std::fs::write(dir.path.join("tables.json"), r#"{"tables": []}"#).unwrap();
        assert_fails(&dir, "must be an object");
    }

    #[test]
    fn not_utf8() {
        let dir = SeedDir::new();
        dir.write_bytes("codes", b"code,label\nA,\xff\n");
        assert_fails(&dir, "not valid UTF-8");
    }

    #[test]
    fn oversized_file() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\n");
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path.join("codes.csv"))
            .unwrap();
        file.set_len(MAX_SEED_FILE_BYTES + 1).unwrap();
        assert_fails(&dir, "seed file limit");
    }
}

mod seed {
    //! `test_seed.py`; `spec/20-lookup-service.md` §7.3.

    use super::common::{SeedDir, codes_definition, make_row, repo_seed_dir};
    use lookup_service::config::SeedLocation;
    use lookup_service::memory_store::MemoryTableStore;
    use lookup_service::seed::import_missing_seed_tables;

    #[tokio::test]
    async fn imports_all_seed_tables_into_an_empty_store() {
        let store = MemoryTableStore::default();
        let mut imported = import_missing_seed_tables(&store, &SeedLocation::Dir(repo_seed_dir()))
            .await
            .unwrap();
        imported.sort();
        assert_eq!(imported, ["health_board_mapping", "ward_map"]);
        let record = store.record("health_board_mapping").unwrap();
        assert_eq!(record.stats.last_upload_by.as_deref(), Some("seed"));
        assert_eq!(
            store
                .row("health_board_mapping", &["224"])
                .unwrap()
                .values
                .get("health_board"),
            Some("VCC")
        );
    }

    #[tokio::test]
    async fn never_overwrites_existing_tables() {
        let store = MemoryTableStore::default();
        let mut definition = codes_definition();
        definition.name = "health_board_mapping".into();
        definition.key_columns = vec!["msh3".into()];
        definition.value_columns = vec!["health_board".into()];
        definition.default_value_column = "health_board".into();
        store.put_table(
            definition,
            vec![make_row(&["224"], &[("health_board", "UPLOADED")])],
        );
        let imported = import_missing_seed_tables(&store, &SeedLocation::Dir(repo_seed_dir()))
            .await
            .unwrap();
        assert_eq!(imported, ["ward_map"]);
        assert_eq!(
            store
                .row("health_board_mapping", &["224"])
                .unwrap()
                .values
                .get("health_board"),
            Some("UPLOADED")
        );
    }

    #[tokio::test]
    async fn read_only_store_is_skipped() {
        let imported = import_missing_seed_tables(
            &MemoryTableStore::new(false),
            &SeedLocation::Dir(repo_seed_dir()),
        )
        .await
        .unwrap();
        assert!(imported.is_empty());
    }

    #[tokio::test]
    async fn bad_seed_stops_startup() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,1\nA,2\n");
        assert!(
            import_missing_seed_tables(
                &MemoryTableStore::default(),
                &SeedLocation::Dir(dir.path.clone())
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn missing_seed_dir_is_skipped() {
        let dir = SeedDir::new();
        let imported = import_missing_seed_tables(
            &MemoryTableStore::default(),
            &SeedLocation::Dir(dir.path.join("none")),
        )
        .await
        .unwrap();
        assert!(imported.is_empty());
    }
}
