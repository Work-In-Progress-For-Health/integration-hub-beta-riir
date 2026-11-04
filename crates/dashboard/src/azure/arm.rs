//! The Azure Resource Manager calls.
//!
//! Implements `spec/15-dashboard.md` §4.6's Service Bus and Container Apps
//! rows.
//!
//! # The API versions are ours now
//!
//! The Python reaches these through the Azure SDK, so the versions were
//! that SDK's problem. Calling REST directly makes them this workspace's,
//! and a wrong one is a `400` that §4.2 turns into a blank panel. They are
//! pinned as constants with a test, and named in §4.6.

use crate::azure::http::{HttpError, HttpsClient};
use crate::health::{ContainerState, QueueDepths};
use crate::observe::Container;

/// The Service Bus management API version (§4.6).
pub const SERVICE_BUS_API_VERSION: &str = "2021-11-01";

/// The Container Apps management API version (§4.6).
pub const CONTAINER_APPS_API_VERSION: &str = "2024-03-01";

/// The ARM host (§4.6).
pub const ARM_HOST: &str = "https://management.azure.com";

/// Where to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// `AZURE_SUBSCRIPTION_ID`.
    pub subscription_id: String,
    /// `AZURE_RESOURCE_GROUP`.
    pub resource_group: String,
}

impl Scope {
    /// The URL listing every queue in a namespace, with its runtime counts.
    ///
    /// # Examples
    ///
    /// ```
    /// use dashboard::azure::arm::Scope;
    ///
    /// let scope = Scope {
    ///     subscription_id: "sub".to_string(),
    ///     resource_group: "rg".to_string(),
    /// };
    ///
    /// assert_eq!(
    ///     scope.queues_url("ns"),
    ///     "https://management.azure.com/subscriptions/sub/resourceGroups/rg\
    ///      /providers/Microsoft.ServiceBus/namespaces/ns/queues\
    ///      ?api-version=2021-11-01"
    /// );
    /// ```
    #[must_use]
    pub fn queues_url(&self, namespace: &str) -> String {
        format!(
            "{ARM_HOST}/subscriptions/{}/resourceGroups/{}\
             /providers/Microsoft.ServiceBus/namespaces/{}/queues\
             ?api-version={SERVICE_BUS_API_VERSION}",
            self.subscription_id, self.resource_group, namespace,
        )
    }

    /// The URL listing every topic in a namespace, with its runtime counts
    /// (`spec/15-dashboard.md` §10.5.1).
    #[must_use]
    pub fn topics_url(&self, namespace: &str) -> String {
        format!(
            "{ARM_HOST}/subscriptions/{}/resourceGroups/{}\
             /providers/Microsoft.ServiceBus/namespaces/{}/topics\
             ?api-version={SERVICE_BUS_API_VERSION}",
            self.subscription_id, self.resource_group, namespace,
        )
    }

    /// The URL listing a topic's subscriptions (§10.5.1).
    #[must_use]
    pub fn subscriptions_url(&self, namespace: &str, topic: &str) -> String {
        format!(
            "{ARM_HOST}/subscriptions/{}/resourceGroups/{}\
             /providers/Microsoft.ServiceBus/namespaces/{}/topics/{}/subscriptions\
             ?api-version={SERVICE_BUS_API_VERSION}",
            self.subscription_id, self.resource_group, namespace, topic,
        )
    }

    /// The URL listing every Container App in the group.
    #[must_use]
    pub fn container_apps_url(&self) -> String {
        format!(
            "{ARM_HOST}/subscriptions/{}/resourceGroups/{}\
             /providers/Microsoft.App/containerApps\
             ?api-version={CONTAINER_APPS_API_VERSION}",
            self.subscription_id, self.resource_group,
        )
    }
}

/// Read every queue's depths (§4.3, §3.2).
///
/// # Errors
///
/// [`HttpError`]. The caller turns it into unknown depths for every queue,
/// which is §3.3 — not zero.
pub async fn queue_depths(
    client: &HttpsClient,
    token: &str,
    scope: &Scope,
    namespace: &str,
) -> Result<std::collections::BTreeMap<String, QueueDepths>, HttpError> {
    let document = client
        .get_json(&scope.queues_url(namespace), Some(token))
        .await?;

    Ok(parse_queues(&document))
}

/// Read the queue list ARM returns.
///
/// Split out so the shape can be tested against a captured response
/// without a subscription.
#[must_use]
pub fn parse_queues(
    document: &serde_json::Value,
) -> std::collections::BTreeMap<String, QueueDepths> {
    let Some(values) = document.get("value").and_then(serde_json::Value::as_array) else {
        return std::collections::BTreeMap::new();
    };

    values
        .iter()
        .filter_map(|queue| {
            let name = queue.get("name")?.as_str()?.to_string();
            let counts = queue.get("properties").and_then(|p| p.get("countDetails"));

            // Each count is optional **separately**: a response that
            // carries one and not the other must not report the missing one
            // as zero (§3.3).
            let depths = QueueDepths {
                active: counts
                    .and_then(|c| c.get("activeMessageCount"))
                    .and_then(serde_json::Value::as_u64),
                dead_letter: counts
                    .and_then(|c| c.get("deadLetterMessageCount"))
                    .and_then(serde_json::Value::as_u64),
            };

            Some((name, depths))
        })
        .collect()
}

