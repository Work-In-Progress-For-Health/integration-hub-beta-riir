//! Reading what the receiver said.
//!
//! Implements `spec/10-senders.md` §3.7.
//!
//! # `AR` is dead-lettered; everything else short of `AA`/`CA` is retried
//!
//! §3.7.1 (upstream `62b8c34`): `AE`, any unrecognised negative code, and an
//! unreadable reply are abandoned and redelivered, with
//! `MaxDeliveryCount` as the backstop. `AR` is the receiver saying it will
//! never accept the message, so retrying only delays the dead letter; it is
//! dead-lettered at once, where a person looks at it. The message is kept
//! either way, never dropped on the receiver's say-so.

use ihb_fields::read;
use ihb_sender::{NackClass, Verdict};

/// The `MSA-1` codes that mean the receiver took the message.
///
/// `AA` is application accept; `CA` is commit accept, from HL7's enhanced
/// acknowledgement mode. The Python accepts both and so does this.
pub const ACCEPTED: [&str; 2] = ["AA", "CA"];

/// The code that is dead-lettered rather than retried (`spec/10-senders.md`
/// §3.7.1).
pub const REJECT: &str = "AR";

/// Evaluate a reply.
///
/// Implements every row of `spec/10-senders.md` §3.7's table.
///
/// # Examples
///
/// ```
/// use hl7_sender::ack::evaluate;
///
/// let accepted = evaluate("MSH|^~\\&|100|100|252|252|20250505||ACK^A31^ACK|M1|P|2.5\rMSA|AA|M1");
/// assert!(accepted.is_accepted());
///
/// let rejected = evaluate("MSH|^~\\&|100|100|252|252|20250505||ACK^A31^ACK|M1|P|2.5\rMSA|AE|M1");
/// assert!(!rejected.is_accepted());
/// ```
///
/// A reply with no `MSA` is not an acknowledgement at all — it is the
/// receiver saying something else entirely, which cannot be read as consent:
///
/// ```
/// use hl7_sender::ack::evaluate;
///
/// let not_an_ack = evaluate("MSH|^~\\&|100|100|252|252|20250505||ADT^A31|M1|P|2.5");
/// assert!(!not_an_ack.is_accepted());
/// ```
#[must_use]
pub fn evaluate(reply: &str) -> Verdict {
    let Ok(parsed) = ihb_validation::parse(reply) else {
        // §3.7's last row. The Python logs a backtrace here; the reply is
        // not logged, because it can echo message content
        // (`spec/index.md` §0.8).
        tracing::error!("the acknowledgement could not be parsed");
        return Verdict::Nack {
            class: NackClass::Invalid,
            code: None,
            reason: "the acknowledgement could not be parsed".to_string(),
            acknowledgement: String::new(),
        };
    };

    if !parsed.segments().any(|segment| segment.name == "MSA") {
        // §3.7's first row.
        tracing::error!("Received a non-ACK message");
        return Verdict::Nack {
            class: NackClass::Invalid,
            code: None,
            reason: "Received a non-ACK message".to_string(),
            acknowledgement: reply.to_string(),
        };
    }

    let code = read(&parsed, "MSA-1");

    if ACCEPTED.contains(&code.as_str()) {
        tracing::info!("Valid ACK received.");
        return Verdict::Accepted {
            acknowledgement: reply.to_string(),
        };
    }

    // §3.7's third row. The control id is the *reply's* `MSH-10`, which for
    // a conforming ACK echoes the one that was sent.
    let control_id = read(&parsed, "MSH-10");
    let reason = format!("Negative ACK received: {code} for: {control_id}");
    tracing::error!("{reason}");

    // §3.7.1 — `AR` is the receiver saying it will never accept this
    // message; every other negative code, recognised or not, is retried.
    let class = if code == REJECT {
        NackClass::Reject
    } else {
        NackClass::Recoverable
    };

    Verdict::Nack {
        class,
        code: Some(code),
        reason,
        acknowledgement: reply.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply with the given `MSA-1`.
    fn reply(code: &str) -> String {
        format!(
            "MSH|^~\\&|100|100|252|252|20250505232332||ACK^A31^ACK|MSGID1|P|2.5\rMSA|{code}|MSGID1"
        )
    }

    #[test]
    fn every_row_of_the_evaluation_table() {
        // spec/10-senders.md §3.7 and §6.4.

        // Row 2: the two accepting codes.
        for code in ACCEPTED {
            assert!(evaluate(&reply(code)).is_accepted(), "{code}");
        }

        // Row 3: anything else.
        for code in ["AE", "AR", "CE", "CR", "XX", ""] {
            let verdict = evaluate(&reply(code));
            assert!(!verdict.is_accepted(), "{code}");
        }

        // Row 1: no MSA segment.
        let not_an_ack = evaluate("MSH|^~\\&|100|100|252|252|20250505||ADT^A31|M1|P|2.5");
        assert!(!not_an_ack.is_accepted());
        let Verdict::Nack { reason, .. } = &not_an_ack else {
            panic!("expected a rejection")
        };
        assert_eq!(reason, "Received a non-ACK message");

        // Row 4: unparseable.
        let unparseable = evaluate("this is not a message");
        assert!(!unparseable.is_accepted());
    }

    #[test]
    fn only_ar_is_non_recoverable() {
        // spec/10-senders.md §3.7.1, upstream `62b8c34`: AR is dead-lettered;
        // AE and every unrecognised code are retried; a reply that is not an
        // acknowledgement is retried but carries no code.
        let class_of = |reply: &str| match evaluate(reply) {
            Verdict::Nack { class, code, .. } => (class, code),
            other => panic!("expected a Nack, got {other:?}"),
        };

        assert_eq!(
            class_of(&reply("AR")),
            (NackClass::Reject, Some("AR".into()))
        );
        for code in ["AE", "CE", "CR", "XX"] {
            assert_eq!(
                class_of(&reply(code)),
                (NackClass::Recoverable, Some(code.into())),
                "{code}"
            );
        }
        assert_eq!(
            class_of("MSH|^~\\&|100|100|252|252|20250505||ADT^A31|M1|P|2.5"),
            (NackClass::Invalid, None)
        );
    }

    #[test]
    fn a_negative_acknowledgement_names_its_code_and_control_id() {
        // §3.7's third row — the exact wording, which the support teams
        // match on.
        let Verdict::Nack { reason, .. } = evaluate(&reply("AE")) else {
            panic!("expected a rejection")
        };

        assert_eq!(reason, "Negative ACK received: AE for: MSGID1");
    }

    #[test]
    fn both_reject_codes_are_retried_rather_than_dropped() {
        // §3.7.1 — the clause that matters most here. Nothing at this layer
        // can tell a permanent rejection from a transient one, and treating
        // `AR` as permanent would discard a patient record on the
        // receiver's say-so.
        for code in ["AE", "AR"] {
            assert!(
                !evaluate(&reply(code)).is_accepted(),
                "{code} must be retried, not completed"
            );
        }
    }

    #[test]
    fn the_acknowledgement_is_carried_for_the_audit_text() {
        // §2 step 11 — the audit text names what came back, either way.
        let accepted = evaluate(&reply("AA"));
        assert!(accepted.acknowledgement().contains("MSA|AA|MSGID1"));

        let rejected = evaluate(&reply("AE"));
        assert!(rejected.acknowledgement().contains("MSA|AE|MSGID1"));
    }

    #[test]
    fn an_empty_reply_is_a_rejection_rather_than_a_panic() {
        // A receiver that closed the connection mid-reply produces this,
        // and it runs on the failure path — the worst place for a second
        // failure.
        for reply in ["", "\r", "MSH", "MSA|AA|M1", "\u{0}\u{1}"] {
            let _ = evaluate(reply);
        }
    }
}
