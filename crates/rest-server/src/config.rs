//! What one deployment of the REST ingress is.
//!
//! Implements `spec/02-configuration.md` §12.
//!
//! The identity and the HL7 variables are `hl7-rest-server`'s, read by the
//! same functions (`spec/19-rest-servers.md` §0.1), so the two services
//! cannot disagree about what `WRRS_QUEUE_NAME` means.

use std::net::SocketAddr;
use std::path::PathBuf;

use hl7_rest_server::config::{
    Hl7Settings, Identity, SERVICE_BUS_LIMIT, Tls, read_tls, socket_addr,
};
use ihb_config::{ConfigErrors, Reader};
use ihb_validation::Flow;

use crate::adapter::ContentAdapter;
use crate::validator::Validator;

/// Default HTTP port.
pub const DEFAULT_PORT: u16 = 8080;

/// Default endpoint path.
pub const DEFAULT_PATH: &str = "/ingest";

/// Default `ALLOWED_HL7_STRUCTURES`.
pub const DEFAULT_STRUCTURES: &str = "ADT_A05,ADT_A39";

/// Default request-body limit, 1 MiB.
pub const DEFAULT_MAX_REQUEST_SIZE: usize = 1_048_576;

/// The values `CONTENT_ADAPTER` takes.
const CONTENT_ADAPTERS: [&str; 2] = ["soap", "xml-raw"];

/// The values `VALIDATOR_TYPE` takes.
const VALIDATOR_TYPES: [&str; 4] = ["hl7-xsd", "hl7apy", "xsd", "none"];

/// The values `OUTPUT_FORMAT` takes.
const OUTPUT_FORMATS: [&str; 2] = ["er7", "raw"];

/// What `OUTPUT_FORMAT` publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// The payload converted to ER7.
    Er7,
    /// The validated payload, unchanged.
    Raw,
}

/// The `generic` pipeline's settings.
#[derive(Debug, Clone)]
pub struct Generic {
    /// `ENDPOINT_PATH`.
    pub endpoint_path: String,
    /// `CONTENT_ADAPTER`, with its locators.
    pub adapter: ContentAdapter,
    /// `VALIDATOR_TYPE`, with what it needs.
    pub validator: Validator,
    /// `ALLOWED_SOURCE_IDENTIFIERS`. Empty means no allow-list.
    pub allowed_source_identifiers: Vec<String>,
    /// `ALLOWED_TRIGGER_EVENTS` (upstream `#455`). Empty means any. Checked
    /// against `MSH-9.2` after the source allow-list (`spec/19-rest-servers.md`
    /// §9.5.4).
    pub allowed_trigger_events: Vec<String>,
    /// `OUTPUT_FORMAT`.
    pub output_format: OutputFormat,
}

/// Which pipeline this deployment runs.
#[derive(Debug, Clone)]
pub enum Pipeline {
    /// `PIPELINE=generic`.
    Generic(Generic),
    /// `PIPELINE=hl7`: `hl7-rest-server`'s pipeline.
    Hl7(Hl7Settings),
}

/// One deployment's configuration.
///
/// # Examples
///
/// ```
/// use rest_server::config::{Config, Pipeline};
///
/// let config = Config::from_pairs([
///     ("WORKFLOW_ID", "lims-rest"),
///     ("MICROSERVICE_ID", "rest-server"),
///     ("HEALTH_BOARD", "WLIMS"),
///     ("PEER_SERVICE", "MPI"),
///     ("EGRESS_QUEUE_NAME", "lims-transformer-ingress"),
///     ("EGRESS_SESSION_ID", "lims-rest"),
///     ("CONTENT_ADAPTER", "xml-raw"),
///     ("VALIDATOR_TYPE", "none"),
///     ("OUTPUT_FORMAT", "raw"),
/// ])
/// .expect("this configuration is complete");
///
/// assert_eq!(config.listen.to_string(), "127.0.0.1:8080");
/// assert!(matches!(config.pipeline, Pipeline::Generic(_)));
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Who this is and where it sends.
    pub identity: Identity,
    /// What it runs.
    pub pipeline: Pipeline,
    /// Where the HTTP listener binds.
    pub listen: SocketAddr,
    /// Where the TCP health check binds.
    pub health_check: SocketAddr,
    /// `ENVIRONMENT`, upper-cased.
    pub environment: String,
    /// `MAX_REQUEST_SIZE_BYTES`, resolved.
    pub max_request_size: usize,
    /// TLS, when both files are configured.
    pub tls: Option<Tls>,
}

