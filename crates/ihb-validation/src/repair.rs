//! Filling in the two segments the Python quietly supplies.
//!
//! Implements `spec/06-validation.md` §4.6.
//!
//! The Python's ER7 → v2.xml conversion synthesises two segments when the
//! message structure requires them and the message omits them:
//!
//! - **`EVN`**, with `EVN-2` copied from `MSH-7`.
//! - **`PV1`**, with `PV1-2` set to the literal `U`.
//!
//! It is worth being clear about what that means: a message arriving without
//! an event segment is currently **accepted**, with an event time invented
//! from the message header. That is a real editorial act on clinical data,
//! and it is what production does today, so the Rust does the same rather
//! than start rejecting a class of message mid-cutover. See
//! `spec/06-validation.md` §4.6.4 for the question that belongs with the Hub
//! team.
//!
//! # What is repaired, and what is not
//!
//! The repair applies to the **copy used for validation and for the message
//! store's XML**, never to what is forwarded. A downstream system receives
//! exactly the bytes the sender sent; only the Hub's own record of the
//! message carries the synthesised segment. That is the Python's behaviour
//! too — its insertion happens inside the XML builder — and it is the
//! property that keeps the Hub a router rather than an editor.

use hl7_2::Message;
use hl7_2::dictionary::Item;

use crate::flow::Flow;

/// The value `PV1-2` takes when a `PV1` is synthesised.
///
/// `U` for "unknown" patient class. From `convert.py :: PV1_DEFAULT_VALUE`.
const PV1_DEFAULT: &str = "U";

/// Segments that may be synthesised, in the order they appear in a message.
///
/// Only these two. The Python synthesises no others, and a longer list would
/// mean inventing more clinical content than it does.
const REPAIRABLE: [&str; 2] = ["EVN", "PV1"];

/// Add any required-but-missing `EVN` or `PV1` to a copy of the message.
///
/// Returns the repaired message and the names of what was added, so a caller
/// can say what it did.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::{Flow, repair::supply_required_segments};
///
/// // A PHW message with no EVN. The PHW ADT_A05 structure requires one.
/// let message = hl7_2::parse(
///     "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
///      PID|||8888888^^^252^PI||SURNAME^FORENAME\r\
///      PV1||U",
/// )?;
///
/// let (repaired, added) = supply_required_segments(&message, Flow::Phw, "ADT_A05");
///
/// assert_eq!(added, ["EVN"]);
/// // EVN-2 is taken from MSH-7, as the Python does.
/// assert_eq!(repaired.get("EVN-2")?.as_deref(), Some("20250505232332"));
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn supply_required_segments(
    message: &Message,
    flow: Flow,
    structure_id: &str,
) -> (Message, Vec<String>) {
    let dictionary = flow.dictionary();
    let Some(items) = dictionary.structure(structure_id) else {
        // No grammar, so nothing is known to be required.
        return (message.clone(), Vec::new());
    };

    let mut repaired = message.clone();
    let mut added = Vec::new();

    for name in REPAIRABLE {
        if !is_required(items, name) || repaired.segment(name).is_some() {
            continue;
        }
        if !repair_is_effective(items, name) {
            // See `repair_is_effective`: the Python's insertion misplaces
            // the segment for these structures and the message is rejected
            // anyway. Supplying it here would accept a message production
            // rejects.
            continue;
        }

        let index = insertion_point(&repaired, items, name);
        repaired.insert_segment(index, name);

        let value = match name {
            // EVN-2, the recorded date and time of the event, from the
            // message's own header.
            "EVN" => (
                "EVN-2",
                repaired.get("MSH-7").ok().flatten().unwrap_or_default(),
            ),
            // PV1-2, the patient class: unknown.
            _ => ("PV1-2", PV1_DEFAULT.to_string()),
        };

        // A synthesised segment that could not be filled in is worse than
        // none, so it is only counted as added when the value took.
        if repaired.set(value.0, &value.1).is_ok() {
            added.push(name.to_string());
        }
    }

    (repaired, added)
}

