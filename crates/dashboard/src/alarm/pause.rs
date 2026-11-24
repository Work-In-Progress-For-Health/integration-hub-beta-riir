//! Scheduled alarm pauses.
//!
//! Implements `spec/15-dashboard.md` §10.2 (upstream `5eae123`).
//!
//! # Why a pause is its own document
//!
//! The evaluator rewrites the whole state document whenever anything in it
//! moves (§5.1.3). A pause written into that document by an operator would
//! race the evaluation cycle, and the cycle would win and undo the pause.
//! So a pause is a document in its own partition (§10.2.1), and the
//! evaluator only ever *reads* it.
//!
//! # Fail open to evaluate, fail closed to write
//!
//! §10.2.6. Not being able to read the pauses must never silence an alarm,
//! so the evaluator carries on as if there were none. But creating or
//! cancelling a pause on the strength of a read that failed would be acting
//! on a guess, so those refuse.

use std::collections::BTreeMap;

use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Duration, OffsetDateTime, PrimitiveDateTime};
use time_tz::{OffsetResult, PrimitiveDateTimeExt, TimeZone};

use crate::alarm::documents::ALARM_PAUSE;
use crate::alarm::store::{AlarmStore, StoreError};

/// The Cosmos partition pauses live in (§10.2.1).
pub const PARTITION: &str = "alarm-pause";

/// The three alarm types, as the partition names spell them.
pub const ALARM_TYPES: [&str; 3] = ["alarm1", "alarm2", "alarm3"];

/// Longest reason (§10.2.7).
pub const MAX_REASON_LENGTH: usize = 200;
/// Longest `requested_by` (§10.2.7).
pub const MAX_REQUESTED_BY_LENGTH: usize = 100;
/// Most targets in one pause (§10.2.7).
pub const MAX_TARGETS: usize = 500;
/// Longest `duration` pause, in minutes: a year.
pub const MAX_DURATION_MINUTES: i64 = 365 * 24 * 60;

/// A start this far in the past is taken as now (§10.2.7): a form left open
/// for a minute, or a clock a little behind.
const START_TOLERANCE: Duration = Duration::minutes(2);
/// How far ahead a pause may be scheduled.
const MAX_LEAD_TIME: Duration = Duration::days(365);
/// How long an ended pause stays on the page.
const RECENT_WINDOW: Duration = Duration::days(7);
/// How long an ended pause is kept before it is purged.
const RETENTION: Duration = Duration::days(30);

/// What a pause covers (§10.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Named rules.
    Rule,
    /// Every alarm of the named flows.
    Flows,
    /// Everything.
    All,
}

impl Scope {
    /// As stored.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Flows => "flows",
            Self::All => "all",
        }
    }

    /// Read a scope name as written on the wire; `None` for an unknown one.
    fn parse(text: &str) -> Option<Self> {
        match text {
            "rule" => Some(Self::Rule),
            "flows" => Some(Self::Flows),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// One rule a `rule`-scoped pause names (§10.2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleTarget {
    /// `alarm1`, `alarm2` or `alarm3`.
    pub alarm_type: String,
    /// The rule.
    pub rule_id: String,
    /// The rule's workflow, stored so a flow can be badged.
    pub workflow_id: String,
}

/// A pause, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pause {
    /// Its identifier — in a field of its own, because `id` does not
    /// survive a round trip (§10.1.3).
    pub pause_id: String,
    /// What it covers.
    pub scope: Scope,
    /// The flows, for [`Scope::Flows`].
    pub flows: Vec<String>,
    /// The rules, for [`Scope::Rule`].
    pub rules: Vec<RuleTarget>,
    /// When it starts. `None` is an unreadable document, which reads as
    /// ended (§10.2.3).
    pub start_at: Option<OffsetDateTime>,
    /// When it ends; `None` is until cancelled.
    pub end_at: Option<OffsetDateTime>,
    /// Why.
    pub reason: String,
    /// Who asked.
    pub requested_by: String,
    /// When it was made.
    pub created_at: Option<OffsetDateTime>,
    /// When it was cancelled, if it was.
    pub cancelled_at: Option<OffsetDateTime>,
}

/// A pause's state at an instant (§10.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseStatus {
    /// Not yet started.
    Scheduled,
    /// In force.
    Active,
    /// Over.
    Ended,
    /// Cancelled before it began.
    Cancelled,
}

impl PauseStatus {
    /// As the API spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Active => "active",
            Self::Ended => "ended",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Why a pause operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseError {
    /// The request failed validation. The message is safe to show.
    Invalid(String),
    /// No pause has that id.
    NotFound,
    /// The pause could not be kept or read. The message is safe to show.
    Persistence(String),
}

impl PauseError {
    /// The message for the response body.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Invalid(message) | Self::Persistence(message) => message,
            Self::NotFound => "Pause not found.",
        }
    }

    /// A refusal of the request itself (400), as opposed to a persistence failure.
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl std::fmt::Display for PauseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// What the caller is told when the store cannot be reached.
const UNAVAILABLE: &str = "Pause persistence is currently unavailable.";
/// What the caller is told when no store is configured.
const NOT_CONFIGURED: &str = "Pause persistence is not configured.";

/// The zone every displayed pause time is shown in.
const fn london() -> &'static time_tz::Tz {
    time_tz::timezones::db::europe::LONDON
}

/// `None` when the moment cannot be written (a year outside 0..=9999), so a
/// pause shows no time rather than an empty one (RCA-IN-6).
fn rfc3339(at: OffsetDateTime) -> Option<String> {
    at.format(&Rfc3339).ok()
}

/// Read a stored timestamp; `None` when it is absent or not RFC 3339.
fn parse_time(value: Option<&Value>) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value?.as_str()?, &Rfc3339).ok()
}

