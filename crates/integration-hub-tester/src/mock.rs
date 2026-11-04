//! Launching a mock receiver for the SOAP senders to post to.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §7.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Which mock receiver (§7.1). The ports are those the receivers listen on
/// by default; this launcher passes no configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `hl7-mock-receiver`, MLLP on 2576.
    Mllp,
    /// `http-mock-receiver`, SOAP on 8080.
    Soap,
}

impl Mode {
    /// `mllp` or `soap` (§7.1); anything else is §7.2's usage error.
    ///
    /// # Errors
    ///
    /// A message naming the two valid modes.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "mllp" => Ok(Self::Mllp),
            "soap" => Ok(Self::Soap),
            other => Err(format!(
                "unknown mock mode '{other}': expected 'mllp' or 'soap'"
            )),
        }
    }

    /// The binary's name, without the platform's executable suffix.
    #[must_use]
    pub const fn binary(self) -> &'static str {
        match self {
            Self::Mllp => "hl7-mock-receiver",
            Self::Soap => "http-mock-receiver",
        }
    }

    /// Human-readable name and port, for error and status lines.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mllp => "MLLP mock receiver (port 2576)",
            Self::Soap => "SOAP mock receiver (port 8080)",
        }
    }
}

/// Where to run the binary from: beside `exe_dir` if it is there, else the
/// bare name for the platform's `PATH` lookup (§7.1).
#[must_use]
pub fn resolve(mode: Mode, exe_dir: Option<&Path>) -> PathBuf {
    let name = format!("{}{}", mode.binary(), std::env::consts::EXE_SUFFIX);
    exe_dir
        .map(|dir| dir.join(&name))
        .filter(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Start the receiver and wait for it.
///
/// # Errors
///
/// A message that includes the `cargo build` command, when the binary
/// cannot be started.
pub fn run(mode: Mode) -> Result<i32, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let program = resolve(mode, exe_dir.as_deref());
    let status = Command::new(&program).status().map_err(|error| {
        format!(
            "could not start {} ({error}); build it with `cargo build -p {}`",
            mode.label(),
            mode.binary()
        )
    })?;
    Ok(status.code().unwrap_or(1))
}