/// Whether the structure requires a segment at its top level.
///
/// Only the top level: a segment required *inside* an optional group is not
/// required of the message, and synthesising one would invent a group that
/// was never there.
fn is_required(items: &[Item], name: &str) -> bool {
    items.iter().any(|item| match item {
        Item::Segment {
            name: item_name,
            required,
            ..
        } => item_name == name && *required,
        Item::Group { .. } => false,
    })
}

/// Whether supplying this segment would actually rescue the message, in the
/// Python.
///
/// **This function reproduces a defect on purpose.** The Python appends a
/// synthesised segment to the document root *while walking the message*, and
/// by the time it reaches the first segment that is not `MSH`/`SFT`/`EVN` it
/// has already appended that segment's enclosing group. So when the
/// structure puts the segment after `EVN` inside a **group**, the
/// synthesised `EVN` lands *after* the group and the document is rejected:
///
/// ```text
/// Unexpected child with tag ADT_A39.PIDPD1MRGPV1 at position 2.
/// Tag EVN expected.
/// ```
///
/// Measured across the corpus: the repair works for `phw` `ADT_A05` and
/// `chemo` `ADT_A05`, whose `EVN` is followed by a top-level `PID`, and
/// fails for `pims` `ADT_A39`, whose `EVN` is followed by the
/// `PIDPD1MRGPV1` group.
///
/// Reproducing it is the conservative choice, and deliberately so. The
/// alternative — repairing correctly — would start **accepting** PIMS
/// messages that production rejects today, forwarding them to MPI with an
/// event time invented from the message header. Changing clinical data flow
/// in that direction is the Hub team's call, not a side effect of a rewrite.
/// Recorded at `spec/06-validation.md` §4.6.5.
fn repair_is_effective(items: &[Item], name: &str) -> bool {
    let Some(position) = items.iter().position(|item| item.name() == name) else {
        return false;
    };

    // Whatever comes next must be a plain segment for the Python's append to
    // land in the right place.
    // A position that cannot be advanced is not an effective repair.
    position.checked_add(1).is_some_and(|after| {
        items
            .get(after)
            .is_none_or(|next| matches!(next, Item::Segment { .. }))
    })
}

