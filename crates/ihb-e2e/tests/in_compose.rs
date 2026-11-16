//! Every flow, through the **containers**, not through in-process services.
//!
//! Implements `tasks.md` P5.7's second half and `spec/18-deployment.md` §5.
//!
//! # What this proves that `phw_to_mpi.rs` cannot
//!
//! That test wires the same crates together inside one process. It proves
//! the services agree on queue names, sessions, properties and message
//! rendering — which is the part most likely to be wrong, and is why it
//! exists. What it cannot prove is anything about the *deployment*:
//!
//! 1. **That the image runs.** A binary that works under `cargo test` and
//!    an image that starts are different claims. P8.2 shipped a container
//!    whose `main` was `fn main() {}` — it built, it started, and it did
//!    nothing, while the task was marked done.
//! 2. **That the container's configuration is the one the service wants.**
//!    Every `.env` file in `local/` is exercised here and nowhere else. A
//!    queue name that is right in a test constant and wrong in an `.env`
//!    file is invisible to the in-process test.
//! 3. **That the services find each other by their network aliases**, over
//!    the broker the compose file starts, rather than over `127.0.0.1`.
//! 4. **That a profile is self-sufficient.** `--profile paris-to-mpi` must
//!    bring up everything that flow needs and nothing it does not.
//!
//! # The five flows
//!
//! Two shapes, deliberately both covered:
//!
//! ```text
//! phw, pims, chemo   MLLP → hl7-server → transformer → hl7-sender → mock
//! paris, mosaiq      MLLP → hl7-server →               hl7-sender → mock
//! ```
//!
//! and four HL7 releases and trigger types between them — 2.5 `ADT^A31`
//! (phw, mosaiq), 2.3.1 `ADT^A40`, 2.4 `ADT^A28`, 2.5.1 `ADT^A31`. A flow that only works
//! for the version its transformer was written against fails here.
//!
//! `lims-to-mpi` is covered separately, by
//! [`the_lims_soap_flow_reaches_its_topic`]. It differs twice over: its
//! ingress is SOAP over HTTP rather than MLLP, and the assertion is taken
//! **at the LIMS topic**, from a subscription nothing in that profile
//! consumes (`spec/01-architecture.md` §2.1.3), not at the mock receiver.
//!
//! # Running it
//!
//! ```sh
//! ./docker/build-all.sh                      # the images of §2
//! cargo test -p ihb-e2e --features compose --test in_compose -- --test-threads=1
//! ```
//!
//! Slow by nature: it starts twenty-odd containers and waits for them to
//! become healthy. Behind the `compose` feature so neither a normal
//! `cargo test` nor the `emulator` suite picks it up.

#![cfg(feature = "compose")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::large_futures)]

use std::process::Command;
use std::time::{Duration, Instant};

use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_bus::message::Outcome;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The emulator, reached from the host rather than from the compose network.
const EMULATOR: &str = "Endpoint=sb://127.0.0.1;\
     SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;\
     UseDevelopmentEmulator=true;";

/// Where every flow ends up: the mock receiver's egress queue, non-session
/// per `spec/01-architecture.md` §4.1.
const MOCK_EGRESS: &str = "local-inthub-mpi-mock-receiver";

/// Where the **dedicated-sender** flows end up instead: the topic mock
/// receiver's verifier subscription, and the session its `.env` publishes
/// under (`local/mpi-hl7-topic-mock-receiver.env`). Paris and Mosaiq moved to
/// a topic-fed sender and the topic mock receiver, per
/// `spec/01-architecture.md` §2.1.3, so the queue above never sees them.
const TOPIC_MOCK_EGRESS: &str = "local-inthub-hl7-topic-mock-receiver";
const TOPIC_MOCK_SUBSCRIPTION: &str = "local-inthub-hl7-topic-mock-receiver-verifier";
const TOPIC_MOCK_SESSION: &str = "mock";

/// The compose file, relative to the workspace root.
const COMPOSE: &str = "local/docker-compose.yml";

/// How long a container may take to become healthy.
///
/// Generous: the Service Bus emulator waits on SQL before it opens, and
/// `spec/18-deployment.md` §5.4a's `restart: on-failure` can add a restart
/// cycle on top.
const HEALTHY_WITHIN: Duration = Duration::from_secs(240);

