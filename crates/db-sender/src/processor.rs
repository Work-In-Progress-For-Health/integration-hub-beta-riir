//! One message, from receipt to disposition.
//!
//! Implements `spec/10-senders.md` §7.2 and §7.3. Upstream:
//! `db_sender/application.py :: process_message`.

use ihb_bus::message::Incoming;
use ihb_bus::metadata::{LogValues, correlation_id_for_event};
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::config::{AppConfig, ProcedureType};
use crate::db::{DatabaseError, FailureKind, ProcedureRunner};
use crate::idupdate;
use crate::msh::{MshInfo, extract_msh_for_logging};
use crate::trrr;

/// Dead-letter reason for a procedure the server refused for good (`§7.3`).
pub const FATAL_REASON: &str = "StoredProcedureExecutionFailed";

/// Dead-letter reason for a body that is not UTF-8 (`§7.2` step 1).
pub const ENCODING_REASON: &str = "InvalidMessageEncoding";

/// What to do with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// The procedure committed. Complete it.
    Complete,
    /// Not written. Abandon it, and everything behind it in the batch
    /// (`spec/03-message-bus.md` §5.2.2).
    Abandon,
    /// It can never succeed. Dead-letter it and carry on.
    DeadLetter {
        /// The reason.
        reason: String,
        /// The description: a sanitised server message, never payload.
        description: String,
    },
}

/// Log a refusal and turn it into the dead letter (`spec/10-senders.md` §7.8).
fn reject(
    rejected: &idupdate::Rejected,
    procedure: &str,
    msh: &MshInfo,
    events: &EventLogger,
    correlation: Option<&str>,
) -> Disposition {
    events.log_message_failed(
        &msh.msh_text,
        &format!(
            "Message rejected for {procedure}: {} - {}",
            rejected.reason, rejected.description
        ),
        None,
        correlation,
    );
    tracing::error!(
        reason = rejected.reason,
        control_id = %msh.control_id,
        "dead-lettering a message for {procedure}: {}",
        rejected.description
    );
    Disposition::DeadLetter {
        reason: rejected.reason.to_string(),
        description: rejected.description.clone(),
    }
}

/// Run the configured call, or reject the message before any call.
///
/// `Err` is a dead letter decided without touching the database
/// (`spec/10-senders.md` §7.8); `Ok` carries what the database said.
async fn dispatch<R: ProcedureRunner>(
    runner: &mut R,
    config: &AppConfig,
    message: &Incoming,
    body: &str,
    msh: &MshInfo,
    events: &EventLogger,
    correlation: Option<&str>,
) -> Result<Result<(), DatabaseError>, Disposition> {
    let procedure = &config.db.stored_procedure;
    match config.procedure_type {
        ProcedureType::QueueMessage => Ok(runner.execute(body, config.default_is_priority).await),
        // The identifiers are read here, and a message they cannot be read
        // from can never succeed.
        ProcedureType::QueueIdUpdate => {
            match idupdate::plan(body, &message.properties, msh, &config.source_system) {
                Ok(update) => Ok(runner.execute_id_update(&update).await),
                Err(rejected) => Err(reject(&rejected, procedure, msh, events, correlation)),
            }
        }
        ProcedureType::UpdateIdentifier => match idupdate::plan_update_identifier(body, msh) {
            Ok(update) => Ok(runner.execute_update_identifier(&update).await),
            Err(rejected) => Err(reject(&rejected, procedure, msh, events, correlation)),
        },
        ProcedureType::Trrr(mapping) => match trrr::map(body, mapping) {
            Ok(request) => Ok(runner.execute_trrr(&request).await),
            Err(rejected) => Err(reject(&rejected, procedure, msh, events, correlation)),
        },
    }
}

