//! `cargo xtask stack up|down|status`: the local broker and database.
//!
//! Implements `spec/21-local-ci.md` §6. Three rules, each from a failure:
//!
//! - **Which Compose** (§6.2). `podman compose` hands the work to a Compose
//!   provider and fails on some machines (`podman machine list` reports an
//!   unknown state); the standalone `podman-compose` works there. The helper
//!   tries both, uses the first that answers, and **says which**.
//! - **Wait on a real connection** (§6.1, `spec/17` §9.2.1e). The emulator
//!   opens its port long before the broker will answer, and `podman-compose`
//!   rewrites a `["CMD", "<path>"]` health check to `CMD-SHELL`, which a
//!   distroless image cannot run (§6.3). So readiness here is a protocol
//!   answer, never an open port or a container's health state: the AMQP
//!   header echoed back by the broker, and a TDS pre-login reply from the
//!   database.
//! - **Only the broker and the database** (`up -d sb-emulator sqledge`). A
//!   bare `up -d` also starts `message-store-service`, which consumes the
//!   queue the in-process chain waits on (the hosted run that did this timed
//!   out on "the store event").

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

/// The Compose file, relative to the workspace root.
const COMPOSE_FILE: &str = "local/docker-compose.yml";

/// The services this tier needs, and nothing else.
const SERVICES: [&str; 2] = ["sb-emulator", "sqledge"];

/// The AMQP 1.0 protocol header. A broker that is up answers with its own
/// (`AMQP` then a protocol id, `0` plain or `3` for SASL).
const AMQP_HEADER: [u8; 8] = *b"AMQP\x00\x01\x00\x00";

/// The Service Bus emulator's AMQP port on the host.
const AMQP_PORT: u16 = 5672;

/// SQL Edge's TDS port on the host.
const TDS_PORT: u16 = 1433;

/// How long one probe may wait for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long `up` waits for each service before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(300);

/// Which Compose is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compose {
    /// `podman compose ...`.
    Plugin,
    /// `podman-compose ...`.
    Standalone,
}

impl Compose {
    /// The program and its leading arguments.
    const fn prefix(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Plugin => ("podman", &["compose"]),
            Self::Standalone => ("podman-compose", &[]),
        }
    }

    /// What the log calls it.
    const fn name(self) -> &'static str {
        match self {
            Self::Plugin => "podman compose",
            Self::Standalone => "podman-compose",
        }
    }
}

/// Pick a Compose from what answered (`spec/21` §6.2): the plugin is
/// preferred because it is what the documentation and the hosted job use.
const fn choose(plugin_works: bool, standalone_works: bool) -> Option<Compose> {
    if plugin_works {
        Some(Compose::Plugin)
    } else if standalone_works {
        Some(Compose::Standalone)
    } else {
        None
    }
}

/// Does `program args... version` exit zero?
fn answers(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .arg("version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Detect a usable Compose and say which.
fn detect() -> Result<Compose> {
    let compose = choose(
        answers("podman", &["compose"]),
        answers("podman-compose", &[]),
    )
    .ok_or_else(|| {
        anyhow!(
            "neither `podman compose` nor `podman-compose` answers `version`; \
             install one (spec/18 §5.0: Podman, not Docker)"
        )
    })?;
    println!("compose: using `{}`", compose.name());
    Ok(compose)
}

/// Run Compose with the project's file.
fn run_compose(compose: Compose, args: &[&str]) -> Result<()> {
    let root = crate::workspace_root()?;
    let (program, lead) = compose.prefix();
    let status = Command::new(program)
        .args(lead)
        .args(["-f", COMPOSE_FILE])
        .args(args)
        .current_dir(&root)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("`{} {}` failed", compose.name(), args.join(" "));
    }
    Ok(())
}

/// The bytes of a TDS pre-login request: version and encryption options.
///
/// Header (8): type `0x12`, status `0x01` (last packet), length, SPID 0,
/// packet id 1, window 0. Payload (18): an option table (`VERSION`,
/// `ENCRYPTION`, terminator) then their data. Encryption `0x02` is "not
/// supported", so the server answers without a TLS handshake.
fn tds_prelogin() -> Vec<u8> {
    let payload: Vec<u8> = [
        &[0x00, 0x00, 0x0B, 0x00, 0x06][..], // VERSION at offset 11, 6 bytes
        &[0x01, 0x00, 0x11, 0x00, 0x01],     // ENCRYPTION at offset 17, 1 byte
        &[0xFF],                             // terminator
        &[0, 0, 0, 0, 0, 0],                 // version data
        &[0x02],                             // encryption data
    ]
    .concat();
    // The payload is the fixed 18 bytes above, so the packet is 8 + 18 = 26.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "RCA-EXC-2: the payload is a fixed 18-byte literal, so 8 + len is 26"
    )]
    let length = u16::try_from(8 + payload.len()).unwrap_or(26);
    let mut packet = vec![0x12, 0x01];
    packet.extend_from_slice(&length.to_be_bytes());
    packet.extend_from_slice(&[0, 0, 1, 0]);
    packet.extend(payload);
    packet
}

