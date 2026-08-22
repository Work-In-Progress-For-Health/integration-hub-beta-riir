//! What the estate is made of.
//!
//! Implements `spec/15-dashboard.md` §3.1: "Flow definitions mirror
//! `01-architecture.md` §2.1 — the same flows, the same queue names,
//! resolved from the same environment variables."
//!
//! # Identity is the `WORKFLOW_ID`, and nothing else
//!
//! Every alarm rule, every Log Analytics query and every telemetry event
//! keys on `workflow_id` (`spec/04-observability.md` §1). A flow the
//! dashboard calls by a different name is a flow whose alarms match
//! nothing — and match nothing *quietly*, reporting `unknown` forever
//! rather than failing.
//!
//! The Python has exactly that defect for the outbound flow; see
//! `spec/15-dashboard.md` §9.5.

/// One end-to-end message path (`spec/01-architecture.md` §2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// The `WORKFLOW_ID` every service in this flow emits.
    ///
    /// The join key for telemetry and alarms. Not a display name.
    pub workflow_id: &'static str,
    /// What an operator sees.
    pub label: &'static str,
    /// The source system.
    pub source: &'static str,
    /// The destination.
    pub destination: &'static str,
    /// The ingress port (`spec/01-architecture.md` §2.1.2).
    pub port: u16,
    /// The transformer, when the flow has one.
    pub transformer: Option<&'static str>,
}

/// Every flow of `spec/01-architecture.md` §2.1, in that order.
///
/// A `const` rather than discovery: §3.1 says these mirror the architecture
/// specification, and a dashboard that discovered a flow the estate does
/// not have would be showing something nobody can act on.
///
/// # Examples
///
/// ```
/// use dashboard::flows::{FLOWS, flow};
///
/// assert_eq!(FLOWS.len(), 8);
///
/// let phw = flow("phw-to-mpi").expect("the PHW flow");
/// assert_eq!(phw.port, 2575);
/// assert_eq!(phw.transformer, Some("hl7-phw-transformer"));
///
/// // The outbound flow is `mpi-to-topic`, which is what the services
/// // actually emit -- see spec/15-dashboard.md §9.5.
/// assert!(flow("mpi-to-topic").is_some());
/// assert!(flow("mpi-outbound").is_none(), "the Python's name is not a workflow id");
/// ```
pub const FLOWS: [Flow; 8] = [
    Flow {
        workflow_id: "phw-to-mpi",
        label: "PHW → MPI",
        source: "PHW",
        destination: "MPI",
        port: 2575,
        transformer: Some("hl7-phw-transformer"),
    },
    Flow {
        workflow_id: "paris-to-mpi",
        label: "Paris → MPI",
        source: "Paris",
        destination: "MPI",
        port: 2577,
        transformer: None,
    },
    Flow {
        workflow_id: "chemocare-to-mpi",
        label: "ChemoCare → MPI",
        source: "ChemoCare",
        destination: "MPI",
        port: 2578,
        transformer: Some("hl7-chemo-transformer"),
    },
    Flow {
        workflow_id: "pims-to-mpi",
        label: "PIMS → MPI",
        source: "PIMS",
        destination: "MPI",
        port: 2579,
        transformer: Some("hl7-pims-transformer"),
    },
    Flow {
        workflow_id: "mpi-to-topic",
        label: "MPI → Topic",
        source: "MPI",
        destination: "Downstream systems",
        port: 2580,
        transformer: None,
    },
    Flow {
        workflow_id: "wds-to-mpi",
        label: "WDS → MPI",
        source: "WDS",
        destination: "MPI",
        port: 2582,
        transformer: None,
    },
    Flow {
        workflow_id: "mosaiq-to-mpi",
        label: "Mosaiq → MPI",
        source: "Mosaiq",
        destination: "MPI",
        port: 2583,
        transformer: None,
    },
    Flow {
        workflow_id: "lims-soapserver",
        label: "LIMS → MPI",
        source: "LIMS",
        destination: "MPI",
        port: 8080,
        transformer: None,
    },
];

