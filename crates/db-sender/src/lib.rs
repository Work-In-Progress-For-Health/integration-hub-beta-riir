#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! The generic database sender.
//!
//! Implements `spec/10-senders.md` §7. Upstream: `senders/db_sender`
//! (PRs `#422`, `#433`; the local service is `wrrs-stmsg-dbsender`).
//!
//! One topic subscription in, one stored-procedure call out. The first
//! deployment is RISP to WRRS (`dbo.prQueueMessage` in `DRS`); the next
//! database flow is another environment file, not another crate.
//!
//! # The rules that govern it
//!
//! - **A message is completed only after the procedure committed** (`§7.1`).
//! - **Only the MSH is ever logged** (`§7.5`), and **server messages are
//!   sanitised** before they are logged or dead-lettered (`§7.4`): SQL Server
//!   echoes parameter values into some of them.
//!
//! | Module | Owns |
//! | --- | --- |
//! | [`config`] | the environment (`§7.6`) |
//! | [`msh`] | MSH-only extraction for logging (`§7.5`) |
//! | [`sanitize`] | redacting echoed values (`§7.4`) |
//! | [`db`] | the connection, the call and failure classes (`§7.3`) |
//! | [`ids`] | reading `PID-3` and `MRG-1` identifiers (`§7.8`) |
//! | [`idupdate`] | choosing the parameters of `dbo.prQueueIdUpdate` (`§7.8`) |
//! | [`trrr`] | the TRRR mappings: association and merge (`§7.10`) |
//! | [`processor`] | one message, start to disposition (`§7.2`) |
//! | [`main_loop`] | assembling and running the service |

pub mod config;
pub mod db;
pub mod ids;
pub mod idupdate;
pub mod main_loop;
pub mod msh;
pub mod processor;
pub mod sanitize;
pub mod trrr;

pub use config::{AppConfig, ProcedureType};
pub use db::{DatabaseError, FailureKind, ProcedureRunner};
pub use processor::{Disposition, process_batch, process_one};
