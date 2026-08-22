//! Building the acknowledgement this mock sends back.
//!
//! `hl7_2_mllp::ack` already builds a conforming ACK. This module wraps it
//! so the result matches what the Python mock produced, field for field,
//! because the services under test parse these replies and their tests
//! assert on them. Two points where the two differ, and this follows the
//! Python:
//!
//! - **MSH-11 is always `P`.** The crate copies it from the message being
//!   answered, which is what HL7 v2.5 §2.9.2.2 asks for and what the real
//!   `hl7_server` does. The mock hard-coded `P`, so a test that sends `T`
//!   and asserts on the reply would notice the difference.
//! - **The fallback reply is framed.** When a message cannot be parsed at
//!   all, the Python returned its last-resort NACK as a bare string while
//!   every other path returned `to_mllp()`. A sender waiting for `<FS><CR>`
//!   never sees the end of that reply, so it is framed here.

use hl7_2::Message;
use hl7_2_mllp::{AckCode, ack, encode};

/// The last-resort reply, for a message that cannot be read as HL7 at all.
///
/// Byte-for-byte the Python's fallback, minus the framing question above,
/// and normative: `spec/11-mock-receivers.md` §2.5 gives these exact
/// characters. Public so a test asserts against the constant rather than
/// against a second copy of the string that could drift from it.
pub const FALLBACK: &str =
    "MSH|^~\\&|||||||NACK||P|2.5|||AL\rMSA|AE|UNKNOWN|Error processing message\r";

/// Build the framed reply to a received payload.
///
/// Never fails: a payload that cannot be parsed gets [`FALLBACK`], because
/// a mock that answers nothing looks like a hung endpoint rather than a
/// rejection, and that is the harder failure to diagnose.
pub fn reply(payload: &[u8], code: AckCode, timestamp: &str) -> Vec<u8> {
    match build(payload, code, timestamp) {
        Ok(frame) => frame,
        Err(error) => {
            tracing::warn!(%error, "could not build an acknowledgement; sending the fallback");
            encode(FALLBACK.as_bytes())
        }
    }
}

/// Build the reply, reporting why if the message cannot be answered.
fn build(payload: &[u8], code: AckCode, timestamp: &str) -> Result<Vec<u8>, ack::Error> {
    let message = ack::parse(payload)?;
    let control_id = field(&message, "MSH-10");
    let mut acknowledgement = ack::acknowledge_message(&message, code, &control_id, timestamp)
        .map_err(ack::Error::Build)?;
    // The mock answers as production whatever it was asked in; see above.
    acknowledgement
        .set("MSH-11.1", "P")
        .map_err(ack::Error::Build)?;
    Ok(encode(acknowledgement.to_er7().as_bytes()))
}

/// A field's value, or empty — a mock has nothing better to say about a
/// message that omits one than to echo the omission.
fn field(message: &Message, path: &str) -> String {
    message.get(path).ok().flatten().unwrap_or_default()
}

/// Whether this payload should be answered with a negative acknowledgement.
///
/// The mock's contract with its callers: a message whose text contains
/// `fail`, in any case, comes back rejected. That is how a test drives the
/// unhappy path without needing a malformed message.
#[must_use]
pub fn should_reject(payload: &[u8]) -> bool {
    // Not text: it cannot be a message this mock accepts, so it is
    // rejected rather than forwarded.
    std::str::from_utf8(payload).map_or(true, |text| text.to_ascii_lowercase().contains("fail"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &str = "MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20250101120000||ADT^A01|MSG00001|T|2.5\rEVN|A01|20250101120000\r";

    fn text(frame: &[u8]) -> String {
        String::from_utf8(frame[1..frame.len() - 2].to_vec()).unwrap()
    }

    #[test]
    fn acknowledges_with_the_fields_the_python_mock_sent() {
        let frame = reply(MESSAGE.as_bytes(), AckCode::Accept, "20250102030405");
        let reply_text = text(&frame);
        let segments: Vec<&str> = reply_text.split('\r').collect();
        let msh: Vec<&str> = segments[0].split('|').collect();

        // Sender and receiver swap.
        assert_eq!(msh[2], "RECVAPP");
        assert_eq!(msh[3], "RECVFAC");
        assert_eq!(msh[4], "SENDAPP");
        assert_eq!(msh[5], "SENDFAC");
        assert_eq!(msh[6], "20250102030405");
        assert_eq!(msh[8], "ACK^A01^ACK");
        assert_eq!(msh[9], "MSG00001");
        // Always production, even though the message said T.
        assert_eq!(msh[10], "P");
        assert_eq!(msh[11], "2.5");
        assert_eq!(segments[1], "MSA|AA|MSG00001");
    }

    #[test]
    fn a_rejection_differs_only_in_the_code() {
        let frame = reply(MESSAGE.as_bytes(), AckCode::Error, "20250102030405");
        assert!(text(&frame).contains("MSA|AE|MSG00001"));
    }

    #[test]
    fn every_reply_is_framed() {
        for payload in [MESSAGE.as_bytes(), b"not a message at all", &[0xff, 0xfe]] {
            let frame = reply(payload, AckCode::Accept, "20250102030405");
            assert_eq!(frame.first(), Some(&hl7_2_mllp::START_BLOCK));
            assert_eq!(
                &frame[frame.len() - 2..],
                [hl7_2_mllp::END_BLOCK, hl7_2_mllp::CARRIAGE_RETURN]
            );
        }
    }

    #[test]
    fn an_unreadable_payload_gets_the_fallback() {
        let frame = reply(b"not a message at all", AckCode::Error, "20250102030405");
        assert!(text(&frame).contains("MSA|AE|UNKNOWN|Error processing message"));
    }

    #[test]
    fn fail_anywhere_in_the_text_asks_for_a_rejection() {
        assert!(should_reject(b"MSH|^~\\&|...|please FAIL this"));
        assert!(should_reject(b"...Failure..."));
        assert!(!should_reject(MESSAGE.as_bytes()));
        // Bytes that are not text cannot be accepted.
        assert!(should_reject(&[0xff, 0xfe]));
    }
}