/// Take one message through `§7.2`.
///
/// The message is **completed only when the procedure committed**
/// (`spec/10-senders.md` §1.1). Every other path abandons or dead-letters.
///
/// # Examples
///
/// ```
/// use db_sender::{AppConfig, DatabaseError, Disposition, ProcedureRunner, process_one};
/// use ihb_bus::fake::FakeBus;
/// use ihb_bus::message::Outgoing;
/// use ihb_telemetry::{events::EventLogger, metrics::Metrics};
///
/// struct Ok_;
/// impl ProcedureRunner for Ok_ {
///     async fn execute(&mut self, _: &str, _: bool) -> Result<(), DatabaseError> { Ok(()) }
/// }
///
/// # tokio_test::block_on(async {
/// let config = AppConfig::from_pairs([
///     ("INGRESS_TOPIC_NAME", "t"), ("INGRESS_SUBSCRIPTION_NAME", "s"),
///     ("DB_USERNAME", "u"), ("DB_PASSWORD", "p"), ("DB_ENCRYPT", "No"),
/// ])?;
/// let bus = FakeBus::new();
/// bus.send("in", Outgoing::new("MSH|^~\\&|349|349|||2026||ADT^A40|C1|P|2.5"))?;
/// let message = bus.receive("in", 1).remove(0);
///
/// let outcome = process_one(
///     &message, &mut Ok_, &config,
///     &EventLogger::new("w", "m"), &Metrics::new("w", "m", "h", "p"),
/// ).await;
/// assert_eq!(outcome, Disposition::Complete);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// ```
pub async fn process_one<R: ProcedureRunner>(
    message: &Incoming,
    runner: &mut R,
    config: &AppConfig,
    events: &EventLogger,
    metrics: &Metrics,
) -> Disposition {
    let procedure = &config.db.stored_procedure;
    let values = LogValues::from_properties(&message.properties);
    let correlation = correlation_id_for_event(&values.correlation_id);

    // Not in the upstream (it raised `UnicodeDecodeError` out of the
    // handler). `spec/index.md` §0.6 forbids sending a payload that is not
    // valid UTF-8 on, and the bytes can never become valid, so retrying only
    // burns the delivery count.
    let Ok(body) = std::str::from_utf8(&message.body) else {
        tracing::error!(correlation_id = %values.correlation_id, "message body is not valid UTF-8");
        return Disposition::DeadLetter {
            reason: ENCODING_REASON.into(),
            description: format!(
                "Message body is not valid UTF-8 - CorrelationId: {}",
                values.correlation_id
            ),
        };
    };

    // The MSH only, for every log and event below (`§7.5`).
    let msh = extract_msh_for_logging(body);
    tracing::info!(
        correlation_id = %values.correlation_id,
        control_id = %msh.control_id,
        workflow_id = %values.workflow_id,
        target_procedure = %procedure,
        "message received for DB execution"
    );
    events.log_message_received(
        &msh.msh_text,
        Some(&format!("Message received for execution of {procedure}")),
        correlation,
    );

    let result = match dispatch(runner, config, message, body, &msh, events, correlation).await {
        Ok(result) => result,
        Err(rejected) => return rejected,
    };

    match result {
        Ok(()) => {
            metrics.message_sent();
            // After the commit. Nothing here can change the disposition: the
            // event logger does not return an error, and even if it did, the
            // row is written, so redelivery would insert it twice
            // (upstream's "post-commit logging is best effort").
            events.log_message_processed(
                &msh.msh_text,
                Some(&format!(
                    "Stored procedure {procedure} executed successfully"
                )),
                correlation,
            );
            tracing::info!(
                control_id = %msh.control_id,
                "stored procedure {procedure} executed successfully"
            );
            Disposition::Complete
        }
        Err(error) if error.kind == FailureKind::Fatal => {
            events.log_message_failed(
                &msh.msh_text,
                &format!(
                    "Non-recoverable database error executing {procedure}: {}",
                    error.message
                ),
                None,
                correlation,
            );
            tracing::error!(
                code = error.code,
                control_id = %msh.control_id,
                "fatal database error executing {procedure}: {}",
                error.message
            );
            Disposition::DeadLetter {
                reason: FATAL_REASON.into(),
                description: error.description(),
            }
        }
        Err(error) => {
            // No MESSAGE_FAILED: the message is not failed, it is retried
            // (upstream logs a warning only).
            tracing::warn!(
                code = error.code,
                control_id = %msh.control_id,
                "transient database error executing {procedure}: {}; abandoning the message for retry",
                error.message
            );
            Disposition::Abandon
        }
    }
}