impl Pause {
    /// Read a stored document. `None` when it has no `pause_id` — a
    /// document of some other kind in the partition is skipped, not
    /// guessed at.
    #[must_use]
    pub fn from_document(document: &Value) -> Option<Self> {
        let pause_id = document.get("pause_id")?.as_str()?.to_string();
        if pause_id.is_empty() {
            return None;
        }

        let scope = Scope::parse(document.get("scope_type")?.as_str()?)?;
        let targets = document
            .get("targets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let text = |name: &str| {
            document
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };

        let (flows, rules) = match scope {
            Scope::Flows => (
                targets
                    .iter()
                    .filter_map(|t| t.as_str().map(ToString::to_string))
                    .collect(),
                Vec::new(),
            ),
            Scope::Rule => (
                Vec::new(),
                targets
                    .iter()
                    .filter_map(|t| {
                        let field = |name: &str| {
                            t.get(name)
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string()
                        };
                        // A target without both halves of its key can match
                        // nothing; dropping it is the same as not covering.
                        let target = RuleTarget {
                            alarm_type: field("alarm_type"),
                            rule_id: field("rule_id"),
                            workflow_id: field("workflow_id"),
                        };
                        (!target.alarm_type.is_empty() && !target.rule_id.is_empty())
                            .then_some(target)
                    })
                    .collect(),
            ),
            Scope::All => (Vec::new(), Vec::new()),
        };

        Some(Self {
            pause_id,
            scope,
            flows,
            rules,
            start_at: parse_time(document.get("start_at")),
            end_at: parse_time(document.get("end_at")),
            reason: text("reason"),
            requested_by: text("requested_by"),
            created_at: parse_time(document.get("created_at")),
            cancelled_at: parse_time(document.get("cancelled_at")),
        })
    }

    /// The document to store (without routing metadata).
    #[must_use]
    pub fn to_document(&self) -> Value {
        serde_json::json!({
            "pause_id": self.pause_id,
            "scope_type": self.scope.as_str(),
            "targets": self.targets_json(),
            "start_at": self.start_at.and_then(rfc3339),
            "end_at": self.end_at.and_then(rfc3339),
            "reason": self.reason,
            "requested_by": self.requested_by,
            "created_at": self.created_at.and_then(rfc3339),
            "cancelled_at": self.cancelled_at.and_then(rfc3339),
        })
    }

    /// The pause targets as stored: flows and rules, each only when present.
    fn targets_json(&self) -> Value {
        match self.scope {
            Scope::All => Value::Array(Vec::new()),
            Scope::Flows => Value::Array(self.flows.iter().cloned().map(Value::String).collect()),
            Scope::Rule => Value::Array(
                self.rules
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "alarm_type": t.alarm_type,
                            "rule_id": t.rule_id,
                            "workflow_id": t.workflow_id,
                        })
                    })
                    .collect(),
            ),
        }
    }
}

/// A pause's status at `now` (§10.2.3).
#[must_use]
pub fn status(pause: &Pause, now: OffsetDateTime) -> PauseStatus {
    let Some(start) = pause.start_at else {
        return PauseStatus::Ended;
    };
    if pause
        .cancelled_at
        .is_some_and(|cancelled| cancelled <= start)
    {
        return PauseStatus::Cancelled;
    }
    if start > now {
        return PauseStatus::Scheduled;
    }
    if pause.end_at.is_none_or(|end| end > now) {
        return PauseStatus::Active;
    }
    PauseStatus::Ended
}

/// Whether a pause applies to a rule, ignoring the clock (§10.2.4).
#[must_use]
pub fn covers(pause: &Pause, alarm_type: &str, rule_id: &str, workflow_id: &str) -> bool {
    match pause.scope {
        Scope::All => true,
        Scope::Flows => !workflow_id.is_empty() && pause.flows.iter().any(|f| f == workflow_id),
        Scope::Rule => pause
            .rules
            .iter()
            .any(|t| t.alarm_type == alarm_type && t.rule_id == rule_id),
    }
}

/// The active pause covering a rule, preferring the one that ends last; an
/// indefinite pause counts as latest of all (§10.2.5).
#[must_use]
pub fn active_for<'a>(
    pauses: &'a [Pause],
    alarm_type: &str,
    rule_id: &str,
    workflow_id: &str,
    now: OffsetDateTime,
) -> Option<&'a Pause> {
    pauses
        .iter()
        .filter(|p| {
            status(p, now) == PauseStatus::Active && covers(p, alarm_type, rule_id, workflow_id)
        })
        // `None` (indefinite) must sort above every finite end, which
        // `Option`'s ordering has the wrong way round.
        .max_by_key(|p| (p.end_at.is_none(), p.end_at))
}

/// The soonest scheduled pause covering a rule (§10.2.5).
#[must_use]
pub fn next_scheduled_for<'a>(
    pauses: &'a [Pause],
    alarm_type: &str,
    rule_id: &str,
    workflow_id: &str,
    now: OffsetDateTime,
) -> Option<&'a Pause> {
    pauses
        .iter()
        .filter(|p| {
            status(p, now) == PauseStatus::Scheduled && covers(p, alarm_type, rule_id, workflow_id)
        })
        .min_by_key(|p| p.start_at)
}

/// How many pauses are in force or waiting (§10.2.9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PauseSummary {
    /// In force now.
    pub active: usize,
    /// Not yet started.
    pub scheduled: usize,
}

/// Count active and scheduled pauses.
#[must_use]
pub fn summarise(pauses: &[Pause], now: OffsetDateTime) -> PauseSummary {
    PauseSummary {
        active: pauses
            .iter()
            .filter(|p| status(p, now) == PauseStatus::Active)
            .count(),
        scheduled: pauses
            .iter()
            .filter(|p| status(p, now) == PauseStatus::Scheduled)
            .count(),
    }
}

/// A moment in UK time, as the page shows it: `30 Sep 2026  15:05 BST`, or
/// `None` when it cannot be written (RCA-IN-6).
#[must_use]
pub fn format_london(at: OffsetDateTime) -> Option<String> {
    use time_tz::OffsetDateTimeExt;

    let local = at.to_timezone(london());
    let zone = london().get_offset_utc(&at);
    let name = time_tz::Offset::name(&zone);
    let body = local
        .format(&format_description!(
            "[day] [month repr:short] [year]  [hour]:[minute]"
        ))
        .ok()?;

    Some(format!("{body} {name}"))
}

/// What a status row says about the pause holding it (§10.2.9).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PauseFields {
    /// The pause, when it is a record and not a legacy `paused_until`.
    pub pause_id: Option<String>,
    /// `rule`, `flows` or `all`.
    pub pause_scope: &'static str,
    /// Who asked.
    pub paused_by: String,
    /// Why.
    pub pause_reason: String,
    /// Minutes left; absent when indefinite.
    pub pause_remaining: Option<f64>,
    /// Whether it ends only when cancelled.
    pub pause_indefinite: bool,
    /// When it ends, in UK time.
    pub paused_until: Option<String>,
}

