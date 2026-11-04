//! A structure XSD as a recursive grammar, and a matcher for segment tags.
//!
//! Implements `spec/06-validation.md` §10.2 and §10.3. Ported from upstream
//! `hl7_message_processor/{xsd_structure,matcher}.py` (`c17b134`).
//!
//! Why a grammar rather than the flow dictionaries of `flow`: the standard
//! release XSDs (2.4, 2.5, 2.5.1, 2.6) are not per-sender dialects, and there
//! is no compiled dictionary for them. Reading the group structure straight
//! out of the XSD gives unbounded nesting for free, because the matcher's call
//! stack *is* the group stack. The Python's older converter tracked a single
//! "current group" and so could not nest.
//!
//! Only segment **tags** are ever looked at, so no error here can carry a
//! field value (`spec/06-validation.md` §3.3.3, §10.6.1).
//!
//! # Examples
//!
//! ```
//! use ihb_validation::grammar::{load, match_structure};
//!
//! let xsd = include_str!("../schemas/2_5_1/ORU_R01_2_5_1.xsd");
//! let grammar = load(xsd, "ORU_R01")?;
//! let layout = match_structure(&grammar, &["MSH", "PID", "PV1", "OBR", "OBX"])?;
//! assert_eq!(layout.name, "ORU_R01");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use roxmltree::{Document, Node};

use crate::error::ValidationError;

/// The XML Schema namespace; only elements in it are grammar.
const XS_NS: &str = "http://www.w3.org/2001/XMLSchema";

/// How many times an item may occur at most.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Max {
    /// At most this many.
    Count(u32),
    /// `maxOccurs="unbounded"`.
    Unbounded,
}

impl Max {
    /// Whether a second occurrence is allowed.
    #[must_use]
    pub const fn repeats(self) -> bool {
        match self {
            Self::Unbounded => true,
            Self::Count(n) => n > 1,
        }
    }
}

/// One entry in a group: a segment or a nested group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A leaf segment reference such as `PID`.
    Segment {
        /// The segment code.
        name: String,
        /// `minOccurs`.
        min: u32,
        /// `maxOccurs`.
        max: Max,
    },
    /// A named group such as `ORU_R01.PATIENT_RESULT`.
    Group(Group),
}

impl Item {
    /// `minOccurs` of either kind of item.
    const fn min(&self) -> u32 {
        match self {
            Self::Segment { min, .. } => *min,
            Self::Group(g) => g.min,
        }
    }
}

/// A named group of items, recursively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The group name; for the root, the structure identifier.
    pub name: String,
    /// `minOccurs`.
    pub min: u32,
    /// `maxOccurs`.
    pub max: Max,
    /// Children in schema order.
    pub items: Vec<Item>,
}

/// A group name contains a dot; a segment code never does
/// (`spec/06-validation.md` §10.2.2).
fn is_group_name(name: &str) -> bool {
    name.contains('.')
}

/// `spec/06-validation.md` §10.2.4.
fn occurs(node: Node<'_, '_>) -> (u32, Max) {
    let min = node
        .attribute("minOccurs")
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let max = match node.attribute("maxOccurs") {
        None => Max::Count(1),
        Some("unbounded") => Max::Unbounded,
        // The Python's `except ValueError: max_occurs = 1`.
        Some(v) => Max::Count(v.parse().unwrap_or(1)),
    };
    (min, max)
}

/// The XML Schema child elements of `node` named `local`.
fn xs_children<'a, 'i>(
    node: Node<'a, 'i>,
    local: &'static str,
) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(move |c| {
        c.is_element() && c.tag_name().name() == local && c.tag_name().namespace() == Some(XS_NS)
    })
}

/// Whether the file includes a shared `*_segments.xsd`.
fn is_modular(root: Node<'_, '_>) -> bool {
    xs_children(root, "include").any(|inc| {
        inc.attribute("schemaLocation")
            .and_then(|l| l.rsplit(['/', '\\']).next())
            .is_some_and(|f| f.ends_with("_segments.xsd"))
    })
}

/// Parse `xsd` into the grammar of `structure_id`.
///
/// Both shapes are accepted (`spec/06-validation.md` §10.2.3); which one is
/// in use is decided by whether the file includes a `*_segments.xsd`.
///
/// # Errors
///
/// [`ValidationError`] if the XSD is not well-formed XML.
pub fn load(xsd: &str, structure_id: &str) -> Result<Group, ValidationError> {
    let doc = Document::parse(xsd).map_err(|e| ValidationError::xml(e.to_string()))?;
    let root = doc.root_element();
    let items = if is_modular(root) {
        modular_items(root, structure_id)
    } else {
        inline_items(root, structure_id)
    };
    Ok(Group {
        name: structure_id.to_owned(),
        min: 1,
        max: Max::Count(1),
        items,
    })
}

