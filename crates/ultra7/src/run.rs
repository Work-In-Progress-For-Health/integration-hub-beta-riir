//! The repeat-send loop.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§8`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::iteration::apply_iteration;
use crate::model::{Endpoint, EndpointKind, Message, Project};
use crate::sender::{SendResult, send, url_host_port};

/// Something the loop reports while it runs. Borrowed so a front end can
/// print without cloning the (possibly large) result.
#[derive(Debug)]
pub enum Event<'a> {
    /// The announce line, sent before the send (`§8.2`).
    Info(String),
    /// A finished send and its label.
    Result(&'a str, &'a SendResult),
}

/// Totals for a run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Sends attempted.
    pub sent: usize,
    /// Of which failed.
    pub failed: usize,
    /// Whether the run stopped early on the cancel flag (`§8.4`).
    pub cancelled: bool,
}

/// One more than `count`, holding at `usize::MAX` (a tally for the summary;
/// a run that long does not finish).
const fn tally(count: usize) -> usize {
    match count.checked_add(1) {
        Some(next) => next,
        None => usize::MAX,
    }
}

/// `<host> on port <port>` for MLLP; the URL's host, with the port only when
/// the URL names one, otherwise (`§8.2`).
///
/// An MLLP endpoint with no port prints `None`, which is the Python's
/// rendering of an unset port, kept so logs from the two tools read alike.
#[must_use]
pub fn describe_destination(endpoint: &Endpoint) -> String {
    if endpoint.kind == EndpointKind::Mllp {
        let port = endpoint
            .port
            .map_or_else(|| "None".to_owned(), |p| p.to_string());
        return format!("{} on port {port}", endpoint.host);
    }
    match url_host_port(&endpoint.url) {
        (host, Some(port)) => format!("{host} on port {port}"),
        (host, None) => host,
    }
}

/// Send `messages` `repeat_count` times each in order (`§8.1`, `§8.2`),
/// through `sender`, reporting to `sink`.
///
/// `repeat_count` is clamped to at least 1. The delay is slept after every
/// send when non-zero, and the cancel flag is checked before each send.
///
/// `sender` is injected so tests can run the loop without a socket. The
/// iteration index is the *repeat* number, not a running count over
/// messages, so every message in one pass sees the same index (`§8.2`). A
/// failed send does not stop the run; it is counted in [`Summary::failed`].
/// The cancel flag uses `Relaxed` ordering because it carries no data
/// other than itself.
pub fn send_messages(
    endpoint: &Endpoint,
    messages: &[&Message],
    repeat_count: u32,
    delay_ms: u64,
    cancel: &AtomicBool,
    sender: &dyn Fn(&Endpoint, &Message) -> SendResult,
    sink: &mut dyn FnMut(Event<'_>),
) -> Summary {
    let repeat_count = repeat_count.max(1);
    let destination = describe_destination(endpoint);
    let mut summary = Summary::default();
    for (repeat, ordinal) in (0..repeat_count).zip(1..=repeat_count) {
        for message in messages {
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                return summary;
            }
            let mut outgoing = (*message).clone();
            if let Some(spec) = &message.iteration {
                let Ok(index) = usize::try_from(repeat) else {
                    summary.cancelled = true;
                    return summary;
                };
                outgoing.content = apply_iteration(&message.content, spec, index);
            }
            let label = format!("{} [{}/{}]", message.name, ordinal, repeat_count);
            sink(Event::Info(format!("Sending {label} to {destination}")));
            let result = sender(endpoint, &outgoing);
            sink(Event::Result(&label, &result));
            summary.sent = tally(summary.sent);
            if !result.ok {
                summary.failed = tally(summary.failed);
            }
            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
        }
    }
    summary
}

/// Batch send of a project's enabled messages (`§8.1`).
///
/// Disabled messages are skipped here but reachable by name through
/// [`send_selected_once`] (`§8.3`).
///
/// # Errors
/// `No enabled messages to send` when there is nothing to do.
pub fn send_enabled(
    project: &Project,
    repeat_count: u32,
    delay_ms: u64,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Event<'_>),
) -> Result<Summary, String> {
    let messages: Vec<&Message> = project.messages.iter().filter(|m| m.enabled).collect();
    if messages.is_empty() {
        return Err("No enabled messages to send".to_owned());
    }
    Ok(send_messages(
        &project.endpoint,
        &messages,
        repeat_count,
        delay_ms,
        cancel,
        &send,
        sink,
    ))
}

/// Send the named messages once each, at index 0, ignoring repeat, delay and
/// the enabled flag (`§8.3`).
///
/// # Errors
/// `No message selected` for an empty list; an unknown name.
pub fn send_selected_once(
    project: &Project,
    names: &[String],
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(Event<'_>),
) -> Result<Summary, String> {
    if names.is_empty() {
        return Err("No message selected".to_owned());
    }
    let mut messages = Vec::new();
    for name in names {
        let found = project
            .messages
            .iter()
            .find(|m| &m.name == name)
            .ok_or_else(|| format!("No message named '{name}' in this project"))?;
        messages.push(found);
    }
    Ok(send_messages(
        &project.endpoint,
        &messages,
        1,
        0,
        cancel,
        &send,
        sink,
    ))
}
