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
//! The LIMS WRRS classifier.
//!
//! Implements `spec/09-transformers.md` §10. Upstream:
//! `transformers/hl7_lims_wrrs_transformer` (PRs `#418`, `#429`, `#431`).
//!
//! It does not transform. An inbound LIMS `A31`/`A40` is published
//! **unchanged** once per rule it satisfies, so downstream subscriptions
//! (filtering on the `MappingType` property) can apply the right WRRS
//! mapping. That is one message in and N out, which the shared
//! `ihb-transformer` runtime (`spec/09` §1) deliberately cannot express, so
//! this crate has its own loop and borrows only that crate's configuration.
//!
//! | Module | Owns |
//! | --- | --- |
//! | [`mapping`] | the two `MappingType` values (`§10.7`) |
//! | [`classifier`] | the rules (`§10.2`, `§10.3`) |
//! | [`processor`] | one message to N sends or a dead letter (`§10.4` to `§10.6`) |
//! | [`main_loop`] | assembling and running the service |

pub mod classifier;
pub mod main_loop;
pub mod mapping;
pub mod processor;

pub use classifier::{classify, trigger_event};
pub use mapping::MappingTypes;
pub use processor::{Disposition, process_batch, process_one};
