//! Routing properties added after the tracking ones.
//!
//! Implements `spec/19-rest-servers.md` §7 and `spec/03-message-bus.md` §4.
//!
//! A failure to build them is not fatal (`spec/19-rest-servers.md` §3,
//! step 10): a subscription that does not match on them still gets the
//! message and can read the payload, so losing them costs efficiency, while
//! dropping the message costs a patient record. Nothing here can fail; a
//! field that cannot be read is empty.

use hl7_2::Message;
use ihb_bus::metadata::Properties;
use ihb_fields::{authority_codes, pipe_wrap, read};

use crate::config::FlowRule;
use crate::rules::resolve_structure;

/// `MSH-9.2`, the trigger event.
pub const MESSAGE_TYPE: &str = "MessageType";

/// The resolved message structure, `risp` only.
pub const MESSAGE_STRUCTURE: &str = "MessageStructure";

/// Pipe-wrapped `PID-2` assigning authority codes.
pub const UPDATE_SOURCES: &str = "UpdateSources";

/// Pipe-wrapped `PID-3` assigning authority codes.
pub const ASSIGNING_AUTHORITIES: &str = "AssigningAuthorities";

/// `PID-29.1`.
pub const DATE_DEATH: &str = "DateDeath";

/// `PID-30`.
pub const REASON_DEATH: &str = "ReasonDeath";

/// The properties a flow adds, or none.
///
/// # Examples
///
/// ```
/// use hl7_rest_server::config::FlowRule;
/// use hl7_rest_server::routing::{flow_properties, MESSAGE_STRUCTURE, MESSAGE_TYPE};
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|350|350|MPI|MPI|2024||ORU^R01^ORU_R01|1|P|2.5.1",
/// )?;
/// let properties = flow_properties(Some(FlowRule::Risp), &message);
///
/// assert_eq!(properties[MESSAGE_TYPE], "R01");
/// assert_eq!(properties[MESSAGE_STRUCTURE], "ORU_R01");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
#[must_use]
pub fn flow_properties(flow: Option<FlowRule>, message: &Message) -> Properties {
    let mut properties = Properties::new();
    match flow {
        Some(FlowRule::Mpi) => {
            properties.insert(MESSAGE_TYPE.to_string(), read(message, "MSH-9.2"));
            properties.insert(
                UPDATE_SOURCES.to_string(),
                pipe_wrap(&authority_codes(message, 2)),
            );
            properties.insert(
                ASSIGNING_AUTHORITIES.to_string(),
                pipe_wrap(&authority_codes(message, 3)),
            );
            properties.insert(DATE_DEATH.to_string(), read(message, "PID-29.1"));
            properties.insert(REASON_DEATH.to_string(), read(message, "PID-30"));
        }
        Some(FlowRule::Risp) => {
            properties.insert(MESSAGE_TYPE.to_string(), read(message, "MSH-9.2"));
            properties.insert(MESSAGE_STRUCTURE.to_string(), resolve_structure(message));
        }
        Some(FlowRule::Schema(_)) | None => {}
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(er7: &str) -> Message {
        ihb_validation::parse(er7).unwrap()
    }

    #[test]
    fn mpi_carries_the_five_properties() {
        // spec/19 §7.1.
        let message = parse(
            "MSH|^~\\&|100|100|252|252|2024||ADT^A31^ADT_A05|M|P|2.5\r\
             PID|1|8888888^^^252^PI~7777777^^^109^PI|4444444444^^^NHS^NH",
        );
        let properties = flow_properties(Some(FlowRule::Mpi), &message);
        assert_eq!(properties.len(), 5);
        assert_eq!(properties[MESSAGE_TYPE], "A31");
        assert_eq!(properties[UPDATE_SOURCES], "|252|109|");
        assert_eq!(properties[ASSIGNING_AUTHORITIES], "|NHS|");
    }

    #[test]
    fn risp_carries_the_trigger_and_the_structure() {
        // spec/19 §7.2 — the part the retired hl7_rest_server lacks.
        let message = parse("MSH|^~\\&|349|349|MPI|MPI|2024||ADT^A40|1|P|2.5.1");
        let properties = flow_properties(Some(FlowRule::Risp), &message);
        assert_eq!(properties.len(), 2);
        assert_eq!(properties[MESSAGE_TYPE], "A40");
        assert_eq!(properties[MESSAGE_STRUCTURE], "ADT_A40");
    }

    #[test]
    fn risp_structure_is_the_messages_own_or_the_resolved_one() {
        // spec/19 §7.2 (upstream 2b26c7f): MSH-9.3 wins when present, even
        // where it is not what the trigger would give (A40 declares ADT_A39);
        // absent, it is `{MSH-9.1}_{MSH-9.2}`.
        let declared = parse("MSH|^~\\&|349|349|MPI|MPI|2024||ADT^A28^ADT_A05|1|P|2.5.1");
        let absent = parse("MSH|^~\\&|349|349|MPI|MPI|2024||ADT^A28|1|P|2.5.1");
        let a40 = parse("MSH|^~\\&|349|349|MPI|MPI|2024||ADT^A40^ADT_A39|1|P|2.5.1");

        for (message, trigger, structure) in [
            (&declared, "A28", "ADT_A05"),
            (&absent, "A28", "ADT_A28"),
            (&a40, "A40", "ADT_A39"),
        ] {
            let properties = flow_properties(Some(FlowRule::Risp), message);
            assert_eq!(properties[MESSAGE_TYPE], trigger);
            assert_eq!(properties[MESSAGE_STRUCTURE], structure);
        }
    }

    #[test]
    fn other_flows_add_nothing() {
        let message = parse("MSH|^~\\&|A|B|C|D|2024||ADT^A31|1|P|2.5");
        assert!(flow_properties(None, &message).is_empty());
        assert!(
            flow_properties(Some(FlowRule::Schema(ihb_validation::Flow::Phw)), &message).is_empty()
        );
    }
}
