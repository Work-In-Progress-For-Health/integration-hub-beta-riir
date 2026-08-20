//! The XML normalisers must never panic on arbitrary input.
//!
//! Implements `spec/17-testing-and-quality.md` §10.2 for
//! `spec/06-validation.md` §5.8.
//!
//! `xml_to_er7` does string surgery on a SOAP payload before handing it to a
//! parser: it strips namespace prefixes and drops empty elements
//! (§5.8.1, §5.8.2). Both walk raw text looking for `<` and `>`, which is
//! exactly the kind of code that panics on a slice boundary in the middle of
//! a multi-byte character.
//!
//! The input is attacker-adjacent — an unauthenticated SOAP endpoint — so
//! "it only sees well-formed XML" is not an assumption available here.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // The only property that matters: it returns. A malformed document is a
    // rejection, not a crash.
    let _ = ihb_validation::convert::xml_to_er7(text);

    // Wrapping the input in a plausible envelope exercises the tag walker
    // with real structure around the arbitrary part, which is where the
    // slice arithmetic actually runs.
    let wrapped = format!(
        r#"<ns0:ADT_A05 xmlns:ns0="urn:hl7-org:v2xml"><ns0:MSH><ns0:MSH.1>{text}</ns0:MSH.1></ns0:MSH></ns0:ADT_A05>"#
    );
    let _ = ihb_validation::convert::xml_to_er7(&wrapped);
});
