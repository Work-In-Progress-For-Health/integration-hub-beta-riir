# Upstream report — `uppsala`: unprefixed `base="ID"` resolves to `xs:ID`

**Filed as <https://github.com/kushaldas/uppsala/issues/39>** on 2026-08-23.

This document is the working copy: the issue body is the same content with
the internal references below removed, since `tasks.md` and `spec/` numbers
mean nothing to the maintainer.

Tracked as `tasks.md` P2.17; the defect itself is
[`spec/06-validation.md`](../../spec/06-validation.md) §9.4.

Everything below was **run** against `uppsala` 0.9.0 on 2026-08-23, not
recalled. The reproduction is in this document in full.

---

## Title

`XsdValidator` resolves an unprefixed `base="ID"` to the built-in `xs:ID`
instead of the schema's own `ID` type

## Version

`uppsala` 0.9.0 (latest on crates.io at the time of writing).

## Summary

When a schema's default namespace is its own target namespace, an unprefixed
type reference such as `base="ID"` must resolve to a type **declared in that
schema**. `uppsala` resolves it to the XML Schema built-in `xs:ID` instead.

The instance is then checked against `NCName` rules it was never meant to
satisfy: empty values, values containing a space, and values beginning with
a digit are all rejected.

## Reproduction

Fifteen lines, no HL7 involved.

```rust
const SCHEMA: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"
            xmlns="urn:example"
            targetNamespace="urn:example"
            elementFormDefault="qualified">
  <xsd:simpleType name="ID">
    <xsd:restriction base="xsd:string"/>
  </xsd:simpleType>
  <xsd:element name="root" type="ID"/>
</xsd:schema>
"#;

fn main() {
    let schema_doc = uppsala::parse(SCHEMA).unwrap();
    let validator = uppsala::XsdValidator::from_schema(&schema_doc).unwrap();

    for xml in [
        r#"<root xmlns="urn:example"></root>"#,
        r#"<root xmlns="urn:example">252</root>"#,
        r#"<root xmlns="urn:example">a b</root>"#,
        r#"<root xmlns="urn:example">plain</root>"#,
    ] {
        let doc = uppsala::parse(xml).unwrap();
        let errors: Vec<String> =
            validator.validate(&doc).iter().map(|e| e.to_string()).collect();
        println!("{xml:<42} {errors:?}");
    }
}
```

### Observed

```text
<root xmlns="urn:example"></root>          1:1: '' is not a valid NCName/ID/IDREF
<root xmlns="urn:example">252</root>       1:1: '252' is not a valid NCName/ID/IDREF
<root xmlns="urn:example">a b</root>       1:1: 'a b' is not a valid NCName/ID/IDREF
<root xmlns="urn:example">plain</root>     valid
```

### Expected

All four valid. The schema's own `ID` is
`<xsd:restriction base="xsd:string"/>` — unconstrained.

The fourth case is worth noticing: it passes only because `plain` happens to
be a valid `NCName` as well. That coincidence is what lets the bug hide —
a corpus of well-behaved identifiers looks fine until one of them starts
with a digit.

## Why it matters here

The HL7 v2.xml schemas declare exactly this shape:

```xml
<xsd:schema xmlns="urn:hl7-org:v2xml" targetNamespace="urn:hl7-org:v2xml">
    <xsd:simpleType name="ID"><xsd:restriction base="xsd:string"/></xsd:simpleType>
    <xsd:complexType name="HD.3.CONTENT">
        <xsd:simpleContent><xsd:extension base="ID"/></xsd:simpleContent>
    </xsd:complexType>
```

So every field of HL7 type `ID` is validated against `NCName`. In a clinical
message `252` is an ordinary sending-application identifier, and a numeric
identifier is not an edge case.

Measured effect on our corpus: **11 of 24 cases wrongly invalid, including
every real sample message.**

## What we are doing meanwhile

`uppsala` runs as a cross-check beside another validator rather than on the
ingest path, and the disagreement is pinned by a test that asserts it. When
this is fixed that test fails, which is the signal to promote the
cross-check — the assertion is deliberately not weakened in the meantime.

## Suggested fix

Resolve an unprefixed type reference against the default namespace in scope
at the point of reference. Where that namespace is the schema's own target
namespace, the schema's declarations must take precedence over the built-in
XML Schema types of the same local name — `ID`, `IDREF`, `NAME`, `TOKEN`
and the rest are all names a schema is entitled to reuse.
