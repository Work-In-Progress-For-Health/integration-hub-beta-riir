//! What redacting one message costs.
//!
//! Implements `spec/17-testing-and-quality.md` §11.
//!
//! Redaction runs on **every event**, and a component emits several events
//! per message — received, validated, processed. It is the most frequently
//! executed function in the estate, so it is worth knowing what it costs and
//! noticing if that changes.
//!
//! Run with `cargo bench -p ihb-telemetry`.

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
use ihb_telemetry::redact::redact_hl7;
use std::hint::black_box;

/// A message of the size the flows actually carry.
const TYPICAL: &str = concat!(
    "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r",
    "EVN||20250502092900\r",
    "PID|||8888888^^^252^PI~4444444444^^^NHS^NH||SURNAME^FORENAME^M||19990101|M|||",
    "99 ROAD^PLACE^CITY^COUNTY^SA99 1XX||555-1111~555-2222\r",
    "PD1|||^^W00000|G999999\r",
    "PV1||U"
);

/// Redaction, on the payload shapes the estate actually sees.
fn redaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("redact one payload");

    group.bench_function("typical message", |b| {
        b.iter(|| redact_hl7(black_box(TYPICAL)));
    });

    // The fast path: anything unrecognisable is masked whole without being
    // walked, so it should be far cheaper than a real message.
    group.bench_function("unrecognisable payload", |b| {
        b.iter(|| redact_hl7(black_box(r#"{"nhsNumber": "4444444444"}"#)));
    });

    // A large message — an ORU with many OBX segments is the realistic worst
    // case, and redaction is linear in segments.
    let large = {
        use std::fmt::Write as _;

        let mut text = String::from(TYPICAL);
        for i in 0..200 {
            text.push('\r');
            let _ = write!(text, "OBX|{i}|NM|2093-3^Cholesterol^LN||187|mg/dL|||||F");
        }
        text
    };
    group.bench_function("200 extra segments", |b| {
        b.iter(|| redact_hl7(black_box(&large)));
    });

    group.finish();
}

criterion_group!(benches, redaction);
criterion_main!(benches);
