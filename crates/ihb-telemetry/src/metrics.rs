//! The metrics every Integration Hub component emits.
//!
//! Implements `spec/04-observability.md` §3.
//!
//! Three instruments, four dimensions, and a hard rule about what may not
//! become a dimension. Like the events in [`crate::events`], the names are a
//! contract with Log Analytics and the alert rules rather than an
//! implementation detail.
//!
//! # The rule that matters
//!
//! **No dimension may be derived from message content.** Two reasons, and
//! either alone would be sufficient: a patient identifier in a dimension is
//! a PII leak into a telemetry store that is queried far more widely than
//! the message store, and an unbounded dimension multiplies the stored time
//! series until the metric is unusable and the bill is not. The four
//! standard dimensions are all configuration, and [`Metrics::count`] and [`Metrics::record`] take
//! extra dimensions as a fixed list of names a caller has thought about.
//!
//! # Examples
//!
//! ```
//! use ihb_telemetry::metrics::Metrics;
//!
//! let metrics = Metrics::new("phw-to-mpi", "phw_hl7_server", "PHW", "MPI");
//!
//! metrics.message_received();
//! metrics.message_sent();
//! metrics.retry_delay_seconds(5.0, "local-inthub-phw-transformer-ingress", 1);
//! ```

use std::sync::Arc;

/// The counter incremented when a component takes in a message.
pub const MESSAGES_RECEIVED: &str = "messages_received";

/// The counter incremented when a sender gets a positive acknowledgement.
pub const MESSAGES_SENT: &str = "messages_sent";

/// The histogram recording how long a failing receive will wait before its
/// next attempt. Unit: seconds.
pub const RETRY_DELAY_SECONDS: &str = "retry_delay_seconds";

/// The dimension values every metric carries, from configuration.
///
/// The defaults are the Python's, from `message_receiver_client.py`, and
/// apply when a component does not configure one
/// (`spec/04-observability.md` §3.2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dimensions {
    /// `WORKFLOW_ID`, or `unknown-workflow`.
    pub workflow_id: String,
    /// `MICROSERVICE_ID`, or `unknown-microservice`.
    pub microservice_id: String,
    /// `HEALTH_BOARD`, or `UNKNOWN`.
    pub health_board: String,
    /// `PEER_SERVICE`, or `service_bus`.
    pub peer_service: String,
}

impl Dimensions {
    /// The dimensions for a component, substituting a default for anything
    /// blank or absent.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_telemetry::metrics::Dimensions;
    ///
    /// let d = Dimensions::new("phw-to-mpi", "", "PHW", "  ");
    /// assert_eq!(d.workflow_id, "phw-to-mpi");
    /// assert_eq!(d.microservice_id, "unknown-microservice");
    /// assert_eq!(d.peer_service, "service_bus");
    /// ```
    #[must_use]
    pub fn new(
        workflow_id: &str,
        microservice_id: &str,
        health_board: &str,
        peer_service: &str,
    ) -> Self {
        Self {
            workflow_id: or_default(workflow_id, "unknown-workflow"),
            microservice_id: or_default(microservice_id, "unknown-microservice"),
            health_board: or_default(health_board, "UNKNOWN"),
            peer_service: or_default(peer_service, "service_bus"),
        }
    }
}