/// Process a batch in order.
///
/// An [`Disposition::Abandon`] is the last entry; the tail is left
/// unattempted and abandoned by the caller. A dead letter does not stop the
/// batch.
pub async fn process_batch<R: ProcedureRunner>(
    batch: &[Incoming],
    runner: &mut R,
    config: &AppConfig,
    events: &EventLogger,
    metrics: &Metrics,
) -> Vec<Disposition> {
    let mut outcomes = Vec::with_capacity(batch.len());
    for message in batch {
        metrics.message_received();
        let outcome = process_one(message, runner, config, events, metrics).await;
        let stop = outcome == Disposition::Abandon;
        outcomes.push(outcome);
        if stop {
            break;
        }
    }
    outcomes
}

#[cfg(test)]
#[allow(
    clippy::assert_is_empty,
    clippy::unused_async_trait_impl,
    reason = "plain asserts read best in tests; the fake runner's async fn is the trait's shape"
)]
mod tests {
    use super::*;
    use crate::db::DatabaseError;
    use ihb_bus::fake::FakeBus;
    use ihb_bus::message::Outgoing;

    /// A runner that records what it was given and returns what it is told.
    struct Fake {
        calls: Vec<(String, bool)>,
        id_calls: Vec<idupdate::IdUpdate>,
        identifier_calls: Vec<idupdate::IdentifierUpdate>,
        trrr_calls: Vec<trrr::TrrrRequest>,
        result: Result<(), DatabaseError>,
    }

    impl Fake {
        fn ok() -> Self {
            Self {
                calls: vec![],
                id_calls: vec![],
                identifier_calls: vec![],
                trrr_calls: vec![],
                result: Ok(()),
            }
        }
        fn failing(kind: FailureKind, code: u32, message: &str) -> Self {
            Self {
                calls: vec![],
                id_calls: vec![],
                identifier_calls: vec![],
                trrr_calls: vec![],
                result: Err(DatabaseError::new(kind, code, message)),
            }
        }
    }

    impl ProcedureRunner for Fake {
        async fn execute(&mut self, message: &str, is_priority: bool) -> Result<(), DatabaseError> {
            self.calls.push((message.to_string(), is_priority));
            self.result.clone()
        }

        async fn execute_id_update(
            &mut self,
            update: &idupdate::IdUpdate,
        ) -> Result<(), DatabaseError> {
            self.id_calls.push(update.clone());
            self.result.clone()
        }

        async fn execute_trrr(&mut self, request: &trrr::TrrrRequest) -> Result<(), DatabaseError> {
            self.trrr_calls.push(request.clone());
            self.result.clone()
        }

        async fn execute_update_identifier(
            &mut self,
            update: &idupdate::IdentifierUpdate,
        ) -> Result<(), DatabaseError> {
            self.identifier_calls.push(update.clone());
            self.result.clone()
        }
    }

    fn config() -> AppConfig {
        AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "test-topic"),
            ("INGRESS_SUBSCRIPTION_NAME", "test-sub"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "pwd"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap()
    }

    const XML: &str = "<ADT_A39><MSH><MSH.9><MSG.1>ADT</MSG.1><MSG.2>A40</MSG.2><MSG.3>ADT_A39</MSG.3></MSH.9>\
        <MSH.10><MSH.10.1>MSG-123</MSH.10.1></MSH.10></MSH>\
        <PID><PID.5><XPN.1><FN.1>SECRET_PATIENT</FN.1></XPN.1></PID.5></PID></ADT_A39>";
    const ER7: &str = "MSH|^~\\&|349|349|||20261022||ADT^A40|CTRL1|P|2.5\rPID|1||999";