/// Recursively expand `type_name`'s rows, treating dotted refs as groups.
fn build(
    sequences: &std::collections::HashMap<&str, Vec<(String, u32, Max)>>,
    type_name: &str,
) -> Vec<Item> {
    sequences
        .get(type_name)
        .into_iter()
        .flatten()
        .map(|(name, min, max)| {
            if is_group_name(name) {
                Item::Group(Group {
                    name: name.clone(),
                    min: *min,
                    max: *max,
                    items: build(sequences, &format!("{name}.CONTENT")),
                })
            } else {
                Item::Segment {
                    name: name.clone(),
                    min: *min,
                    max: *max,
                }
            }
        })
        .collect()
}

/// Modular: every group is a top-level `<NAME>.CONTENT` complex type of
/// `element ref=` entries.
fn modular_items(root: Node<'_, '_>, structure_id: &str) -> Vec<Item> {
    type Row = (String, u32, Max);
    let mut sequences: std::collections::HashMap<&str, Vec<Row>> = std::collections::HashMap::new();
    for ctype in xs_children(root, "complexType") {
        let (Some(name), Some(seq)) = (
            ctype.attribute("name"),
            xs_children(ctype, "sequence").next(),
        ) else {
            continue;
        };
        let rows: Vec<Row> = xs_children(seq, "element")
            .filter_map(|el| {
                let r = el.attribute("ref")?;
                let (min, max) = occurs(el);
                Some((r.to_owned(), min, max))
            })
            .collect();
        if !rows.is_empty() {
            sequences.insert(name, rows);
        }
    }

    build(&sequences, &format!("{structure_id}.CONTENT"))
}

/// Self-contained: walk the top-level `element name=<structure>` inline.
/// A missing root element gives an empty grammar (`spec/06-validation.md`
/// §10.2.5).
fn inline_items(root: Node<'_, '_>, structure_id: &str) -> Vec<Item> {
    fn walk(element: Node<'_, '_>) -> Vec<Item> {
        let Some(seq) = xs_children(element, "complexType")
            .next()
            .and_then(|c| xs_children(c, "sequence").next())
        else {
            return Vec::new();
        };
        xs_children(seq, "element")
            .filter_map(|child| {
                let name = child.attribute("name").filter(|n| !n.is_empty())?;
                let (min, max) = occurs(child);
                Some(
                    if child.attribute("type").is_none() && is_group_name(name) {
                        Item::Group(Group {
                            name: name.to_owned(),
                            min,
                            max,
                            items: walk(child),
                        })
                    } else {
                        Item::Segment {
                            name: name.to_owned(),
                            min,
                            max,
                        }
                    },
                )
            })
            .collect()
    }
    xs_children(root, "element")
        .find(|e| e.attribute("name") == Some(structure_id))
        .map(walk)
        .unwrap_or_default()
}

/// What a successful match produced: the segments arranged as the grammar
/// nests them. Holds indices into the caller's tag slice, never values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The group's name.
    pub name: String,
    /// Its matched children.
    pub items: Vec<Matched>,
}

/// One matched child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matched {
    /// A segment, by position in the input.
    Segment {
        /// The segment code.
        name: String,
        /// Index in the input tag slice.
        index: usize,
    },
    /// A nested group.
    Group(Layout),
}

/// Why a message did not fit the grammar. Carries a structure, a position and
/// a segment code, nothing from a field (`spec/06-validation.md` §10.3.5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "message does not fit structure {structure}: matching stopped at segment {position} ({found})"
)]
pub struct MatchError {
    /// The structure identifier.
    pub structure: String,
    /// Zero-based position at which matching stopped.
    pub position: usize,
    /// The tag there, or `end of message`.
    pub found: String,
}

/// Match `items` in order from `pos`; returns the matches and the new position.
fn match_items(items: &[Item], tags: &[&str], mut pos: usize) -> (Option<Vec<Matched>>, usize) {
    let mut matched = Vec::new();
    for item in items {
        let mut count = 0u32;
        loop {
            match item {
                Item::Segment { name, max, .. } => {
                    if tags.get(pos) == Some(&name.as_str()) {
                        matched.push(Matched::Segment {
                            name: name.clone(),
                            index: pos,
                        });
                        pos += 1;
                        count += 1;
                        if max.repeats() {
                            continue;
                        }
                    }
                    break;
                }
                Item::Group(group) => {
                    let (sub, new_pos) = match_items(&group.items, tags, pos);
                    // A group that consumed nothing has not matched; without
                    // this an all-optional group would loop forever.
                    let Some(sub) = sub.filter(|_| new_pos != pos) else {
                        break;
                    };
                    matched.push(Matched::Group(Layout {
                        name: group.name.clone(),
                        items: sub,
                    }));
                    pos = new_pos;
                    count += 1;
                    if group.max.repeats() {
                        continue;
                    }
                    break;
                }
            }
        }
        if count == 0 && item.min() >= 1 {
            return (None, pos);
        }
    }
    (Some(matched), pos)
}

