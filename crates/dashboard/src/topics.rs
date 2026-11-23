//! Topics and subscriptions: the namespace inventory.
//!
//! Implements `spec/15-dashboard.md` §10.5 (upstream `f7da495`).
//!
//! # Unknown is not zero
//!
//! Every count here is optional, for the reason `crate::health` gives for
//! queues: a subscription ARM listed without counts is not empty, and a
//! topic whose subscriptions could not be listed has not got none.

use serde_json::Value;

use crate::config::Thresholds;
use crate::health::{Health, QueueDepths, assess};

/// A subscription (§10.5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Its name.
    pub name: String,
    /// The topic it belongs to.
    pub topic: String,
    /// What was read of its depths.
    pub depths: QueueDepths,
}

impl Subscription {
    /// `{topic}/{subscription}` (§10.5.1).
    #[must_use]
    pub fn entity_name(&self) -> String {
        format!("{}/{}", self.topic, self.name)
    }
}

/// A topic (§10.5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topic {
    /// Its name.
    pub name: String,
    /// What was read of its depths.
    pub depths: QueueDepths,
    /// Its subscriptions, by name.
    pub subscriptions: Vec<Subscription>,
    /// Whether the subscriptions could be listed at all.
    pub subscriptions_read: bool,
}

/// The depths of one queue or subscription entity, as the management API reports them.
fn depths_of(entity: &Value) -> QueueDepths {
    let counts = entity.get("properties").and_then(|p| p.get("countDetails"));
    QueueDepths {
        active: counts
            .and_then(|c| c.get("activeMessageCount"))
            .and_then(Value::as_u64),
        dead_letter: counts
            .and_then(|c| c.get("deadLetterMessageCount"))
            .and_then(Value::as_u64),
    }
}

/// Read ARM's topic list.
#[must_use]
pub fn parse_topics(document: &Value) -> Vec<Topic> {
    let mut topics: Vec<Topic> = document
        .get("value")
        .and_then(Value::as_array)
        .map_or_default(|values| {
            values
                .iter()
                .filter_map(|t| {
                    Some(Topic {
                        name: t.get("name")?.as_str()?.to_string(),
                        depths: depths_of(t),
                        subscriptions: Vec::new(),
                        subscriptions_read: true,
                    })
                })
                .collect()
        });
    topics.sort_by(|a, b| a.name.cmp(&b.name));
    topics
}

/// Read ARM's subscription list for `topic`.
#[must_use]
pub fn parse_subscriptions(document: &Value, topic: &str) -> Vec<Subscription> {
    let mut subscriptions: Vec<Subscription> = document
        .get("value")
        .and_then(Value::as_array)
        .map_or_default(|values| {
            values
                .iter()
                .filter_map(|s| {
                    Some(Subscription {
                        name: s.get("name")?.as_str()?.to_string(),
                        topic: topic.to_string(),
                        depths: depths_of(s),
                    })
                })
                .collect()
        });
    subscriptions.sort_by(|a, b| a.name.cmp(&b.name));
    subscriptions
}

/// An entity's health: §3.2's queue rule with no container (§10.5.3).
#[must_use]
pub fn health(depths: QueueDepths, thresholds: Thresholds) -> Health {
    assess(depths, &[], thresholds)
}

/// A total that is absent if any term was (§2.2.3, §10.5.4).
fn total<I: IntoIterator<Item = Option<u64>>>(terms: I) -> Option<u64> {
    terms
        .into_iter()
        .try_fold(0_u64, |sum, term| sum.checked_add(term?))
}

/// The namespace KPIs (§10.5.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Kpis {
    /// Topics.
    pub topic_count: usize,
    /// Subscriptions.
    pub subscription_count: usize,
    /// Waiting on topics; absent if any was unreadable.
    pub topic_active_messages: Option<u64>,
    /// Dead-lettered on topics.
    pub topic_dead_letter_messages: Option<u64>,
    /// Waiting on subscriptions.
    pub subscription_active_messages: Option<u64>,
    /// Dead-lettered on subscriptions.
    pub subscription_dead_letter_messages: Option<u64>,
    /// Topics and subscriptions whose depths could not be read.
    pub entities_unknown: usize,
}

/// Compute the KPIs.
#[must_use]
pub fn kpis(topics: &[Topic]) -> Kpis {
    let subscriptions: Vec<&Subscription> = topics.iter().flat_map(|t| &t.subscriptions).collect();
    // One count over both kinds, so no addition of two counts is needed.
    let unknown = topics
        .iter()
        .map(|t| &t.depths)
        .chain(subscriptions.iter().map(|s| &s.depths))
        .filter(|depths| !depths.is_known())
        .count();

    Kpis {
        topic_count: topics.len(),
        subscription_count: subscriptions.len(),
        topic_active_messages: total(topics.iter().map(|t| t.depths.active)),
        topic_dead_letter_messages: total(topics.iter().map(|t| t.depths.dead_letter)),
        subscription_active_messages: total(subscriptions.iter().map(|s| s.depths.active)),
        subscription_dead_letter_messages: total(
            subscriptions.iter().map(|s| s.depths.dead_letter),
        ),
        entities_unknown: unknown,
    }
}

