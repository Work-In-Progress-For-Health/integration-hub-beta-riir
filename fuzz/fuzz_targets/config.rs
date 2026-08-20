//! Configuration parsing must never panic, and must never print a secret.
//!
//! Implements `spec/17-testing-and-quality.md` §10.2 for
//! `spec/02-configuration.md` §1 and §10.4.
//!
//! Configuration arrives from a container definition, which is a templating
//! system with a human at the end of it. A value that is blank, enormous, or
//! full of control characters is a Tuesday, and a service that panics while
//! reading one crash-loops without ever saying why.
//!
//! The second property is the one worth fuzzing: a [`Secret`] must render as
//! `<redacted>` whatever it wraps, including values that look like format
//! strings or contain the mask itself.

#![no_main]

use ihb_config::{Reader, Secret};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // Property 1: every reader returns, for any value.
    let mut reader = Reader::from_pairs([
        ("WORKFLOW_ID", text),
        ("PORT", text),
        ("MAX_MESSAGES_PER_MINUTE", text),
        ("SENDING_APP", text),
        ("REPLAY_BATCH_ID", text),
        ("HL7_LOG_REDACTION_ENABLED", text),
    ]);

    let _ = reader.required_string("WORKFLOW_ID");
    let _ = reader.integer_or::<u16>("PORT", 2575);
    let _ = reader.optional_positive_integer::<u32>("MAX_MESSAGES_PER_MINUTE");
    let _ = reader.list("SENDING_APP");
    let _ = reader.required_uuid("REPLAY_BATCH_ID");
    let _ = reader.bool_or("HL7_LOG_REDACTION_ENABLED", true);
    let _ = reader.optional_one_of("MISSING", &["2.4", "2.5"]);
    let _ = reader.finish();

    // Property 2: a secret never renders its value, whatever it is.
    let secret = Secret::new(text);
    let debug = format!("{secret:?}");
    let display = secret.to_string();

    assert_eq!(debug, "<redacted>", "Debug rendered a secret");
    assert_eq!(display, "<redacted>", "Display rendered a secret");

    // ...including inside a derived Debug, which is how it is actually used.
    #[derive(Debug)]
    #[allow(dead_code)]
    struct Config {
        secret: Secret,
    }
    let rendered = format!("{:?}", Config { secret });
    if text.len() > 8 && !text.contains("redacted") {
        assert!(
            !rendered.contains(text),
            "a derived Debug printed the secret"
        );
    }
});
