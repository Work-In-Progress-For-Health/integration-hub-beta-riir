//! Field access must never panic, whatever the message or the path.
//!
//! Implements `spec/17-testing-and-quality.md` §10.2 for
//! `spec/09-transformers.md` §2.
//!
//! Every transformer is a few hundred calls into `ihb-fields`, against a
//! message from a system that has its own ideas about HL7. The contract is
//! that a missing or malformed path reads as empty rather than failing, and
//! a copy of something absent is a no-op — so that a mapper is a list of
//! rules rather than a list of rules wrapped in error handling.
//!
//! This checks that contract holds for arbitrary messages, and that the
//! repetition-preserving copy of §2.2.1 does not lose data on odd input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // A message with the arbitrary part in a field, so the parser sees real
    // structure around it.
    let source_text = format!("MSH|^~\\&|252|252|100|100|20260729||ADT^A28|M1|P|2.5\rPID|1||{text}");
    let Ok(source) = hl7_2::parse(&source_text) else {
        return;
    };

    // Property 1: reading anything returns, and a missing path is empty.
    for path in ["PID-3", "PID-3.1", "PID-3.4.1", "PID-99", "ZZZ-1", "PID-0", ""] {
        let _ = ihb_fields::read(&source, path);
        let _ = ihb_fields::read_some(&source, path);
        let _ = ihb_fields::repetitions(&source, path);
    }
    // Deliberately no assertion that `PID-99` or `ZZZ-1` read as empty. The
    // fuzzer's text goes *into* the message, so an input full of `|` or `\r`
    // creates those very fields — it found that in under a minute. The
    // "missing path reads as empty" contract is a claim about paths the
    // message does not carry, and the unit tests check it with a controlled
    // message. Here the property is only that reading never panics.

    // A path that is malformed rather than merely absent cannot be created
    // by any content, so this one does hold for every input.
    assert_eq!(ihb_fields::read(&source, "PID-0"), "");
    assert_eq!(ihb_fields::read(&source, ""), "");

    // Property 2: copying returns, and never clears a target it found
    // nothing for (spec/09 §2.1).
    let Ok(mut target) = hl7_2::parse("MSH|^~\\&|252|252|100|100|20260729||ADT^A28|M2|P|2.5\rPID|")
    else {
        return;
    };
    let _ = ihb_fields::set(&mut target, "PID-4", "SENTINEL");
    let _ = ihb_fields::copy(&source, &mut target, "PID-99");
    assert_eq!(
        ihb_fields::read(&target, "PID-4"),
        "SENTINEL",
        "an absent source cleared the target"
    );

    // Property 3: a copied repeating field loses no **value**
    // (spec/09 §2.2.1).
    //
    // Not the repetition *count*: `copy` skips empty values by design
    // (§2.1), so a field of `~` — two empty repetitions — copies as nothing.
    // A fuzzer found that in seconds, and it is the contract rather than a
    // defect. What must never happen is a populated repetition going
    // missing, which is what this asserts.
    if ihb_fields::copy(&source, &mut target, "PID-3").is_ok() {
        let before: Vec<String> = ihb_fields::repetitions(&source, "PID-3")
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();
        let after: Vec<String> = ihb_fields::repetitions(&target, "PID-3")
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();

        assert_eq!(
            before, after,
            "copying PID-3 lost or altered a populated repetition"
        );
    }
});
