//! Ported from `test_iteration.py`. `spec/ultra7-test-message-sender.md` `§4`.
#![allow(missing_docs, clippy::unwrap_used)]

use time::macros::datetime;
use ultra7::iteration::{IterationError, apply_iteration, apply_iteration_at, strftime};
use ultra7::model::{IterationMode, IterationSpec};

fn try_at(content: &str, spec: &IterationSpec, i: usize) -> Result<String, IterationError> {
    apply_iteration_at(content, spec, i, datetime!(2025-07-03 12:34:56.000789 UTC))
}

fn at(content: &str, spec: &IterationSpec, i: usize) -> String {
    try_at(content, spec, i).unwrap()
}

#[test]
fn increments_by_step() {
    let content = "MSH|...|000001|P|2.5";
    let start = content.find("000001").unwrap();
    let spec = IterationSpec::new(start, start + 6, IterationMode::Increment);
    assert!(at(content, &spec, 0).ends_with("000001|P|2.5"));
    assert!(at(content, &spec, 1).ends_with("000002|P|2.5"));
    assert!(at(content, &spec, 5).ends_with("000006|P|2.5"));
}

#[test]
fn preserves_original_width_by_default() {
    let spec = IterationSpec::new(3, 6, IterationMode::Increment);
    assert_eq!(at("ID:007:END", &spec, 2), "ID:009:END");
}

#[test]
fn custom_pad_width() {
    let mut spec = IterationSpec::new(3, 4, IterationMode::Increment);
    spec.pad_width = 5;
    assert_eq!(at("ID:7:END", &spec, 3), "ID:00010:END");
}

#[test]
fn negative_value_is_not_padded() {
    let mut spec = IterationSpec::new(0, 2, IterationMode::Increment);
    spec.step = -5;
    assert_eq!(at("03", &spec, 1), "-2");
}

#[test]
fn a_non_numeric_original_is_refused_not_left_unchanged() {
    // `§4.2`, RCA-IN-2: the Python sent the message with the text as it was.
    let spec = IterationSpec::new(3, 6, IterationMode::Increment);
    assert_eq!(
        try_at("ID:ABC:END", &spec, 4),
        Err(IterationError::NotAnInteger)
    );
}

#[test]
fn cycles_through_values() {
    let content = "PID|1||PATIENT_A";
    let start = content.find("PATIENT_A").unwrap();
    let mut spec = IterationSpec::new(start, start + 9, IterationMode::List);
    spec.values = vec!["PATIENT_A".into(), "PATIENT_B".into(), "PATIENT_C".into()];
    assert!(at(content, &spec, 0).ends_with("PATIENT_A"));
    assert!(at(content, &spec, 1).ends_with("PATIENT_B"));
    assert!(at(content, &spec, 2).ends_with("PATIENT_C"));
    assert!(at(content, &spec, 3).ends_with("PATIENT_A"));
}

#[test]
fn an_empty_values_list_is_refused() {
    let spec = IterationSpec::new(7, 8, IterationMode::List);
    assert_eq!(try_at("PID|1||X", &spec, 0), Err(IterationError::NoValues));
}

#[test]
fn replaces_with_formatted_timestamp() {
    let content = "MSH|...|TS_PLACEHOLDER|...";
    let start = content.find("TS_PLACEHOLDER").unwrap();
    let mut spec = IterationSpec::new(start, start + 14, IterationMode::Timestamp);
    spec.timestamp_format = "%Y%m%d".into();
    assert_eq!(at(content, &spec, 0), "MSH|...|20250703|...");
}

#[test]
fn wall_clock_variant_replaces_the_placeholder() {
    let mut spec = IterationSpec::new(0, 2, IterationMode::Timestamp);
    spec.timestamp_format = "%Y%m%d".into();
    let out = apply_iteration("XX|", &spec, 0).unwrap();
    assert!(out[..8].chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn an_out_of_bounds_range_is_refused() {
    // `§4.2`, RCA-DECL-6: a stale range in a hand-edited project file is not
    // "sent as it was"; the message is not the one the project describes.
    let spec = IterationSpec::new(10, 20, IterationMode::Increment);
    assert_eq!(
        try_at("short", &spec, 3),
        Err(IterationError::RangeOutOfBounds)
    );
}

#[test]
fn an_inverted_or_empty_range_is_refused() {
    for (start, end) in [(5, 2), (3, 3)] {
        let spec = IterationSpec::new(start, end, IterationMode::Increment);
        assert_eq!(
            try_at("MSG000001", &spec, 1),
            Err(IterationError::RangeOutOfBounds),
            "{start}..{end}"
        );
    }
}

#[test]
fn offsets_are_characters_not_bytes() {
    // `§4.1`: a two-byte character before the range must not shift it.
    let spec = IterationSpec::new(2, 4, IterationMode::Increment);
    assert_eq!(at("é|09", &spec, 1), "é|10");
}

#[test]
fn strftime_subset() {
    let t = datetime!(2025-07-03 01:02:03.000004 UTC);
    assert_eq!(
        strftime("%Y-%m-%d %H:%M:%S.%f %y %j %% %Q", t),
        "2025-07-03 01:02:03.000004 25 184 % %Q"
    );
}

/// RCA-INT-4: a result that does not fit `i128` is not saturated; it is an
/// error, as is a non-integer original.
#[test]
fn an_increment_that_overflows_is_refused() {
    let mut spec = IterationSpec::new(3, 6, IterationMode::Increment);
    spec.step = i64::MAX;
    // base is near the top of i128: base + step * index overflows.
    let huge = "ID:170141183460469231731687303715884105727:END";
    let mut wide = IterationSpec::new(3, 42, IterationMode::Increment);
    wide.step = i64::MAX;
    assert_eq!(try_at(huge, &wide, 1), Err(IterationError::ValueOutOfRange));
    // A hostile step on a small base still works while it fits.
    assert_eq!(at("ID:001:END", &spec, 0), "ID:001:END");
}

#[test]
fn a_list_cycles_through_its_values() {
    let mut spec = IterationSpec::new(0, 1, IterationMode::List);
    spec.values = vec!["a".into(), "b".into()];
    assert_eq!(at("x", &spec, 0), "a");
    assert_eq!(at("x", &spec, 3), "b");
    spec.values.clear();
    assert_eq!(try_at("x", &spec, 3), Err(IterationError::NoValues));
}
