//! The HL7 transformers: PHW, ChemoCare, PIMS and core reference.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §4.1 to §4.4.

use hl7_chemo_transformer::Chemo;
use hl7_core_reference_transformer::{CoreReference, Rows, SUPPORTED_TRIGGERS};
use hl7_phw_transformer::Phw;
use hl7_pims_transformer::Pims;
use ihb_transformer::Transformer;
use wrds_service::Row;

use crate::plugin::{Outcome, Plugin, Sample, display, to_er7};

/// Parse for a transformer; a failure is the tester's `Err` (§4.3).
fn parse(input: &str) -> Result<(String, hl7_2::Message), String> {
    let er7 = to_er7(input);
    let message = ihb_validation::parse(&er7)
        .map_err(|error| format!("could not parse the message: {error}"))?;
    Ok((er7, message))
}

/// Synthetic sample `PHW_A28` (`spec/integration-hub-tester-component-preview.md` §5.1).
const PHW_A28: &str = "\
MSH|^~\\&|252|252|100|100|2025-05-05 23:23:28||ADT^A28^ADT_A05|202505052323326666666666|P|2.5|||||GBR||EN
EVN||20250502102000|20250505232328|||20250505232328
PID|||8888888^^^252^PI~9990000018^^^NHS^NH||TESTSURNAME^TESTFIRST^TESTMIDDLE^^MR||19870101|M|^^||ADDRESS1^ADDRESS2^ADDRESS3^ADDRESS4^XX99 9XX^^H~^^^^^^||^^^~|||||||||||||||||||01
PD1|||^^W99999^|G7777777
PV1||U";

/// Synthetic sample `CHEMO_A31_SW` (`spec/integration-hub-tester-component-preview.md` §5.1).
const CHEMO_A31_SW: &str = "\
MSH|^~\\&|192|192|200|200|20250624161510||ADT^A31|369913945290925|P|2.4|||NE|NE
EVN|Sub|20250624161510
PID|1|1000000001^^^^NH|1000000001^^^^NH~B1000001^^^^PAS||TEST^TEST^^^Mrs.||20000101000000|F|||1 TEST TEST TEST TEST^TEST^TEST^TEST^CF11 9AD||01000 000001^PRN|01000 000001^WPN||||||||||||||||||1
PD1||||G7000001
PV1||U";

/// Synthetic sample `CHEMO_A28_SW` (`spec/integration-hub-tester-component-preview.md` §5.1).
const CHEMO_A28_SW: &str = "\
MSH|^~\\&|192|192|200|200|20250701154910||ADT^A28|474997159036153|P|2.4|||NE|NE
EVN|Sub|20250701154910
PID|1|1000000001^^^^NH|1000001^^^^PAS~1000000001^^^^NH||TEST^TEST^TEST^^Mr.||20000101000000|M|||^^^^CF11 9AD||01000 000001^PRN^^test@test.com~07000000001^PRS|07000000001^WPN||||||||||||||||||1
PD1||||G7000001
PV1||U";

/// Synthetic sample `CHEMO_A28_VEL` (`spec/integration-hub-tester-component-preview.md` §5.1).
const CHEMO_A28_VEL: &str = "\
MSH|^~\\&|224|224|100|100|20250624165855||ADT^A28|951317629075403|P|2.4|||NE|NE
EVN|Sub|20250624165855
PID|1|1000000001^^^^NH|1000000001^^^^NH~V1000001^^^^PAS||TEST^TEST^^^Mrs.||20000101000000|F|||^^^^CF11 9AD||07000000001^PRN^^test@test.co.uk~07000000001^PRS~07000000001^PRN^^test@test.co.uk~|07000000001^WPN||||||||||||||||||1
PD1||||G7000001
PV1||U";

/// Synthetic sample `PIMS_A04` (`spec/integration-hub-tester-component-preview.md` §5.1).
const PIMS_A04: &str = "\
MSH|^~\\&|PIMS|BroMor HL7Sender|EMPI|EMPI|20250702085450+0000||ADT^A04^ADT_A01|73726643|P|2.3.1
EVN||20250702085440+0000||||20250702085450+0000
PID|||N4000000001^03^^^NI~N1000001^^^^PI||TEST^TEST-TEST^\"\"^^MISS||20000101+^D|F|||1 TEST^TEST^TEST^\"\"^CF11 9AD||07000000001^PRN^PH~07000000001^ORN^CP|07000000001^WPN^PH||S|||||||||||||^D||||20250702085440+0000
PD1||||G7000001~W90001
PV1||NA";