/// The row fields for an active pause.
#[must_use]
pub fn fields_for(pause: &Pause, now: OffsetDateTime) -> PauseFields {
    PauseFields {
        pause_id: Some(pause.pause_id.clone()),
        pause_scope: pause.scope.as_str(),
        paused_by: pause.requested_by.clone(),
        pause_reason: pause.reason.clone(),
        pause_remaining: pause
            .end_at
            .map(|end| super::minutes_between(now, end))
            .map(f64::round),
        pause_indefinite: pause.end_at.is_none(),
        paused_until: pause.end_at.and_then(format_london),
    }
}

/// A scheduled pause, as a row mentions it (§10.2.9).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScheduledFields {
    /// The pause.
    pub pause_id: String,
    /// When it starts, in UK time.
    pub start_display: String,
}

/// The row field for the soonest scheduled pause.
#[must_use]
pub fn scheduled_fields_for(pause: &Pause) -> ScheduledFields {
    ScheduledFields {
        pause_id: pause.pause_id.clone(),
        start_display: pause.start_at.and_then(format_london).unwrap_or_default(),
    }
}

/// A pause as the API and the page show it (`to_view` upstream).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PauseView {
    /// The pause.
    pub pause_id: String,
    /// `rule`, `flows` or `all`.
    pub scope_type: &'static str,
    /// The targets, as stored.
    pub targets: Value,
    /// Start, RFC 3339.
    pub start_at: Option<String>,
    /// End, RFC 3339; absent when indefinite.
    pub end_at: Option<String>,
    /// Start, in UK time.
    pub start_display: Option<String>,
    /// End, in UK time.
    pub end_display: Option<String>,
    /// Ends only when cancelled.
    pub indefinite: bool,
    /// Why.
    pub reason: String,
    /// Who asked.
    pub requested_by: String,
    /// When made, in UK time.
    pub created_display: Option<String>,
    /// When cancelled, in UK time.
    pub cancelled_display: Option<String>,
    /// Cancelled after it had begun.
    pub cancelled_early: bool,
    /// `scheduled`, `active`, `ended` or `cancelled`.
    pub status: &'static str,
    /// Minutes left, for an active pause with an end.
    pub remaining_minutes: Option<f64>,
}

/// Build a pause's view.
#[must_use]
pub fn to_view(pause: &Pause, now: OffsetDateTime) -> PauseView {
    let state = status(pause, now);

    PauseView {
        pause_id: pause.pause_id.clone(),
        scope_type: pause.scope.as_str(),
        targets: pause.targets_json(),
        start_at: pause.start_at.and_then(rfc3339),
        end_at: pause.end_at.and_then(rfc3339),
        start_display: pause.start_at.and_then(format_london),
        end_display: pause.end_at.and_then(format_london),
        indefinite: pause.end_at.is_none(),
        reason: pause.reason.clone(),
        requested_by: pause.requested_by.clone(),
        created_display: pause.created_at.and_then(format_london),
        cancelled_display: pause.cancelled_at.and_then(format_london),
        cancelled_early: pause.cancelled_at.is_some() && state == PauseStatus::Ended,
        status: state.as_str(),
        remaining_minutes: match (pause.end_at, state) {
            (Some(end), PauseStatus::Active) => Some(super::minutes_between(now, end).round()),
            _ => None,
        },
    }
}

/// The pauses page's three lists.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Groups {
    /// In force, oldest start first.
    pub active: Vec<PauseView>,
    /// Waiting, soonest first.
    pub scheduled: Vec<PauseView>,
    /// Over in the last seven days, most recent first.
    pub recent: Vec<PauseView>,
}

/// When a pause stopped applying: its cancellation, else its end.
fn finished_at(pause: &Pause) -> Option<OffsetDateTime> {
    pause.cancelled_at.or(pause.end_at)
}

/// Split pauses into the page's three lists (§10.2.8).
#[must_use]
pub fn group_for_display(pauses: &[Pause], now: OffsetDateTime) -> Groups {
    let mut active: Vec<&Pause> = Vec::new();
    let mut scheduled: Vec<&Pause> = Vec::new();
    let mut recent: Vec<&Pause> = Vec::new();

    for pause in pauses {
        match status(pause, now) {
            PauseStatus::Active => active.push(pause),
            PauseStatus::Scheduled => scheduled.push(pause),
            PauseStatus::Ended | PauseStatus::Cancelled => {
                // A cutoff before the earliest representable date is earlier
                // than every pause, so everything is recent (RCA-TIME-3).
                let recent_since = now.checked_sub(RECENT_WINDOW);
                if recent_since.is_none_or(|since| finished_at(pause).unwrap_or(now) >= since) {
                    recent.push(pause);
                }
            }
        }
    }

    active.sort_by_key(|p| p.start_at);
    scheduled.sort_by_key(|p| p.start_at);
    recent.sort_by_key(|p| std::cmp::Reverse(finished_at(p).unwrap_or(now)));

    let views = |list: Vec<&Pause>| list.into_iter().map(|p| to_view(p, now)).collect();

    Groups {
        active: views(active),
        scheduled: views(scheduled),
        recent: views(recent),
    }
}

/// Rules that exist, by alarm type then rule id, to their workflow
/// (§10.2.7's "must name an existing rule").
pub type KnownRules = BTreeMap<String, BTreeMap<String, String>>;

/// A required text field, trimmed and bounded by `max`; `label` names it in the refusal.
fn text_field(payload: &Value, name: &str, label: &str, max: usize) -> Result<String, PauseError> {
    let Some(value) = payload.get(name).and_then(Value::as_str) else {
        return Err(PauseError::invalid(format!("{label} is required.")));
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(PauseError::invalid(format!("{label} is required.")));
    }
    if value.chars().count() > max {
        return Err(PauseError::invalid(format!(
            "{label} is too long (max {max} characters)."
        )));
    }
    Ok(value.to_string())
}

