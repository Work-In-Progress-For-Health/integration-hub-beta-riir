//! The `ultra7` binary: the terminal front end of `spec/ultra7-test-message-sender.md` `§9`.
//!
//! Output goes to stdout and stderr only, never to a log sink (`§1.3`).

// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): see `lib.rs`.
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use ultra7::export::{extension_for, sanitize_filename, unique_filename};
use ultra7::format::{detect_format, pretty_print};
use ultra7::model::MessageFormat;
use ultra7::run::{Event, send_enabled, send_selected_once};
use ultra7::storage::{ProjectStore, default_projects_dir};

/// The usage text (`§9.1`).
const USAGE: &str = "usage: ultra7 [--dir DIR] <command>
  projects
  send PROJECT [--repeat N] [--delay-ms N] [--only NAME]...
  export PROJECT OUT_DIR
  detect FILE
  format FILE";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir = default_projects_dir();
    if args.first().is_some_and(|a| a == "--dir") {
        if args.len() < 2 {
            return usage();
        }
        dir = PathBuf::from(args.remove(1));
        args.remove(0);
    }
    let store = ProjectStore::new(dir);
    let Some((command, rest)) = args.split_first() else {
        return usage();
    };
    let outcome = match command.as_str() {
        "projects" => projects(&store),
        "send" => send(&store, rest),
        "export" => export(&store, rest),
        "detect" => detect(rest),
        "format" => format_file(rest),
        _ => return usage(),
    };
    match outcome {
        Ok(code) => code,
        Err(Failure::Usage) => usage(),
        Err(Failure::Error(e)) => {
            eprintln!("ultra7: {e}");
            ExitCode::from(1)
        }
    }
}

/// Why a command failed (`§9.2`). Split in two because the exit status
/// differs: a usage mistake is 2, everything else 1, and a CI script can
/// tell "I called it wrongly" from "the send failed".
enum Failure {
    /// Exit 2.
    Usage,
    /// Exit 1; a reason, never message content.
    Error(String),
}

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(e: E) -> Self {
        Self::Error(e.to_string())
    }
}

/// Print usage to stderr and return exit status 2 (`§9.2`).
fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// `ultra7 projects`: list stored project names, one per line, sorted (`§3.4`).
fn projects(store: &ProjectStore) -> Result<ExitCode, Failure> {
    for name in store.list_projects()? {
        println!("{name}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `ultra7 send` (`§8`): flags override the project's own repeat and delay.
///
/// Exits 1 when any send failed (`§9.2`), so a shell loop can gate on it.
/// `--only` selects by message name and bypasses `enabled`, repeat and delay
/// (`§8.3`). The cancel flag is never set: the binary has no signal handler,
/// and Ctrl-C ends the process, which is enough for a terminal tool.
fn send(store: &ProjectStore, args: &[String]) -> Result<ExitCode, Failure> {
    let (name, mut rest) = args.split_first().ok_or(Failure::Usage)?;
    let project = store.load(name)?;
    let mut repeat = project.repeat_count;
    let mut delay = project.delay_ms;
    let mut only: Vec<String> = Vec::new();
    while let Some((flag, tail)) = rest.split_first() {
        let (value, tail) = tail.split_first().ok_or(Failure::Usage)?;
        match flag.as_str() {
            "--repeat" => repeat = value.parse().map_err(|_| Failure::Usage)?,
            "--delay-ms" => delay = value.parse().map_err(|_| Failure::Usage)?,
            "--only" => only.push(value.clone()),
            _ => return Err(Failure::Usage),
        }
        rest = tail;
    }
    let cancel = AtomicBool::new(false);
    let mut sink = |event: Event<'_>| match event {
        Event::Info(line) => println!("{line}"),
        Event::Result(label, r) => {
            let status = if r.ok { "OK" } else { "ERROR" };
            let detail = r.error.as_deref().unwrap_or(&r.response_summary);
            println!("{label} {status} {:.0}ms {detail}\n", r.latency_ms);
        }
    };
    let summary = if only.is_empty() {
        send_enabled(&project, repeat, delay, &cancel, &mut sink)?
    } else {
        send_selected_once(&project, &only, &cancel, &mut sink)?
    };
    Ok(if summary.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `ultra7 export` (`§6`): one file per message, named from the message name.
///
/// `unique_filename` is given a shared `used` set so two messages with the
/// same name do not overwrite each other. Prints each file name written.
fn export(store: &ProjectStore, args: &[String]) -> Result<ExitCode, Failure> {
    let [name, out] = args else {
        return Err(Failure::Usage);
    };
    let project = store.load(name)?;
    std::fs::create_dir_all(out)?;
    let mut used = std::collections::BTreeSet::new();
    for m in &project.messages {
        let file = unique_filename(
            &sanitize_filename(&m.name),
            extension_for(m.format),
            &mut used,
        );
        std::fs::write(PathBuf::from(out).join(&file), &m.content)?;
        println!("{file}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `ultra7 detect` (`§5.1`): print `hl7`, `xml` or `json` for a file.
fn detect(args: &[String]) -> Result<ExitCode, Failure> {
    let [file] = args else {
        return Err(Failure::Usage);
    };
    let text = std::fs::read_to_string(file)?;
    println!(
        "{}",
        match detect_format(&text) {
            MessageFormat::Hl7 => "hl7",
            MessageFormat::Xml => "xml",
            MessageFormat::Json => "json",
        }
    );
    Ok(ExitCode::SUCCESS)
}

/// `ultra7 format` (`§5.2`): pretty-print a JSON or XML file to stdout.
///
/// An HL7 file is an error, not a pass-through (`§5.2`).
fn format_file(args: &[String]) -> Result<ExitCode, Failure> {
    let [file] = args else {
        return Err(Failure::Usage);
    };
    let text = std::fs::read_to_string(file)?;
    println!("{}", pretty_print(&text, detect_format(&text))?);
    Ok(ExitCode::SUCCESS)
}