    fn inbound(body: impl Into<Vec<u8>>) -> Incoming {
        let bus = FakeBus::new();
        let props = [
            ("CorrelationId", "test-corr-id-123"),
            ("WorkflowID", "risp-to-wrrs"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        bus.send("in", Outgoing::new(body).with_properties(props))
            .unwrap();
        bus.receive("in", 1).remove(0)
    }

    async fn run(body: &str, runner: &mut Fake) -> Disposition {
        process_one(
            &inbound(body),
            runner,
            &config(),
            &EventLogger::new("risp-to-wrrs", "wrrs_stmsg_dbsender"),
            &Metrics::new("risp-to-wrrs", "wrrs_stmsg_dbsender", "SHARED", "WRRS"),
        )
        .await
    }

    #[tokio::test]
    async fn success_calls_the_procedure_once_with_the_exact_body() {
        // §7.2, §7.7 (test_process_message_success).
        let mut runner = Fake::ok();
        assert_eq!(run(XML, &mut runner).await, Disposition::Complete);
        assert_eq!(runner.calls, [(XML.to_string(), false)]);
    }

    #[tokio::test]
    async fn priority_follows_the_configuration() {
        let mut config = config();
        config.default_is_priority = true;
        let mut runner = Fake::ok();
        process_one(
            &inbound(ER7),
            &mut runner,
            &config,
            &EventLogger::new("w", "m"),
            &Metrics::new("w", "m", "h", "p"),
        )
        .await;
        assert!(runner.calls[0].1);
    }

    #[tokio::test]
    async fn a_fatal_error_dead_letters_with_the_server_message() {
        // §7.3 (test_process_message_fatal_error).
        let mut runner = Fake::failing(
            FailureKind::Fatal,
            2812,
            "Could not find stored procedure 'dbo.prQueueMessage'",
        );
        let Disposition::DeadLetter {
            reason,
            description,
        } = run(ER7, &mut runner).await
        else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "StoredProcedureExecutionFailed");
        assert!(
            description.contains("Could not find stored procedure"),
            "{description}"
        );
        assert!(
            description.starts_with("SQLState: UNKNOWN, Code: 2812: "),
            "{description}"
        );
    }

    #[tokio::test]
    async fn a_transient_error_abandons() {
        // test_process_message_transient_error.
        let mut runner = Fake::failing(FailureKind::Transient, 0, "Query timeout expired");
        assert_eq!(run(ER7, &mut runner).await, Disposition::Abandon);
    }

    #[tokio::test]
    async fn patient_data_echoed_by_the_server_never_reaches_the_dead_letter() {
        // §7.4 (test_process_message_fatal_error_redacts_patient_data...).
        let mut runner = Fake::failing(
            FailureKind::Fatal,
            245,
            "Conversion failed when converting the varchar value '9999999999' to data type int.",
        );
        let Disposition::DeadLetter { description, .. } = run(ER7, &mut runner).await else {
            panic!("expected a dead letter");
        };
        assert!(!description.contains("9999999999"), "{description}");
        assert!(description.contains("converting the varchar value '***' to data type int"));
    }

    #[tokio::test]
    async fn a_body_that_is_not_utf8_is_dead_lettered_without_a_call() {
        let mut runner = Fake::ok();
        let outcome = process_one(
            &inbound(b"MSH|\xff\xfe".to_vec()),
            &mut runner,
            &config(),
            &EventLogger::new("w", "m"),
            &Metrics::new("w", "m", "h", "p"),
        )
        .await;
        let Disposition::DeadLetter {
            reason,
            description,
        } = outcome
        else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "InvalidMessageEncoding");
        assert!(description.contains("test-corr-id-123"));
        assert!(runner.calls.is_empty());
    }

    #[tokio::test]
    async fn the_batch_stops_at_an_abandon_but_not_at_a_dead_letter() {
        let batch = vec![inbound(ER7), inbound(ER7)];
        let events = EventLogger::new("w", "m");
        let metrics = Metrics::new("w", "m", "h", "p");

        let mut fatal = Fake::failing(FailureKind::Fatal, 1, "x");
        let out = process_batch(&batch, &mut fatal, &config(), &events, &metrics).await;
        assert_eq!(out.len(), 2, "a dead letter does not stop the batch");

        let mut transient = Fake::failing(FailureKind::Transient, 0, "timeout");
        let out = process_batch(&batch, &mut transient, &config(), &events, &metrics).await;
        assert_eq!(
            out,
            [Disposition::Abandon],
            "the tail is left for the caller to abandon"
        );
        assert_eq!(transient.calls.len(), 1);
    }

