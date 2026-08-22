//! Talking to Azure.
//!
//! Implements `spec/15-dashboard.md` §4.
//!
//! Four pieces, in the order a request goes through them:
//!
//! - [`http`] — HTTPS with a deadline (§4.6, §4.7).
//! - [`token`] — an AAD token, cached (§4.1, §4.6.1–§4.6.4).
//! - [`arm`] — Service Bus depths and Container Apps state (§4.6).
//! - [`logs`] — Log Analytics queries (§4.3, §4.5).
//! - [`cosmos`] — signing alarm-store requests (§5.7).
//! - [`email`] — escalation e-mail through ACS (§5.8).
//! - [`live`] — the [`Observatory`] that uses them, and the one place a
//!   failure becomes an empty answer and a log line (§4.2).
//!
//! # Nothing here decides what a page shows
//!
//! §4.2: every integration degrades to empty. These modules return
//! `Result`, and the [`Observatory`] implementation that uses them is where
//! a failure becomes an empty answer and a log line. Keeping the two apart
//! means the reason a panel is blank is still available to be logged, which
//! is the only way an operator finds out.
//!
//! [`Observatory`]: crate::observe::Observatory

pub mod arm;
pub mod cosmos;
pub mod email;
pub mod http;
pub mod live;
pub mod logs;
pub mod token;