impl Config {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        Self::read(Reader::from_env())
    }

    /// Read from explicit pairs, for tests and examples.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigErrors>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self::read(Reader::from_pairs(pairs))
    }

    /// The shared body of the two constructors.
    fn read(mut reader: Reader) -> Result<Self, ConfigErrors> {
        let identity = Identity::read(&mut reader);

        let endpoint_path = reader.string_or("ENDPOINT_PATH", DEFAULT_PATH);
        if !endpoint_path.starts_with('/') {
            reader.invalid(
                "ENDPOINT_PATH",
                "a path beginning with \"/\"",
                endpoint_path.clone(),
            );
        }

        let pipeline_name = reader.string_or("PIPELINE", "generic");
        let pipeline = match pipeline_name.as_str() {
            "generic" => read_generic(&mut reader, endpoint_path).map(Pipeline::Generic),
            "hl7" => {
                // `spec/02-configuration.md` §12.1 — a misapplied setting is
                // a startup failure, never a silent no-op.
                for name in ["CONTENT_ADAPTER", "VALIDATOR_TYPE", "OUTPUT_FORMAT"] {
                    if reader.optional_string(name).is_some() {
                        reader.conflict(format!(
                            "{name} is only valid when PIPELINE=generic (current PIPELINE=hl7)."
                        ));
                    }
                }
                Some(Pipeline::Hl7(Hl7Settings::read(&mut reader)))
            }
            _ => {
                reader.invalid("PIPELINE", "generic or hl7", pipeline_name);
                None
            }
        };

        let host = reader.string_or("HOST", "127.0.0.1");
        let port = reader.integer_or("PORT", DEFAULT_PORT);

        // A value is returned even when the pipeline could not be read, so
        // the rest of the configuration is read and its problems reported
        // too; every path that yields `None` has recorded an error, so
        // `finish` refuses regardless.
        let pipeline = pipeline.unwrap_or_else(|| Pipeline::Hl7(Hl7Settings::default()));

        let config = Self {
            identity,
            pipeline,
            listen: socket_addr(&host, port),
            health_check: read_health_check(&mut reader),
            environment: reader.string_or("ENVIRONMENT", "DEV").to_uppercase(),
            max_request_size: read_max_request_size(&mut reader),
            tls: read_tls(&mut reader),
        };

        reader.finish()?;
        Ok(config)
    }
}

/// `HEALTH_CHECK_HOST`/`HEALTH_CHECK_PORT` (`spec/05-health-check.md` §2.1).
fn read_health_check(reader: &mut Reader) -> SocketAddr {
    let port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");
    let host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
    socket_addr(&host, port.unwrap_or(ihb_health::DEFAULT_PORT))
}

/// The `generic` pipeline's variables (`spec/02-configuration.md` §12).
fn read_generic(reader: &mut Reader, endpoint_path: String) -> Option<Generic> {
    let adapter_name = reader.required_one_of("CONTENT_ADAPTER", &CONTENT_ADAPTERS);
    let validator_name = reader.required_one_of("VALIDATOR_TYPE", &VALIDATOR_TYPES);
    let output_name = reader.required_one_of("OUTPUT_FORMAT", &OUTPUT_FORMATS);

    let validation_schema = reader.optional_string("VALIDATION_SCHEMA");
    let allowed_structures = reader.list_or("ALLOWED_HL7_STRUCTURES", DEFAULT_STRUCTURES);

    let adapter = match adapter_name.as_deref()? {
        "soap" => ContentAdapter::Soap,
        _ => ContentAdapter::XmlRaw {
            source_identifier_path: read_path(reader, "SOURCE_IDENTIFIER_LOCATOR"),
            message_control_id_path: read_path(reader, "MESSAGE_CONTROL_ID_LOCATOR"),
        },
    };

    let validator = match validator_name.as_deref()? {
        "hl7-xsd" => {
            let Some(group) = validation_schema else {
                reader.conflict("VALIDATION_SCHEMA is required when VALIDATOR_TYPE is 'hl7-xsd'.");
                return None;
            };
            let Ok(flow) = group.parse::<Flow>() else {
                let known: Vec<&str> = Flow::ALL.iter().map(|flow| flow.as_str()).collect();
                reader.invalid(
                    "VALIDATION_SCHEMA",
                    format!("one of {}", known.join(", ")),
                    group,
                );
                return None;
            };
            Validator::Hl7Xsd {
                flow,
                allowed_structures,
            }
        }
        "hl7apy" => Validator::Hl7Standard { allowed_structures },
        "xsd" => {
            let Some(path) = validation_schema else {
                reader.conflict("VALIDATION_SCHEMA is required when VALIDATOR_TYPE is 'xsd'.");
                return None;
            };
            Validator::Xsd {
                path: PathBuf::from(path),
            }
        }
        _ => Validator::None,
    };

    Some(Generic {
        endpoint_path,
        adapter,
        validator,
        allowed_source_identifiers: reader.list("ALLOWED_SOURCE_IDENTIFIERS"),
        allowed_trigger_events: reader.list("ALLOWED_TRIGGER_EVENTS"),
        output_format: if output_name.as_deref()? == "er7" {
            OutputFormat::Er7
        } else {
            OutputFormat::Raw
        },
    })
}

