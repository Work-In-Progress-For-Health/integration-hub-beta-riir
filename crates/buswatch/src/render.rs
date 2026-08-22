//! Making a message body readable.
//!
//! Implements `spec/14-buswatch.md` §5.4.
//!
//! # Nothing here may fail the page
//!
//! §5.4 is explicit: "Neither rendering may fail the page: an unrecognised
//! body is shown raw." A developer looking at BusWatch is usually looking
//! at a message that has already gone wrong somewhere, so the body is the
//! *least* likely thing in the estate to be well formed. Every function
//! here falls back to [`Body::Raw`] rather than returning an error.

/// How a body is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// An HL7 v2 message, one segment per row.
    Er7 {
        /// The segments, in order, each already split from its neighbours.
        segments: Vec<Segment>,
    },
    /// The message-store document of `spec/03-message-bus.md` §6.
    StoreEvent {
        /// The document, indented.
        pretty: String,
        /// Its `RawPayload`, rendered as ER7 in its own right — which is
        /// what a reader is almost always actually after.
        payload: Option<Vec<Segment>>,
    },
    /// Anything else, shown as it arrived.
    Raw {
        /// The body.
        text: String,
    },
}

/// Segments whose field 1 **is** the field separator.
///
/// In `MSH|^~\\&|252`, `MSH-1` is the `|` itself and `MSH-2` is `^~\\&`.
/// Splitting on `|` therefore yields MSH-2 first, and numbering the parts
/// from one would label every MSH field one lower than it is — so a reader
/// looking for `MSH-9`, the message type, would be shown `MSH-8`.
///
/// `BHS` and `FHS` are built the same way. Every other segment starts at
/// field 1.
pub const SEPARATOR_IS_FIELD_ONE: [&str; 3] = ["MSH", "BHS", "FHS"];

/// One HL7 segment, split for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The three-character name, e.g. `PID`.
    pub name: String,
    /// The fields, in order, starting at [`Segment::first_field_number`].
    pub fields: Vec<String>,
}

impl Segment {
    /// The HL7 number of `fields[0]`.
    ///
    /// 1 for almost every segment, and **2** for the three of
    /// [`SEPARATOR_IS_FIELD_ONE`].
    ///
    /// # Examples
    ///
    /// ```
    /// use buswatch::render::{render, Body};
    ///
    /// let body = render("MSH|^~\\&|252|252|100|100|20260821||ADT^A31|M1\rPID|||888");
    /// let Body::Er7 { segments } = &body else { panic!() };
    ///
    /// // MSH-9 is the message type, and it must be labelled 9.
    /// let msh = &segments[0];
    /// assert_eq!(msh.first_field_number(), 2);
    /// assert_eq!(msh.number_of(msh.fields.iter().position(|f| f == "ADT^A31").unwrap()), 9);
    ///
    /// // Everything else starts at 1, so PID-3 is the third part.
    /// let pid = &segments[1];
    /// assert_eq!(pid.first_field_number(), 1);
    /// assert_eq!(pid.number_of(2), 3);
    /// ```
    #[must_use]
    pub fn first_field_number(&self) -> usize {
        if SEPARATOR_IS_FIELD_ONE.contains(&self.name.as_str()) {
            2
        } else {
            1
        }
    }

    /// The HL7 number of the field at `index`.
    #[must_use]
    pub fn number_of(&self, index: usize) -> usize {
        self.first_field_number() + index
    }
}

impl Body {
    /// A short label for the rendering that was chosen.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Er7 { .. } => "HL7 v2",
            Self::StoreEvent { .. } => "message store event",
            Self::Raw { .. } => "raw",
        }
    }
}