/// One flow: how to reach it, and what to send.
struct Flow {
    /// The compose profile that brings it up.
    profile: &'static str,
    /// The MLLP port published on the host.
    port: u16,
    /// The sample message.
    sample: &'static str,
    /// The control id in that sample, replaced per run so a queue left
    /// dirty by an earlier run cannot make this one pass
    /// (`spec/17-testing-and-quality.md` §9.2.1b).
    control_id: &'static str,
    /// Whether a transformer sits in the middle. Not used to weaken an
    /// assertion — recorded so a failure says which shape broke.
    transformed: bool,
    /// Whether the flow ends at the topic mock receiver rather than the
    /// queue one (`spec/01-architecture.md` §2.1.3).
    topic_sink: bool,
}

/// The five MLLP flows.
///
/// `lims-to-mpi` is absent: SOAP over HTTP needs a different client, and a
/// test that quietly covered four flows while claiming five would be worse
/// than one that says so.
const FLOWS: &[Flow] = &[
    Flow {
        profile: "phw-to-mpi",
        port: 2575,
        sample: include_str!("../../../local/sample_messages/phw-to-mpi.sample.hl7"),
        control_id: "202505052323364444444444",
        transformed: true,
        topic_sink: false,
    },
    Flow {
        profile: "pims-to-mpi",
        port: 2579,
        sample: include_str!("../../../local/sample_messages/pims-to-mpi.sample.hl7"),
        control_id: "48209218",
        transformed: true,
        topic_sink: false,
    },
    Flow {
        profile: "chemo-to-mpi",
        port: 2578,
        sample: include_str!("../../../local/sample_messages/chemocare-to-mpi.sample.hl7"),
        control_id: "951317629075403",
        transformed: true,
        topic_sink: false,
    },
    Flow {
        profile: "paris-to-mpi",
        port: 2577,
        sample: include_str!("../../../local/sample_messages/paris-to-mpi.sample.hl7"),
        control_id: "185352920250619115040",
        transformed: false,
        topic_sink: true,
    },
    Flow {
        profile: "mosaiq-to-mpi",
        port: 2583,
        sample: include_str!("../../../local/sample_messages/mosaiq-to-mpi.sample.hl7"),
        control_id: "185352920250619115040",
        transformed: false,
        topic_sink: true,
    },
];

/// A marker unique to this run.
fn marker(label: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos() % 1_000_000_000)
        .unwrap_or_default();
    format!("E2E{label}{nanos}")
}

/// The workspace root, from this crate's manifest.
fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root resolves")
}

/// Run `podman compose` with the given arguments.
fn compose(args: &[&str]) -> std::process::Output {
    let mut command = Command::new("podman");
    command
        .current_dir(root())
        .arg("compose")
        .arg("-f")
        .arg(COMPOSE);
    command.args(args);
    command.output().expect("podman is installed and on PATH")
}

