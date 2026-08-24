#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! The transformer runtime.
//!
//! Implements `spec/09-transformers.md` §1 and `spec/02-configuration.md`
//! §5.
//!
//! Four services — PHW, ChemoCare, PIMS and the PROMs transformer — are the
//! same loop with a different [`Transformer`] in the middle. This crate is
//! that loop; the mappings are `spec/09` §3, §4 and §5, and live in their
//! own crates.
//!
//! # What a transformer is, and is not
//!
//! A transformer reads one queue and writes another. It does **not**
//! validate — that happened at ingress (`spec/07-hl7-server.md` §3) — and
//! it does not decide routing. It maps fields, and the interesting part is
//! that it must map them without losing anything the next hop needs.
//!
//! The two rules that carry the most weight:
//!
//! - **The incoming application properties are forwarded as received**
//!   (§1.3.1), not rebuilt. That is the mechanism by which one correlation
//!   id spans the whole journey from a hospital's socket to MPI. A
//!   transformer that minted fresh metadata would break every audit trail
//!   in the estate while looking perfectly correct in isolation.
//! - **A failure abandons the message and everything behind it** (§1.7).
//!   Within a session, order is the contract
//!   (`spec/01-architecture.md` §6.2), so a transformer cannot skip a
//!   message it could not handle and carry on with the next.

pub mod config;
pub mod error;
pub mod main_loop;
pub mod runtime;

pub use error::TransformError;
pub use runtime::{Outcome, Runtime, process_one};

use hl7_2::Message;

/// One flow's mapping.
///
/// Implements the trait of `spec/09-transformers.md` §1.
///
/// # Examples
///
/// A transformer that does nothing but rename the sending application —
/// enough to show the shape:
///
/// ```
/// use hl7_2::Message;
/// use ihb_transformer::{TransformError, Transformer};
///
/// struct Rename;
///
/// impl Transformer for Rename {
///     fn name(&self) -> &'static str {
///         "Rename"
///     }
///
///     fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
///         let mut out = message.clone();
///         out.set("MSH-3.1", "MPI")?;
///         Ok(out)
///     }
/// }
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5",
/// )?;
///
/// let out = Rename.transform(&message)?;
///
/// assert_eq!(ihb_fields::read(&out, "MSH-3.1"), "MPI");
/// assert_eq!(
///     Rename.received_audit_text(),
///     "Message received for Rename transformation"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait Transformer: Send {
    /// The name used in audit text and span names, e.g. `PHW`.
    ///
    /// `&'static str` because every implementation returns a literal, and
    /// tying it to `&self` would make a caller hold the transformer borrowed
    /// while it formats a log line.
    fn name(&self) -> &'static str;

    /// Transform one message. Returns the message to forward.
    ///
    /// # Errors
    ///
    /// [`TransformError`] when the message cannot be mapped. The runtime
    /// turns that into §1.6's first row: logged, a `MESSAGE_FAILED` event,
    /// and the message abandoned rather than completed.
    fn transform(&mut self, message: &Message) -> Result<Message, TransformError>;

    /// Audit text for `MESSAGE_RECEIVED` (§1.4).
    fn received_audit_text(&self) -> String {
        format!("Message received for {} transformation", self.name())
    }

    /// Audit text for `MESSAGE_PROCESSED` (§1.5).
    ///
    /// `UNKNOWN` when `MSH-3.1` cannot be read — the Python's wording, and
    /// what an operator reading the audit trail will match on.
    ///
    /// PHW overrides this to report which timestamps it normalised
    /// (`spec/09-transformers.md` §3.5).
    fn processed_audit_text(&self, message: &Message) -> String {
        let sending_app = ihb_fields::read(message, "MSH-3.1");
        let sending_app = if sending_app.is_empty() {
            "UNKNOWN"
        } else {
            &sending_app
        };
        format!(
            "{} transformation applied for SENDING_APP: {sending_app}",
            self.name()
        )
    }
}

/// A transformer at the **payload** level.
///
/// [`Transformer`] is the usual shape: ER7 in, ER7 out, with the parse and
/// the serialise done once by the runtime so three implementations cannot
/// disagree about them. Every `Transformer` is a `PayloadTransformer`
/// through the blanket implementation below.
///
/// This trait exists for the one transformer whose payload is not ER7.
/// `spec/16-proms-transformer.md` §1.1 puts the PROMs transformer on the
/// **same runtime, the same configuration and the same receive loop** as
/// the others — only its `transform` differs, taking WPAS XML and
/// returning FHIR JSON. Duplicating the loop for it would mean two copies
/// of `spec/09-transformers.md` §1.3's ordering, which is exactly what
/// having one runtime avoids.
pub trait PayloadTransformer: Send {
    /// The name used in audit text and span names.
    fn name(&self) -> &'static str;

    /// Transform one payload. Returns the body to forward.
    ///
    /// # Errors
    ///
    /// [`TransformError`] when the payload cannot be mapped. The runtime
    /// turns that into §1.6's first row: logged, a `MESSAGE_FAILED` event,
    /// and the message abandoned rather than completed.
    fn transform_payload(&mut self, body: &str) -> Result<String, TransformError>;

    /// Audit text for `MESSAGE_RECEIVED` (§1.4).
    fn received_audit_text(&self) -> String {
        format!("Message received for {} transformation", self.name())
    }

    /// Audit text for `MESSAGE_PROCESSED` (§1.5).
    ///
    /// Takes the **outgoing** body, because a payload-level transformer
    /// has no parsed intermediate to describe.
    fn processed_audit_text_for(&self, _outgoing: &str) -> String {
        format!("Message processed for {} transformation", self.name())
    }
}

/// Every ER7 transformer is a payload transformer.
///
/// The parse of `spec/09-transformers.md` §1.3 step 5 and the serialise of
/// step 7 live here, so they happen once however many transformers there
/// are — and so a parse failure keeps the "unexpected" wording §1.6's
/// second row gives it.
impl<T: Transformer> PayloadTransformer for T {
    fn name(&self) -> &'static str {
        Transformer::name(self)
    }

    fn transform_payload(&mut self, body: &str) -> Result<String, TransformError> {
        let parsed = ihb_validation::parse(body).map_err(|error| {
            // §1.6's second row. A transformer receives what an ingress
            // server already parsed and forwarded, so this means the two
            // disagree — worth the "unexpected" wording it gets.
            TransformError::Unexpected(error.to_string())
        })?;

        Ok(Transformer::transform(self, &parsed)?.to_er7())
    }

    fn received_audit_text(&self) -> String {
        Transformer::received_audit_text(self)
    }

    fn processed_audit_text_for(&self, _outgoing: &str) -> String {
        // The ER7 form takes the *parsed incoming* message, which this
        // signature does not carry. Re-parsing the outgoing body to
        // describe it would be work for a log line, so the default text is
        // used; no current transformer overrides it.
        format!(
            "Message processed for {} transformation",
            Transformer::name(self)
        )
    }
}
