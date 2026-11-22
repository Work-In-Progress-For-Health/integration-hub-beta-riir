//! Ported from `test_iteration.py`. `spec/ultra7-test-message-sender.md` `§4`.
#![allow(missing_docs, clippy::unwrap_used)]

use time::macros::datetime;
use ultra7::iteration::{apply_iteration, apply_iteration_at, strftime};
use ultra7::model::{IterationMode, IterationSpec};

fn at(content: &str, spec: &IterationSpec, i: usize) -> String {
    apply_iteration_at(content, spec, i, datetime!(2025-07-03 12:34:56.000789 UTC))
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
fn non_numeric_original_is_left_unchanged() {
    let spec = IterationSpec::new(3, 6, IterationMode::Increment);
    assert_eq!(at("ID:ABC:END", &spec, 4), "ID:ABC:END");
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
fn empty_values_list_leaves_original() {
    let spec = IterationSpec::new(7, 8, IterationMode::List);
    assert_eq!(at("PID|1||X", &spec, 0), "PID|1||X");
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
    let out = apply_iteration("XX|", &spec, 0);
    assert!(out[..8].chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn out_of_bounds_range_is_ignored() {
    let spec = IterationSpec::new(10, 20, IterationMode::Increment);
    assert_eq!(at("short", &spec, 3), "short");
}

#[test]
fn inverted_range_is_ignored() {
    let spec = IterationSpec::new(5, 2, IterationMode::Increment);
    assert_eq!(at("MSG000001", &spec, 1), "MSG000001");
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

/// RCA-INT-4: a result that does not fit `i128` is not saturated; the original
/// stays, as a non-integer original does.
#[test]
fn an_increment_that_overflows_leaves_the_original_unchanged() {
    let mut spec = IterationSpec::new(3, 6, IterationMode::Increment);
    spec.step = i64::MAX;
    // base is near the top of i128: base + step * index overflows.
    let huge = "ID:170141183460469231731687303715884105727:END";
    let mut wide = IterationSpec::new(3, 42, IterationMode::Increment);
    wide.step = i64::MAX;
    assert_eq!(at(huge, &wide, 1), huge);
    // A hostile step on a small base still works while it fits.
    assert_eq!(at("ID:001:END", &spec, 0), "ID:001:END");
}

#[test]
fn a_list_cycles_and_an_empty_list_keeps_the_original() {
    let mut spec = IterationSpec::new(0, 1, IterationMode::List);
    spec.values = vec!["a".into(), "b".into()];
    assert_eq!(at("x", &spec, 0), "a");
    assert_eq!(at("x", &spec, 3), "b");
    spec.values.clear();
    assert_eq!(at("x", &spec, 3), "x");
}
