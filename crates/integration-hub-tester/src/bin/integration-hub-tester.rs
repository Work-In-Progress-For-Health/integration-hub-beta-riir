//! The `integration-hub-tester` binary.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §6. No
//! `tracing` subscriber is installed (§1.3).

use std::process::ExitCode;

fn main() -> ExitCode {
    // `MSH-7` of a generated ACK is local time (`spec/index.md` §0.7), and
    // the offset can only be read before the process has threads.
    ihb_fields::timestamp::capture_local_offset();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = integration_hub_tester::cli::run(
        &args,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
