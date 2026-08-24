//! Logging setup, trace context, and the handler span.
//!
//! Implements `spec/04-observability.md` §4, §5 and §6.
//!
//! Three jobs, and they are less alike than they look:
//!
//! 1. **Install a subscriber** so `tracing` output goes somewhere, at the
//!    levels the deployment asked for ([`configure`]).
//! 2. **Carry trace context across a queue**, so one message is one
//!    distributed trace end to end ([`inject`] and [`extract`]).
//! 3. **Wrap a message handler in a span**, so a slow or failing message is
//!    attributable ([`handler_span`]).
//!
//! # What is deliberately not here
//!
//! **Auto-instrumentation.** The Python disables eight instrumentors by
//! name, having found that their per-call metric time series grew without
//! bound and cost measurable CPU over a container's lifetime. Rust has none
//! to disable, and per `spec/04-observability.md` §4.4 none may be added to
//! these services without a specification change.
//!
//! **An `AZURE_MONITOR_OWNER` handshake.** The Python has three libraries
//! racing to initialise the exporter and an environment variable to
//! arbitrate. [`configure`] is the one place, called once, and the variable
//! is accepted and ignored (§6.5).
//!
//! # Examples
//!
//! ```
//! use ihb_telemetry::otel::{Config, configure};
//!
//! // Absent connection string: no export, and the process still starts.
//! let outcome = configure(&Config {
//!     service_name: "phw_hl7_server".into(),
//!     log_level: "info".into(),
//!     azure_log_level: "warn".into(),
//!     rust_log: None,
//!     connection_string: None,
//! });
//!
//! assert!(!outcome.export_enabled);
//! ```

use std::collections::BTreeMap;

use opentelemetry_sdk::trace::TracerProvider;

/// The trace id and span id written on a record when no span is active.
///
/// Emitted rather than omitted: a Log Analytics join behaves differently on
/// a missing column than on a zero value, and the Python emits the zeros
/// (`spec/04-observability.md` §4.5).
pub const NO_TRACE_ID: &str = "00000000000000000000000000000000";

/// The span id counterpart of [`NO_TRACE_ID`].
pub const NO_SPAN_ID: &str = "0000000000000000";

/// The W3C trace-context header carried on a Service Bus message.
pub const TRACEPARENT: &str = "traceparent";

/// The W3C vendor-state header carried alongside [`TRACEPARENT`].
pub const TRACESTATE: &str = "tracestate";

/// What [`configure`] needs, already read from the environment.
///
/// Taking values rather than variable names keeps this testable without
/// mutating the process environment
/// (`spec/02-configuration.md` §10.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `MICROSERVICE_ID`; the OpenTelemetry `service.name`.
    pub service_name: String,
    /// `LOG_LEVEL`.
    pub log_level: String,
    /// `AZURE_LOG_LEVEL`, applied to the Azure SDK targets.
    pub azure_log_level: String,
    /// `RUST_LOG`. When set, it replaces the two levels above.
    pub rust_log: Option<String>,
    /// `APPLICATIONINSIGHTS_CONNECTION_STRING`. Blank or absent disables
    /// export entirely (`spec/04-observability.md` §6.1).
    pub connection_string: Option<String>,
}

/// What [`configure`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Whether telemetry export is on.
    pub export_enabled: bool,
    /// Whether a tracer provider was actually built.
    ///
    /// Distinct from [`Outcome::export_enabled`], which records what the
    /// configuration **asked for**. §6.2 says a failure to initialise the
    /// exporter is logged and the process continues, so these two differ
    /// exactly when export was wanted and could not be set up — which is
    /// the case an operator needs to see.
    pub tracing_installed: bool,
    /// The `EnvFilter` directive that was installed, for the log line that
    /// says what the levels ended up as.
    pub filter: String,
    /// True when this call installed the subscriber, false when one was
    /// already in place. `configure` is idempotent
    /// (`spec/04-observability.md` §4.1).
    pub installed: bool,
}

