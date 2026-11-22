//! The Integration Hub reference-data lookup service.
//!
//! Implements `spec/20-lookup-service.md`.
//!
//! **Phase 1: not production ready** (§1.1). It serves reference tables to
//! the transformers, read-only from seed files, and in a development
//! environment accepts CSV uploads. Authentication is required (§12): an
//! Entra bearer token for the lookup API, Container Apps Easy Auth headers
//! for the GUI and admin API, or an explicit, acknowledged switch-off for
//! `LOCAL` and the `DEV`/`TST` trial (§2.8).
//!
//! # Examples
//!
//! A key is normalised the same way when a table is loaded and when it is
//! looked up (§4.1):
//!
//! ```
//! use std::collections::BTreeMap;
//! use lookup_service::keys::canonical_key;
//! use lookup_service::model::{Case, KeyPartRule, TableDefinition};
//!
//! let definition = TableDefinition {
//!     name: "ward_map".into(),
//!     description: String::new(),
//!     key_columns: vec!["facility".into(), "ward".into()],
//!     value_columns: vec!["code".into()],
//!     default_value_column: "code".into(),
//!     key_normalisation: BTreeMap::from([(
//!         "ward".to_owned(),
//!         KeyPartRule { trim: true, case: Case::Upper },
//!     )]),
//!     ttl_seconds: None,
//!     preload: false,
//! };
//! let key = canonical_key(
//!     &definition,
//!     &["FAC1".to_owned(), " w2 ".to_owned()],
//!     &BTreeMap::new(),
//! )
//! .unwrap();
//! assert_eq!(key, ["FAC1", "W2"]);
//! ```

// `significant_drop_tightening` is a nursery lint that fires on a lock guard
// held to the end of a function a few lines long, which is every use of a
// guard in this crate: each critical section is the whole body by design,
// and shortening one would split an update that must be atomic.
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
#![allow(clippy::significant_drop_tightening)]
// `option_if_let_else` would rewrite `if let Some(..) .. else ..` blocks that
// build large structs into `map_or_else` closures, which reads worse where
// the `else` arm is the longer one.
#![allow(clippy::option_if_let_else)]

pub mod admin;
pub mod cache;
pub mod config;
pub mod error;
pub mod file_store;
pub mod form;
pub mod import;
pub mod jwks;
pub mod keys;
pub mod mapping;
pub mod memory_store;
pub mod model;
pub mod pages;
pub mod resolver;
pub mod security;
pub mod seed;
pub mod store;
pub mod ttl;
pub mod web;