/// A configured value, or a default when it is blank.
fn or_default(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// One extra dimension on a single measurement.
///
/// A pair rather than a map, so a call site names both halves and a reader
/// can see at a glance that neither came from a message.
pub type Extra<'a> = (&'a str, &'a str);

/// Emits the metrics of `spec/04-observability.md` §3 for one component.
///
/// Cheap to clone; a clone shares the dimensions.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::metrics::Metrics;
///
/// let metrics = Metrics::new("chemocare-to-mpi", "chemo_hl7_server", "CHEMO", "MPI");
/// assert_eq!(metrics.dimensions().health_board, "CHEMO");
/// ```
#[derive(Debug, Clone)]
pub struct Metrics {
    /// The four standard dimensions, shared by every measurement.
    dimensions: Arc<Dimensions>,
}

impl Metrics {
    /// Metrics for one component.
    #[must_use]
    pub fn new(
        workflow_id: &str,
        microservice_id: &str,
        health_board: &str,
        peer_service: &str,
    ) -> Self {
        Self::with_dimensions(Dimensions::new(
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
        ))
    }

    /// Metrics with dimensions already resolved.
    #[must_use]
    pub fn with_dimensions(dimensions: Dimensions) -> Self {
        Self {
            dimensions: Arc::new(dimensions),
        }
    }

    /// The dimensions every measurement from this instance carries.
    #[must_use]
    pub fn dimensions(&self) -> &Dimensions {
        &self.dimensions
    }

    /// A message arrived. Increments [`MESSAGES_RECEIVED`] by one.
    pub fn message_received(&self) {
        self.count(MESSAGES_RECEIVED, 1, &[]);
    }

    /// A message was delivered and acknowledged. Increments
    /// [`MESSAGES_SENT`] by one.
    pub fn message_sent(&self) {
        self.count(MESSAGES_SENT, 1, &[]);
    }

    /// A failing receive has been rescheduled.
    ///
    /// Records [`RETRY_DELAY_SECONDS`] with the `queue` and `attempt`
    /// dimensions of `spec/03-message-bus.md` §5.4.4. `attempt` is bounded
    /// by the backoff ceiling, and `queue` by the number of queues, so
    /// neither is an unbounded dimension.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_telemetry::metrics::Metrics;
    ///
    /// let metrics = Metrics::new("phw-to-mpi", "phw_hl7_sender", "PHW", "MPI");
    /// metrics.retry_delay_seconds(10.0, "local-inthub-mpi-sender-ingress", 2);
    /// ```
    pub fn retry_delay_seconds(&self, seconds: f64, queue: &str, attempt: u32) {
        let attempt = attempt.to_string();
        self.record(
            RETRY_DELAY_SECONDS,
            seconds,
            &[("queue", queue), ("attempt", attempt.as_str())],
        );
    }

    /// Increment a counter.
    ///
    /// Public so a component can add an instrument without a change here;
    /// `name` MUST be a constant, and `extra` MUST NOT be derived from
    /// message content (`spec/04-observability.md` §3.2.2).
    pub fn count(&self, name: &str, value: u64, extra: &[Extra<'_>]) {
        // Until an exporter is wired up (spec/04 §6.3, task P1.6), a metric
        // is a log line in the shape the Python used when export was
        // disabled. That is not a placeholder: it is what §3.3.1 requires
        // for a local run, and it is what the tests assert on.
        tracing::info!(
            metric = name,
            value,
            workflow_id = %self.dimensions.workflow_id,
            microservice_id = %self.dimensions.microservice_id,
            health_board = %self.dimensions.health_board,
            peer_service = %self.dimensions.peer_service,
            attributes = %render_extra(extra),
            "Integration Hub Metric (local log): {name}={value}"
        );
    }

    /// Record a histogram value.
    ///
    /// As [`Metrics::count`], including the rule about `extra`.
    pub fn record(&self, name: &str, value: f64, extra: &[Extra<'_>]) {
        tracing::info!(
            metric = name,
            value,
            workflow_id = %self.dimensions.workflow_id,
            microservice_id = %self.dimensions.microservice_id,
            health_board = %self.dimensions.health_board,
            peer_service = %self.dimensions.peer_service,
            attributes = %render_extra(extra),
            "Integration Hub Metric (local log): {name}={value}"
        );
    }
}

/// Render the per-call dimensions for the log line of §3.3.1.
fn render_extra(extra: &[Extra<'_>]) -> String {
    if extra.is_empty() {
        return String::new();
    }
    extra
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_instrument_names_are_frozen() {
        // spec/04-observability.md §3.1 — alert rules match on these.
        assert_eq!(MESSAGES_RECEIVED, "messages_received");
        assert_eq!(MESSAGES_SENT, "messages_sent");
        assert_eq!(RETRY_DELAY_SECONDS, "retry_delay_seconds");
    }

    #[test]
    fn an_unconfigured_dimension_takes_the_pythons_default() {
        // spec/04-observability.md §3.2.3
        let d = Dimensions::new("", "", "", "");
        assert_eq!(d.workflow_id, "unknown-workflow");
        assert_eq!(d.microservice_id, "unknown-microservice");
        assert_eq!(d.health_board, "UNKNOWN");
        assert_eq!(d.peer_service, "service_bus");
    }

    #[test]
    fn a_blank_dimension_is_the_same_as_an_absent_one() {
        // spec/02-configuration.md §1.1 applied to dimensions.
        let d = Dimensions::new("  ", "\t", " \n", "   ");
        assert_eq!(d.workflow_id, "unknown-workflow");
        assert_eq!(d.peer_service, "service_bus");
    }

    #[test]
    fn a_configured_dimension_is_trimmed_and_kept() {
        let d = Dimensions::new(" phw-to-mpi ", "phw_hl7_server", "PHW", "MPI");
        assert_eq!(d.workflow_id, "phw-to-mpi");
        assert_eq!(d.microservice_id, "phw_hl7_server");
        assert_eq!(d.health_board, "PHW");
        assert_eq!(d.peer_service, "MPI");
    }

    #[test]
    fn extra_dimensions_render_in_order() {
        // spec/03-message-bus.md §5.4.4 — `queue` then `attempt`.
        assert_eq!(
            render_extra(&[("queue", "q"), ("attempt", "3")]),
            "queue=q, attempt=3"
        );
        assert_eq!(render_extra(&[]), "");
    }

    #[test]
    fn every_emit_path_runs() {
        let metrics = Metrics::new("phw-to-mpi", "phw_hl7_server", "PHW", "MPI");
        metrics.message_received();
        metrics.message_sent();
        metrics.retry_delay_seconds(5.0, "a-queue", 1);
        metrics.count("custom_counter", 2, &[("reason", "probe")]);
        metrics.record("custom_histogram", 1.5, &[]);
    }

    #[test]
    fn metrics_are_cheap_to_clone_and_keep_their_dimensions() {
        let metrics = Metrics::new("w", "m", "h", "p");
        let clone = metrics.clone();
        assert_eq!(clone.dimensions(), metrics.dimensions());
    }
}