/// Send `request` and read the first `want` bytes of the answer.
///
/// `None` means no answer: refused, closed, or silent past [`PROBE_TIMEOUT`].
/// That is the whole point of the probe: an open port that says nothing is
/// *not ready*.
fn exchange(address: SocketAddr, request: &[u8], want: usize) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&address, PROBE_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream.write_all(request).ok()?;
    let mut answer = vec![0_u8; want];
    stream.read_exact(&mut answer).ok()?;
    Some(answer)
}

/// Does a broker answer the AMQP header at `address`?
fn amqp_ready(address: SocketAddr) -> bool {
    exchange(address, &AMQP_HEADER, 4).is_some_and(|answer| answer == b"AMQP")
}

/// Does a SQL Server answer a TDS pre-login at `address`?
///
/// The first byte of the reply is the packet type, `0x04` (tabular result).
fn tds_ready(address: SocketAddr) -> bool {
    exchange(address, &tds_prelogin(), 1).is_some_and(|answer| answer == [0x04])
}

/// The two hosts' addresses.
fn addresses() -> (SocketAddr, SocketAddr) {
    (
        SocketAddr::from(([127, 0, 0, 1], AMQP_PORT)),
        SocketAddr::from(([127, 0, 0, 1], TDS_PORT)),
    )
}

/// Both services answering now? (For `status` and for tier 2's pre-check.)
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn both_ready() -> (bool, bool) {
    let (amqp, tds) = addresses();
    (amqp_ready(amqp), tds_ready(tds))
}

