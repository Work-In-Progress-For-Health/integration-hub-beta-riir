//! What the two-engine XSD validation costs.
//!
//! Implements `spec/06-validation.md` §9.5.
//!
//! Not a benchmark — a guard rail. Compiling a schema is expensive and
//! validating against a compiled one is not, and the difference decides
//! whether this can sit on an ingress path at all. These tests measure both
//! and print them, so the numbers in §9.5 can be checked rather than
//! believed.

// An integration test is its own crate, so the crate-level allow in
// `src/lib.rs` does not reach it. Asserting is what this file is for.
// See spec/17-testing-and-quality.md §2.3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use ihb_validation::{Flow, xsd};

/// A PHW message that conforms.
const PHW: &str = concat!(
    "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r",
    "EVN||20250502092900\r",
    "PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r",
    "PV1||U"
);

fn document() -> String {
    ihb_validation::er7_to_xml_for_flow(PHW, Flow::Phw, None).expect("the fixture converts")
}

#[test]
fn the_first_validation_pays_for_compilation_and_the_rest_do_not() {
    // spec/06-validation.md §9.5. The first call compiles both schemas; the
    // rest reuse them. If that ever stopped being true the ingress would
    // slow by two orders of magnitude and this is what would say so.
    let xml = document();

    let started = Instant::now();
    xsd::validate(&xml, Flow::Phw, "ADT_A05").unwrap();
    let cold = started.elapsed();

    let started = Instant::now();
    for _ in 0..20 {
        xsd::validate(&xml, Flow::Phw, "ADT_A05").unwrap();
    }
    let warm = started.elapsed() / 20;

    println!("  cold (compile + validate): {cold:?}");
    println!("  warm (validate only):      {warm:?}");

    assert!(
        warm * 10 < cold,
        "compilation does not appear to be cached: cold {cold:?}, warm {warm:?}"
    );
    assert!(
        warm < std::time::Duration::from_millis(50),
        "a warm validation costs {warm:?}, which is too much for an ingress path"
    );
}

#[test]
fn compiling_every_schema_under_both_engines_is_bounded() {
    // spec/06-validation.md §9.5. Thirteen schemas, two engines, and the
    // caches are per-thread — so this is what one worker thread pays once,
    // and it has to be small enough to sit inside a container's startup.
    let started = Instant::now();

    for flow in Flow::ALL {
        for structure in flow.structures() {
            // A trivially wrong document: the point is the compile, and the
            // verdict is irrelevant.
            let _ = xsd::validate("<nothing/>", flow, &structure);
        }
    }

    let elapsed = started.elapsed();
    println!("  all 13 schemas, both engines: {elapsed:?}");

    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "compiling every schema took {elapsed:?}"
    );
}

#[test]
fn each_engine_is_measured_separately() {
    // spec/06-validation.md §9.5. The two are not comparable in cost, and
    // which one is slow decides what this module can be used for.
    let xml = document();

    for engine in [xsd::Engine::XsdSchema, xsd::Engine::Uppsala] {
        // Warm the cache first, so compilation is not in the measurement.
        let _ = xsd::validate_with(&xml, Flow::Phw, "ADT_A05", engine).unwrap();

        let started = Instant::now();
        for _ in 0..20 {
            xsd::validate_with(&xml, Flow::Phw, "ADT_A05", engine).unwrap();
        }
        println!("  {engine:>12}: {:?} per document", started.elapsed() / 20);
    }
}
