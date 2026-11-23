//! Resource identity: UUIDs and `urn:uuid:` references.
//!
//! Implements `spec/16-proms-transformer.md` §4.
//!
//! # Why the factory is injectable
//!
//! §4.4. A bundle is seven fresh UUIDs, so without an injectable factory a
//! test can assert almost nothing about one: every reference differs on
//! every run. The Python threads a `UuidFactory` for exactly this reason
//! and this does the same.
//!
//! # Allocation order is not entry order
//!
//! Every slot in a bundle's shape gets a UUID **up front**, in the
//! canonical order of §3, and a resource that turns out to be absent
//! simply is not emitted (§3.2). So the numbering has gaps: in a minimal
//! referral the `Location` is the seventh UUID allocated and the fifth
//! entry present.
//!
//! That is measurable in the Python's own output, and it is the reason
//! this module hands out identity before anything is built.

use std::cell::Cell;

/// A source of fresh resource identifiers (§4.1, §4.4).
///
/// A trait so a golden test can supply [`SequentialUuids`] and compare the
/// whole bundle byte for byte; production uses [`RandomUuids`]. Takes `&self`
/// (interior mutability for the sequential case), so one factory is shared by
/// reference across the builders.
pub trait UuidFactory {
    /// A new UUID, as its lowercase hyphenated text form (36 characters).
    /// Every call returns a value not returned before by this factory.
    fn next(&self) -> String;
}

/// Real UUIDs, for production: version 4, from the operating system's RNG.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomUuids;

impl UuidFactory for RandomUuids {
    fn next(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Predictable UUIDs, for tests and goldens.
///
/// Produces `00000000-0000-4000-8000-{n:012}` from 1, which is the shape
/// the captured Python goldens use — a valid version-4 UUID layout, so a
/// consumer validating the form still accepts it.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::identity::{SequentialUuids, UuidFactory};
///
/// let uuids = SequentialUuids::new();
///
/// assert_eq!(uuids.next(), "00000000-0000-4000-8000-000000000001");
/// assert_eq!(uuids.next(), "00000000-0000-4000-8000-000000000002");
/// ```
#[derive(Debug, Default)]
pub struct SequentialUuids {
    /// The next number to hand out.
    next: Cell<u64>,
}

impl SequentialUuids {
    /// A factory starting at one.
    #[must_use]
    pub fn new() -> Self {
        Self { next: Cell::new(1) }
    }

    /// How many have been handed out; lets a test assert the slot count
    /// (including slots that ended up unfilled).
    #[must_use]
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "RCA-EXC-2: `next` starts at 1 and only grows, so `next - 1` cannot underflow"
    )]
    pub fn issued(&self) -> u64 {
        self.next.get() - 1
    }
}

impl UuidFactory for SequentialUuids {
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "RCA-EXC-2: a test double that counts its own calls; 2^64 of them are unreachable"
    )]
    fn next(&self) -> String {
        let n = self.next.get();
        self.next.set(n + 1);
        format!("00000000-0000-4000-8000-{n:012}")
    }
}

/// An entry's `fullUrl`, and the form every cross-reference uses (§4.2, §4.3).
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::identity::urn_uuid;
///
/// assert_eq!(
///     urn_uuid("00000000-0000-4000-8000-000000000001"),
///     "urn:uuid:00000000-0000-4000-8000-000000000001"
/// );
/// ```
#[must_use]
pub fn urn_uuid(resource_uuid: &str) -> String {
    format!("urn:uuid:{resource_uuid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_uuids_start_at_one_and_count_up() {
        // spec/16-proms-transformer.md §4.4 — this is what makes a whole
        // bundle assertable.
        let uuids = SequentialUuids::new();

        assert_eq!(uuids.next(), "00000000-0000-4000-8000-000000000001");
        assert_eq!(uuids.next(), "00000000-0000-4000-8000-000000000002");
        assert_eq!(uuids.next(), "00000000-0000-4000-8000-000000000003");
        assert_eq!(uuids.issued(), 3);
    }

    #[test]
    fn a_sequential_uuid_is_shaped_like_a_version_four_one() {
        // The goldens use this form, and a consumer validating the layout
        // should still accept it: version nibble 4, variant nibble 8.
        let uuids = SequentialUuids::new();
        let first = uuids.next();

        let parsed = uuid::Uuid::parse_str(&first).expect("a valid UUID");
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(first.len(), 36);
        assert_eq!(first.matches('-').count(), 4);
    }

    #[test]
    fn random_uuids_are_fresh_every_time() {
        // §4.1 — every resource gets its own.
        let uuids = RandomUuids;

        let a = uuids.next();
        let b = uuids.next();

        assert_ne!(a, b);
        assert_eq!(uuid::Uuid::parse_str(&a).unwrap().get_version_num(), 4);
    }

    #[test]
    fn a_reference_is_the_urn_form() {
        // §4.2 and §4.3 — the same form for an entry's `fullUrl` and for
        // every cross-reference, which is what makes a bundle
        // self-contained.
        assert_eq!(urn_uuid("abc"), "urn:uuid:abc");
    }

    #[test]
    fn the_factory_is_usable_behind_a_reference() {
        // The builders take `&dyn UuidFactory`, so both implementations
        // have to work through one.
        fn take(factory: &dyn UuidFactory) -> String {
            factory.next()
        }

        assert_eq!(
            take(&SequentialUuids::new()),
            "00000000-0000-4000-8000-000000000001"
        );
        assert!(!take(&RandomUuids).is_empty());
    }
}
