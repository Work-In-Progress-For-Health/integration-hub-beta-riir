//! Reading a service's configuration out of the environment.
//!
//! Implements `spec/02-configuration.md` §1 and §10.
//!
//! Every Integration Hub service is configured by environment variables and
//! nothing else — no files, no defaults baked into an image, no service
//! discovery. This crate is the one place that turns a variable into a
//! value, so the rules that turn out to matter are stated once.
//!
//! Three of those rules are worth knowing before reading anything else.
//!
//! **Blank is absent.** A variable set to `""`, or to spaces, means the same
//! thing as an unset variable. Container definitions produce blank values
//! routinely — a templated value whose source was empty, an `.env` line with
//! nothing after the `=` — and treating those as "configured, to the empty
//! string" produces failures a long way from their cause. The Python does
//! this in most places; here it is uniform, including for integers, where
//! the Python would have called `int("")` and crashed.
//!
//! **Every problem is reported at once.** [`Reader`] accumulates errors
//! rather than returning the first, so a service with four missing variables
//! says so once instead of over four restarts.
//!
//! **A secret never prints.** [`Secret`] holds one and renders as
//! `<redacted>` through both [`Debug`] and [`std::fmt::Display`], because a
//! configuration struct ends up in a log line eventually and a connection
//! string with a shared access key in it must not be what is there.
//!
//! # Examples
//!
//! ```
//! use ihb_config::Reader;
//!
//! let mut reader = Reader::from_pairs([
//!     ("WORKFLOW_ID", "phw-to-mpi"),
//!     ("PORT", "2575"),
//!     ("HL7_VERSION", "   "), // blank: the same as absent
//! ]);
//!
//! let workflow: String = reader.required_string("WORKFLOW_ID");
//! let port: u16 = reader.integer_or("PORT", 2575);
//! let version: Option<String> = reader.optional_string("HL7_VERSION");
//!
//! reader.finish().expect("the configuration is complete");
//! assert_eq!(workflow, "phw-to-mpi");
//! assert_eq!(port, 2575);
//! assert_eq!(version, None);
//! ```

#![forbid(unsafe_code)]
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod error;
mod reader;
mod secret;
mod value;

pub use error::{ConfigError, ConfigErrors};
pub use reader::Reader;
pub use secret::Secret;
pub use value::{parse_bool, parse_csv, present};
