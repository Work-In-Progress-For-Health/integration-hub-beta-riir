//! The command line and the mock launcher.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §6 and §7.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use integration_hub_tester::cli::{pick_sample, run};
use integration_hub_tester::mock::{Mode, resolve};
use integration_hub_tester::registry::find;

fn call(args: &[&str], stdin: &str) -> (i32, String, String) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&args, &mut stdin.as_bytes(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn list_shows_three_categories_and_every_id() {
    // §6.1.
    let (code, out, _) = call(&["list"], "");
    assert_eq!(code, 0);
    for heading in ["Transformers", "Servers", "Senders"] {
        assert!(out.contains(heading), "{out}");
    }
    for id in [
        "phw",
        "chemo",
        "pims",
        "core-reference",
        "proms",
        "hl7-server",
        "rest-server",
        "hl7-soap-server",
        "hl7-sender",
        "hl7-sender-ack",
        "soap-sender",
        "soap-subscription-sender",
    ] {
        assert!(out.contains(id), "{id} missing: {out}");
    }
}

#[test]
fn samples_lists_names_one_per_line() {
    // §6.2.
    let (code, out, _) = call(&["samples", "chemo"], "");
    assert_eq!(code, 0);
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["A31 Southwest", "A28 Southwest", "A28 Velindre"]
    );
}

#[test]
fn run_with_a_sample_prints_output_and_summary_and_exits_zero() {
    // §6.3 and §6.5: output on stdout, summary on stderr.
    let (code, out, err) = call(&["run", "chemo", "--sample", "velindre"], "");
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("MSH|"), "{out}");
    assert_eq!(err.trim(), "ok: Chemocare transformation applied");
}

#[test]
fn run_reads_standard_input_when_no_source_is_given() {
    // §6.3.
    let (code, out, _) = call(
        &["run", "hl7-sender-ack"],
        "MSH|^~\\&|A|B|C|D|20250101||ACK|M9|P|2.5\nMSA|AA|M9",
    );
    assert_eq!(code, 0);
    assert!(out.contains("Outcome        : SUCCESS"), "{out}");
}

#[test]
fn a_rejection_exits_one_and_a_usage_error_exits_two() {
    // §6.5.
    let (code, ..) = call(&["run", "hl7-server", "--sample", "wrong version"], "");
    assert_eq!(code, 1);
    for bad in [
        &[][..],
        &["nope"],
        &["run"],
        &["run", "nope"],
        &["run", "phw", "--sample"],
        &["run", "phw", "--bogus", "x"],
        &["samples", "nope"],
        &["mock", "ftp"],
    ] {
        let (code, _, err) = call(bad, "");
        assert_eq!(code, 2, "{bad:?}: {err}");
        assert!(err.contains("USAGE"), "{bad:?}: {err}");
    }
}

#[test]
fn a_tester_error_exits_one_with_the_reason() {
    // §2.3 and §6.5.
    let (code, _, err) = call(&["run", "phw"], "THIS IS NOT HL7");
    assert_eq!(code, 1);
    assert!(err.starts_with("failed: could not parse"), "{err}");
}

#[test]
fn sample_matching_is_exact_then_unique_substring() {
    // §6.3.
    let plugin = find("pims").unwrap();
    assert!(
        pick_sample(plugin.as_ref(), "a40 (merge)")
            .unwrap()
            .contains("MRG|")
    );
    assert!(
        pick_sample(plugin.as_ref(), "merge")
            .unwrap()
            .contains("MRG|")
    );
    assert!(
        pick_sample(plugin.as_ref(), "A0")
            .unwrap_err()
            .contains("matches 2")
    );
    assert!(
        pick_sample(plugin.as_ref(), "zzz")
            .unwrap_err()
            .contains("no sample")
    );
}

#[test]
fn output_is_written_only_when_asked_for() {
    // §6.4 and §1.2.
    let path = std::env::temp_dir().join(format!("iht-cli-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let (code, ..) = call(&["run", "chemo", "--sample", "velindre"], "");
    assert_eq!(code, 0);
    assert!(!path.exists());

    let (code, out, _) = call(
        &[
            "run",
            "chemo",
            "--sample",
            "velindre",
            "--output",
            path.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().trim_end(),
        out.trim_end()
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_file_can_be_the_source() {
    // §6.3.
    let path = std::env::temp_dir().join(format!("iht-in-{}.hl7", std::process::id()));
    std::fs::write(&path, "MSH|^~\\&|A|B|C|D|20250101||ACK|M1|P|2.5\nMSA|AR|M1").unwrap();
    let (code, out, _) = call(
        &["run", "hl7-sender-ack", "--file", path.to_str().unwrap()],
        "",
    );
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, 1);
    assert!(out.contains("Outcome        : AR"), "{out}");
}

#[test]
fn mock_modes_and_binary_resolution() {
    // §7.
    assert_eq!(Mode::parse("mllp").unwrap().binary(), "hl7-mock-receiver");
    assert_eq!(Mode::parse("soap").unwrap().binary(), "http-mock-receiver");
    let error = Mode::parse("ftp").unwrap_err();
    assert!(error.contains("'mllp' or 'soap'"), "{error}");

    // Not beside the executable: the bare name, for PATH.
    let empty = std::env::temp_dir().join(format!("iht-empty-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        resolve(Mode::Soap, Some(&empty)).file_name().unwrap(),
        format!("http-mock-receiver{}", std::env::consts::EXE_SUFFIX).as_str()
    );
    assert!(
        resolve(Mode::Soap, Some(&empty))
            .parent()
            .is_none_or(|p| p.as_os_str().is_empty())
    );

    // Beside it: that one.
    let name = format!("hl7-mock-receiver{}", std::env::consts::EXE_SUFFIX);
    std::fs::write(empty.join(&name), b"").unwrap();
    assert_eq!(resolve(Mode::Mllp, Some(&empty)), empty.join(&name));
    let _ = std::fs::remove_dir_all(&empty);
}