/// Build the `EnvFilter` directive for a configuration.
///
/// `RUST_LOG` wins outright when set, because someone debugging a container
/// has reached for it deliberately and should not have it merged with
/// anything. Otherwise the directive is the root level plus an override for
/// the Azure SDK targets, which are noisy at anything below `warn`.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::otel::{Config, filter_directive};
///
/// let config = Config {
///     service_name: "phw_hl7_server".into(),
///     log_level: "info".into(),
///     azure_log_level: "warn".into(),
///     rust_log: None,
///     connection_string: None,
/// };
/// assert_eq!(filter_directive(&config), "info,azservicebus=warn,azure_core=warn");
///
/// // RUST_LOG replaces the lot.
/// let overridden = Config { rust_log: Some("debug".into()), ..config };
/// assert_eq!(filter_directive(&overridden), "debug");
/// ```
#[must_use]
pub fn filter_directive(config: &Config) -> String {
    if let Some(rust_log) = config
        .rust_log
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return rust_log.to_string();
    }

    let root = normalise_level(&config.log_level, "info");
    let azure = normalise_level(&config.azure_log_level, "warn");
    format!("{root},azservicebus={azure},azure_core={azure}")
}

/// Map a configured level onto what `EnvFilter` understands.
///
/// `WARNING` is accepted because Python's `logging` spells it that way and
/// a deployment may have copied the value across
/// (`spec/02-configuration.md` §2).
fn normalise_level(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "warning" => "warn".to_string(),
        "critical" | "fatal" => "error".to_string(),
        "notset" => fallback.to_string(),
        other => other.to_string(),
    }
}

/// Install the logging subscriber for this process.
///
/// Safe to call more than once: a global subscriber can only be installed
/// once, so a second call leaves the first in place and reports
/// `installed: false` (`spec/04-observability.md` §4.1).
///
/// With no connection string, nothing is exported and the process starts
/// cleanly — the normal case for a local run and for every test (§6.1).
///
/// # Export
///
/// There is no Azure Monitor exporter for Rust, so this exports over OTLP
/// to whatever `OTEL_EXPORTER_OTLP_ENDPOINT` names — a collector, not
/// Application Insights directly, which does not ingest raw OTLP. The
/// exporter sits behind [`TraceExporter`] so an Azure-native one can
/// replace it without touching a call site (§6.3).
///
/// Two things worth knowing before changing this function:
///
/// - Building the OTLP exporter needs a **Tokio runtime**. Call this from
///   inside one, as every service's `main` does (§6.3.3).
/// - The provider must be built **before** the subscriber, because the
///   subscriber needs the tracer to build its layer — and without that
///   layer the provider is inert (§6.3.2).
///
/// # Examples
///
/// ```
/// use ihb_telemetry::otel::{Config, configure};
///
/// let config = Config {
///     service_name: "phw_hl7_server".into(),
///     log_level: "info".into(),
///     azure_log_level: "warn".into(),
///     rust_log: None,
///     connection_string: Some("   ".into()),
/// };
///
/// // A blank connection string is an absent one.
/// assert!(!configure(&config).export_enabled);
/// ```
#[must_use]
pub fn configure(config: &Config) -> Outcome {
    configure_with(config, &Otlp)
}