/// Read every topic and its subscriptions (§10.5.1).
///
/// A topic whose subscriptions could not be listed is returned with none and
/// `subscriptions_read: false`, so the caller can tell "no subscriptions"
/// from "could not ask".
///
/// # Errors
///
/// [`HttpError`] when the topic list itself could not be read.
pub async fn topics(
    client: &HttpsClient,
    token: &str,
    scope: &Scope,
    namespace: &str,
) -> Result<Vec<crate::topics::Topic>, HttpError> {
    let document = client
        .get_json(&scope.topics_url(namespace), Some(token))
        .await?;
    let mut topics = crate::topics::parse_topics(&document);

    for topic in &mut topics {
        match client
            .get_json(
                &scope.subscriptions_url(namespace, &topic.name),
                Some(token),
            )
            .await
        {
            Ok(document) => {
                topic.subscriptions = crate::topics::parse_subscriptions(&document, &topic.name);
            }
            Err(error) => {
                tracing::warn!(%error, "could not list a topic's subscriptions");
                topic.subscriptions_read = false;
            }
        }
    }

    Ok(topics)
}

/// Read every Container App's state (§3.2's container row).
///
/// # Errors
///
/// [`HttpError`].
pub async fn containers(
    client: &HttpsClient,
    token: &str,
    scope: &Scope,
) -> Result<Vec<Container>, HttpError> {
    let document = client
        .get_json(&scope.container_apps_url(), Some(token))
        .await?;

    Ok(parse_containers(&document))
}

/// Read the Container Apps list ARM returns.
#[must_use]
pub fn parse_containers(document: &serde_json::Value) -> Vec<Container> {
    let Some(values) = document.get("value").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    values
        .iter()
        .filter_map(|app| {
            let name = app.get("name")?.as_str()?.to_string();
            let properties = app.get("properties");

            let provisioning = properties
                .and_then(|p| p.get("provisioningState"))
                .and_then(serde_json::Value::as_str);
            let running = properties
                .and_then(|p| p.get("runningStatus"))
                .and_then(serde_json::Value::as_str);

            Some(Container {
                name,
                state: state_of(provisioning, running),
                replicas: None,
                restarts: None,
            })
        })
        .collect()
}