    // ---- spec/10-senders.md §7.8: the Queue ID Update ----------------------

    fn id_update_config() -> AppConfig {
        AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_PROCEDURE_TYPE", "queue_id_update"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "pwd"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap()
    }

    const A31: &str = "MSH|^~\\&|328|328|||2026||ADT^A31^ADT_A05|CTRL31|P|2.5\r\
                       PID|1||555^^^328~9999999999^^^NHS||SECRET^PATIENT";
    const A40: &str = "MSH|^~\\&|328|328|||2026||ADT^A40^ADT_A39|CTRL40|P|2.5\r\
                       PID|1||7777777777^^^NHS\rMRG|1111111111^^^NHS";

    async fn run_id_update(body: &str, runner: &mut Fake) -> Disposition {
        process_one(
            &inbound(body),
            runner,
            &id_update_config(),
            &EventLogger::new("lims-to-wrrs", "wrrs_idupd_dbsender"),
            &Metrics::new("lims-to-wrrs", "wrrs_idupd_dbsender", "WLIMS", "WRRS"),
        )
        .await
    }

    #[tokio::test]
    async fn an_a31_calls_the_id_update_with_the_six_parameters_and_never_the_message_procedure() {
        let mut runner = Fake::ok();
        assert_eq!(run_id_update(A31, &mut runner).await, Disposition::Complete);
        assert!(runner.calls.is_empty(), "the message procedure is not used");
        assert_eq!(runner.id_calls.len(), 1);
        assert_eq!(runner.id_calls[0].new_identifier, "9999999999");
        assert_eq!(
            runner.id_calls[0].provider_identifier.as_deref(),
            Some("555")
        );
    }

    #[tokio::test]
    async fn an_a40_calls_the_id_update_as_a_merge() {
        let mut runner = Fake::ok();
        assert_eq!(run_id_update(A40, &mut runner).await, Disposition::Complete);
        assert_eq!(
            runner.id_calls[0].old_identifier.as_deref(),
            Some("1111111111")
        );
        assert_eq!(runner.id_calls[0].new_identifier, "7777777777");
    }

    #[tokio::test]
    async fn a_message_the_identifiers_cannot_be_read_from_is_dead_lettered_without_a_call() {
        let mut runner = Fake::ok();
        let outcome = run_id_update(
            "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\rPID|1||5551234567^^^328",
            &mut runner,
        )
        .await;
        let Disposition::DeadLetter {
            reason,
            description,
        } = outcome
        else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "MissingNhsIdentifier");
        assert!(!description.contains("5551234567"), "{description}");
        assert!(runner.id_calls.is_empty());

