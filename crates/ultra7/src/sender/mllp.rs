//! MLLP over a plain socket. `spec/ultra7-test-message-sender.md` `§7.2`.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Instant;

use super::{SendResult, timeout_of};
use crate::model::{Endpoint, Message};

/// MLLP start block.
const START_BLOCK: u8 = 0x0b;
/// MLLP end block (followed by CR).
const END_BLOCK: u8 = 0x1c;

/// Segment separators to CR (`§7.2`). Messages typed, pasted or loaded from
/// disk usually carry `\n` or `\r\n`, which receivers fail to parse.
#[must_use]
pub fn to_er7(content: &str) -> String {
    content.replace("\r\n", "\r").replace('\n', "\r")
}

/// `MSA-1` of the first `MSA|` segment, if present and non-empty (`§7.2`).
#[must_use]
pub fn extract_ack_code(ack: &str) -> Option<String> {
    ack.replace('\n', "\r")
        .split('\r')
        .filter(|s| s.starts_with("MSA|"))
        .find_map(|s| {
            s.split('|')
                .nth(1)
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
        })
}

/// Send one framed message and judge the ACK (`§7.2`).
///
/// Success is `MSA-1` of `AA` or `CA`; any other code is `NAK: <code>`. A
/// reply with no `MSA-1` at all counts as success, because the tool judges
/// transport, not content. The reply is read until the end block, EOF or the
/// timeout, so a receiver that never closes and never sends `0x1C` yields a
/// timeout failure rather than a hang.
#[must_use]
pub fn send_mllp(endpoint: &Endpoint, message: &Message) -> SendResult {
    let Some(port) = endpoint.port.filter(|p| *p != 0) else {
        return SendResult::failure(0.0, String::new(), "Host and port are required");
    };
    if endpoint.host.is_empty() {
        return SendResult::failure(0.0, String::new(), "Host and port are required");
    }
    let mut frame = vec![START_BLOCK];
    frame.extend_from_slice(to_er7(&message.content).as_bytes());
    frame.extend_from_slice(&[END_BLOCK, b'\r']);

    let timeout = timeout_of(endpoint);
    let start = Instant::now();
    let ms = |s: Instant| s.elapsed().as_secs_f64() * 1000.0;
    let exchange = || -> std::io::Result<Vec<u8>> {
        let addr = (endpoint.host.as_str(), port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "host did not resolve")
            })?;
        let mut sock = TcpStream::connect_timeout(&addr, timeout)?;
        sock.set_read_timeout(Some(timeout))?;
        sock.set_write_timeout(Some(timeout))?;
        sock.write_all(&frame)?;
        // Read to the end block, EOF or the timeout (`§7.2`).
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        while !buf.contains(&END_BLOCK) {
            let n = sock.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            let read = chunk
                .get(..n)
                .ok_or_else(|| std::io::Error::other("read more than the buffer"))?;
            buf.extend_from_slice(read);
        }
        Ok(buf)
    };
    let response = match exchange() {
        Ok(r) => r,
        Err(e) => return SendResult::failure(ms(start), String::new(), e.to_string()),
    };
    let latency = ms(start);
    let text = String::from_utf8_lossy(&response)
        .trim_matches(|c| matches!(c, '\x0b' | '\x1c' | '\r' | '\n'))
        .to_owned();
    if text.is_empty() {
        return SendResult::failure(
            latency,
            String::new(),
            "No ACK received (connection closed)",
        );
    }
    match extract_ack_code(&text) {
        Some(code) if code != "AA" && code != "CA" => {
            SendResult::failure(latency, text, format!("NAK: {code}"))
        }
        _ => SendResult {
            ok: true,
            latency_ms: latency,
            response_summary: text,
            error: None,
        },
    }
}