/// [`configure`], with the exporter named explicitly.
///
/// §6.3's seam made usable: a test passes its own [`TraceExporter`] and a
/// future Azure-native exporter is a different argument here rather than an
/// edit to every `main`.
///
/// # Examples
///
/// An exporter that refuses to build must not stop the process (§6.2):
///
/// ```
/// use ihb_telemetry::otel::{Config, TraceExporter, configure_with};
/// use opentelemetry_sdk::trace::TracerProvider;
///
/// struct Broken;
/// impl TraceExporter for Broken {
///     fn build(&self, _: &str) -> Result<TracerProvider, String> {
///         Err("the collector is not reachable".into())
///     }
///     fn describe(&self) -> &'static str { "broken" }
/// }
///
/// let outcome = configure_with(
///     &Config {
///         service_name: "s".into(),
///         log_level: "info".into(),
///         azure_log_level: "warn".into(),
///         rust_log: None,
///         connection_string: Some("InstrumentationKey=x".into()),
///     },
///     &Broken,
/// );
///
/// // Export was asked for and could not be set up. The process continues.
/// assert!(outcome.export_enabled);
/// assert!(!outcome.tracing_installed);
/// ```
pub fn configure_with(config: &Config, exporter: &dyn TraceExporter) -> Outcome {
    let filter = filter_directive(config);
    let export_enabled = config
        .connection_string
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());

    // §6.3 — build the provider **before** the subscriber, because the
    // subscriber needs the tracer to build its layer. §6.2 — a failure
    // here is not fatal; the error is held and logged once the subscriber
    // that can carry it exists.
    let (provider, export_error) = if export_enabled {
        match exporter.build(&config.service_name) {
            Ok(provider) => (Some(provider), None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (None, None)
    };
    let tracing_installed = provider.is_some();

    // Installing a global subscriber can only happen once per process.
    // `try_init` failing means another one is already there, which is the
    // idempotence §4.1 asks for rather than an error.
    let installed = install_subscriber(&filter, provider);

    if let Some(error) = export_error {
        tracing::error!(
            %error,
            exporter = exporter.describe(),
            "could not initialise trace export; continuing without it \
             (spec/04-observability.md §6.2)"
        );
    }

    if export_enabled {
        tracing::info!(
            service_name = %config.service_name,
            exporter = exporter.describe(),
            tracing_installed,
            "telemetry export is configured"
        );
    } else {
        tracing::info!(
            service_name = %config.service_name,
            "APPLICATIONINSIGHTS_CONNECTION_STRING not set — telemetry export disabled"
        );
    }

    Outcome {
        export_enabled,
        tracing_installed,
        filter,
        installed,
    }
}

/// Where traces are sent.
///
/// `spec/04-observability.md` §6.3's seam. There is no Azure Monitor
/// exporter for Rust, so this ships an OTLP one — but the call sites know
/// only this trait, so an Azure-native exporter can replace it later
/// without touching them.
///
/// The trait is deliberately tiny: building a provider is the only thing
/// that differs between exporters, and everything downstream of it is
/// `opentelemetry`'s own API.
pub trait TraceExporter {
    /// Build a tracer provider, or explain why not.
    ///
    /// # Errors
    ///
    /// A string describing the failure. §6.2 makes this non-fatal — the
    /// caller logs it and continues with export disabled — so there is no
    /// typed error to match on.
    fn build(&self, service_name: &str) -> Result<TracerProvider, String>;

    /// What to call this exporter in a log line.
    fn describe(&self) -> &'static str;
}

/// The OTLP exporter (§6.3).
///
/// Sends over gRPC to whatever `OTEL_EXPORTER_OTLP_ENDPOINT` names, which
/// in this estate is a collector sidecar rather than Application Insights
/// directly: App Insights does not ingest raw OTLP, which is the whole
/// reason [`plan.md` §8] lists this as a gap.
#[derive(Debug, Clone, Copy, Default)]
pub struct Otlp;

impl TraceExporter for Otlp {
    fn build(&self, service_name: &str) -> Result<TracerProvider, String> {
        use opentelemetry_otlp::WithExportConfig as _;

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_timeout(EXPORT_TIMEOUT)
            .build()
            .map_err(|error| format!("building the OTLP span exporter: {error}"))?;

        Ok(TracerProvider::builder()
            .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
            .with_resource(opentelemetry_sdk::Resource::new([
                opentelemetry::KeyValue::new("service.name", service_name.to_owned()),
            ]))
            .build())
    }

    fn describe(&self) -> &'static str {
        "OTLP/gRPC"
    }
}

