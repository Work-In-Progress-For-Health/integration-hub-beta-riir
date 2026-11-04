//! The plugin contract.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §2.

/// A named synthetic sample as `(name, body)` (`spec/integration-hub-tester-component-preview.md` §5).
///
/// Both halves are `'static` because samples are compiled in. The body must
/// be synthetic (§5.1): invented names, identifiers from a test range.
pub type Sample = (&'static str, &'static str);

/// What one run produced (`spec/integration-hub-tester-component-preview.md` §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The text to show.
    pub output: String,
    /// One line, prefixed `ok:` or `failed:`. Built by [`Outcome::accepted`]
    /// and [`Outcome::rejected`] so the prefix and `ok` cannot disagree (the
    /// Python inferred `ok` from a leading check mark, which could).
    pub summary: String,
    /// Whether the component accepted the input.
    pub ok: bool,
}

impl Outcome {
    /// The component accepted the input: `ok` is true and `summary` gains an
    /// `ok: ` prefix (§2.2).
    #[must_use]
    pub fn accepted(output: String, summary: impl AsRef<str>) -> Self {
        Self {
            output,
            summary: format!("ok: {}", summary.as_ref()),
            ok: true,
        }
    }

    /// The component rejected the input. The rejection is the thing being
    /// previewed, so this is a successful run (§2.3).
    #[must_use]
    pub fn rejected(output: String, summary: impl AsRef<str>) -> Self {
        Self {
            output,
            summary: format!("failed: {}", summary.as_ref()),
            ok: false,
        }
    }
}

/// One previewable component (§2.1).
///
/// A plugin calls the component's own library function and never a copy of
/// its logic (§3.1), so a preview cannot drift from the service. It is
/// synchronous; a plugin with an `async` component drives it with
/// `plugins::block_on`.
pub trait Plugin {
    /// The command-line name, e.g. `hl7-server`; unique across the registry,
    /// because [`crate::registry::find`] returns the first match.
    fn id(&self) -> &'static str;
    /// The tab label of the Python's Tk window; kept for parity with it, and
    /// not used by the command line (§6.6).
    fn tab_label(&self) -> &'static str;
    /// One line saying what the component does, shown by `list`.
    fn description(&self) -> &'static str;
    /// Caption for the input, saying what shape the component expects.
    fn input_label(&self) -> &'static str;
    /// Caption for the output, saying what the component produced.
    fn output_label(&self) -> &'static str;
    /// Synthetic samples, in display order. Every one must run to completion
    /// (`run` returns `Ok`), though not necessarily with `ok == true` (§5.2).
    fn samples(&self) -> &'static [Sample];
    /// Run the component over `input` (raw text, not yet normalised: use
    /// [`to_er7`] for HL7 components).
    ///
    /// # Errors
    ///
    /// A reason, when the tester could not ask the component at all
    /// (`spec/integration-hub-tester-component-preview.md` §2.3).
    fn run(&self, input: &str) -> Result<Outcome, String>;
}

/// Trim, and make every line ending the HL7 segment separator (`\r`)
/// (`spec/integration-hub-tester-component-preview.md` §2.4).
#[must_use]
pub fn to_er7(input: &str) -> String {
    input.trim().replace("\r\n", "\r").replace('\n', "\r")
}

/// ER7 for a terminal: `\r` shown as `\n` (§2.4). Display only; never feed
/// the result back to a component.
#[must_use]
pub fn display(er7: &str) -> String {
    er7.replace("\r\n", "\n").replace('\r', "\n")
}

/// A titled block, the Python's `"=" * 60` heading: the title between two
/// 60-character rules, no trailing newline.
#[must_use]
pub fn heading(title: &str) -> String {
    let rule = "=".repeat(60);
    format!("{rule}\n{title}\n{rule}")
}

/// The header lines every ER7 preview opens with.
///
/// Reads through `ihb_fields::read`, which answers an empty string for an
/// absent field, so a sparse message previews rather than failing.
#[must_use]
pub fn summary_lines(message: &hl7_2::Message) -> Vec<String> {
    use ihb_fields::read;
    let segments: Vec<String> = message.segments().map(|s| s.name.clone()).collect();
    vec![
        format!("  Message type  : {}", read(message, "MSH-9")),
        format!("  Version       : {}", read(message, "MSH-12")),
        format!(
            "  Sending app   : {}  ->  Sending fac: {}",
            read(message, "MSH-3"),
            read(message, "MSH-4")
        ),
        format!(
            "  Receiving app : {}  ->  Receiving fac: {}",
            read(message, "MSH-5"),
            read(message, "MSH-6")
        ),
        format!("  Control ID    : {}", read(message, "MSH-10")),
        format!("  Date/time     : {}", read(message, "MSH-7")),
        format!("  Segments      : {}", segments.join(", ")),
    ]
}
