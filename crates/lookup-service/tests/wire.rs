//! Query strings, urlencoded forms and multipart bodies.
//!
//! Implements `spec/20-lookup-service.md` §4.3 and §8.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lookup_service::form::{
    MalformedEncoding, boundary_of, parse_multipart, parse_pairs, percent_decode, percent_encode,
};

#[test]
fn percent_decoding_handles_plus_and_escapes_and_refuses_bad_input() {
    assert_eq!(percent_decode("a+b%20c").as_deref(), Ok("a b c"));
    assert_eq!(percent_decode("%C5%B5").as_deref(), Ok("ŵ"));
    // `spec/20-lookup-service.md` §4.3, RCA-IN-2: an escape that is not hex,
    // or is cut short, and bytes that are not UTF-8, are refused. They are not
    // kept literally or replaced with U+FFFD (the framework behaviour this
    // replaces).
    for malformed in ["100%", "%zz", "%4", "%", "a%2", "%FF", "%C5", "%E2%82"] {
        assert_eq!(
            percent_decode(malformed),
            Err(MalformedEncoding),
            "{malformed}"
        );
    }
    assert!(parse_pairs("k=%zz").is_err());
    assert!(parse_pairs("%FF=1").is_err());
}

#[test]
fn encoding_round_trips() {
    let text = "a b/c?d=e&f ŵ";
    assert_eq!(percent_decode(&percent_encode(text)).as_deref(), Ok(text));
}

#[test]
fn pairs_keep_order_and_repeats() {
    let pairs = parse_pairs("k=A&k=B&key.x=1&flag&=v").unwrap();
    assert_eq!(
        pairs,
        [
            ("k", "A"),
            ("k", "B"),
            ("key.x", "1"),
            ("flag", ""),
            ("", "v")
        ]
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
}

#[test]
fn boundary_is_read_from_the_content_type() {
    assert_eq!(
        boundary_of("multipart/form-data; boundary=abc").as_deref(),
        Some("abc")
    );
    assert_eq!(
        boundary_of("multipart/form-data; boundary=\"a b\"").as_deref(),
        Some("a b")
    );
    assert_eq!(boundary_of("application/json"), None);
    assert_eq!(boundary_of("multipart/form-data"), None);
}

#[test]
fn multipart_parts_names_files_and_binary_data() {
    let body = b"--b\r\nContent-Disposition: form-data; name=\"t\"\r\n\r\nval\r\n\
        --b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.csv\"\r\n\
        Content-Type: text/csv\r\n\r\na,b\r\n\x00\xff--not-a-boundary\r\n--b--\r\n";
    let parts = parse_multipart(body, "b").unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(
        (parts[0].name.as_str(), parts[0].file_name.as_deref()),
        ("t", None)
    );
    assert_eq!(parts[0].data, b"val");
    assert_eq!(parts[1].file_name.as_deref(), Some("x.csv"));
    assert_eq!(parts[1].data, b"a,b\r\n\x00\xff--not-a-boundary");
}

#[test]
fn multipart_empty_part_and_broken_framing() {
    let body = b"--b\r\nContent-Disposition: form-data; name=\"e\"\r\n\r\n\r\n--b--\r\n";
    let parts = parse_multipart(body, "b").unwrap();
    assert_eq!(parts[0].data, b"");
    assert!(parse_multipart(b"nothing here", "b").is_err());
    assert!(
        parse_multipart(
            b"--b\r\nContent-Disposition: form-data; name=\"e\"\r\n\r\nno end",
            "b"
        )
        .is_err()
    );
}
