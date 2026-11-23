//! The NHS Wales Integration Hub operations dashboard.
//!
//! Implements [`spec/15-dashboard.md`]. Flow health, queue depths,
//! exceptions, message throughput, and three alarm rules with e-mail
//! escalation.
//!
//! [`spec/15-dashboard.md`]: ../../../spec/15-dashboard.md
//!
//! # The guarantee everything else is arranged around
//!
//! §1.2: **it starts and serves every page with no Azure configuration at
//! all**, returning empty state rather than errors. It is the most
//! frequently exercised behaviour in the Python's tests, and it is what
//! makes the dashboard developable and testable on a laptop.
//!
//! That is why the shape of this crate is what it is. Azure access sits
//! behind traits with an empty implementation; configuration is almost
//! entirely optional; and no page's rendering can depend on a query having
//! succeeded.
//!
//! # Empty is not zero
//!
//! §2.2.2 and §3.3. A queue that could not be read is **Unknown**, never
//! Healthy and never zero. An operator has to be able to tell "nothing is
//! wrong" from "I cannot see". Every type here that can carry a count
//! carries an absent case, and it is not there for tidiness.

#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::assert_is_empty,
        clippy::cast_precision_loss,
        clippy::duration_suboptimal_units,
        clippy::unused_async_trait_impl,
        clippy::missing_docs_in_private_items,
        clippy::too_many_lines,
        clippy::needless_pass_by_value
    )
)]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the
// whole crate. Tests may use plain operators and indexing (RCA-EXC-5).
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

pub mod alarm;
pub mod azure;
pub mod cache;
pub mod config;
pub mod contacts;
pub mod counting;
pub mod csv;
pub mod flow_sources;
pub mod flows;
pub mod health;
pub mod kql;
pub mod language;
pub mod network_test;
pub mod observe;
pub mod pages;
pub mod period;
pub mod status;
pub mod topics;
pub mod web;