/// Match `tags` (segment codes, in message order) against `root`.
///
/// Greedy, recursive and all-or-nothing (`spec/06-validation.md` §10.3): a
/// missing required item, or segments left over once the grammar is spent,
/// fails the match. There is no backtracking, matching the Python.
///
/// # Errors
///
/// [`MatchError`] naming the position at which matching stopped.
pub fn match_structure(root: &Group, tags: &[&str]) -> Result<Layout, MatchError> {
    let fail = |position: usize| MatchError {
        structure: root.name.clone(),
        position,
        found: tags
            .get(position)
            .map_or_else(|| "end of message".to_owned(), |t| (*t).to_owned()),
    };
    match match_items(&root.items, tags, 0) {
        (Some(items), end) if end == tags.len() => Ok(Layout {
            name: root.name.clone(),
            items,
        }),
        (Some(_) | None, end) => Err(fail(end)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORU: &str = include_str!("../schemas/2_5_1/ORU_R01_2_5_1.xsd");
    const OMG: &str = include_str!("../schemas/2_5_1/OMG_O19_2_5_1.xsd");
    const MODULAR: &str = include_str!("../schemas/phw/ADT_A05.xsd");

    fn names(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .map(|i| match i {
                Item::Segment { name, .. } => name.as_str(),
                Item::Group(g) => g.name.as_str(),
            })
            .collect()
    }

    /// `spec/06-validation.md` §10.2.3, modular shape.
    #[test]
    fn modular_grammar_reads_refs_and_groups() {
        let g = load(MODULAR, "ADT_A05").unwrap();
        let n = names(&g.items);
        assert_eq!(&n[..4], ["MSH", "SFT", "EVN", "PID"]);
        assert!(n.contains(&"ADT_A05.PROCEDURE"));
        let Item::Group(proc) = g
            .items
            .iter()
            .find(|i| matches!(i, Item::Group(g) if g.name == "ADT_A05.PROCEDURE"))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(proc.max, Max::Unbounded);
        assert_eq!(proc.min, 0);
        assert_ne!(proc.items.len(), 0);
    }

    /// `spec/06-validation.md` §10.2.3, self-contained shape.
    #[test]
    fn inline_grammar_nests_groups() {
        let g = load(OMG, "OMG_O19").unwrap();
        let Item::Group(order) = g
            .items
            .iter()
            .find(|i| matches!(i, Item::Group(g) if g.name == "OMG_O19.ORDER"))
            .unwrap()
        else {
            panic!()
        };
        let Item::Group(specimen) = order
            .items
            .iter()
            .find(|i| matches!(i, Item::Group(g) if g.name == "OMG_O19.SPECIMEN"))
            .unwrap()
        else {
            panic!()
        };
        assert!(
            specimen
                .items
                .iter()
                .any(|i| matches!(i, Item::Group(g) if g.name == "OMG_O19.CONTAINER"))
        );
    }

    /// `spec/06-validation.md` §10.2.5.
    #[test]
    fn missing_root_element_gives_empty_grammar() {
        let g = load(ORU, "NOPE_N00").unwrap();
        assert_eq!(g.items.len(), 0);
        assert!(match_structure(&g, &["MSH"]).is_err());
        assert!(match_structure(&g, &[]).is_ok());
    }

    /// `spec/06-validation.md` §10.2.4.
    #[test]
    fn occurrence_defaults() {
        let xsd = r#"<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"><xsd:element name="X_Y01"><xsd:complexType><xsd:sequence>
            <xsd:element name="AAA" type="AAA"/>
            <xsd:element name="BBB" type="BBB" minOccurs="0" maxOccurs="unbounded"/>
            <xsd:element name="CCC" type="CCC" maxOccurs="many"/>
            </xsd:sequence></xsd:complexType></xsd:element></xsd:schema>"#;
        let g = load(xsd, "X_Y01").unwrap();
        assert_eq!(
            g.items[0],
            Item::Segment {
                name: "AAA".into(),
                min: 1,
                max: Max::Count(1)
            }
        );
        assert_eq!(
            g.items[1],
            Item::Segment {
                name: "BBB".into(),
                min: 0,
                max: Max::Unbounded
            }
        );
        assert_eq!(
            g.items[2],
            Item::Segment {
                name: "CCC".into(),
                min: 1,
                max: Max::Count(1)
            }
        );
    }

    #[test]
    fn malformed_xsd_is_an_error() {
        assert!(load("<not xml", "X").is_err());
    }

    /// `spec/06-validation.md` §10.3.3: a minimal valid ORU matches.
    #[test]
    fn minimal_oru_matches() {
        let g = load(ORU, "ORU_R01").unwrap();
        let layout = match_structure(&g, &["MSH", "PID", "PV1", "OBR", "OBX", "OBX"]).unwrap();
        assert_eq!(layout.items.len(), 2);
    }

    /// `spec/06-validation.md` §10.3.2, §10.5.3: four-deep group nesting.
    #[test]
    fn omg_nested_specimen_container_matches() {
        fn depth(l: &Layout) -> usize {
            1 + l
                .items
                .iter()
                .filter_map(|m| {
                    if let Matched::Group(g) = m {
                        Some(depth(g))
                    } else {
                        None
                    }
                })
                .max()
                .unwrap_or(0)
        }
        let g = load(OMG, "OMG_O19").unwrap();
        let tags = [
            "MSH", "PID", "PV1", "ORC", "OBR", "SPM", "SAC", "OBX", "OBX", "CTI",
        ];
        let layout = match_structure(&g, &tags).unwrap();
        assert!(depth(&layout) >= 4, "root > ORDER > SPECIMEN > CONTAINER");
    }

    /// `spec/06-validation.md` §10.3.3: required item missing.
    #[test]
    fn missing_required_segment_fails_at_position() {
        let g = load(MODULAR, "ADT_A05").unwrap();
        let err = match_structure(&g, &["MSH", "PID", "PV1"]).unwrap_err();
        assert_eq!(err.structure, "ADT_A05");
        assert_eq!(err.position, 1);
        assert_eq!(err.found, "PID");
    }

    /// `spec/06-validation.md` §10.3.3: leftovers fail, and §10.3.5 names
    /// the tag only.
    #[test]
    fn trailing_segments_fail() {
        let g = load(MODULAR, "ADT_A05").unwrap();
        let err = match_structure(&g, &["MSH", "EVN", "PID", "PV1", "ZZZ"]).unwrap_err();
        assert_eq!(err.position, 4);
        assert_eq!(err.found, "ZZZ");
        assert_eq!(
            err.to_string(),
            "message does not fit structure ADT_A05: matching stopped at segment 4 (ZZZ)"
        );
    }

    #[test]
    fn valid_adt_a05_matches_flat() {
        let g = load(MODULAR, "ADT_A05").unwrap();
        assert!(match_structure(&g, &["MSH", "EVN", "PID", "PV1"]).is_ok());
    }

    /// `spec/06-validation.md` §10.3.4: greedy, no backtracking. `A*` then a
    /// required `A` can never match.
    #[test]
    fn greedy_without_backtracking() {
        let xsd = r#"<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"><xsd:element name="X_Y01"><xsd:complexType><xsd:sequence>
            <xsd:element name="AAA" type="AAA" minOccurs="0" maxOccurs="unbounded"/>
            <xsd:element name="AAA" type="AAA"/>
            </xsd:sequence></xsd:complexType></xsd:element></xsd:schema>"#;
        let g = load(xsd, "X_Y01").unwrap();
        assert!(match_structure(&g, &["AAA", "AAA"]).is_err());
    }

    /// `spec/06-validation.md` §10.3.2: an all-optional group that consumes
    /// nothing neither matches nor loops.
    #[test]
    fn empty_optional_group_terminates() {
        let xsd = r#"<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"><xsd:element name="X_Y01"><xsd:complexType><xsd:sequence>
            <xsd:element name="X_Y01.G" minOccurs="0" maxOccurs="unbounded"><xsd:complexType><xsd:sequence>
              <xsd:element name="AAA" type="AAA" minOccurs="0"/>
            </xsd:sequence></xsd:complexType></xsd:element>
            <xsd:element name="BBB" type="BBB"/>
            </xsd:sequence></xsd:complexType></xsd:element></xsd:schema>"#;
        let g = load(xsd, "X_Y01").unwrap();
        assert!(match_structure(&g, &["BBB"]).is_ok());
        assert!(match_structure(&g, &["AAA", "AAA", "BBB"]).is_ok());
    }
}
