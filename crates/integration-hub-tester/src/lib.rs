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
        clippy::unreachable,
        clippy::float_arithmetic,
        clippy::float_cmp,
        clippy::lossy_float_literal
    )
)]
//! Integration Hub tester: one message through one component, no Service Bus.
//!
//! Implements `spec/integration-hub-tester-component-preview.md`.
//!
//! A developer tool (§1.1): it is in no production image and has no
//! pipeline. It installs no `tracing` subscriber (§1.3), so the components
//! it drives log nowhere, and it keeps no copy of what it is given (§1.2).
//!
//! Each [`plugin::Plugin`] wraps one component's own library function
//! (§3.1). [`registry::categories`] is the list, [`cli::run`] is the command
//! line, and [`mock`] launches a mock receiver for the SOAP senders.

pub mod cli;
pub mod mock;
pub mod plugin;
pub mod plugins;
pub mod registry;