/// How long an export attempt may take.
///
/// Short on purpose: an exporter that blocks is an exporter that turns a
/// telemetry problem into a service problem, which §6.2 forbids.
const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Install a subscriber, reporting whether this call was the one that did.
///
/// Kept separate so [`configure`] reads as policy and this reads as
/// mechanism.
///
/// A test that wants to capture output uses
/// `tracing::subscriber::with_default`, which sets a thread-local dispatcher
/// that takes precedence over whatever is installed globally — so this does
/// not fight the test suite.
fn install_subscriber(filter: &str, provider: Option<TracerProvider>) -> bool {
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let Ok(env_filter) = EnvFilter::try_new(filter) else {
        // An unparseable directive is the operator's typo, not a reason to
        // run with no logging at all. Fall back to the default and say so,
        // once the subscriber that can say it is in place.
        let installed = fmt::Subscriber::builder()
            .with_env_filter(EnvFilter::new("info"))
            .try_init()
            .is_ok();
        tracing::warn!(
            filter,
            "the configured log filter could not be parsed; using `info`"
        );
        return installed;
    };

    // Without this layer the tracer provider is inert: setting it globally
    // does **not** make `tracing` spans become OTel spans, so nothing would
    // ever be exported and `tracing_installed` would be a lie. The layer is
    // what connects the two.
    let Some(provider) = provider else {
        return fmt::Subscriber::builder()
            .with_env_filter(env_filter)
            .try_init()
            .is_ok();
    };

    let tracer = provider.tracer("ihb");
    opentelemetry::global::set_tracer_provider(provider);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .try_init()
        .is_ok()
}

/// Inject the current trace context into a property map.
///
/// The map is **copied**, never mutated in place: the caller's tracking
/// metadata is used for other things and must not gain fields as a side
/// effect (`spec/04-observability.md` §4.6.1).
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use ihb_telemetry::otel::inject;
///
/// let mut properties = BTreeMap::new();
/// properties.insert("CorrelationId".to_string(), "3f6d".to_string());
///
/// let carried = inject(&properties);
///
/// // The original is untouched.
/// assert_eq!(properties.len(), 1);
/// assert!(carried.contains_key("CorrelationId"));
/// ```
#[must_use]
pub fn inject(properties: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut carrier = properties.clone();
    if let Some(traceparent) = current_traceparent() {
        carrier.insert(TRACEPARENT.to_string(), traceparent);
    }
    carrier
}

/// The `traceparent` for the current span, if there is one.
///
/// Returns `None` until an OpenTelemetry tracer provider is installed, which is the
/// honest answer: fabricating a trace id here would produce traces that
/// join nothing.
#[allow(
    clippy::missing_const_for_fn,
    reason = "this reads a thread-local tracer provider once P1.6a lands"
)]
fn current_traceparent() -> Option<String> {
    let (trace_id, span_id) = current_ids();
    if trace_id == NO_TRACE_ID {
        return None;
    }
    // W3C `traceparent`: version 00, sampled. The sampled flag is `01`
    // because a span that reached here was recorded; an unsampled span
    // carries no ids worth propagating.
    Some(format!("00-{trace_id}-{span_id}-01"))
}

/// The current trace and span ids, or the all-zero placeholders.
///
/// `spec/04-observability.md` §4.5. Both are lower-case hex, 32 and 16
/// characters — the widths [`NO_TRACE_ID`] and [`NO_SPAN_ID`] declare, so a
/// consumer can parse one column without knowing which it got.
///
/// Zeros mean there is no recording span: either export is off, or this
/// code is not inside a handler span. That is information, not a failure —
/// it says "this record cannot be joined to a trace" rather than pretending
/// it can.
///
/// # Examples
///
/// Outside any span, with no exporter configured:
///
/// ```
/// use ihb_telemetry::otel::{current_ids, NO_SPAN_ID, NO_TRACE_ID};
///
/// let (trace_id, span_id) = current_ids();
/// assert_eq!(trace_id, NO_TRACE_ID);
/// assert_eq!(span_id, NO_SPAN_ID);
/// ```
#[must_use]
pub fn current_ids() -> (String, String) {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = tracing::Span::current().context();
    let span = context.span();
    let sc = span.span_context();

    if sc.is_valid() {
        (sc.trace_id().to_string(), sc.span_id().to_string())
    } else {
        (NO_TRACE_ID.to_owned(), NO_SPAN_ID.to_owned())
    }
}