        let unsupported = run_id_update(
            "MSH|^~\\&|328|328|||2026||ADT^A28|C|P|2.5\rPID|1||5^^^328",
            &mut runner,
        )
        .await;
        assert!(matches!(
            unsupported,
            Disposition::DeadLetter { reason, .. } if reason == "UnsupportedTriggerEvent"
        ));
    }

    // ---- spec/10-senders.md §7.9: the WCRS identifier update ---------------

    async fn run_update_identifier(body: &str, runner: &mut Fake) -> Disposition {
        let config = AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_PROCEDURE_TYPE", "update_identifier"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "pwd"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        process_one(
            &inbound(body),
            runner,
            &config,
            &EventLogger::new("cavpms-to-wcrs", "wcrs_idupd_dbsender"),
            &Metrics::new("cavpms-to-wcrs", "wcrs_idupd_dbsender", "CAVPMS", "WCRS"),
        )
        .await
    }

    #[tokio::test]
    async fn an_a40_calls_update_identifier_with_three_strings_and_nothing_else() {
        let mut runner = Fake::ok();
        let body = "MSH|^~\\&|CAV|CAV|||2026||ADT^A40^ADT_A39|C40|P|2.5\r\
                    PID|1||0042^^^VAL\rMRG|0007^^^VAL";
        assert_eq!(
            run_update_identifier(body, &mut runner).await,
            Disposition::Complete
        );
        assert!(runner.calls.is_empty() && runner.id_calls.is_empty());
        assert_eq!(runner.identifier_calls.len(), 1);
        assert_eq!(runner.identifier_calls[0].old_identifier, "0007");
        assert_eq!(runner.identifier_calls[0].new_identifier, "0042");
    }

    #[tokio::test]
    async fn an_unplannable_a40_is_dead_lettered_before_any_call() {
        let mut runner = Fake::ok();
        let two = "MSH|^~\\&|CAV|CAV|||2026||ADT^A40|C|P|2.5\rPID|1||1^^^V\rMRG|2^^^V\rPID|1||3^^^V\rMRG|4^^^V";
        assert!(matches!(
            run_update_identifier(two, &mut runner).await,
            Disposition::DeadLetter { reason, .. } if reason == "MultiplePatientGroups"
        ));
        let a31 = "MSH|^~\\&|CAV|CAV|||2026||ADT^A31|C|P|2.5\rPID|1||1^^^V";
        assert!(matches!(
            run_update_identifier(a31, &mut runner).await,
            Disposition::DeadLetter { reason, .. } if reason == "UnsupportedTriggerEvent"
        ));
        assert!(runner.identifier_calls.is_empty());
    }

    // ---- spec/10-senders.md §7.10: the TRRR senders ------------------------

    async fn run_trrr(body: &str, mapping: &'static str, runner: &mut Fake) -> Disposition {
        let config = AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_PROCEDURE_TYPE", "trrr"),
            ("TRRR_MAPPING_TYPE", mapping),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "pwd"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        process_one(
            &inbound(body),
            runner,
            &config,
            &EventLogger::new("lims-to-trrr", "db_sender_trrr"),
            &Metrics::new("lims-to-trrr", "db_sender_trrr", "SHARED", "TRRR"),
        )
        .await
    }

    const TRRR_A40: &str = "MSH|^~\\&|SENDER|SENDER|||2026||ADT^A40^ADT_A39|C40|P|2.5\r\
                            PID|1||0042^^^SENDER~9999999999^^^NHS||SECRET^PATIENT\rMRG|0007^^^SENDER";

    #[tokio::test]
    async fn one_a40_is_served_by_both_instances_with_their_own_parameters() {
        // An A40 reaches both subscriptions; each instance makes its own call.
        let mut runner = Fake::ok();
        assert_eq!(
            run_trrr(TRRR_A40, "NHS_ID_UPDATE", &mut runner).await,
            Disposition::Complete
        );
        assert_eq!(
            run_trrr(TRRR_A40, "MERGE", &mut runner).await,
            Disposition::Complete
        );
        assert!(runner.calls.is_empty() && runner.id_calls.is_empty());
        assert!(matches!(
            runner.trrr_calls[0],
            trrr::TrrrRequest::Association { .. }
        ));
        assert!(matches!(
            runner.trrr_calls[1],
            trrr::TrrrRequest::Merge { .. }
        ));
    }

    #[tokio::test]
    async fn a_message_the_mapping_cannot_use_is_dead_lettered_with_mapping_failed() {
        let mut runner = Fake::ok();
        let outcome = run_trrr(
            "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\rPID|1||555^^^328",
            "NHS_ID_UPDATE",
            &mut runner,
        )
        .await;
        let Disposition::DeadLetter {
            reason,
            description,
        } = outcome
        else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "MappingFailed");
        assert_eq!(description, "Missing NHS identifier");
        assert!(runner.trrr_calls.is_empty());
    }

    #[tokio::test]
    async fn the_id_update_has_the_same_failure_classes_as_the_message_procedure() {
        // §7.3 applies unchanged.
        let mut fatal = Fake::failing(FailureKind::Fatal, 2812, "Could not find stored procedure");
        let Disposition::DeadLetter { reason, .. } = run_id_update(A31, &mut fatal).await else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "StoredProcedureExecutionFailed");

        let mut transient = Fake::failing(FailureKind::Transient, 1205, "deadlock");
        assert_eq!(
            run_id_update(A31, &mut transient).await,
            Disposition::Abandon
        );
    }
}