/// Where a synthesised segment goes.
///
/// After the last segment that precedes it in the structure's own order, so
/// `EVN` lands after `MSH` and `SFT`, and `PV1` after whatever the structure
/// puts before it. Ordering matters: a segment in the wrong place is a
/// different validation failure rather than a repair.
fn insertion_point(message: &Message, items: &[Item], name: &str) -> usize {
    let order: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            Item::Segment { name, .. } => Some(name.as_str()),
            Item::Group { .. } => None,
        })
        .collect();

    let Some(position) = order.iter().position(|item| *item == name) else {
        return message.segments().count();
    };

    let precede: Vec<&str> = order.iter().take(position).copied().collect();

    // The index after the last present segment that sorts before this one:
    // the number of segments up to and including it, counted from the end
    // without any `+ 1`. None present counts none, so index 0.
    let names: Vec<&str> = message.segments().map(|s| s.name.as_str()).collect();
    names
        .iter()
        .rev()
        .skip_while(|name| !precede.contains(name))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PHW message, with the segments named in `segments` removed.
    fn phw_without(segments: &[&str]) -> Message {
        let full = concat!(
            "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r",
            "EVN||20250502092900\r",
            "PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r",
            "PV1||U"
        );
        let text: String = full
            .split('\r')
            .filter(|line| !segments.iter().any(|drop| line.starts_with(drop)))
            .collect::<Vec<_>>()
            .join("\r");
        hl7_2::parse(&text).expect("the fixture parses")
    }

    #[test]
    fn a_missing_evn_is_supplied_from_the_header() {
        // spec/06-validation.md §4.6
        let (repaired, added) =
            supply_required_segments(&phw_without(&["EVN"]), Flow::Phw, "ADT_A05");

        assert_eq!(added, ["EVN"]);
        assert_eq!(
            repaired.get("EVN-2").unwrap().as_deref(),
            Some("20250505232332"),
            "EVN-2 comes from MSH-7"
        );
    }

    #[test]
    fn a_missing_pv1_is_supplied_as_unknown() {
        // spec/06-validation.md §4.6
        let (repaired, added) =
            supply_required_segments(&phw_without(&["PV1"]), Flow::Phw, "ADT_A05");

        assert_eq!(added, ["PV1"]);
        assert_eq!(repaired.get("PV1-2").unwrap().as_deref(), Some("U"));
    }

    #[test]
    fn both_can_be_supplied_at_once() {
        let (repaired, added) =
            supply_required_segments(&phw_without(&["EVN", "PV1"]), Flow::Phw, "ADT_A05");

        assert_eq!(added, ["EVN", "PV1"]);
        assert!(repaired.segment("EVN").is_some());
        assert!(repaired.segment("PV1").is_some());
    }

    #[test]
    fn a_segment_that_is_present_is_left_exactly_as_it_was() {
        // Repair must never overwrite what a sender actually sent.
        let original = phw_without(&[]);
        let (repaired, added) = supply_required_segments(&original, Flow::Phw, "ADT_A05");

        assert_eq!(added, [] as [std::string::String; 0]);
        assert_eq!(
            repaired.get("EVN-2").unwrap().as_deref(),
            Some("20250502092900"),
            "the sender's own EVN-2 survives"
        );
        assert_eq!(repaired.to_er7(), original.to_er7());
    }

    #[test]
    fn the_supplied_segment_lands_in_the_right_place() {
        // spec/06-validation.md §4.6.1 — a segment in the wrong position is
        // a different failure, not a repair.
        let (repaired, _) = supply_required_segments(&phw_without(&["EVN"]), Flow::Phw, "ADT_A05");

        let names: Vec<&str> = repaired
            .raw()
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect();
        assert_eq!(names, ["MSH", "EVN", "PID", "PV1"]);
    }

    #[test]
    fn the_original_message_is_not_modified() {
        // The forwarded message must be exactly what the sender sent; only
        // the Hub's own copy carries the repair.
        let original = phw_without(&["EVN"]);
        let before = original.to_er7();

        let (_, added) = supply_required_segments(&original, Flow::Phw, "ADT_A05");

        assert_eq!(added, ["EVN"]);
        assert_eq!(original.to_er7(), before, "the input was mutated");
        assert!(original.segment("EVN").is_none());
    }

    #[test]
    fn a_structure_whose_evn_is_followed_by_a_group_is_not_repaired() {
        // spec/06-validation.md §4.6.5 — a reproduced Python defect. PIMS
        // ADT_A39 puts PID inside PIDPD1MRGPV1, so the Python's synthesised
        // EVN lands after the group and the message is rejected regardless.
        let pims = hl7_2::parse(concat!(
            "MSH|^~\\&|PIMS|BroMor|EMPI|EMPI|20250121100427||ADT^A40^ADT_A39|M1|P|2.3.1\r",
            "PID|||123^^^103^PI\r",
            "MRG|456"
        ))
        .unwrap();

        let (repaired, added) = supply_required_segments(&pims, Flow::Pims, "ADT_A39");

        assert!(added.is_empty(), "PIMS ADT_A39 must not be repaired");
        assert!(repaired.segment("EVN").is_none());
    }

    #[test]
    fn a_structure_whose_evn_is_followed_by_a_segment_is_repaired() {
        // The other side of §4.6.5: PHW ADT_A05 has a top-level PID after
        // EVN, so the Python's insertion lands correctly and the message is
        // accepted.
        let (_, added) = supply_required_segments(&phw_without(&["EVN"]), Flow::Phw, "ADT_A05");
        assert_eq!(added, ["EVN"]);
    }

    #[test]
    fn an_unknown_structure_repairs_nothing() {
        let (_, added) = supply_required_segments(&phw_without(&["EVN"]), Flow::Phw, "ADT_A99");
        assert_eq!(added, [] as [std::string::String; 0]);
    }

    #[test]
    fn nothing_beyond_evn_and_pv1_is_ever_invented() {
        // The list is deliberately two long. A message missing its PID is
        // rejected, not repaired: synthesising a patient is not a repair.
        let (repaired, added) =
            supply_required_segments(&phw_without(&["PID"]), Flow::Phw, "ADT_A05");

        assert_eq!(added, [] as [std::string::String; 0]);
        assert!(repaired.segment("PID").is_none());
    }
}