/// A `/`-separated path of element names, or `None` when blank.
fn read_path(reader: &Reader, name: &str) -> Option<Vec<String>> {
    let raw = reader.optional_string(name)?;
    let segments: Vec<String> = raw
        .split('/')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(ToString::to_string)
        .collect();
    (!segments.is_empty()).then_some(segments)
}

/// `MAX_REQUEST_SIZE_BYTES` (`spec/02-configuration.md` §12): zero or unset
/// is the default, `-1` is the Service Bus ceiling — "no explicit cap below
/// it", never unbounded (OWASP A05) — and anything else outside
/// `1..=ceiling` is a startup failure.
fn read_max_request_size(reader: &mut Reader) -> usize {
    let Some(size) = reader.optional_integer::<i64>("MAX_REQUEST_SIZE_BYTES") else {
        return DEFAULT_MAX_REQUEST_SIZE;
    };

    match size {
        0 => DEFAULT_MAX_REQUEST_SIZE,
        -1 => SERVICE_BUS_LIMIT,
        size if size < -1 => {
            reader.invalid(
                "MAX_REQUEST_SIZE_BYTES",
                "a positive value, 0 (default), or -1 (the Service Bus ceiling)",
                size.to_string(),
            );
            DEFAULT_MAX_REQUEST_SIZE
        }
        size => match usize::try_from(size) {
            Ok(size) if size <= SERVICE_BUS_LIMIT => size,
            _ => {
                reader.invalid(
                    "MAX_REQUEST_SIZE_BYTES",
                    format!("at most {SERVICE_BUS_LIMIT}, the Service Bus message limit"),
                    size.to_string(),
                );
                DEFAULT_MAX_REQUEST_SIZE
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use hl7_rest_server::config::{Egress, FlowRule};

    use super::*;

    const BASE: [(&str, &str); 10] = [
        ("EGRESS_QUEUE_NAME", "egress-queue"),
        ("EGRESS_SESSION_ID", "rest-session"),
        ("SERVICE_BUS_CONNECTION_STRING", "Endpoint=sb://localhost"),
        ("MESSAGE_STORE_QUEUE_NAME", "message-store"),
        ("WORKFLOW_ID", "workflow-rest"),
        ("MICROSERVICE_ID", "rest-server"),
        ("HEALTH_BOARD", "test-board"),
        ("PEER_SERVICE", "hl7-sender"),
        ("CONTENT_ADAPTER", "xml-raw"),
        ("VALIDATOR_TYPE", "none"),
    ];

    fn with(extra: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        let mut pairs: Vec<(String, String)> = BASE
            .iter()
            .chain(std::iter::once(&("OUTPUT_FORMAT", "raw")))
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        for (key, value) in extra {
            pairs.retain(|(name, _)| name != key);
            pairs.push(((*key).to_string(), (*value).to_string()));
        }
        Config::from_pairs(pairs)
    }

    fn without(name: &str) -> Result<Config, ConfigErrors> {
        let pairs: Vec<(String, String)> = BASE
            .iter()
            .chain(std::iter::once(&("OUTPUT_FORMAT", "raw")))
            .filter(|(k, _)| *k != name)
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        Config::from_pairs(pairs)
    }

    fn generic(config: &Config) -> &Generic {
        let Pipeline::Generic(generic) = &config.pipeline else {
            panic!("expected the generic pipeline");
        };
        generic
    }

    #[test]
    fn the_defaults() {
        // test_app_config.py::test_read_env_config_defaults
        let config = with(&[]).unwrap();
        assert_eq!(config.listen.to_string(), "127.0.0.1:8080");
        assert_eq!(generic(&config).endpoint_path, "/ingest");
        assert_eq!(generic(&config).allowed_source_identifiers.len(), 0);
        assert_eq!(generic(&config).allowed_trigger_events.len(), 0);
        assert_eq!(config.max_request_size, 1_048_576);
        assert!(matches!(
            generic(&config).adapter,
            ContentAdapter::XmlRaw {
                source_identifier_path: None,
                ..
            }
        ));
    }

    #[test]
    fn the_hl7_structures_default_to_the_lims_pair() {
        let config = with(&[("VALIDATOR_TYPE", "hl7apy")]).unwrap();
        let Validator::Hl7Standard { allowed_structures } = &generic(&config).validator else {
            panic!("expected hl7apy");
        };
        assert_eq!(allowed_structures, &["ADT_A05", "ADT_A39"]);
    }

    #[test]
    fn an_endpoint_path_without_a_slash_is_a_startup_failure() {
        // test_invalid_endpoint_path_raises
        assert!(with(&[("ENDPOINT_PATH", "ingest")]).is_err());
    }

    #[test]
    fn unsupported_adapter_validator_or_format_is_a_startup_failure() {
        // test_invalid_content_adapter_raises, ..._validator_type_..., ..._output_format_...
        assert!(with(&[("CONTENT_ADAPTER", "carrier-pigeon")]).is_err());
        assert!(with(&[("VALIDATOR_TYPE", "vibes")]).is_err());
        assert!(with(&[("OUTPUT_FORMAT", "csv")]).is_err());
    }

    #[test]
    fn the_generic_trio_is_required() {
        // test_missing_content_adapter_raises
        let error = without("CONTENT_ADAPTER").unwrap_err().to_string();
        assert!(error.contains("CONTENT_ADAPTER"), "{error}");
        assert!(without("VALIDATOR_TYPE").is_err());
    }

    #[test]
    fn hl7_xsd_requires_a_schema_group_that_exists() {
        // test_hl7_xsd_validator_requires_validation_schema; spec/02 §12.2.
        let error = with(&[("VALIDATOR_TYPE", "hl7-xsd")])
            .unwrap_err()
            .to_string();
        assert!(error.contains("VALIDATION_SCHEMA is required"), "{error}");
        assert!(with(&[("VALIDATOR_TYPE", "hl7-xsd"), ("VALIDATION_SCHEMA", "nope")]).is_err());

        let config = with(&[("VALIDATOR_TYPE", "hl7-xsd"), ("VALIDATION_SCHEMA", "phw")]).unwrap();
        assert!(matches!(
            generic(&config).validator,
            Validator::Hl7Xsd {
                flow: Flow::Phw,
                ..
            }
        ));
    }

    #[test]
    fn xsd_accepts_a_schema_file_path_and_hl7apy_needs_none() {
        // test_xsd_validator_accepts_schema_file_path
        let config = with(&[
            ("VALIDATOR_TYPE", "xsd"),
            ("VALIDATION_SCHEMA", "/schemas/partner.xsd"),
        ])
        .unwrap();
        assert_eq!(
            generic(&config).validator,
            Validator::Xsd {
                path: PathBuf::from("/schemas/partner.xsd")
            }
        );
        assert!(with(&[("VALIDATOR_TYPE", "xsd")]).is_err());
        assert!(with(&[("VALIDATOR_TYPE", "hl7apy")]).is_ok());
    }

    #[test]
    fn the_locator_is_split_on_slash() {
        // test_source_identifier_locator_is_split_on_slash
        let config = with(&[("SOURCE_IDENTIFIER_LOCATOR", "Header/SourceSystem")]).unwrap();
        let ContentAdapter::XmlRaw {
            source_identifier_path,
            ..
        } = &generic(&config).adapter
        else {
            panic!("expected xml-raw");
        };
        assert_eq!(
            source_identifier_path.as_deref(),
            Some(["Header".to_string(), "SourceSystem".to_string()].as_slice())
        );
    }

    #[test]
    fn the_request_size_rules() {
        // test_request_size_above_service_bus_limit_raises, ..._minus_one_...,
        // ..._below_minus_one_raises
        assert!(with(&[("MAX_REQUEST_SIZE_BYTES", "104857601")]).is_err());
        assert_eq!(
            with(&[("MAX_REQUEST_SIZE_BYTES", "104857600")])
                .unwrap()
                .max_request_size,
            104_857_600
        );
        assert_eq!(
            with(&[("MAX_REQUEST_SIZE_BYTES", "-1")])
                .unwrap()
                .max_request_size,
            104_857_600
        );
        assert!(with(&[("MAX_REQUEST_SIZE_BYTES", "-2")]).is_err());
        assert_eq!(
            with(&[("MAX_REQUEST_SIZE_BYTES", "0")])
                .unwrap()
                .max_request_size,
            1_048_576
        );
        assert_eq!(
            with(&[("MAX_REQUEST_SIZE_BYTES", "2048")])
                .unwrap()
                .max_request_size,
            2048
        );
    }

    #[test]
    fn queue_and_topic_together_are_refused() {
        // test_cannot_configure_both_queue_and_topic; spec/02-configuration.md §3.1.
        let error = with(&[("EGRESS_TOPIC_NAME", "egress-topic")])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Cannot specify both EGRESS_QUEUE_NAME and EGRESS_TOPIC_NAME"),
            "{error}"
        );
    }

    #[test]
    fn neither_queue_nor_topic_is_refused() {
        // spec/02-configuration.md §3.1: exactly one egress is required.
        let error = without("EGRESS_QUEUE_NAME").unwrap_err().to_string();
        assert!(
            error.contains("Either EGRESS_QUEUE_NAME or EGRESS_TOPIC_NAME must be provided"),
            "{error}"
        );
    }

    #[test]
    fn a_topic_alone_is_a_topic_egress() {
        // The LIMS ingress is a topic (spec/01-architecture.md §2.1.3), so
        // `local/rest-server-lims.env` sets `EGRESS_TOPIC_NAME` alone.
        let pairs: Vec<(&str, &str)> = BASE
            .into_iter()
            .filter(|(k, _)| *k != "EGRESS_QUEUE_NAME")
            .chain([
                ("EGRESS_TOPIC_NAME", "egress-topic"),
                ("OUTPUT_FORMAT", "raw"),
            ])
            .collect();
        let config = Config::from_pairs(pairs).unwrap();
        assert_eq!(config.identity.egress, Egress::Topic("egress-topic".into()));
    }

    #[test]
    fn the_message_store_is_optional() {
        // test_message_store_queue_name_optional_when_unset
        assert!(
            without("MESSAGE_STORE_QUEUE_NAME")
                .unwrap()
                .identity
                .message_store_queue
                .is_none()
        );
    }

    #[test]
    fn tls_needs_both_files() {
        assert!(with(&[("TLS_CERT_FILE", "/c")]).is_err());
        assert!(
            with(&[("TLS_CERT_FILE", "/c"), ("TLS_KEY_FILE", "/k")])
                .unwrap()
                .tls
                .is_some()
        );
    }

    // ---- PIPELINE=hl7 ---------------------------------------------------

    fn hl7(extra: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        let mut pairs: Vec<(String, String)> = BASE
            .iter()
            .filter(|(k, _)| !["CONTENT_ADAPTER", "VALIDATOR_TYPE"].contains(k))
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        pairs.push(("PIPELINE".into(), "hl7".into()));
        pairs.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        );
        Config::from_pairs(pairs)
    }

    #[test]
    fn hl7_does_not_need_the_generic_settings() {
        // test_hl7_pipeline_does_not_require_generic_pipeline_settings
        assert!(matches!(hl7(&[]).unwrap().pipeline, Pipeline::Hl7(_)));
    }

    #[test]
    fn hl7_fails_fast_on_the_generic_settings() {
        // test_hl7_pipeline_fails_fast_on_content_adapter / validator_type / output_format
        for (name, value) in [
            ("CONTENT_ADAPTER", "soap"),
            ("VALIDATOR_TYPE", "none"),
            ("OUTPUT_FORMAT", "raw"),
        ] {
            let error = hl7(&[(name, value)]).unwrap_err().to_string();
            assert!(
                error.contains(&format!(
                    "{name} is only valid when PIPELINE=generic (current PIPELINE=hl7)."
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn hl7_risp_requires_wrrs() {
        // test_risp_flow_requires_wrrs_config
        assert!(hl7(&[("HL7_VALIDATION_FLOW", "risp")]).is_err());
    }

    #[test]
    fn hl7_risp_with_wrrs_succeeds() {
        // test_risp_flow_with_wrrs_config_succeeds
        let config = hl7(&[
            ("HL7_VALIDATION_FLOW", "risp"),
            ("WRRS_QUEUE_NAME", "wrrs-queue"),
            ("WRRS_EGRESS_SESSION_ID", "risp-to-wrrs"),
            ("WRRS_WORKFLOW_ID", "risp-to-wrrs"),
        ])
        .unwrap();
        let Pipeline::Hl7(settings) = config.pipeline else {
            panic!("expected hl7");
        };
        assert_eq!(settings.flow, Some(FlowRule::Risp));
        assert_eq!(
            settings.wrrs.unwrap().egress,
            Egress::Queue("wrrs-queue".into())
        );
    }

    #[test]
    fn an_unknown_pipeline_is_a_startup_failure() {
        // test_invalid_pipeline_raises
        let error = hl7(&[("PIPELINE", "carrier-pigeon")])
            .unwrap_err()
            .to_string();
        assert!(error.contains("PIPELINE"), "{error}");
    }
}