/// The flow with this `WORKFLOW_ID`.
#[must_use]
pub fn flow(workflow_id: &str) -> Option<&'static Flow> {
    FLOWS.iter().find(|f| f.workflow_id == workflow_id)
}

/// Every `WORKFLOW_ID`, for a query's `in (...)` list.
#[must_use]
pub fn workflow_ids() -> Vec<String> {
    FLOWS.iter().map(|f| f.workflow_id.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flow_of_the_architecture_table_is_here() {
        // spec/01-architecture.md §2.1, all eight rows.
        let ids: Vec<&str> = FLOWS.iter().map(|f| f.workflow_id).collect();

        assert_eq!(
            ids,
            vec![
                "phw-to-mpi",
                "paris-to-mpi",
                "chemocare-to-mpi",
                "pims-to-mpi",
                "mpi-to-topic",
                "wds-to-mpi",
                "mosaiq-to-mpi",
                "lims-soapserver",
            ]
        );
    }

    #[test]
    fn the_outbound_flow_is_named_what_the_services_emit() {
        // spec/15-dashboard.md §9.5 — THE test of this file.
        //
        // `local/mpi-hl7-server.env` sets `WORKFLOW_ID="mpi-to-topic"`. The
        // Python's dashboard calls the same flow `mpi-outbound`, so an
        // alarm rule created against it matches no telemetry at all — and
        // reports `unknown` rather than failing, which looks like a quiet
        // flow rather than a wrong name.
        assert!(flow("mpi-to-topic").is_some());
        assert!(flow("mpi-outbound").is_none());
    }

    #[test]
    fn the_lims_flow_is_present() {
        // §9.6 — the Python's flow list omits it, so the LIMS ingress does
        // not appear on the dashboard at all and cannot be alarmed on.
        let lims = flow("lims-soapserver").expect("the LIMS flow");

        assert_eq!(lims.port, 8080, "the SOAP ingress, not an MLLP port");
    }

    #[test]
    fn the_ports_are_the_deployment_contract() {
        // §2.1.2. A wrong port here is a health check against nothing.
        let ports: Vec<u16> = FLOWS.iter().map(|f| f.port).collect();

        assert_eq!(ports, vec![2575, 2577, 2578, 2579, 2580, 2582, 2583, 8080]);
    }

    #[test]
    fn only_three_flows_have_a_transformer() {
        // §2.1 — PHW, ChemoCare and PIMS. The others cross unchanged, and
        // a dashboard showing a transformer they do not have would send
        // somebody looking for a container that does not exist.
        let with_transformer: Vec<&str> = FLOWS
            .iter()
            .filter(|f| f.transformer.is_some())
            .map(|f| f.workflow_id)
            .collect();

        assert_eq!(
            with_transformer,
            vec!["phw-to-mpi", "chemocare-to-mpi", "pims-to-mpi"]
        );
    }

    #[test]
    fn no_two_flows_share_an_id_or_a_port() {
        let mut ids: Vec<&str> = FLOWS.iter().map(|f| f.workflow_id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate workflow id");

        let mut ports: Vec<u16> = FLOWS.iter().map(|f| f.port).collect();
        ports.sort_unstable();
        let count = ports.len();
        ports.dedup();
        assert_eq!(ports.len(), count, "duplicate port");
    }

    #[test]
    fn every_workflow_id_survives_the_kql_sanitiser_unchanged() {
        // §4.4 strips anything outside `[A-Za-z0-9_-]`. A flow whose id did
        // not survive would be queried under a *different* name than it
        // reports itself as, and would silently never match.
        for flow in &FLOWS {
            assert_eq!(
                crate::kql::identifier(flow.workflow_id),
                flow.workflow_id,
                "{} is changed by the sanitiser",
                flow.workflow_id
            );
        }
    }

    #[test]
    fn the_id_list_is_what_a_query_needs() {
        let ids = workflow_ids();

        assert_eq!(ids.len(), FLOWS.len());
        assert!(ids.contains(&"phw-to-mpi".to_string()));
    }
}
