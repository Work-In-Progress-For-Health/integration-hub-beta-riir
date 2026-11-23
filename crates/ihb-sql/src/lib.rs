#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]
//! Azure SQL access, shared by the message store and the replay job.
//!
//! Implements `spec/12-message-store.md` §5.
//!
//! # Three rules that shape everything here
//!
//! **One lazily-opened, long-lived connection** (§5.1). Opening per batch
//! against Azure SQL is a login round trip each time, and at a hundred
//! batches a minute that is the dominant cost.
//!
//! **Discard on error** (§5.2). A connection that has seen a database error
//! may be in an unknown transaction state, so it is closed rather than
//! reused, and the next call reconnects. That also refreshes an AAD token
//! (§5.4.2) without any expiry tracking.
//!
//! **No credential in any log line, at any level** (§5.7). Not the
//! password, not the username, not the connection string. There is a test
//! that captures log output and asserts it.

pub mod chunk;
pub mod config;
pub mod connection;
pub mod error;

pub use chunk::{MAX_PARAMETERS, PARAMETERS_PER_ROW, ROWS_PER_STATEMENT, chunks};
pub use config::{Auth, SqlConfig};
pub use connection::SqlConnection;
pub use error::SqlError;
