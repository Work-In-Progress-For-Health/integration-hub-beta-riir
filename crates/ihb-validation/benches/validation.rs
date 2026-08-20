//! What validating one message costs.
//!
//! Implements `spec/17-testing-and-quality.md` §11 and
//! `spec/06-validation.md` §9.5.
//!
//! Every message that enters the Hub is validated, and an ingress server is
//! expected to keep up with a hospital. These benchmarks exist to answer one
//! question with numbers rather than opinion: **which validation route can
//! sit on that path?**
//!
//! Three routes are measured against the same message:
//!
//! - the **dictionary** route, which is what runs today (§3.2);
//! - **`xsd-schema`**, real XSD validation, the primary engine of §9;
//! - **`uppsala`**, the cross-check.
//!
//! Run with `cargo bench -p ihb-validation`.

// `criterion_group!`/`criterion_main!` expand to items this crate does not
// write and cannot document. A benchmark is not shipped code.
// See spec/17-testing-and-quality.md §2.3.
#![allow(
    clippy::missing_docs_in_private_items,
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use criterion::{Criterion, criterion_group, criterion_main};
use ihb_validation::{Flow, xsd};
use std::hint::black_box;

/// A PHW message that conforms — the shape the flow actually carries.
const PHW: &str = concat!(
    "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r",
    "EVN||20250502092900\r",
    "PID|||8888888^^^252^PI~4444444444^^^NHS^NH||SURNAME^FORENAME^M||19990101|M|||",
    "99 ROAD^PLACE^CITY^COUNTY^SA99 1XX||555-1111~555-2222\r",
    "PV1||U"
);

/// The same message as the v2.xml the XSD engines validate.
fn document() -> String {
    ihb_validation::er7_to_xml_for_flow(PHW, Flow::Phw, None).expect("the fixture converts")
}

/// The three validation routes, head to head.
fn routes(c: &mut Criterion) {
    let xml = document();

    // Warm every cache, so what is measured is steady-state throughput
    // rather than one-off schema compilation. §9.5 measures that separately.
    let _ = ihb_validation::validate_and_convert(PHW, Flow::Phw, None);
    let _ = xsd::validate(&xml, Flow::Phw, "ADT_A05");

    let mut group = c.benchmark_group("validate one message");

    // What the ingress path uses today: parse, repair, convert, validate.
    group.bench_function("dictionary (end to end)", |b| {
        b.iter(|| ihb_validation::validate_and_convert(black_box(PHW), Flow::Phw, None));
    });

    group.bench_function("xsd-schema", |b| {
        b.iter(|| {
            xsd::validate_with(
                black_box(&xml),
                Flow::Phw,
                "ADT_A05",
                xsd::Engine::XsdSchema,
            )
        });
    });

    group.bench_function("uppsala", |b| {
        b.iter(|| xsd::validate_with(black_box(&xml), Flow::Phw, "ADT_A05", xsd::Engine::Uppsala));
    });

    // Both engines plus the comparison: what §9's redundancy actually costs.
    group.bench_function("both engines, compared", |b| {
        b.iter(|| xsd::validate(black_box(&xml), Flow::Phw, "ADT_A05"));
    });

    group.finish();
}

/// Conversion, which every ingress message pays for whether or not it is
/// validated — the message store keeps the XML.
fn conversion(c: &mut Criterion) {
    let xml = document();

    let mut group = c.benchmark_group("convert one message");

    group.bench_function("ER7 to v2.xml (flow schema)", |b| {
        b.iter(|| ihb_validation::er7_to_xml_for_flow(black_box(PHW), Flow::Phw, None));
    });

    group.bench_function("ER7 to v2.xml (no schema)", |b| {
        b.iter(|| ihb_validation::er7_to_xml(black_box(PHW)));
    });

    group.bench_function("v2.xml to ER7", |b| {
        b.iter(|| ihb_validation::xml_to_er7(black_box(&xml)));
    });

    group.finish();
}

/// The floor: a document with nothing in it, through both engines with warm
/// caches.
///
/// Not a measure of compilation — the API deliberately offers no way to drop
/// a cache, and a benchmark that pretended otherwise would be measuring
/// nothing. What this pins is the fixed overhead per call, so the cost of a
/// real document can be read as "this much, plus the document". Compilation
/// itself is asserted, with a threshold that fails the build, in
/// `tests/xsd_cost.rs`.
fn floor(c: &mut Criterion) {
    let mut group = c.benchmark_group("fixed overhead");

    group.bench_function("empty document, both engines", |b| {
        b.iter(|| xsd::validate(black_box("<nothing/>"), Flow::Phw, "ADT_A05"));
    });

    group.finish();
}

criterion_group!(benches, routes, conversion, floor);
criterion_main!(benches);