/// Decide a container's state from what ARM said.
///
/// `runningStatus` is the one that matters and it is not always present —
/// an app mid-deployment reports a `provisioningState` and no running
/// status. An absent answer is [`ContainerState::Unknown`], never
/// [`ContainerState::NotRunning`]: §3.2 makes "not running" **Critical**,
/// so guessing it would page somebody because a field was missing.
#[must_use]
pub fn state_of(provisioning: Option<&str>, running: Option<&str>) -> ContainerState {
    match running.map(str::to_ascii_lowercase).as_deref() {
        Some("running") => ContainerState::Running,
        Some("stopped" | "suspended") => ContainerState::NotRunning,
        // `Progressing`, `Ready`, or something this version has not seen.
        Some(_) => ContainerState::Unknown,
        None => match provisioning.map(str::to_ascii_lowercase).as_deref() {
            Some("failed" | "canceled" | "cancelled") => ContainerState::NotRunning,
            _ => ContainerState::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            subscription_id: "sub-1".to_string(),
            resource_group: "rg-1".to_string(),
        }
    }

    #[test]
    fn the_api_versions_are_the_ones_the_specification_pins() {
        // spec/15-dashboard.md §4.6. These are ours now: the Python got
        // them from the SDK, and a wrong one is a 400 that §4.2 turns into
        // a blank panel with no explanation on the page.
        assert_eq!(SERVICE_BUS_API_VERSION, "2021-11-01");
        assert_eq!(CONTAINER_APPS_API_VERSION, "2024-03-01");

        assert!(scope().queues_url("ns").contains("api-version=2021-11-01"));
        assert!(
            scope()
                .container_apps_url()
                .contains("api-version=2024-03-01")
        );
    }

    #[test]
    fn the_urls_are_arm_resource_paths() {
        let queues = scope().queues_url("my-namespace");

        assert!(queues.starts_with("https://management.azure.com/subscriptions/sub-1/"));
        assert!(queues.contains("/resourceGroups/rg-1/"));
        assert!(queues.contains("/providers/Microsoft.ServiceBus/namespaces/my-namespace/queues"));

        let apps = scope().container_apps_url();
        assert!(apps.contains("/providers/Microsoft.App/containerApps"));
    }

    /// A queue list shaped like ARM's, including the awkward cases.
    fn queue_response() -> serde_json::Value {
        serde_json::json!({
            "value": [
                {
                    "name": "local-inthub-phw-transformer-ingress",
                    "properties": {
                        "countDetails": {
                            "activeMessageCount": 3,
                            "deadLetterMessageCount": 1,
                            "scheduledMessageCount": 0
                        }
                    }
                },
                {
                    "name": "half-reported",
                    "properties": { "countDetails": { "activeMessageCount": 7 } }
                },
                {
                    "name": "no-counts-at-all",
                    "properties": {}
                },
                { "properties": { "countDetails": { "activeMessageCount": 1 } } }
            ]
        })
    }

    #[test]
    fn the_queue_counts_are_read_from_count_details() {
        // §4.3 and §3.2.
        let queues = parse_queues(&queue_response());

        let phw = queues["local-inthub-phw-transformer-ingress"];
        assert_eq!(phw.active, Some(3));
        assert_eq!(phw.dead_letter, Some(1));
    }

    #[test]
    fn a_count_that_is_absent_stays_absent() {
        // §3.3 — THE test of this file. `unwrap_or(0)` here is exactly the
        // Python's defect (§9.8) one layer lower: a queue reporting an
        // active count and no dead-letter count would read as having no
        // dead-letters, which is a claim nobody made.
        let queues = parse_queues(&queue_response());

        let half = queues["half-reported"];
        assert_eq!(half.active, Some(7));
        assert_eq!(half.dead_letter, None, "not Some(0)");

        let none = queues["no-counts-at-all"];
        assert_eq!(none.active, None);
        assert_eq!(none.dead_letter, None);
        assert!(!none.is_known());
    }

    #[test]
    fn a_queue_with_no_name_is_skipped_rather_than_keyed_on_nothing() {
        let queues = parse_queues(&queue_response());

        assert_eq!(queues.len(), 3, "the nameless entry is dropped: {queues:?}");
        assert!(!queues.contains_key(""));
    }

    #[test]
    fn an_unexpected_response_is_empty_rather_than_a_panic() {
        // A `400` body, a changed shape, an empty document. §4.2 makes all
        // of them the same thing, and none of them may panic a page.
        for document in [
            serde_json::json!({}),
            serde_json::json!({"error": {"code": "InvalidApiVersionParameter"}}),
            serde_json::json!({"value": "not an array"}),
            serde_json::json!([]),
            serde_json::Value::Null,
        ] {
            assert!(parse_queues(&document).is_empty(), "{document}");
            assert!(parse_containers(&document).is_empty(), "{document}");
        }
    }

    #[test]
    fn a_container_is_running_only_when_it_says_so() {
        // §3.2 makes "not running" **Critical**, so this must never be a
        // guess. Anything unrecognised is Unknown.
        assert_eq!(
            state_of(Some("Succeeded"), Some("Running")),
            ContainerState::Running
        );
        assert_eq!(
            state_of(Some("Succeeded"), Some("running")),
            ContainerState::Running
        );
        assert_eq!(
            state_of(Some("Succeeded"), Some("Stopped")),
            ContainerState::NotRunning
        );
        assert_eq!(
            state_of(Some("Succeeded"), Some("Suspended")),
            ContainerState::NotRunning
        );

        // Mid-deployment, and a status this version has not seen.
        assert_eq!(
            state_of(Some("Succeeded"), Some("Progressing")),
            ContainerState::Unknown
        );
        assert_eq!(
            state_of(Some("Succeeded"), Some("SomethingNew")),
            ContainerState::Unknown
        );
    }

    #[test]
    fn a_missing_running_status_falls_back_to_provisioning() {
        // An app mid-deployment reports no running status at all.
        assert_eq!(state_of(Some("Failed"), None), ContainerState::NotRunning);
        assert_eq!(state_of(Some("Canceled"), None), ContainerState::NotRunning);
        assert_eq!(state_of(Some("Succeeded"), None), ContainerState::Unknown);
        assert_eq!(state_of(Some("InProgress"), None), ContainerState::Unknown);
        assert_eq!(state_of(None, None), ContainerState::Unknown);
    }

    #[test]
    fn containers_are_read_with_their_names() {
        let document = serde_json::json!({
            "value": [
                {
                    "name": "uks-dev-phw-hl7transformer-ca",
                    "properties": { "provisioningState": "Succeeded", "runningStatus": "Running" }
                },
                {
                    "name": "uks-dev-pims-hl7transformer-ca",
                    "properties": { "provisioningState": "Succeeded", "runningStatus": "Stopped" }
                }
            ]
        });

        let containers = parse_containers(&document);

        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].name, "uks-dev-phw-hl7transformer-ca");
        assert_eq!(containers[0].state, ContainerState::Running);
        assert_eq!(containers[1].state, ContainerState::NotRunning);
    }
}