/// A topic as the API shows it (§10.5.5).
#[must_use]
pub fn topic_json(topic: &Topic, thresholds: Thresholds) -> Value {
    serde_json::json!({
        "name": topic.name,
        "entity_type": "topic",
        "entity_name": topic.name,
        "active_message_count": topic.depths.active,
        "dead_letter_message_count": topic.depths.dead_letter,
        "known": topic.depths.is_known(),
        "health": health(topic.depths, thresholds).css_class(),
        "subscriptions_read": topic.subscriptions_read,
        "subscriptions": topic.subscriptions.iter().map(|s| serde_json::json!({
            "name": s.name,
            "topic": s.topic,
            "entity_type": "subscription",
            "entity_name": s.entity_name(),
            "active_message_count": s.depths.active,
            "dead_letter_message_count": s.depths.dead_letter,
            "known": s.depths.is_known(),
            "health": health(s.depths, thresholds).css_class(),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm_topics() -> Value {
        serde_json::json!({"value": [
            {"name": "sbt-b", "properties": {"countDetails": {"activeMessageCount": 2, "deadLetterMessageCount": 0}}},
            {"name": "sbt-a", "properties": {}},
        ]})
    }

    #[test]
    fn topics_are_read_sorted_with_unknown_counts_absent() {
        // spec/15-dashboard.md §10.5.1, §10.5.2.
        let topics = parse_topics(&arm_topics());

        assert_eq!(topics[0].name, "sbt-a");
        assert_eq!(
            topics[0].depths,
            QueueDepths::unknown(),
            "unlisted counts are unknown, not zero"
        );
        assert_eq!(topics[1].depths, QueueDepths::known(2, 0));
    }

    #[test]
    fn a_response_with_no_value_is_no_topics_not_a_panic() {
        assert!(parse_topics(&serde_json::json!({})).is_empty());
        assert!(parse_subscriptions(&Value::Null, "t").is_empty());
    }

    #[test]
    fn subscriptions_know_their_topic_and_entity_name() {
        let subs = parse_subscriptions(
            &serde_json::json!({"value": [{"name": "sub-1", "properties": {"countDetails": {"activeMessageCount": 0}}}]}),
            "sbt-a",
        );

        assert_eq!(subs[0].entity_name(), "sbt-a/sub-1");
        assert_eq!(subs[0].depths.active, Some(0));
        assert_eq!(
            subs[0].depths.dead_letter, None,
            "one count present does not imply the other"
        );
    }

    #[test]
    fn health_is_the_queue_rule() {
        // §10.5.3.
        let t = Thresholds::default();

        assert_eq!(health(QueueDepths::known(0, 0), t), Health::Healthy);
        assert_eq!(
            health(QueueDepths::known(t.queue_warning, 0), t),
            Health::Warning
        );
        assert_eq!(
            health(QueueDepths::known(t.queue_critical, 0), t),
            Health::Critical
        );
        assert_eq!(
            health(QueueDepths::known(0, 1), t),
            Health::Critical,
            "a dead letter is critical"
        );
        assert_eq!(health(QueueDepths::unknown(), t), Health::Unknown);
        assert_eq!(
            health(
                QueueDepths {
                    active: Some(0),
                    dead_letter: None
                },
                t
            ),
            Health::Unknown
        );
    }

    #[test]
    fn a_total_over_an_unreadable_entity_is_absent() {
        // §10.5.4, §2.2.3.
        let mut topics = parse_topics(&arm_topics());
        topics[1].subscriptions = vec![Subscription {
            name: "s".into(),
            topic: "sbt-b".into(),
            depths: QueueDepths::known(5, 1),
        }];

        let k = kpis(&topics);

        assert_eq!(k.topic_count, 2);
        assert_eq!(k.subscription_count, 1);
        assert_eq!(k.topic_active_messages, None, "sbt-a was unreadable");
        assert_eq!(k.subscription_active_messages, Some(5));
        assert_eq!(k.subscription_dead_letter_messages, Some(1));
        assert_eq!(k.entities_unknown, 1);
    }

    #[test]
    fn nothing_at_all_totals_zero_not_absent() {
        let k = kpis(&[]);

        assert_eq!(k.topic_active_messages, Some(0));
        assert_eq!(k.entities_unknown, 0);
    }

    #[test]
    fn the_json_carries_the_entity_names_and_health() {
        let mut topics = parse_topics(&arm_topics());
        topics[1].subscriptions = vec![Subscription {
            name: "s".into(),
            topic: "sbt-b".into(),
            depths: QueueDepths::known(0, 3),
        }];

        let json = topic_json(&topics[1], Thresholds::default());

        assert_eq!(json["subscriptions"][0]["entity_name"], "sbt-b/s");
        assert_eq!(json["subscriptions"][0]["health"], "critical");
        assert_eq!(json["health"], "healthy");
        assert_eq!(
            topic_json(&topics[0], Thresholds::default())["active_message_count"],
            Value::Null
        );
    }
}