/// Decide how to show a body (§5.4).
///
/// # Examples
///
/// An HL7 message is split into segments:
///
/// ```
/// use buswatch::render::{render, Body};
///
/// let body = render("MSH|^~\\&|252|252\rPID|||8888888");
///
/// let Body::Er7 { segments } = &body else { panic!("{body:?}") };
/// assert_eq!(segments.len(), 2);
/// assert_eq!(segments[0].name, "MSH");
/// assert_eq!(segments[1].fields[2], "8888888");
/// ```
///
/// A store document is indented, and its payload rendered in its own right:
///
/// ```
/// use buswatch::render::{render, Body};
///
/// let json = r#"{"CorrelationId":"3f6d","RawPayload":"MSH|^~\\&|252"}"#;
/// let body = render(json);
///
/// let Body::StoreEvent { pretty, payload } = &body else { panic!("{body:?}") };
/// assert!(pretty.contains("\n  \"CorrelationId\""), "{pretty}");
/// assert!(payload.is_some(), "the ER7 inside is rendered too");
/// ```
///
/// Anything else is shown as it arrived:
///
/// ```
/// use buswatch::render::{render, Body};
///
/// assert_eq!(render("hello"), Body::Raw { text: "hello".to_string() });
/// ```
#[must_use]
pub fn render(body: &str) -> Body {
    // A store event first, because its `RawPayload` starts with `MSH` too
    // and testing for ER7 first would show the JSON as one long segment.
    if let Some(rendered) = as_store_event(body) {
        return rendered;
    }
    if let Some(segments) = as_er7(body) {
        return Body::Er7 { segments };
    }

    Body::Raw {
        text: body.to_string(),
    }
}

/// A body as the message-store document, if it is one.
fn as_store_event(body: &str) -> Option<Body> {
    let document: serde_json::Value = serde_json::from_str(body).ok()?;

    // An object with the two keys every store event carries
    // (`spec/03-message-bus.md` §6). Testing for *any* JSON would show a
    // bare `"MSH|…"` string as a store event.
    let object = document.as_object()?;
    if !object.contains_key("CorrelationId") || !object.contains_key("RawPayload") {
        return None;
    }

    let payload = object
        .get("RawPayload")
        .and_then(serde_json::Value::as_str)
        .and_then(as_er7);

    Some(Body::StoreEvent {
        pretty: serde_json::to_string_pretty(&document).unwrap_or_else(|_| body.to_string()),
        payload,
    })
}

/// A body as HL7 v2, if it looks like one.
///
/// The test is that it starts with a segment header — three characters and
/// a field separator — rather than a full parse. BusWatch shows messages
/// that have already gone wrong, so demanding a parse would show exactly
/// the messages a developer most needs to read as raw text.
fn as_er7(body: &str) -> Option<Vec<Segment>> {
    let trimmed = body.trim_start();
    if !looks_like_a_segment(trimmed) {
        return None;
    }

    let segments: Vec<Segment> = trimmed
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .split('\r')
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('|');
            Segment {
                name: fields.next().unwrap_or_default().to_string(),
                fields: fields.map(ToString::to_string).collect(),
            }
        })
        .collect();

    (!segments.is_empty()).then_some(segments)
}

/// Whether text opens with something shaped like a segment header.
fn looks_like_a_segment(text: &str) -> bool {
    let mut characters = text.chars();
    let name: String = characters.by_ref().take(3).collect();

    name.len() == 3
        && name.chars().all(|c| c.is_ascii_alphanumeric())
        && characters.next() == Some('|')
}

#[cfg(test)]
mod tests {
    use super::*;

    const ER7: &str = "MSH|^~\\&|252|252|100|100|20260821||ADT^A31^ADT_A05|M1|P|2.5\r\
                       EVN||20260821092900\r\
                       PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M";