/// Synthetic sample `PIMS_A08` (`spec/integration-hub-tester-component-preview.md` §5.1).
const PIMS_A08: &str = "\
MSH|^~\\&|PIMS|BroMor HL7Sender|EMPI|EMPI|20241231101053+0000||ADT^A08^ADT_A01|48209024|P|2.3.1
EVN||20241231101035+0000||||20241231101035+0000
PID|||^03^^^NI~N5022039^^^^PI||TESTER^TEST^\"\"^^MRS.||20000101+^D|F|||TEST HOSPITAL^TEST LANE^TESTTOWN^TESTSHIRE^CF11 9AD||01234567892^PRN^PH~01234567896^ORN^CP|^WPN^PH||M||||||1|||||||^D||||20241231101035+0000
PD1||||G9310201~W98006
PV1||NA";

/// Synthetic sample `PIMS_A40` (`spec/integration-hub-tester-component-preview.md` §5.1).
const PIMS_A40: &str = "\
MSH|^~\\&|PIMS|BroMor HL7Sender|CPI|BroMor|20250630155035+0000||ADT^A40^ADT_A40|73711860|P|2.3.1
EVN||20250630155034+0000||||20250630155034+0000
PID|||1000000001^01^^^NI~T100001^^^^PI||TEST^TEST^TEST^^MS.||20000101+^D|F|||1, TEST^TEST TEST^TEST^TEST^CF11 9AD||07000000001 TEST PTNR^PRN^PH~07000000001 PT^ORN^CP|50500 02920^WPN^PH||S^NONE||||||0|||||||^D||||20250630155034+0000
PD1||||G1000001~W10001
MRG|00100001";

/// `phw` (`spec/integration-hub-tester-component-preview.md` §4.1).
#[derive(Debug, Default, Clone, Copy)]
pub struct PhwPlugin;

impl Plugin for PhwPlugin {
    fn id(&self) -> &'static str {
        "phw"
    }
    fn tab_label(&self) -> &'static str {
        "PHW Transformer"
    }
    fn description(&self) -> &'static str {
        "Public Health Wales HL7v2 -> normalised HL7v2 v2.5"
    }
    fn input_label(&self) -> &'static str {
        "HL7v2 ER7 input (ADT A28 / A31 from PHW)"
    }
    fn output_label(&self) -> &'static str {
        "Transformed HL7v2 ER7 output"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("A28 (PHW fixture)", PHW_A28),
            ("A31 (Southwest)", CHEMO_A31_SW),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        let (_, message) = parse(input)?;
        let mut phw = Phw::default();
        let out = phw.transform(&message).map_err(|e| e.to_string())?;
        // `processed_audit_text` reads what the transform just did, so it
        // comes after it (`spec/09-transformers.md` §3.5).
        let audit = phw.processed_audit_text(&message);
        Ok(Outcome::accepted(display(&out.to_er7()), audit))
    }
}

/// `chemo` (§4.1): the real ChemoCare mapping, printing the rebuilt ER7.
#[derive(Debug, Default, Clone, Copy)]
pub struct ChemoPlugin;

impl Plugin for ChemoPlugin {
    fn id(&self) -> &'static str {
        "chemo"
    }
    fn tab_label(&self) -> &'static str {
        "Chemo Transformer"
    }
    fn description(&self) -> &'static str {
        "ChemoCare HL7v2 -> normalised HL7v2 v2.5"
    }
    fn input_label(&self) -> &'static str {
        "HL7v2 ER7 input (ADT A28 / A31 from ChemoCare)"
    }
    fn output_label(&self) -> &'static str {
        "Transformed HL7v2 ER7 output"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("A31 Southwest", CHEMO_A31_SW),
            ("A28 Southwest", CHEMO_A28_SW),
            ("A28 Velindre", CHEMO_A28_VEL),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        let (_, message) = parse(input)?;
        let out = Chemo.map(&message).map_err(|e| e.to_string())?;
        Ok(Outcome::accepted(
            display(&out.to_er7()),
            "Chemocare transformation applied",
        ))
    }
}

/// `pims` (§4.2): the PIMS mapping without reference-data enrichment, so it
/// needs no network; codes that production would translate stay as sent.
#[derive(Debug, Default, Clone, Copy)]
pub struct PimsPlugin;

impl Plugin for PimsPlugin {
    fn id(&self) -> &'static str {
        "pims"
    }
    fn tab_label(&self) -> &'static str {
        "PIMS Transformer"
    }
    fn description(&self) -> &'static str {
        "PIMS HL7v2 -> normalised HL7v2 v2.5 (A04 / A08 / A40)"
    }
    fn input_label(&self) -> &'static str {
        "HL7v2 ER7 input (ADT A04 / A08 / A40 from PIMS)"
    }
    fn output_label(&self) -> &'static str {
        "Transformed HL7v2 ER7 output"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("A04 (New patient)", PIMS_A04),
            ("A08 (Patient update)", PIMS_A08),
            ("A40 (Merge)", PIMS_A40),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        let (_, message) = parse(input)?;
        let out = Pims.map(&message).map_err(|e| e.to_string())?;
        Ok(Outcome::accepted(
            display(&out.to_er7()),
            "PIMS transformation applied",
        ))
    }
}