/// Extract the propagation headers a received message carries.
///
/// Values that are not strings are **dropped**. Service Bus permits an
/// integer or a boolean application property, and the W3C propagator
/// expects header-shaped strings; handing it a number is how the Python got
/// a `TypeError` out of `re.search` deep inside the propagator
/// (`spec/03-message-bus.md` §4.2).
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use ihb_telemetry::otel::{extract, TRACEPARENT};
///
/// let mut properties = BTreeMap::new();
/// properties.insert(TRACEPARENT.to_string(), "00-abc-def-01".to_string());
/// properties.insert("DeliveryCount".to_string(), "3".to_string());
///
/// let carrier = extract(&properties);
/// assert_eq!(carrier.get(TRACEPARENT).map(String::as_str), Some("00-abc-def-01"));
/// // Only propagation headers are taken; the rest is not trace context.
/// assert!(!carrier.contains_key("DeliveryCount"));
/// ```
#[must_use]
pub fn extract(properties: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut carrier = BTreeMap::new();
    for header in [TRACEPARENT, TRACESTATE] {
        if let Some(value) = properties.get(header) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                carrier.insert(header.to_string(), trimmed.to_string());
            }
        }
    }
    carrier
}

/// Whether a received message carried trace context.
///
/// A message without it starts a new trace, which is worth a warning: the
/// attempt will appear under a new operation id in Application Insights and
/// an operator following a retry needs to be told to use `CorrelationId`
/// instead (`spec/04-observability.md` §4.6.3).
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use ihb_telemetry::otel::{TRACEPARENT, has_trace_context};
///
/// let mut with = BTreeMap::new();
/// with.insert(TRACEPARENT.to_string(), "00-abc-def-01".to_string());
/// assert!(has_trace_context(&with));
///
/// assert!(!has_trace_context(&BTreeMap::new()));
/// ```
#[must_use]
pub fn has_trace_context(properties: &BTreeMap<String, String>) -> bool {
    properties
        .get(TRACEPARENT)
        .map(|value| value.trim())
        .is_some_and(|value| !value.is_empty())
}

/// Warn that a message arrived without trace context.
///
/// Separate from [`has_trace_context`] so the check and the warning can be
/// used independently, and so the wording lives in one place.
pub fn warn_missing_trace_context(message_id: Option<&str>) {
    tracing::warn!(
        message_id = message_id.unwrap_or("unknown"),
        "no W3C traceparent in the message properties — this attempt will appear \
         under a new operation id; use CorrelationId to follow it across retries"
    );
}

