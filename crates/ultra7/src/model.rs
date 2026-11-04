//! The project data model and its JSON file format.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§3.1` and `§3.2`. The
//! field names and defaults are the Python's, so one project file opens in
//! both tools.

use serde::{Deserialize, Serialize};

/// The default `timestamp_format` of an iteration spec (`§3.2`).
pub const DEFAULT_TIMESTAMP_FORMAT: &str = "%Y%m%d%H%M%S";

/// What a message body is (`§5.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFormat {
    /// HL7 v2.x ER7.
    Hl7,
    /// XML.
    Xml,
    /// JSON.
    Json,
}

/// The protocol of an endpoint (`§7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointKind {
    /// Plain TCP with MLLP framing.
    #[default]
    Mllp,
    /// HTTP POST of the raw body.
    Rest,
    /// HTTP POST of a SOAP 1.1 envelope.
    Soap,
}

/// How an iteration field changes per send (`§4.2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IterationMode {
    /// Integer plus `step * index`.
    #[default]
    Increment,
    /// Cycle through `values`.
    List,
    /// The current time.
    Timestamp,
}

/// Serde default: `step` 1 (`§3.2`).
const fn one() -> i64 {
    1
}
/// Serde default: `%Y%m%d%H%M%S` (`§3.2`).
fn default_timestamp_format() -> String {
    DEFAULT_TIMESTAMP_FORMAT.to_owned()
}
/// Serde default: `enabled` true (`§3.2`).
const fn yes() -> bool {
    true
}
/// Serde default: 5 s timeout (`§3.2`).
const fn five() -> f64 {
    5.0
}
/// Serde default: repeat count 1.
const fn one_u32() -> u32 {
    1
}
/// Eight hex characters of a v4 UUID (`§3.2`). Short on purpose, to match
/// the Python's ids; it only needs to be unique within one project.
fn new_id() -> String {
    let mut id = uuid::Uuid::new_v4().simple().to_string();
    id.truncate(8);
    id
}

/// A character range of a message that changes on each repeat send (`§4.1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IterationSpec {
    /// First character offset, inclusive.
    pub start: usize,
    /// Last character offset, exclusive.
    pub end: usize,
    /// What to do to the range.
    #[serde(default)]
    pub mode: IterationMode,
    /// Increment mode: added per send.
    #[serde(default = "one")]
    pub step: i64,
    /// Increment mode: zero-pad to this width; 0 keeps the original width.
    #[serde(default)]
    pub pad_width: usize,
    /// List mode: values cycled in order.
    #[serde(default)]
    pub values: Vec<String>,
    /// Timestamp mode: `strftime` subset of `§4.3`.
    #[serde(default = "default_timestamp_format")]
    pub timestamp_format: String,
}

impl IterationSpec {
    /// A spec over `start..end` with every other field at its default.
    #[must_use]
    pub fn new(start: usize, end: usize, mode: IterationMode) -> Self {
        Self {
            start,
            end,
            mode,
            step: 1,
            pad_width: 0,
            values: Vec::new(),
            timestamp_format: default_timestamp_format(),
        }
    }
}

/// One test message (`§3.1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Eight hex characters; generated when absent.
    #[serde(default = "new_id")]
    pub id: String,
    /// Display name; also the export file stem (`§6.2`).
    pub name: String,
    /// Body format.
    pub format: MessageFormat,
    /// The body. Synthetic data only (`§1.2`).
    pub content: String,
    /// The changing field, if any.
    #[serde(default)]
    pub iteration: Option<IterationSpec>,
    /// Whether a batch send includes it.
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Message {
    /// A new enabled message with a fresh id and no iteration field.
    #[must_use]
    pub fn new(name: &str, format: MessageFormat, content: &str) -> Self {
        Self {
            id: new_id(),
            name: name.to_owned(),
            format,
            content: content.to_owned(),
            iteration: None,
            enabled: true,
        }
    }
}

/// The target being tested (`§3.2`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Protocol.
    #[serde(default)]
    pub kind: EndpointKind,
    /// MLLP host.
    #[serde(default)]
    pub host: String,
    /// MLLP port.
    #[serde(default)]
    pub port: Option<u16>,
    /// REST or SOAP URL.
    #[serde(default)]
    pub url: String,
    /// Extra request headers (REST, SOAP).
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// `SOAPAction` header value.
    #[serde(default)]
    pub soap_action: String,
    /// Connect, read and write timeout in seconds. Zero, negative or
    /// non-finite values are replaced by 5 s at send time rather than
    /// rejected on load.
    #[serde(default = "five")]
    pub timeout_seconds: f64,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            kind: EndpointKind::Mllp,
            host: String::new(),
            port: None,
            url: String::new(),
            headers: std::collections::BTreeMap::new(),
            soap_action: String::new(),
            timeout_seconds: 5.0,
        }
    }
}

/// A named set of messages and an endpoint (`§3.1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Project name; the file stem after `§3.3`.
    pub name: String,
    /// Where to send.
    #[serde(default)]
    pub endpoint: Endpoint,
    /// Messages, in send order.
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Batch repeat count (clamped to at least 1, `§8.1`).
    #[serde(default = "one_u32")]
    pub repeat_count: u32,
    /// Pause after each send (`§8.4`).
    #[serde(default)]
    pub delay_ms: u64,
}

impl Project {
    /// An empty project with defaults.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            endpoint: Endpoint::default(),
            messages: Vec::new(),
            repeat_count: 1,
            delay_ms: 0,
        }
    }
}