/// Synthetic sample `CORE_A28` (`spec/integration-hub-tester-component-preview.md` §5.1).
const CORE_A28: &str = "\
MSH|^~\\&|349|349|100|100|20250624162400||ADT^A28|123456789|P|2.5|||NE|NE
PID|1|1000000001^^^^NH|1000000001^^^^NH||TEST^TEST^^^Mr.||20000101000000|1|||||||EN^English|11^Single|22^SomeReligion|||||33^SomeEthnicity";

/// Synthetic sample `CORE_A31` (`spec/integration-hub-tester-component-preview.md` §5.1).
const CORE_A31: &str = "\
MSH|^~\\&|349|349|100|100|20250624162400||ADT^A31|123456789|P|2.5|||NE|NE
PID|1|1000000001^^^^NH|1000000001^^^^NH||TEST^TEST||20000101000000";

/// Synthetic sample `CORE_A04` (`spec/integration-hub-tester-component-preview.md` §5.1).
const CORE_A04: &str = "\
MSH|^~\\&|349|349|100|100|20250624162400||ADT^A04|123456789|P|2.5|||NE|NE
PID|1||1000000001^^^^NH||TEST^TEST|||1";

/// Demo `(FromCode, Type, ToCode)` rows covering the six in-scope fields.
///
/// The `FromCode`s match the sample messages, so every field resolves and
/// nothing is blanked for want of a row (`spec/09-transformers.md` §8.4
/// clears a field with no match, which would hide the mapping).
pub const DEMO_ROWS: [(&str, &str, &str); 9] = [
    ("1", "Sex", "M"),
    ("2", "Sex", "F"),
    ("Mr.", "Title", "Mr"),
    ("Mrs.", "Title", "Mrs"),
    ("EN", "Language", "eng"),
    ("CY", "Language", "cym"),
    ("11", "Marital Status", "S"),
    ("22", "Religion", "REL22"),
    ("33", "Ethnicity", "ETH33"),
];

/// An in-memory WRDS: no network, no fixture file. Ignores the lookup table
/// and system arguments, so every message sees the same [`DEMO_ROWS`].
struct DemoRows;

impl Rows for DemoRows {
    fn fetch(&self, _table: &str, _from: &str, _to: &str) -> Result<Vec<Row>, wrds_service::Error> {
        Ok(DEMO_ROWS
            .iter()
            .map(|(from, kind, to)| {
                Row::from([
                    ("FromCode".to_string(), (*from).to_string()),
                    ("Type".to_string(), (*kind).to_string()),
                    ("ToCode".to_string(), (*to).to_string()),
                ])
            })
            .collect())
    }
}

/// `core-reference` (§4.4): the real mapping over [`DEMO_ROWS`]. Out-of-scope
/// triggers pass through unchanged; the summary says the codes are demo data,
/// because a `ToCode` here is not a real WRDS answer.
#[derive(Debug, Default, Clone, Copy)]
pub struct CoreReferencePlugin;

impl Plugin for CoreReferencePlugin {
    fn id(&self) -> &'static str {
        "core-reference"
    }
    fn tab_label(&self) -> &'static str {
        "Core Reference Transformer"
    }
    fn description(&self) -> &'static str {
        "RISP ADT (A28/A31/A40) -> PID Sex/Title/Language/Marital Status/Religion/Ethnicity \
         translated via WRDS. WARNING: demo table, not a live WRDS lookup"
    }
    fn input_label(&self) -> &'static str {
        "Inbound HL7v2 ER7 (RISP)"
    }
    fn output_label(&self) -> &'static str {
        "Transformed HL7v2 ER7 (PID fields translated)"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("A28 (all 6 fields populated)", CORE_A28),
            ("A31 (no coded fields)", CORE_A31),
            ("A04 (out of scope - passes through)", CORE_A04),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        let (er7, message) = parse(input)?;
        let trigger = ihb_fields::read(&message, "MSH-9.2");
        if !SUPPORTED_TRIGGERS.contains(&trigger.trim()) {
            let shown = if trigger.trim().is_empty() {
                "(unknown)"
            } else {
                trigger.trim()
            };
            return Ok(Outcome::accepted(
                display(&er7),
                format!("{shown} is out of scope - passed through unchanged"),
            ));
        }
        let mut transformer = CoreReference::new(DemoRows, "FioranoCodeTranslation".to_string());
        let out = transformer.transform(&message).map_err(|e| e.to_string())?;
        Ok(Outcome::accepted(
            display(&out.to_er7()),
            "PID reference fields translated (demo table data, not a WRDS lookup)",
        ))
    }
}
