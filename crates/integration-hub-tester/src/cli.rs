//! The command line.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §6.

use std::io::{Read, Write};

use crate::mock;
use crate::plugin::{Outcome, Plugin};
use crate::registry::{categories, find};

/// What `--help` and every usage error print. The exit-status line is part
/// of the contract (§6.5): scripts distinguish 1 (the component said no, or
/// could not be asked) from 2 (the command line was wrong).
const USAGE: &str = "\
integration-hub-tester: preview one Integration Hub component on one message

USAGE:
    integration-hub-tester list
    integration-hub-tester samples <plugin>
    integration-hub-tester run <plugin> [--sample NAME | --file PATH] [--output PATH]
    integration-hub-tester mock <mllp|soap>

`run` reads standard input when neither --sample nor --file is given.
Exit status: 0 accepted, 1 rejected or could not run, 2 usage error.";

/// Pick a sample by case-insensitive exact name, else unique substring (§6.3).
///
/// # Errors
///
/// No match, or more than one.
pub fn pick_sample(plugin: &dyn Plugin, wanted: &str) -> Result<&'static str, String> {
    let wanted = wanted.to_lowercase();
    let samples = plugin.samples();
    if let Some((_, text)) = samples
        .iter()
        .find(|(name, _)| name.to_lowercase() == wanted)
    {
        return Ok(text);
    }
    let hits: Vec<_> = samples
        .iter()
        .filter(|(name, _)| name.to_lowercase().contains(&wanted))
        .collect();
    match hits.as_slice() {
        [(_, text)] => Ok(text),
        [] => Err(format!(
            "no sample of '{}' matches '{wanted}'; try `samples {}`",
            plugin.id(),
            plugin.id()
        )),
        many => Err(format!(
            "'{wanted}' matches {} samples of '{}': {}",
            many.len(),
            plugin.id(),
            many.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("; ")
        )),
    }
}

/// Run the command line. Returns the exit status (§6.5).
///
/// `stdin`, `out` and `err` are parameters so the tests drive the whole
/// thing without a process.
pub fn run(args: &[String], stdin: &mut dyn Read, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let mut usage = |message: &str| {
        let _ = writeln!(err, "error: {message}\n\n{USAGE}");
        2
    };
    let Some(command) = args.first().map(String::as_str) else {
        return usage("no command given");
    };
    match command {
        "-h" | "--help" | "help" => {
            let _ = writeln!(out, "{USAGE}");
            0
        }
        "list" => {
            for category in categories() {
                let _ = writeln!(out, "{}", category.name);
                for plugin in &category.plugins {
                    let _ = writeln!(out, "  {:<26} {}", plugin.id(), plugin.description());
                }
            }
            0
        }
        "samples" => {
            let Some(plugin) = args.get(1).and_then(|id| find(id)) else {
                return usage("`samples` needs a known plugin id (see `list`)");
            };
            for (name, _) in plugin.samples() {
                let _ = writeln!(out, "{name}");
            }
            0
        }
        "mock" => {
            let mode = match args.get(1).map(|m| mock::Mode::parse(m)) {
                Some(Ok(mode)) => mode,
                Some(Err(message)) => return usage(&message),
                None => return usage("`mock` needs 'mllp' or 'soap'"),
            };
            match mock::run(mode) {
                Ok(code) => code,
                Err(message) => {
                    let _ = writeln!(err, "error: {message}");
                    1
                }
            }
        }
        // `args[0]` is `run`; with nothing after it `run_plugin` answers with usage.
        "run" => run_plugin(args.get(1..).unwrap_or(&[]), stdin, out, err),
        other => usage(&format!("unknown command '{other}'")),
    }
}

/// The `run` command (§6.3): pick the input (sample, file or standard
/// input), run the plugin, write the output to `out` and the summary to
/// `err` (§6.5).
///
/// Returns the exit status: 0 for an accepted outcome, 1 for a rejected one or
/// a plugin `Err`, 2 for a usage error. `stdin` is read only when neither
/// `--sample` nor `--file` is given.
fn run_plugin(
    args: &[String],
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut usage = |message: &str| {
        let _ = writeln!(err, "error: {message}\n\n{USAGE}");
        2
    };
    let Some(plugin) = args.first().and_then(|id| find(id)) else {
        return usage("`run` needs a known plugin id (see `list`)");
    };

    let (mut sample, mut file, mut output) = (None, None, None);
    let mut rest = args.get(1..).unwrap_or(&[]).iter();
    while let Some(flag) = rest.next() {
        let slot = match flag.as_str() {
            "--sample" => &mut sample,
            "--file" => &mut file,
            "--output" => &mut output,
            other => return usage(&format!("unknown option '{other}'")),
        };
        let Some(value) = rest.next() else {
            return usage(&format!("{flag} needs a value"));
        };
        *slot = Some(value.clone());
    }

    // §6.3 order of precedence: sample, then file, then standard input.
    let input = if let Some(name) = sample {
        match pick_sample(plugin.as_ref(), &name) {
            Ok(text) => text.to_string(),
            Err(message) => return usage(&message),
        }
    } else if let Some(path) = file {
        match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                let _ = writeln!(err, "error: could not read {path}: {error}");
                return 1;
            }
        }
    } else {
        let mut text = String::new();
        if let Err(error) = stdin.read_to_string(&mut text) {
            let _ = writeln!(err, "error: could not read standard input: {error}");
            return 1;
        }
        text
    };

    let outcome = match plugin.run(&input) {
        Ok(outcome) => outcome,
        Err(reason) => Outcome::rejected(String::new(), reason),
    };

    let _ = writeln!(out, "{}", outcome.output);
    let _ = writeln!(err, "{}", outcome.summary);
    if let Some(path) = output {
        // §6.4: the only way anything reaches the disk.
        if let Err(error) = std::fs::write(&path, &outcome.output) {
            let _ = writeln!(err, "error: could not write {path}: {error}");
            return 1;
        }
    }
    i32::from(!outcome.ok)
}