/// Wait until `ready` is true, polling every few seconds, or time out.
fn wait_for(what: &str, ready: impl Fn() -> bool) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < READY_TIMEOUT {
        if ready() {
            println!(
                "{what}: answering ({:.0}s)",
                started.elapsed().as_secs_f64()
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    bail!(
        "{what} did not answer within {}s; its logs: `podman logs ihb-rs-sb-emulator` / \
         `podman logs ihb-rs-sqledge`",
        READY_TIMEOUT.as_secs()
    )
}

/// `cargo xtask stack <up|down|status>`.
///
/// # Errors
///
/// An unknown subcommand, no usable Compose, a Compose failure, or a
/// service that never answers.
// See `gate::run` on `redundant_pub_crate` versus `unreachable_pub`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("up") => {
            let compose = detect()?;
            let mut up = vec!["up", "-d"];
            up.extend(SERVICES);
            run_compose(compose, &up)?;
            let (amqp, tds) = addresses();
            // The database first: the emulator waits on it (CLAUDE.md).
            wait_for("SQL Edge (TDS pre-login)", || tds_ready(tds))?;
            wait_for("Service Bus emulator (AMQP header)", || amqp_ready(amqp))
        }
        Some("down") => {
            let compose = detect()?;
            // Best effort (`.github/workflows/gate.yml`): rootless Podman can
            // fail to kill its network process on the way down.
            if let Err(error) = run_compose(compose, &["down", "--volumes"]) {
                eprintln!("warning: teardown failed: {error:#}");
            }
            Ok(())
        }
        Some("status") => {
            let (amqp, tds) = both_ready();
            println!(
                "Service Bus emulator (AMQP): {}",
                if amqp { "answering" } else { "not answering" }
            );
            println!(
                "SQL Edge (TDS):              {}",
                if tds { "answering" } else { "not answering" }
            );
            if amqp && tds {
                Ok(())
            } else {
                Err(anyhow!("the stack is not ready; `cargo xtask stack up`"))
            }
        }
        other => bail!("usage: cargo xtask stack <up|down|status> (got {other:?})"),
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn the_plugin_is_preferred_and_the_standalone_is_the_fallback() {
        // spec/21 §6.2.
        assert_eq!(choose(true, true), Some(Compose::Plugin));
        assert_eq!(choose(true, false), Some(Compose::Plugin));
        assert_eq!(choose(false, true), Some(Compose::Standalone));
        assert_eq!(choose(false, false), None);
    }

    #[test]
    fn the_standalone_is_invoked_without_the_compose_subcommand() {
        assert_eq!(Compose::Plugin.prefix(), ("podman", &["compose"][..]));
        assert_eq!(Compose::Standalone.prefix(), ("podman-compose", &[][..]));
    }

    #[test]
    fn only_the_broker_and_the_database_are_started() {
        // A bare `up -d` also starts message-store-service, which steals the
        // event the in-process chain waits for.
        assert_eq!(SERVICES, ["sb-emulator", "sqledge"]);
    }

    #[test]
    fn the_prelogin_packet_is_well_formed() {
        let packet = tds_prelogin();
        assert_eq!(packet[0], 0x12, "pre-login");
        assert_eq!(packet[1], 0x01, "last packet");
        assert_eq!(
            usize::from(u16::from_be_bytes([packet[2], packet[3]])),
            packet.len()
        );
        assert_eq!(packet.len(), 26);
        // The option table points inside the payload: VERSION at 11, ENCRYPTION at 17.
        let payload = &packet[8..];
        assert_eq!(payload[2], 11);
        assert_eq!(payload[7], 17);
        assert_eq!(payload[10], 0xFF, "terminator");
    }

    /// A one-shot server on a free port that runs `behave` on the connection.
    fn serve(behave: impl FnOnce(TcpStream) + Send + 'static) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                behave(stream);
            }
        });
        address
    }

    #[test]
    fn a_broker_that_echoes_the_amqp_header_is_ready() {
        // spec/21 §6.1.
        let address = serve(|mut stream| {
            let mut header = [0_u8; 8];
            stream.read_exact(&mut header).unwrap();
            stream.write_all(b"AMQP\x03\x01\x00\x00").unwrap();
        });
        assert!(amqp_ready(address));
    }

    #[test]
    fn an_open_port_that_says_nothing_is_not_ready() {
        // The emulator opens its port before the broker will answer
        // (spec/17 §9.2.1e): accepting a connection proves nothing.
        let address = serve(|stream| {
            thread::sleep(PROBE_TIMEOUT + Duration::from_secs(1));
            drop(stream);
        });
        assert!(!amqp_ready(address));
    }

    #[test]
    fn a_port_that_accepts_and_closes_is_not_ready() {
        let address = serve(drop);
        assert!(!amqp_ready(address));
        assert!(!tds_ready(address));
    }

    #[test]
    fn nothing_listening_is_not_ready() {
        let address = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        assert!(!amqp_ready(address) && !tds_ready(address));
    }

    #[test]
    fn a_database_that_answers_the_prelogin_is_ready() {
        let address = serve(|mut stream| {
            let mut request = [0_u8; 26];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(request.to_vec(), tds_prelogin());
            stream.write_all(&[0x04, 0x01, 0x00, 0x20]).unwrap();
        });
        assert!(tds_ready(address));
    }
}