/// The span a message handler runs inside.
///
/// Named `{service}.process_message` with the attributes of
/// `spec/04-observability.md` §4.7.1. Entering it is the caller's job, so
/// the span can be held across an await without this function caring.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::otel::handler_span;
///
/// let span = handler_span("phw-hl7-transformer", "pre-phw-transform", Some("MSG1"));
/// let _entered = span.enter();
/// ```
#[must_use]
pub fn handler_span(
    service_name: &str,
    destination: &str,
    message_id: Option<&str>,
) -> tracing::Span {
    tracing::info_span!(
        "process_message",
        otel.name = %format!("{service_name}.process_message"),
        messaging.system = "azure_service_bus",
        messaging.destination = %destination,
        messaging.message_id = message_id.unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration with nothing unusual in it.
    fn config() -> Config {
        Config {
            service_name: "phw_hl7_server".into(),
            log_level: "info".into(),
            azure_log_level: "warn".into(),
            rust_log: None,
            connection_string: None,
        }
    }

    #[test]
    fn the_zero_trace_ids_are_the_right_widths() {
        // spec/04-observability.md §4.5 — 32 and 16 hex characters.
        assert_eq!(NO_TRACE_ID.len(), 32);
        assert_eq!(NO_SPAN_ID.len(), 16);
        assert!(NO_TRACE_ID.chars().all(|c| c == '0'));
        assert!(NO_SPAN_ID.chars().all(|c| c == '0'));
    }

    #[test]
    fn the_azure_targets_get_their_own_level() {
        // spec/02-configuration.md §2
        assert_eq!(
            filter_directive(&config()),
            "info,azservicebus=warn,azure_core=warn"
        );
    }

    #[test]
    fn rust_log_replaces_both_levels() {
        // spec/02-configuration.md §2 — a deliberate override is not merged.
        let overridden = Config {
            rust_log: Some("ihb_bus=trace,warn".into()),
            ..config()
        };
        assert_eq!(filter_directive(&overridden), "ihb_bus=trace,warn");
    }

    #[test]
    fn a_blank_rust_log_is_absent() {
        // spec/02-configuration.md §1.1
        let blank = Config {
            rust_log: Some("   ".into()),
            ..config()
        };
        assert_eq!(
            filter_directive(&blank),
            "info,azservicebus=warn,azure_core=warn"
        );
    }

    #[test]
    fn pythons_level_spellings_are_understood() {
        // spec/02-configuration.md §2 — a copied-across Python value.
        assert_eq!(normalise_level("WARNING", "info"), "warn");
        assert_eq!(normalise_level("CRITICAL", "info"), "error");
        assert_eq!(normalise_level("ERROR", "info"), "error");
        assert_eq!(normalise_level("", "info"), "info");
        assert_eq!(normalise_level("NOTSET", "warn"), "warn");
    }

    #[test]
    fn a_blank_connection_string_disables_export() {
        // spec/04-observability.md §6.1 — blank is absent.
        for value in [None, Some(String::new()), Some("   ".to_string())] {
            let config = Config {
                connection_string: value.clone(),
                ..config()
            };
            assert!(!configure(&config).export_enabled, "{value:?}");
        }
    }

    /// An exporter that builds nothing, so the decision can be tested
    /// without a collector or a runtime.
    struct Never;

    impl TraceExporter for Never {
        fn build(&self, _: &str) -> Result<TracerProvider, String> {
            Err("no exporter in tests".into())
        }
        fn describe(&self) -> &'static str {
            "never"
        }
    }

    #[test]
    fn a_present_connection_string_enables_export() {
        let config = Config {
            connection_string: Some("InstrumentationKey=abc".into()),
            ..config()
        };
        // `configure_with` rather than `configure`: the real OTLP exporter
        // builds a batch pipeline that needs a Tokio runtime, and this test
        // is about the *decision*, not the transport. See §6.3.1.
        assert!(configure_with(&config, &Never).export_enabled);
    }

    #[test]
    fn ids_inside_a_recording_span_are_real_and_the_right_width() {
        // §4.5. The point of P1.6a: before a tracer provider existed these
        // were always zeros, and a column of zeros is not a partial
        // implementation — it is no information at all.
        use opentelemetry::trace::TracerProvider as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        // An in-memory provider: no collector, no runtime, real ids.
        let provider = TracerProvider::builder().build();
        let tracer = provider.tracer("test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

        let (trace_id, span_id) = tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("handler");
            let _entered = span.enter();
            current_ids()
        });

        assert_ne!(
            trace_id, NO_TRACE_ID,
            "a recording span has a real trace id"
        );
        assert_ne!(span_id, NO_SPAN_ID, "and a real span id");
        assert_eq!(trace_id.len(), NO_TRACE_ID.len(), "32 hex characters");
        assert_eq!(span_id.len(), NO_SPAN_ID.len(), "16 hex characters");
        assert!(
            trace_id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lower-case hex, so one column parses the same way everywhere: {trace_id}"
        );
    }

    #[test]
    fn a_traceparent_is_produced_only_when_there_is_a_trace_to_name() {
        // §4.6.3 — outside a recording span there is nothing to propagate,
        // and inventing a `traceparent` of zeros would make every unrelated
        // message look like one trace.
        assert!(current_traceparent().is_none());
    }

    #[test]
    fn an_exporter_that_cannot_be_built_does_not_stop_the_process() {
        // §6.2. The two flags differ exactly here: export was asked for
        // and could not be set up.
        let config = Config {
            connection_string: Some("InstrumentationKey=abc".into()),
            ..config()
        };
        let outcome = configure_with(&config, &Never);

        assert!(outcome.export_enabled, "the configuration asked for export");
        assert!(
            !outcome.tracing_installed,
            "and it could not be provided, which must be visible rather than \
             indistinguishable from success"
        );
    }

    #[test]
    fn configure_is_idempotent() {
        // spec/04-observability.md §4.1 — calling it again is safe, and at
        // most one call reports having installed the subscriber.
        let first = configure(&config());
        let second = configure(&config());
        assert_eq!(first.filter, second.filter);
        assert_eq!(first.export_enabled, second.export_enabled);
        assert!(
            !second.installed,
            "the second call must not claim to have installed a subscriber"
        );
    }

    #[test]
    fn an_unparseable_filter_does_not_leave_the_process_silent() {
        // An operator typo in LOG_LEVEL must not mean no logs at all.
        let broken = Config {
            rust_log: Some("=====".into()),
            ..config()
        };
        // Reaching this without panicking is the assertion; the fallback
        // path is exercised whether or not this call wins the install race.
        let outcome = configure(&broken);
        assert_eq!(outcome.filter, "=====");
    }

    #[test]
    fn injection_does_not_mutate_the_callers_map() {
        // spec/04-observability.md §4.6.1
        let mut properties = BTreeMap::new();
        properties.insert("CorrelationId".to_string(), "3f6d".to_string());

        let carried = inject(&properties);

        assert_eq!(properties.len(), 1, "the caller's map gained a field");
        assert_eq!(
            carried.get("CorrelationId").map(String::as_str),
            Some("3f6d")
        );
    }

    #[test]
    fn extraction_takes_only_the_propagation_headers() {
        // spec/03-message-bus.md §4.2 — everything else is not trace context,
        // and handing a non-header to the propagator is what broke the Python.
        let mut properties = BTreeMap::new();
        properties.insert(TRACEPARENT.to_string(), "00-abc-def-01".to_string());
        properties.insert(TRACESTATE.to_string(), "vendor=1".to_string());
        properties.insert("CorrelationId".to_string(), "3f6d".to_string());
        properties.insert("DeliveryCount".to_string(), "3".to_string());

        let carrier = extract(&properties);

        assert_eq!(carrier.len(), 2);
        assert!(carrier.contains_key(TRACEPARENT));
        assert!(carrier.contains_key(TRACESTATE));
    }

    #[test]
    fn a_blank_traceparent_counts_as_absent() {
        let mut properties = BTreeMap::new();
        properties.insert(TRACEPARENT.to_string(), "   ".to_string());
        assert!(!has_trace_context(&properties));
        assert!(extract(&properties).is_empty());
    }

    #[test]
    fn trace_context_is_detected_when_present() {
        // spec/04-observability.md §4.6.3
        let mut properties = BTreeMap::new();
        properties.insert(TRACEPARENT.to_string(), "00-abc-def-01".to_string());
        assert!(has_trace_context(&properties));
    }

    #[test]
    fn a_handler_span_carries_the_messaging_attributes() {
        // spec/04-observability.md §4.7.1.
        //
        // A span is only real when a subscriber is interested in it: with
        // none installed, `info_span!` returns `Span::none()` and has no
        // metadata at all. That is the zero-overhead property §4.7.3 asks
        // for, and it means this assertion needs a subscriber.
        let subscriber = tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(tracing::Level::TRACE)
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let span = handler_span("phw-hl7-transformer", "pre-phw-transform", Some("MSG1"));
            let metadata = span
                .metadata()
                .expect("a span with a subscriber has metadata");
            assert_eq!(metadata.name(), "process_message");

            let fields: Vec<&str> = metadata.fields().iter().map(|f| f.name()).collect();
            assert!(fields.contains(&"messaging.system"), "{fields:?}");
            assert!(fields.contains(&"messaging.destination"), "{fields:?}");
            assert!(fields.contains(&"messaging.message_id"), "{fields:?}");
            assert!(fields.contains(&"otel.name"), "{fields:?}");
        });
    }

    #[test]
    fn a_handler_span_is_free_when_nothing_is_listening() {
        // spec/04-observability.md §4.7.3 — zero overhead when tracing is
        // not configured.
        //
        // "Nothing is listening" is established here rather than assumed
        // from the process. `configure` installs a **global** subscriber,
        // permanently and for every test that runs after it, so a bare
        // `assert!(span.is_disabled())` passes or fails according to the
        // order `cargo test` happens to pick — which is the flake this
        // scoping removes. See spec/17-testing-and-quality.md §9.2.1.
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            let span = handler_span("hl7-sender", "topic/subscription", None);
            assert!(span.is_disabled());
        });
    }
}