/// Connect to the broker, retrying until it will actually talk.
///
/// `wait_for_health` cannot cover this. The Service Bus emulator container
/// declares **no health check**, so it reports a bare `Up` the moment the
/// process starts — and its AMQP port accepts TCP roughly ten seconds
/// before it will complete a SASL exchange, because it waits on SQL first.
/// A readiness check that believed either signal returns too early, and
/// the test then fails with `Waiting for SASL header exchange` as though
/// the broker were broken.
///
/// The only probe that answers the real question is a real connection.
/// Same lesson as `spec/17-testing-and-quality.md` §9.2.1e, from the other
/// side: there, TCP-open was too weak to prove readiness; here it is too
/// weak again.
async fn connect_when_ready(endpoint: &Endpoint) -> LiveBus {
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < HEALTHY_WITHIN {
        match LiveBus::connect(endpoint).await {
            Ok(bus) => return bus,
            Err(error) => {
                last = error.to_string();
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    panic!("the broker never became ready within {HEALTHY_WITHIN:?}; last error: {last}");
}

/// Bring up **one** profile, and wait for health.
///
/// One at a time, not all five at once. That was the first design and the
/// emulator refused it: `spec/14-buswatch.md` §6.1 records that the local
/// emulator has a hard connection quota, and five profiles is eleven
/// bus-connected containers plus BusWatch's fan-out. The broker then
/// closes new connections, and the host sees
/// `Connection must have been closed` — which is neither of the two
/// substrings §6.1 teaches the code to recognise, so nothing explains it.
///
/// Running one profile at a time also tests something the combined run
/// could not: that a profile is **self-sufficient**, which is what
/// `spec/18-deployment.md` §5.2 claims.
fn bring_up(profile: &str) {
    let output = compose(&["--profile", profile, "up", "-d"]);
    assert!(
        output.status.success(),
        "compose up failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    wait_for_health();
}

/// Every profile in `local/docker-compose.yml`, for tearing down.
///
/// `podman compose down` **only stops services in the profiles it is
/// given**. A bare `down` leaves every profile-scoped container running,
/// silently: the command succeeds, prints a network-removal line, and the
/// containers stay up. That defeated the one-profile-at-a-time rule this
/// file depends on — each run simply added to the last, until the
/// emulator's connection quota (`spec/14-buswatch.md` §6.1) refused the
/// next connection.
///
/// So every profile is named, every time.
/// Taken from `local/docker-compose.yml`; `compose` accepts a profile name
/// that does not exist **without complaint**, so a typo here does not fail
/// — it silently leaves those containers running. The assertion at the end
/// of `tear_down` is what actually catches that.
const ALL_PROFILES: &[&str] = &[
    "phw-to-mpi",
    "pims-to-mpi",
    "chemo-to-mpi",
    "paris-to-mpi",
    "mosaiq-to-mpi",
    "risp-to-mpi",
    "lims-to-mpi",
    "lims-to-wrrs",
    "lims-to-trrr",
    "mpi-to-topic",
    "wpas-to-proms",
    "dashboard",
    "replay",
];

/// Stop everything, so the next profile starts from nothing.
fn tear_down() {
    let mut args = Vec::new();
    for profile in ALL_PROFILES {
        args.push("--profile");
        args.push(profile);
    }
    args.extend_from_slice(&["down", "--remove-orphans"]);

    let output = compose(&args);
    assert!(
        output.status.success(),
        "compose down failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Believe the container list, not the exit code.
    let listed = Command::new("podman")
        .args(["ps", "--filter", "name=ihb-rs-", "--format", "{{.Names}}"])
        .output()
        .expect("podman ps runs");
    let left = String::from_utf8_lossy(&listed.stdout);
    let left: Vec<_> = left.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        left.is_empty(),
        "compose down reported success but left {} container(s) running: {left:?}",
        left.len()
    );
}

/// Wait until every container that declares a health check reports healthy.
///
/// Three consecutive clean readings, not one. The first version asked
/// `podman ps` once and returned as soon as nothing read `(starting)` —
/// which is true **before the application containers exist at all**. It
/// polled while only `sqledge` and `sb-emulator` were up, neither of which
/// declares a health check, saw no `(starting)`, and declared the stack
/// ready. The test then failed against a SOAP server that was still
/// restarting.
///
/// So: a reading is clean only if something reports `(healthy)` and
/// nothing reports `(starting)` or `(unhealthy)`, and it must stay clean.
fn wait_for_health() {
    /// Consecutive clean readings required.
    const SETTLED: usize = 3;

    let started = Instant::now();
    let mut clean = 0;

    loop {
        let output = Command::new("podman")
            .args([
                "ps",
                "--filter",
                "name=ihb-rs-",
                "--format",
                "{{.Names}}\t{{.Status}}",
            ])
            .output()
            .expect("podman ps runs");
        let last = String::from_utf8_lossy(&output.stdout);

        let unsettled = last
            .lines()
            .any(|line| line.contains("(starting)") || line.contains("(unhealthy)"));
        let any_healthy = last.lines().any(|line| line.contains("(healthy)"));

        if any_healthy && !unsettled {
            clean += 1;
            if clean >= SETTLED {
                return;
            }
        } else {
            clean = 0;
        }

        assert!(
            started.elapsed() <= HEALTHY_WITHIN,
            "containers did not become healthy within {HEALTHY_WITHIN:?}:\n{last}"
        );
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Send one MLLP frame and read the reply.
async fn send_mllp(port: u16, message: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| panic!("connecting to the ingress on {port}: {error}"));

    let mut frame = vec![0x0b];
    frame.extend_from_slice(message.as_bytes());
    frame.extend_from_slice(&[0x1c, 0x0d]);
    stream
        .write_all(&frame)
        .await
        .expect("the frame is written");

    let mut reply = vec![0_u8; 4096];
    let read = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut reply))
        .await
        .expect("the ingress replies within thirty seconds")
        .expect("the reply is readable");

    String::from_utf8_lossy(&reply[..read])
        .trim_matches(|c| c == '\x0b' || c == '\x1c' || c == '\r')
        .to_string()
}

/// A sample with its control id replaced, and its line endings made ER7's.
fn prepared(flow: &Flow, marker: &str) -> String {
    flow.sample
        .trim_end()
        .replace("\r\n", "\n")
        .replace('\n', "\r")
        .replace(flow.control_id, marker)
}

#[tokio::test(flavor = "multi_thread")]
async fn every_mllp_flow_carries_its_sample_through_the_containers() {
    let endpoint = Endpoint::from_config(Some(EMULATOR), None).expect("the endpoint parses");
    let mut carried = Vec::new();

    for flow in FLOWS {
        tear_down();
        bring_up(flow.profile);

        let bus = connect_when_ready(&endpoint).await;
        let marker = marker(flow.profile.split('-').next().unwrap_or("x"));

        let reply = send_mllp(flow.port, &prepared(flow, &marker)).await;
        assert!(
            reply.contains("MSA|AA"),
            "{}: the ingress did not accept the sample. Reply was:\n{reply}",
            flow.profile
        );
        assert!(
            reply.contains(&marker),
            "{}: the ACK does not carry the control id that was sent",
            flow.profile
        );

        // Drain the mock receiver's egress until this run's marker appears.
        // Everything received on the way is settled, so a queue left dirty
        // by an earlier run is cleaned up rather than getting in the way.
        let receiver = if flow.topic_sink {
            // The session may not exist until the first message lands, so
            // ask again for a while, as `phw_to_mpi` does for its sessions.
            let mut accepted = None;
            for _ in 0..240 {
                if let Ok(receiver) = bus
                    .subscription_receiver(
                        TOPIC_MOCK_EGRESS,
                        TOPIC_MOCK_SUBSCRIPTION,
                        TOPIC_MOCK_SESSION,
                    )
                    .await
                {
                    accepted = Some(receiver);
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            accepted.expect("the topic mock receiver's session became available")
        } else {
            bus.queue_receiver(MOCK_EGRESS)
                .await
                .expect("the mock egress queue exists")
        };

        // Twelve receives, not three. Each hop waits out its own receive
        // window on an idle queue (§5.1.2's sixty seconds), and there are
        // three hops: measured end to end at roughly **four minutes** on a
        // cold emulator. An earlier version of this test allowed a third
        // of that and reported a working flow as broken.
        let mut arrived = false;
        for _ in 0..12 {
            // No `tokio::time::timeout` around this receive: cancelling one
            // loses messages the broker has already handed over
            // (`spec/03-message-bus.md` §5.5).
            let batch = receiver.receive(50).await.expect("receiving works");
            for message in &batch {
                if String::from_utf8_lossy(&message.body).contains(&marker) {
                    arrived = true;
                }
                receiver
                    .settle(message, Outcome::Accepted)
                    .await
                    .expect("settling works");
            }
            if arrived {
                break;
            }
        }

        assert!(
            arrived,
            "{} ({}) did not deliver marker {marker} to the mock receiver",
            flow.profile,
            if flow.transformed {
                "with a transformer"
            } else {
                "straight to the sender"
            }
        );

        carried.push(flow.profile);
        let _ = bus.close().await;
    }

    tear_down();
    assert_eq!(carried.len(), FLOWS.len(), "every flow ran: {carried:?}");
}

/// The LIMS flow, which is shaped differently from the other five.
///
/// `spec/01-architecture.md` §2.1.3: LIMS → MPI runs
///
/// ```text
/// SOAP/HTTP → hl7-soap-server → topic local-inthub-lims-transformer-ingress
///                                 ├─ …-ingress-sub        → lims-mpi-hl7-sender
///                                 ├─ …-lims-wrrs-transformer-sub (A31/A40 only)
///                                 └─ …-ingress-trrr-sub   → lims-trrr-hl7-sender
/// ```
///
/// The assertion is taken at the **topic**, from the `-trrr-sub`
/// subscription. The `lims-to-mpi` profile runs `lims-mpi-hl7-sender`, which
/// owns `-ingress-sub` and would race this test for its messages; nothing in
/// that profile reads `-trrr-sub`, so a copy of every message waits there.
/// The session receiver is a *subscription* receiver, as the session-enabled
/// subscriptions of `local/ServiceBusEmulatorConfig.json` require.
#[tokio::test(flavor = "multi_thread")]
async fn the_lims_soap_flow_reaches_its_topic() {
    /// The SOAP ingress, published by `local/docker-compose.yml`.
    const SOAP_PORT: u16 = 8082;
    /// `SOAP_ENDPOINT_PATH` in `local/lims-soap-hl7-server.env`.
    const SOAP_PATH: &str = "/soap";
    /// Where the SOAP server writes (`EGRESS_TOPIC_NAME`).
    const LIMS_INGRESS: &str = "local-inthub-lims-transformer-ingress";
    /// A subscription no container of the `lims-to-mpi` profile consumes.
    const LIMS_SUBSCRIPTION: &str = "local-inthub-lims-transformer-ingress-trrr-sub";
    /// `EGRESS_SESSION_ID`.
    const LIMS_SESSION: &str = "lims-to-mpi";
    /// The control id in the sample, replaced per run.
    const SAMPLE_CONTROL_ID: &str = "6778031837018553261z82215";

    tear_down();
    bring_up("lims-to-mpi");

    let endpoint = Endpoint::from_config(Some(EMULATOR), None).expect("the endpoint parses");
    let bus = connect_when_ready(&endpoint).await;

    let marker = marker("lims");
    let envelope = include_str!("../../../local/sample_messages/lims-to-mpi.sample.xml")
        .replace(SAMPLE_CONTROL_ID, &marker);

    let response = post_soap(SOAP_PORT, SOAP_PATH, &envelope).await;
    assert!(
        response.contains("MSA") || response.contains("Envelope"),
        "the SOAP ingress did not answer with an envelope:\n{response}"
    );
    assert!(
        !response.contains("Fault"),
        "the SOAP ingress returned a fault:\n{response}"
    );

    // The session may not exist until the first message has been copied to
    // the subscription, so ask again for a while, as the MLLP flows do.
    let mut accepted = None;
    for _ in 0..60 {
        if let Ok(receiver) = bus
            .subscription_receiver(LIMS_INGRESS, LIMS_SUBSCRIPTION, LIMS_SESSION)
            .await
        {
            accepted = Some(receiver);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let receiver = accepted.expect("the LIMS topic's subscription session became available");

    let mut arrived = false;
    for _ in 0..8 {
        let batch = receiver.receive(50).await.expect("receiving works");
        for message in &batch {
            if String::from_utf8_lossy(&message.body).contains(&marker) {
                arrived = true;
            }
            receiver
                .settle(message, Outcome::Accepted)
                .await
                .expect("settling works");
        }
        if arrived {
            break;
        }
    }

    assert!(
        arrived,
        "the SOAP ingress accepted the message but {marker} never reached {LIMS_INGRESS}/{LIMS_SUBSCRIPTION}"
    );

    let _ = receiver.close().await;
    let _ = bus.close().await;
    tear_down();
}

/// The WPAS → PROMS flow, through the containers to the Promptly mock.
///
/// `wpas-rest-server` → topic → `proms-fhir-transformer` → queue →
/// `fhir-sender` → `promptly-mock-receiver`, which forwards what it accepted
/// to a plain queue (`spec/11-mock-receivers.md` §3.10.1). The assertion is
/// taken **there**, so it proves the Bundle was delivered through the whole
/// `$process-message` exchange (the sender completes a message only after the
/// poll ends in a non-error outcome), not merely that something was queued.
///
/// What this reaches that nothing else does: the PROMs transformer on a
/// **topic** ingress, `fhir-sender`'s session receiver against the emulator,
/// and the `%24process-message` URL surviving Compose.
#[tokio::test(flavor = "multi_thread")]
async fn the_wpas_flow_reaches_the_promptly_mock() {
    /// `local/docker-compose.yml`: `wpas-rest-server`'s published port.
    const WPAS_PORT: u16 = 8084;
    /// `ENDPOINT_PATH` in `local/wpas-rest-server.env`.
    const WPAS_PATH: &str = "/ingest";
    /// `EGRESS_QUEUE_NAME` in `local/promptly-mock-receiver.env`.
    const MOCK_QUEUE: &str = "local-inthub-promptly-mock-receiver";
    /// `EGRESS_SESSION_ID` in `local/promptly-mock-receiver.env`.
    const MOCK_SESSION: &str = "wpas-to-proms";
    /// The patient reference in the sample, replaced per run.
    const SAMPLE_CRN: &str = "PROMSTEST1";

    tear_down();
    bring_up("wpas-to-proms");

    let endpoint = Endpoint::from_config(Some(EMULATOR), None).expect("the endpoint parses");
    let bus = connect_when_ready(&endpoint).await;

    let marker = marker("wpas");
    let body = include_str!("../../../local/sample_messages/wpas-to-proms.sample.xml")
        .replace(SAMPLE_CRN, &marker);

    let response = post_xml(WPAS_PORT, WPAS_PATH, &body).await;
    assert!(
        response.starts_with("HTTP/1.1 2"),
        "the WPAS ingress did not accept the message:\n{response}"
    );

    // Each hop waits out its own receive window on a cold queue, so this
    // takes minutes (CLAUDE.md), not seconds.
    // A session queue (`spec/01-architecture.md` §4.1 allows two that are
    // not), and the session may not exist until the first message does.
    let mut opened = None;
    for _ in 0..120 {
        if let Ok(receiver) = bus.session_receiver(MOCK_QUEUE, MOCK_SESSION).await {
            opened = Some(receiver);
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let receiver = opened.expect("the mock's queue session became available");
    let mut arrived = false;
    for _ in 0..6 {
        let batch = receiver.receive(50).await.expect("receiving works");
        for message in &batch {
            let text = String::from_utf8_lossy(&message.body);
            if text.contains(&marker) {
                assert!(
                    text.contains("\"resourceType\":\"Bundle\"")
                        || text.contains("\"resourceType\": \"Bundle\""),
                    "what reached the mock is not a FHIR Bundle:\n{text}"
                );
                arrived = true;
            }
            receiver
                .settle(message, Outcome::Accepted)
                .await
                .expect("settling works");
        }
        if arrived {
            break;
        }
    }

    assert!(
        arrived,
        "the WPAS ingress accepted the message but {marker} never reached the Promptly mock's queue {MOCK_QUEUE}"
    );

    let _ = receiver.close().await;
    let _ = bus.close().await;
    tear_down();
}

/// `POST` XML and return the whole response, status line included.
async fn post_xml(port: u16, path: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| panic!("connecting to the WPAS ingress on {port}: {error}"));
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/xml\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("the request is written");

    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut reply))
        .await
        .expect("the WPAS ingress replies within thirty seconds")
        .expect("the reply is readable");
    String::from_utf8_lossy(&reply).into_owned()
}

/// `POST` a SOAP envelope and return the body.
async fn post_soap(port: u16, path: &str, envelope: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| panic!("connecting to the SOAP ingress on {port}: {error}"));

    // Written by hand rather than with an HTTP client: this crate has no
    // dependency on one, and the request is four headers.
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: text/xml; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{envelope}",
        envelope.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("the request is written");

    let mut body = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut body))
        .await
        .expect("the SOAP ingress replies within thirty seconds")
        .expect("the reply is readable");

    String::from_utf8_lossy(&body).into_owned()
}