    #[test]
    fn an_hl7_message_is_split_into_segments() {
        // spec/14-buswatch.md §5.4 and §8.3.
        let Body::Er7 { segments } = render(ER7) else {
            panic!("expected ER7")
        };

        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].name, "MSH");
        assert_eq!(segments[1].name, "EVN");
        assert_eq!(segments[2].name, "PID");
        assert_eq!(segments[2].fields[2], "8888888^^^252^PI");
    }

    #[test]
    fn msh_fields_are_numbered_from_two() {
        // `MSH-1` IS the field separator, so splitting on `|` yields MSH-2
        // first. Numbering from one would label the message type -- MSH-9,
        // the field a developer looks at first -- as MSH-8, in a tool whose
        // entire job is letting somebody read field numbers.
        let Body::Er7 { segments } = render(ER7) else {
            panic!("expected ER7")
        };

        let msh = &segments[0];
        assert_eq!(msh.first_field_number(), 2);
        assert_eq!(msh.fields[0], "^~\\&", "which is MSH-2");
        assert_eq!(msh.number_of(0), 2);

        // MSH-9, the message type.
        let ninth = msh
            .fields
            .iter()
            .position(|field| field == "ADT^A31^ADT_A05")
            .expect("the message type is there");
        assert_eq!(msh.number_of(ninth), 9);

        // MSH-10, the control id.
        assert_eq!(msh.number_of(ninth + 1), 10);
        assert_eq!(msh.fields[ninth + 1], "M1");
    }

    #[test]
    fn every_other_segment_is_numbered_from_one() {
        let Body::Er7 { segments } = render(ER7) else {
            panic!("expected ER7")
        };

        let pid = &segments[2];
        assert_eq!(pid.name, "PID");
        assert_eq!(pid.first_field_number(), 1);
        // PID-3, the identifier list.
        assert_eq!(pid.number_of(2), 3);
        assert_eq!(pid.fields[2], "8888888^^^252^PI");
    }

    #[test]
    fn the_batch_headers_are_numbered_like_msh() {
        // `BHS-1` and `FHS-1` are the field separator too. Not reachable
        // through this estate's flows today, and one line of code to get
        // right rather than a wrong number on a page nobody expected.
        for name in SEPARATOR_IS_FIELD_ONE {
            let Body::Er7 { segments } = render(&format!("{name}|^~\\&|252")) else {
                panic!("{name} did not render as ER7")
            };
            assert_eq!(segments[0].first_field_number(), 2, "{name}");
        }

        // And a segment that merely looks similar is not one of them.
        let Body::Er7 { segments } = render("MSA|AA|M1") else {
            panic!("expected ER7")
        };
        assert_eq!(segments[0].first_field_number(), 1);
    }

    #[test]
    fn a_store_event_is_indented_and_its_payload_rendered() {
        // §8.3 — and the payload matters more than the envelope: it is
        // what a developer opened the page to read.
        let json = serde_json::json!({
            "MessageReceivedAt": "2026-08-21T09:15:00+00:00",
            "CorrelationId": "3f6d",
            "SourceSystem": "252",
            "RawPayload": ER7,
        })
        .to_string();

        let Body::StoreEvent { pretty, payload } = render(&json) else {
            panic!("expected a store event")
        };

        assert!(
            pretty.contains("\n  \"CorrelationId\": \"3f6d\""),
            "{pretty}"
        );
        let segments = payload.expect("the payload is ER7");
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[2].name, "PID");
    }

    #[test]
    fn a_store_event_is_recognised_before_er7() {
        // Its `RawPayload` starts with `MSH` too. Testing for ER7 first
        // would show the whole JSON document as one long segment.
        let json = format!(
            r#"{{"CorrelationId":"x","RawPayload":"{}"}}"#,
            "MSH|^~\\\\&|252"
        );

        assert!(matches!(render(&json), Body::StoreEvent { .. }));
    }

    #[test]
    fn an_unrecognised_body_is_shown_raw() {
        // §5.4 and §8.3 — "an unrecognised body is shown raw", never an
        // error. A developer is usually looking at a message that has
        // already gone wrong.
        for body in [
            "hello",
            "",
            "{ not json",
            "<xml/>",
            "MS|too|short",
            "MSHX|not a separator",
            "\u{0}\u{1}",
        ] {
            assert!(
                matches!(render(body), Body::Raw { .. }),
                "{body:?} should render raw"
            );
        }
    }

    #[test]
    fn json_that_is_not_a_store_event_is_raw_rather_than_pretty() {
        // Testing for *any* JSON would pretty-print things that are not
        // store events, which is a guess rather than a rendering.
        for body in [r#"{"a":1}"#, "[1,2,3]", r#""MSH|^~\\&|252""#] {
            assert!(matches!(render(body), Body::Raw { .. }), "{body:?}");
        }
    }

    #[test]
    fn line_endings_are_normalised_before_splitting() {
        // A sender using `\n` still has segments, and BusWatch should show
        // them as segments.
        let with_lf = ER7.replace('\r', "\n");
        let Body::Er7 { segments } = render(&with_lf) else {
            panic!("expected ER7")
        };
        assert_eq!(segments.len(), 3);

        let with_crlf = ER7.replace('\r', "\r\n");
        let Body::Er7 { segments } = render(&with_crlf) else {
            panic!("expected ER7")
        };
        assert_eq!(segments.len(), 3, "and no empty rows between them");
    }

    #[test]
    fn a_truncated_message_still_renders_as_er7() {
        // The case BusWatch exists for. A full parse would refuse this and
        // show it raw, which is exactly backwards: a developer is here
        // *because* the message is broken.
        let truncated = "MSH|^~\\&|252|252|100\rPID|||";
        let Body::Er7 { segments } = render(truncated) else {
            panic!("expected ER7")
        };

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].fields.len(), 4);
    }

    #[test]
    fn a_number_survives_the_json_round_trip_unchanged() {
        // spec/serde-json-float-roundtrip-arbitrary-precision/index.md.
        //
        // §5.4 re-renders a store document by parsing it to a `Value` and
        // printing it again, so `arbitrary_precision` is what stops that
        // being lossy. Without the feature, `serde_json` parses every
        // number into an `f64` and prints the shortest form that reads back
        // — so `1.0` becomes `1.0` but a value with more digits than an
        // f64 holds is silently rounded, and a developer comparing what
        // BusWatch shows against what was sent would find they differ.
        //
        // The feature flags live in the workspace `Cargo.toml` and nothing
        // else here mentions them, so this test is what notices if they go.
        let numbers = [
            // More significant digits than an f64 can hold. Without
            // `arbitrary_precision` these are rounded to the nearest f64
            // and printed back shorter.
            "12345678901234567890123456789",
            "0.1234567890123456789012345678901234567890",
            "3.141592653589793238462643383279",
            // Forms an f64 round trip would flatten.
            "1.0",
            "1.50",
            "-0.0",
            "0",
            // Beyond f64's range entirely: without the feature this parses
            // to `inf`, which serialises as `null`.
            "1e+400",
            "1e-400",
        ];

        for number in numbers {
            let document = format!(r#"{{"CorrelationId":"x","RawPayload":"y","n":{number}}}"#);

            let Body::StoreEvent { pretty, .. } = render(&document) else {
                panic!("{document} did not render as a store event")
            };

            assert!(
                pretty.contains(&format!("\"n\": {number}")),
                "{number} did not survive the round trip:\n{pretty}"
            );
        }
    }

    #[test]
    fn only_the_spelling_of_an_exponent_is_normalised() {
        // The one place the round trip is not byte-identical, measured
        // rather than assumed. `arbitrary_precision` keeps every
        // significant digit, but re-emits the exponent in a canonical
        // form: `E` becomes `e`, and a bare exponent gains an explicit
        // `+`. No value changes.
        //
        // Recorded because a reader taking "identical to the input" at its
        // word would expect `1e400` back and get `1e+400`, and would go
        // looking for a bug that is not there. See
        // spec/serde-json-float-roundtrip-arbitrary-precision/index.md.
        for (input, expected) in [
            ("1e400", "1e+400"),
            ("1E400", "1e+400"),
            ("1e10", "1e+10"),
            ("1.5e3", "1.5e+3"),
            ("1E+2", "1e+2"),
        ] {
            let document = format!(r#"{{"CorrelationId":"x","RawPayload":"y","n":{input}}}"#);

            let Body::StoreEvent { pretty, .. } = render(&document) else {
                panic!("{document} did not render as a store event")
            };

            assert!(
                pretty.contains(&format!("\"n\": {expected}")),
                "{input} should come back as {expected}:\n{pretty}"
            );
        }
    }

    #[test]
    fn a_float_survives_a_rust_round_trip_unchanged() {
        // The `float_roundtrip` half of the same specification: f64 -> JSON
        // -> f64 is the identity. Without it the parse is a fast
        // approximation that can be one bit out, which is the kind of
        // difference that shows up once in a million messages and is
        // impossible to reproduce.
        // Written as bit patterns where a decimal literal would be a
        // second thing to get right: this test is about bits, so the
        // inputs are bits.
        let values = [
            0.1,
            f64::EPSILON,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::MIN,
            // The smallest subnormal, and its negative.
            f64::from_bits(1),
            f64::from_bits(1 | (1 << 63)),
        ];

        for value in values {
            let json = serde_json::to_string(&value).expect("an f64 serialises");
            let back: f64 = serde_json::from_str(&json).expect("and parses");

            assert_eq!(
                back.to_bits(),
                value.to_bits(),
                "{value:e} became {back:e} via {json}"
            );
        }
    }

    #[test]
    fn arbitrary_input_never_panics() {
        for body in [
            "🏥",
            "MSH|🏥",
            "\r\r\r",
            "|||",
            "MSH|",
            &"x".repeat(100_000),
        ] {
            let _ = render(body);
        }
    }

    #[test]
    fn each_rendering_names_itself() {
        assert_eq!(render(ER7).kind(), "HL7 v2");
        assert_eq!(render("hello").kind(), "raw");
    }
}