/// A flow id (§10.2.7): `^[A-Za-z0-9][A-Za-z0-9_-]{0,119}$`.
fn is_workflow_id(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && text.len() <= 120
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// What a pause is aimed at, once the request has been validated.
enum Targets {
    /// Everything (a scope of `all`).
    None,
    /// These flows.
    Flows(Vec<String>),
    /// These rules.
    Rules(Vec<RuleTarget>),
}

/// Check that the named flows and rules exist in the catalogue and match the scope.
fn validate_targets(
    scope: Scope,
    targets: Option<&Value>,
    known: &KnownRules,
) -> Result<Targets, PauseError> {
    if scope == Scope::All {
        return Ok(Targets::None);
    }
    let list = targets
        .and_then(Value::as_array)
        .filter(|list| !list.is_empty())
        .ok_or_else(|| PauseError::invalid("Select at least one flow or alarm to pause."))?;
    if list.len() > MAX_TARGETS {
        return Err(PauseError::invalid(format!(
            "Too many targets (max {MAX_TARGETS})."
        )));
    }

    if scope == Scope::Flows {
        let mut flows: Vec<String> = Vec::new();
        for target in list {
            let id = target
                .as_str()
                .map(str::trim)
                .filter(|id| is_workflow_id(id))
                .ok_or_else(|| PauseError::invalid("Invalid flow id."))?;
            if !flows.iter().any(|f| f == id) {
                flows.push(id.to_string());
            }
        }
        return Ok(Targets::Flows(flows));
    }

    let mut rules: Vec<RuleTarget> = Vec::new();
    for target in list {
        let alarm_type = target.get("alarm_type").and_then(Value::as_str);
        let rule_id = target.get("rule_id").and_then(Value::as_str);
        let (Some(alarm_type), Some(rule_id)) = (alarm_type, rule_id) else {
            return Err(PauseError::invalid("Invalid alarm rule target."));
        };
        if !ALARM_TYPES.contains(&alarm_type) {
            return Err(PauseError::invalid("Invalid alarm rule target."));
        }
        let Some(workflow_id) = known.get(alarm_type).and_then(|rules| rules.get(rule_id)) else {
            // The id is the caller's; it is echoed because the caller needs
            // to know which of several it got wrong, and it is not patient
            // data.
            return Err(PauseError::invalid(format!(
                "Unknown alarm rule: {rule_id}"
            )));
        };
        let entry = RuleTarget {
            alarm_type: alarm_type.to_string(),
            rule_id: rule_id.to_string(),
            workflow_id: workflow_id.clone(),
        };
        if !rules.contains(&entry) {
            rules.push(entry);
        }
    }
    Ok(Targets::Rules(rules))
}

/// Read a `datetime-local` value entered as UK time, as an instant
/// (§10.2.7).
///
/// # Errors
///
/// [`PauseError::Invalid`] for text that is not a date and time, and for a
/// wall-clock time that does not exist because the clocks went forward.
/// An ambiguous autumn time takes the first occurrence (BST).
pub fn parse_london(raw: &str, label: &str) -> Result<OffsetDateTime, PauseError> {
    let raw = raw.trim();

    // A value that already says what offset it is in needs no zone.
    if let Ok(at) = OffsetDateTime::parse(raw, &Rfc3339) {
        return Ok(at.to_offset(time::UtcOffset::UTC));
    }

    let naive = PrimitiveDateTime::parse(
        raw,
        &format_description!("[year]-[month]-[day]T[hour]:[minute]"),
    )
    .or_else(|_| {
        PrimitiveDateTime::parse(
            raw,
            &format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"),
        )
    })
    .map_err(|_| PauseError::invalid(format!("{label} is not a valid date and time.")))?;

    match naive.assume_timezone(london()) {
        OffsetResult::Some(at) => Ok(at.to_offset(time::UtcOffset::UTC)),
        OffsetResult::Ambiguous(a, b) => Ok(a.min(b).to_offset(time::UtcOffset::UTC)),
        OffsetResult::None => Err(PauseError::invalid(format!(
            "{label} does not exist in UK time (clocks go forward)."
        ))),
    }
}

/// The pause start: the given time, or `now` when none was given.
fn resolve_start(payload: &Value, now: OffsetDateTime) -> Result<OffsetDateTime, PauseError> {
    let raw = match payload.get("start") {
        None | Some(Value::Null) => return Ok(now),
        Some(Value::String(text)) => text.trim(),
        Some(_) => {
            return Err(PauseError::invalid(
                "Start time is not a valid date and time.",
            ));
        }
    };
    if raw.is_empty() || raw == "now" {
        return Ok(now);
    }

    let start = parse_london(raw, "Start time")?;
    // A bound that cannot be represented is one no start time passes
    // (RCA-TIME-3): "before the earliest date" and "after the latest" are
    // false for every `start`.
    if now
        .checked_sub(START_TOLERANCE)
        .is_some_and(|earliest| start < earliest)
    {
        return Err(PauseError::invalid("Start time is in the past."));
    }
    if now
        .checked_add(MAX_LEAD_TIME)
        .is_some_and(|latest| start > latest)
    {
        return Err(PauseError::invalid(
            "Start time must be within the next 365 days.",
        ));
    }
    Ok(start.max(now))
}

/// The pause end from an explicit time or a duration; refused when it is not after the start.
fn resolve_end(
    payload: &Value,
    start: OffsetDateTime,
) -> Result<Option<OffsetDateTime>, PauseError> {
    let mode = payload
        .get("end_mode")
        .and_then(Value::as_str)
        .unwrap_or("duration");

    match mode {
        "indefinite" => Ok(None),
        "until" => {
            let raw = payload
                .get("end")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let end = parse_london(raw, "End time")?;
            if end <= start {
                return Err(PauseError::invalid(
                    "End time must be after the start time.",
                ));
            }
            Ok(Some(end))
        }
        "duration" => {
            // Python's `int()` accepts a number or a numeric string.
            let minutes = match payload.get("duration_minutes") {
                Some(Value::Number(n)) => n.as_i64(),
                Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
                _ => Some(0),
            }
            .ok_or_else(|| PauseError::invalid("Duration must be a whole number of minutes."))?;
            if !(1..=MAX_DURATION_MINUTES).contains(&minutes) {
                return Err(PauseError::invalid(
                    "Duration must be between 1 minute and 365 days.",
                ));
            }
            start
                .checked_add(Duration::minutes(minutes))
                .map(Some)
                .ok_or_else(|| PauseError::invalid("End time is out of range."))
        }
        _ => Err(PauseError::invalid("Invalid end option.")),
    }
}

/// Validate a create request and make the pause (§10.2.7), without storing
/// it.
///
/// # Errors
///
/// [`PauseError::Invalid`], with a message safe to show, for the first
/// field that fails.
pub fn build(
    payload: &Value,
    known: &KnownRules,
    now: OffsetDateTime,
) -> Result<Pause, PauseError> {
    let scope = payload
        .get("scope_type")
        .and_then(Value::as_str)
        .and_then(Scope::parse)
        .ok_or_else(|| PauseError::invalid("Invalid pause scope."))?;

    let targets = validate_targets(scope, payload.get("targets"), known)?;
    let reason = text_field(payload, "reason", "Reason", MAX_REASON_LENGTH)?;
    let requested_by = text_field(
        payload,
        "requested_by",
        "Requested by",
        MAX_REQUESTED_BY_LENGTH,
    )?;
    let start = resolve_start(payload, now)?;
    let end = resolve_end(payload, start)?;

    let (flows, rules) = match targets {
        Targets::None => (Vec::new(), Vec::new()),
        Targets::Flows(flows) => (flows, Vec::new()),
        Targets::Rules(rules) => (Vec::new(), rules),
    };

    Ok(Pause {
        pause_id: uuid::Uuid::new_v4().to_string(),
        scope,
        flows,
        rules,
        start_at: Some(start),
        end_at: end,
        reason,
        requested_by,
        created_at: Some(now),
        cancelled_at: None,
    })
}

/// Every stored pause.
///
/// # Errors
///
/// [`StoreError`] — **not** an empty list (§10.1.4). The evaluator turns it
/// into "no pauses" itself (§10.2.6); a write path must not.
pub async fn list<S: AlarmStore>(store: &S) -> Result<Vec<Pause>, StoreError> {
    Ok(store
        .documents(PARTITION)
        .await?
        .iter()
        .filter_map(Pause::from_document)
        .collect())
}

/// A store failure as the refusal the caller sees; the cause is logged, not shown.
fn persistence(error: &StoreError) -> PauseError {
    tracing::error!(%error, "alarm pause persistence failed");
    PauseError::Persistence(UNAVAILABLE.to_string())
}

/// Delete ended or cancelled pauses that finished over 30 days ago
/// (§10.2.8). Best effort: a failure is logged and nothing else.
async fn purge_old<S: AlarmStore>(store: &S, pauses: &[Pause], now: OffsetDateTime) {
    for pause in pauses {
        if !matches!(
            status(pause, now),
            PauseStatus::Ended | PauseStatus::Cancelled
        ) {
            continue;
        }
        // No representable cutoff means nothing is older than it: keep it.
        if let Some(cutoff) = now.checked_sub(RETENTION)
            && finished_at(pause).is_some_and(|finished| finished < cutoff)
            && let Err(error) = store.delete_document(PARTITION, &pause.pause_id).await
        {
            tracing::warn!(%error, "could not purge an old alarm pause");
        }
    }
}

/// Validate, store and return a new pause (§10.2.7, §10.2.6).
///
/// # Errors
///
/// [`PauseError::Invalid`] for a bad request;
/// [`PauseError::Persistence`] when there is nowhere durable to keep it or
/// the write failed.
pub async fn create<S: AlarmStore>(
    store: &S,
    payload: &Value,
    known: &KnownRules,
    now: OffsetDateTime,
) -> Result<PauseView, PauseError> {
    let pause = build(payload, known, now)?;

    // §10.2.6 — accepted-and-dropped would page on-call at 02:00.
    if !store.is_enabled() {
        return Err(PauseError::Persistence(NOT_CONFIGURED.to_string()));
    }

    store
        .upsert_document(
            PARTITION,
            &pause.pause_id,
            ALARM_PAUSE,
            &pause.to_document(),
        )
        .await
        .map_err(|error| persistence(&error))?;

    // Only ids and scope: `reason` is free text typed by an operator and
    // stays out of the log.
    tracing::info!(
        pause_id = %pause.pause_id,
        scope = pause.scope.as_str(),
        indefinite = pause.end_at.is_none(),
        "alarm pause created"
    );

    if let Ok(all) = list(store).await {
        purge_old(store, &all, now).await;
    }

    Ok(to_view(&pause, now))
}

/// Cancel a scheduled pause, or end an active one now (§10.2.3).
///
/// # Errors
///
/// [`PauseError::NotFound`], [`PauseError::Invalid`] for one that has
/// already ended, [`PauseError::Persistence`] when the pauses could not be
/// read or written.
pub async fn cancel<S: AlarmStore>(
    store: &S,
    pause_id: &str,
    now: OffsetDateTime,
) -> Result<PauseView, PauseError> {
    if !store.is_enabled() {
        return Err(PauseError::Persistence(NOT_CONFIGURED.to_string()));
    }

    // Strict: a failed read is an error, never "no such pause".
    let pauses = list(store).await.map_err(|error| persistence(&error))?;
    let mut pause = pauses
        .iter()
        .find(|p| p.pause_id == pause_id)
        .cloned()
        .ok_or(PauseError::NotFound)?;

    match status(&pause, now) {
        PauseStatus::Scheduled => pause.cancelled_at = Some(now),
        PauseStatus::Active => {
            pause.cancelled_at = Some(now);
            pause.end_at = Some(now);
        }
        PauseStatus::Ended | PauseStatus::Cancelled => {
            return Err(PauseError::invalid("This pause has already ended."));
        }
    }

    store
        .upsert_document(
            PARTITION,
            &pause.pause_id,
            ALARM_PAUSE,
            &pause.to_document(),
        )
        .await
        .map_err(|error| persistence(&error))?;

    tracing::info!(pause_id = %pause.pause_id, "alarm pause cancelled");
    purge_old(store, &pauses, now).await;

    Ok(to_view(&pause, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::store::InMemory;
    use time::macros::datetime;

    /// A Wednesday in BST, 10:00 UTC = 11:00 UK.
    const NOW: OffsetDateTime = datetime!(2026-09-30 10:00 UTC);

    fn known() -> KnownRules {
        let mut rules = KnownRules::new();
        rules
            .entry("alarm1".to_string())
            .or_default()
            .insert("phw-to-mpi".to_string(), "phw-to-mpi".to_string());
        rules
            .entry("alarm3".to_string())
            .or_default()
            .insert("phw-failures".to_string(), "phw-to-mpi".to_string());
        rules
    }

    fn request(extra: Value) -> Value {
        let mut base = serde_json::json!({
            "scope_type": "all",
            "reason": "planned maintenance",
            "requested_by": "ops",
            "end_mode": "duration",
            "duration_minutes": 60,
        });
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
        base
    }

    fn pause(scope: Scope, start: OffsetDateTime, end: Option<OffsetDateTime>) -> Pause {
        Pause {
            pause_id: "p".to_string(),
            scope,
            flows: vec!["phw-to-mpi".to_string()],
            rules: vec![RuleTarget {
                alarm_type: "alarm1".to_string(),
                rule_id: "phw-to-mpi".to_string(),
                workflow_id: "phw-to-mpi".to_string(),
            }],
            start_at: Some(start),
            end_at: end,
            reason: "r".to_string(),
            requested_by: "o".to_string(),
            created_at: Some(start),
            cancelled_at: None,
        }
    }

    #[test]
    fn status_follows_the_clock() {
        // spec/15-dashboard.md §10.2.3.
        let later = NOW + Duration::hours(1);
        let earlier = NOW - Duration::hours(1);

        assert_eq!(
            status(&pause(Scope::All, later, None), NOW),
            PauseStatus::Scheduled
        );
        assert_eq!(
            status(&pause(Scope::All, earlier, None), NOW),
            PauseStatus::Active
        );
        assert_eq!(
            status(&pause(Scope::All, earlier, Some(later)), NOW),
            PauseStatus::Active
        );
        assert_eq!(
            status(&pause(Scope::All, earlier, Some(NOW)), NOW),
            PauseStatus::Ended,
            "a pause ending exactly now is over"
        );
    }

    #[test]
    fn a_pause_cancelled_before_it_began_is_cancelled_and_after_is_ended() {
        let mut scheduled = pause(Scope::All, NOW + Duration::hours(2), None);
        scheduled.cancelled_at = Some(NOW);
        assert_eq!(status(&scheduled, NOW), PauseStatus::Cancelled);

        let mut live = pause(Scope::All, NOW - Duration::hours(1), None);
        live.cancelled_at = Some(NOW);
        live.end_at = Some(NOW);
        assert_eq!(status(&live, NOW), PauseStatus::Ended);
        assert!(to_view(&live, NOW).cancelled_early);
    }

    #[test]
    fn a_pause_with_no_start_reads_as_ended() {
        let mut broken = pause(Scope::All, NOW, None);
        broken.start_at = None;

        assert_eq!(status(&broken, NOW), PauseStatus::Ended);
    }

    #[test]
    fn scope_decides_what_a_pause_covers() {
        // §10.2.4.
        let all = pause(Scope::All, NOW, None);
        assert!(covers(&all, "alarm2", "anything", ""));

        let flows = pause(Scope::Flows, NOW, None);
        assert!(covers(
            &flows,
            "alarm3",
            "new-rule-added-later",
            "phw-to-mpi"
        ));
        assert!(!covers(&flows, "alarm3", "r", "paris-to-mpi"));
        assert!(!covers(&flows, "alarm3", "r", ""), "no workflow, no match");

        let rule = pause(Scope::Rule, NOW, None);
        assert!(covers(&rule, "alarm1", "phw-to-mpi", ""));
        assert!(
            !covers(&rule, "alarm2", "phw-to-mpi", ""),
            "other alarm type"
        );
        assert!(!covers(&rule, "alarm1", "other", ""));
    }

    #[test]
    fn the_pause_that_ends_last_wins_and_indefinite_is_last_of_all() {
        // §10.2.5.
        let mut short = pause(
            Scope::All,
            NOW - Duration::hours(1),
            Some(NOW + Duration::hours(1)),
        );
        short.pause_id = "short".to_string();
        let mut long = pause(
            Scope::All,
            NOW - Duration::hours(1),
            Some(NOW + Duration::hours(9)),
        );
        long.pause_id = "long".to_string();
        let mut forever = pause(Scope::All, NOW - Duration::hours(1), None);
        forever.pause_id = "forever".to_string();

        let both = [short.clone(), long.clone()];
        assert_eq!(
            active_for(&both, "alarm1", "r", "w", NOW).map(|p| p.pause_id.as_str()),
            Some("long")
        );

        let all = [short, long, forever];
        assert_eq!(
            active_for(&all, "alarm1", "r", "w", NOW).map(|p| p.pause_id.as_str()),
            Some("forever")
        );
    }

    #[test]
    fn a_scheduled_pause_does_not_pause_anything_yet() {
        let later = [pause(Scope::All, NOW + Duration::hours(1), None)];

        assert!(active_for(&later, "alarm1", "r", "w", NOW).is_none());
        assert!(next_scheduled_for(&later, "alarm1", "r", "w", NOW).is_some());
    }

    #[test]
    fn a_document_round_trips() {
        // Only the targets of its own scope are stored (§10.2.2).
        let mut original = pause(Scope::Rule, NOW, Some(NOW + Duration::hours(2)));
        original.flows.clear();

        let back = Pause::from_document(&original.to_document()).unwrap();

        assert_eq!(back, original);
    }

    #[test]
    fn a_document_without_a_pause_id_is_skipped() {
        // §10.1.3 — `id` is stripped on read, so the pause's own field is
        // the only handle there is.
        assert!(Pause::from_document(&serde_json::json!({"scope_type": "all"})).is_none());
        assert!(
            Pause::from_document(&serde_json::json!({"pause_id": "", "scope_type": "all"}))
                .is_none()
        );
    }

    #[test]
    fn a_valid_all_pause_builds() {
        let built = build(&request(serde_json::json!({})), &known(), NOW).unwrap();

        assert_eq!(built.scope, Scope::All);
        assert_eq!(built.start_at, Some(NOW));
        assert_eq!(built.end_at, Some(NOW + Duration::hours(1)));
        assert!(built.flows.is_empty() && built.rules.is_empty());
    }

    #[test]
    fn the_rule_workflow_is_stored_with_the_target() {
        // §10.2.2 — so the Flows page can badge the flow.
        let built = build(
            &request(serde_json::json!({
                "scope_type": "rule",
                "targets": [{"alarm_type": "alarm3", "rule_id": "phw-failures"}, {"alarm_type": "alarm3", "rule_id": "phw-failures"}],
            })),
            &known(),
            NOW,
        )
        .unwrap();

        assert_eq!(built.rules.len(), 1, "duplicates collapse");
        assert_eq!(built.rules[0].workflow_id, "phw-to-mpi");
    }

    #[test]
    fn validation_refuses_what_section_10_2_7_says() {
        let refused = |extra: Value, fragment: &str| {
            let error = build(&request(extra), &known(), NOW).unwrap_err();
            assert!(matches!(error, PauseError::Invalid(_)), "{error:?}");
            assert!(error.message().contains(fragment), "{error}");
        };

        refused(serde_json::json!({"scope_type": "everything"}), "scope");
        refused(
            serde_json::json!({"scope_type": "flows", "targets": []}),
            "at least one",
        );
        refused(
            serde_json::json!({"scope_type": "flows", "targets": ["bad id!"]}),
            "Invalid flow id",
        );
        refused(
            serde_json::json!({"scope_type": "rule", "targets": [{"alarm_type": "alarm9", "rule_id": "x"}]}),
            "Invalid alarm rule",
        );
        refused(
            serde_json::json!({"scope_type": "rule", "targets": [{"alarm_type": "alarm1", "rule_id": "nope"}]}),
            "Unknown alarm rule",
        );
        refused(serde_json::json!({"reason": "  "}), "Reason is required");
        refused(serde_json::json!({"reason": "x".repeat(201)}), "too long");
        refused(
            serde_json::json!({"requested_by": "x".repeat(101)}),
            "too long",
        );
        refused(
            serde_json::json!({"duration_minutes": 0}),
            "between 1 minute",
        );
        refused(
            serde_json::json!({"duration_minutes": MAX_DURATION_MINUTES + 1}),
            "between 1 minute",
        );
        refused(
            serde_json::json!({"duration_minutes": "soon"}),
            "whole number",
        );
        refused(
            serde_json::json!({"end_mode": "never"}),
            "Invalid end option",
        );
    }

    #[test]
    fn validation_also_refuses_what_is_missing_or_malformed() {
        // §10.2.7: absent is not blank is not wrong-typed; each is refused
        // with its own reason and none is read as a default.
        let refused = |payload: Value, fragment: &str| {
            let error = build(&payload, &known(), NOW).unwrap_err();
            assert!(error.message().contains(fragment), "{error}");
            assert_eq!(error.to_string(), error.message());
        };
        let without = |name: &str| {
            let mut payload = request(serde_json::json!({}));
            payload.as_object_mut().unwrap().remove(name);
            payload
        };
        refused(without("reason"), "Reason is required");
        refused(without("requested_by"), "Requested by is required");
        refused(without("scope_type"), "Invalid pause scope");
        // No duration at all is a refusal, not a pause of zero minutes.
        refused(without("duration_minutes"), "between 1 minute");
        let many: Vec<Value> = (0..=MAX_TARGETS)
            .map(|i| format!("flow-{i}").into())
            .collect();
        refused(
            request(serde_json::json!({"scope_type": "flows", "targets": many})),
            "Too many targets",
        );
        refused(
            request(
                serde_json::json!({"scope_type": "rule", "targets": [{"alarm_type": "alarm1"}]}),
            ),
            "Invalid alarm rule",
        );
        refused(
            request(serde_json::json!({"start": 5})),
            "Start time is not a valid",
        );
        refused(
            request(serde_json::json!({"start": "soon"})),
            "Start time is not a valid",
        );
    }

    #[test]
    fn a_start_may_be_absent_null_now_or_carry_its_own_offset() {
        let start_of = |extra: Value| build(&request(extra), &known(), NOW).unwrap().start_at;
        let now = start_of(serde_json::json!({}));
        assert_eq!(start_of(serde_json::json!({"start": null})), now);
        assert_eq!(start_of(serde_json::json!({"start": " "})), now);
        assert_eq!(start_of(serde_json::json!({"start": "now"})), now);
        // An RFC 3339 value needs no zone and is kept as that instant.
        let at = start_of(serde_json::json!({"start": "2026-09-30T11:30:00+00:00"}));
        assert_eq!(at, Some(time::macros::datetime!(2026-09-30 11:30 UTC)));
        // A numeric string is a duration, as the Python's `int()` accepted.
        let pause = build(
            &request(serde_json::json!({"duration_minutes": " 30 "})),
            &known(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            pause.end_at.zip(pause.start_at).map(|(e, s)| e - s),
            Some(Duration::minutes(30))
        );
    }

    #[test]
    fn the_duration_limits_are_inclusive() {
        for minutes in [1, MAX_DURATION_MINUTES] {
            assert!(
                build(
                    &request(serde_json::json!({"duration_minutes": minutes})),
                    &known(),
                    NOW
                )
                .is_ok(),
                "{minutes}"
            );
        }
    }

    #[test]
    fn a_start_slightly_in_the_past_is_now_but_not_much_more() {
        // §10.2.7 — a form left open for a minute.
        let one_minute_ago = parse_london("2026-09-30T10:59", "x").unwrap();
        assert_eq!(one_minute_ago, datetime!(2026-09-30 09:59 UTC));

        let tolerated = build(
            &request(serde_json::json!({"start": "2026-09-30T10:59"})),
            &known(),
            NOW,
        )
        .unwrap();
        assert_eq!(tolerated.start_at, Some(NOW), "clamped up to now");

        let too_old = build(
            &request(serde_json::json!({"start": "2026-09-30T10:57"})),
            &known(),
            NOW,
        )
        .unwrap_err();
        assert!(too_old.message().contains("in the past"), "{too_old}");
    }

    #[test]
    fn a_start_more_than_a_year_ahead_is_refused() {
        let error = build(
            &request(serde_json::json!({"start": "2027-10-02T09:00"})),
            &known(),
            NOW,
        )
        .unwrap_err();

        assert!(error.message().contains("365 days"), "{error}");
    }

    #[test]
    fn until_must_be_after_the_start() {
        let error = build(
            &request(serde_json::json!({"end_mode": "until", "end": "2026-09-30T11:00"})),
            &known(),
            NOW,
        )
        .unwrap_err();
        assert!(error.message().contains("after the start"), "{error}");

        let ok = build(
            &request(serde_json::json!({"end_mode": "until", "end": "2026-09-30T13:00"})),
            &known(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            ok.end_at,
            Some(datetime!(2026-09-30 12:00 UTC)),
            "UK time is BST"
        );
    }

    #[test]
    fn indefinite_has_no_end() {
        let built = build(
            &request(serde_json::json!({"end_mode": "indefinite"})),
            &known(),
            NOW,
        )
        .unwrap();

        assert_eq!(built.end_at, None);
    }

    #[test]
    fn uk_time_handles_the_clock_changes() {
        // §10.2.7. 2026-03-29 01:30 does not exist; 2026-10-25 01:30 happens
        // twice and the first (BST, UTC+1) is taken.
        let gap = parse_london("2026-03-29T01:30", "Start time").unwrap_err();
        assert!(gap.message().contains("clocks go forward"), "{gap}");

        assert_eq!(
            parse_london("2026-10-25T01:30", "Start time").unwrap(),
            datetime!(2026-10-25 00:30 UTC)
        );

        // And an ordinary winter time is GMT.
        assert_eq!(
            parse_london("2026-12-01T09:00", "Start time").unwrap(),
            datetime!(2026-12-01 09:00 UTC)
        );
        assert!(parse_london("tomorrow", "Start time").is_err());
    }

    #[test]
    fn times_are_shown_in_uk_time() {
        assert_eq!(
            format_london(datetime!(2026-09-30 14:05 UTC)).as_deref(),
            Some("30 Sep 2026  15:05 BST")
        );
        assert_eq!(
            format_london(datetime!(2026-12-01 14:05 UTC)).as_deref(),
            Some("01 Dec 2026  14:05 GMT")
        );
    }

    #[test]
    fn the_page_groups_active_scheduled_and_recently_ended() {
        let active = pause(Scope::All, NOW - Duration::hours(1), None);
        let scheduled = pause(Scope::All, NOW + Duration::hours(1), None);
        let recent = pause(
            Scope::All,
            NOW - Duration::days(2),
            Some(NOW - Duration::days(1)),
        );
        let old = pause(
            Scope::All,
            NOW - Duration::days(20),
            Some(NOW - Duration::days(19)),
        );

        let groups = group_for_display(&[active, scheduled, recent, old], NOW);

        assert_eq!(groups.active.len(), 1);
        assert_eq!(groups.scheduled.len(), 1);
        assert_eq!(groups.recent.len(), 1, "only the last seven days");
        assert_eq!(
            summarise(&[pause(Scope::All, NOW - Duration::hours(1), None)], NOW),
            PauseSummary {
                active: 1,
                scheduled: 0
            }
        );
    }

    #[tokio::test]
    async fn a_pause_is_stored_typed_in_its_own_partition() {
        // §10.1.2, §10.2.1.
        let store = InMemory::new();

        let view = create(&store, &request(serde_json::json!({})), &known(), NOW)
            .await
            .unwrap();

        let stored = store.raw_document(PARTITION, &view.pause_id).unwrap();
        assert_eq!(stored["type"], "alarm_pause");
        assert_eq!(stored["pk"], "alarm-pause");
        assert_eq!(view.status, "active");
        assert_eq!(list(&store).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_pause_is_refused_when_nothing_is_persisted() {
        // §10.2.6 — accepted-and-dropped would page on-call.
        let error = create(
            &crate::alarm::store::Disabled,
            &request(serde_json::json!({})),
            &known(),
            NOW,
        )
        .await
        .unwrap_err();

        assert_eq!(error, PauseError::Persistence(NOT_CONFIGURED.to_string()));
    }

    #[tokio::test]
    async fn a_failed_write_is_an_error() {
        let store = InMemory::new();
        store.fail_writes("throttled");

        let error = create(&store, &request(serde_json::json!({})), &known(), NOW)
            .await
            .unwrap_err();

        assert!(matches!(error, PauseError::Persistence(_)));
        assert!(
            !error.message().contains("throttled"),
            "the reason stays in the log"
        );
    }

    #[tokio::test]
    async fn cancelling_a_pause_ends_an_active_one_and_cancels_a_scheduled_one() {
        let store = InMemory::new();
        let active = create(&store, &request(serde_json::json!({})), &known(), NOW)
            .await
            .unwrap();
        let scheduled = create(
            &store,
            &request(serde_json::json!({"start": "2026-10-02T09:00"})),
            &known(),
            NOW,
        )
        .await
        .unwrap();
        assert_eq!(scheduled.status, "scheduled");

        let later = NOW + Duration::minutes(5);
        let ended = cancel(&store, &active.pause_id, later).await.unwrap();
        assert_eq!(ended.status, "ended");
        assert!(ended.cancelled_early);

        let cancelled = cancel(&store, &scheduled.pause_id, later).await.unwrap();
        assert_eq!(cancelled.status, "cancelled");

        let again = cancel(&store, &active.pause_id, later).await.unwrap_err();
        assert!(again.message().contains("already ended"), "{again}");
    }

    #[tokio::test]
    async fn cancelling_an_unknown_pause_is_not_found() {
        let store = InMemory::new();

        assert_eq!(
            cancel(&store, "nope", NOW).await.unwrap_err(),
            PauseError::NotFound
        );
    }

    #[tokio::test]
    async fn cancel_on_a_store_that_cannot_be_read_is_unavailable_not_not_found() {
        // §10.2.6 — a failed read must never look like "no such pause", or an
        // outage would tell an operator their pause had never existed.
        let store = InMemory::new();
        store.fail("503");

        let error = cancel(&store, "p", NOW).await.unwrap_err();

        assert!(matches!(error, PauseError::Persistence(_)), "{error:?}");
    }

    #[tokio::test]
    async fn ended_pauses_past_retention_are_purged() {
        let store = InMemory::new();
        let old = pause(
            Scope::All,
            NOW - Duration::days(40),
            Some(NOW - Duration::days(39)),
        );
        let mut old = old;
        old.pause_id = "old".to_string();
        store
            .upsert_document(PARTITION, "old", ALARM_PAUSE, &old.to_document())
            .await
            .unwrap();

        create(&store, &request(serde_json::json!({})), &known(), NOW)
            .await
            .unwrap();

        let ids: Vec<String> = list(&store)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.pause_id)
            .collect();
        assert!(!ids.contains(&"old".to_string()), "{ids:?}");
        assert_eq!(ids.len(), 1);
    }
}
