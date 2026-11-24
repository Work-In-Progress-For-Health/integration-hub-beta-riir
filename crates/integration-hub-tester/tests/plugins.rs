//! One test per plugin, and the sample invariant.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §4 and §5.2.
//! The Python tester has no tests (§8.1), so these are written from the
//! specification rather than ported.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use integration_hub_tester::plugin::{Plugin, to_er7};
use integration_hub_tester::plugins::proms::PromsPlugin;
use integration_hub_tester::plugins::senders::{Hl7SenderAckPlugin, Hl7SenderPlugin};
use integration_hub_tester::plugins::senders::{SoapSenderPlugin, mllp_frame};
use integration_hub_tester::plugins::servers::{
    Hl7ServerPlugin, Hl7SoapServerPlugin, RestServerPlugin,
};
use integration_hub_tester::plugins::transformers::{
    ChemoPlugin, CoreReferencePlugin, PhwPlugin, PimsPlugin,
};
use integration_hub_tester::registry::categories;

/// What §5.2 says each sample's `ok` must be: its name's promise.
fn expected_ok(id: &str, sample: &str) -> bool {
    match id {
        "hl7-server" | "rest-server" | "hl7-soap-server" => sample.starts_with("Valid"),
        "hl7-sender-ack" => sample.starts_with("AA"),
        _ => true,
    }
}

fn sample(plugin: &dyn Plugin, name: &str) -> &'static str {
    plugin
        .samples()
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no sample {name}"))
        .1
}

#[test]
fn every_sample_runs_and_keeps_its_names_promise() {
    // §5.2. The two SOAP senders post to a URL, so they have their own
    // test (`tests/soap_post.rs`) and nothing here touches the network.
    for category in categories() {
        for plugin in category.plugins {
            if plugin.id().starts_with("soap-") {
                continue;
            }
            assert!(
                !plugin.samples().is_empty(),
                "{} has no samples",
                plugin.id()
            );
            for (name, text) in plugin.samples() {
                let outcome = plugin
                    .run(text)
                    .unwrap_or_else(|e| panic!("{} / {name}: {e}", plugin.id()));
                assert_eq!(
                    outcome.ok,
                    expected_ok(plugin.id(), name),
                    "{} / {name}: {}",
                    plugin.id(),
                    outcome.summary
                );
                assert!(
                    outcome
                        .summary
                        .starts_with(if outcome.ok { "ok: " } else { "failed: " }),
                    "{} / {name}: {}",
                    plugin.id(),
                    outcome.summary
                );
            }
        }
    }
}

#[test]
fn plugin_ids_are_unique() {
    // §2.5 — the id is the command-line name.
    let mut ids: Vec<&str> = Vec::new();
    let all = categories();
    for category in &all {
        for plugin in &category.plugins {
            ids.push(plugin.id());
        }
    }
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "{ids:?}");
    assert_eq!(ids.len(), 12, "§2.5 lists twelve plugins");
}

#[test]
fn samples_carry_no_real_looking_nhs_numbers() {
    // §5.1 — the Python's samples carried 9434... numbers, which are from
    // the live range. None may come back.
    for category in categories() {
        for plugin in category.plugins {
            for (name, text) in plugin.samples() {
                assert!(!text.contains("94347"), "{} / {name}", plugin.id());
                assert!(!text.contains("943476"), "{} / {name}", plugin.id());
            }
        }
    }
}

#[test]
fn phw_reports_the_timestamp_it_normalised() {
    // §4.1 and spec/09-transformers.md §3.5.
    let phw = PhwPlugin;
    let outcome = phw.run(sample(&phw, "A28 (PHW fixture)")).unwrap();
    assert!(outcome.ok);
    assert!(
        outcome
            .summary
            .contains("DateTime transformed from 2025-05-05 23:23:28 to 20250505232328"),
        "{}",
        outcome.summary
    );
    assert!(
        outcome.output.contains("20250505232328"),
        "{}",
        outcome.output
    );
    assert!(
        !outcome.output.contains('\r'),
        "output is shown with newlines"
    );
}

#[test]
fn chemo_and_pims_transform_their_samples() {
    // §4.1, §4.2.
    let chemo = ChemoPlugin;
    let out = chemo.run(sample(&chemo, "A28 Velindre")).unwrap();
    assert_eq!(out.summary, "ok: Chemocare transformation applied");
    assert!(out.output.starts_with("MSH|"), "{}", out.output);

    let pims = PimsPlugin;
    let out = pims.run(sample(&pims, "A40 (Merge)")).unwrap();
    assert_eq!(out.summary, "ok: PIMS transformation applied");
    assert!(out.output.contains("MRG|"), "{}", out.output);
}

#[test]
fn a_transformer_that_cannot_parse_is_a_tester_error() {
    // §4.3 and §2.3 — Err, not a rejected outcome.
    for plugin in [
        &PhwPlugin as &dyn Plugin,
        &ChemoPlugin,
        &PimsPlugin,
        &CoreReferencePlugin,
    ] {
        let error = plugin.run("THIS IS NOT HL7").unwrap_err();
        assert!(
            error.contains("could not parse"),
            "{}: {error}",
            plugin.id()
        );
    }
}

#[test]
fn core_reference_translates_six_fields_from_the_demo_table() {
    // §4.4 — and nothing is blanked for want of a row.
    let plugin = CoreReferencePlugin;
    let out = plugin
        .run(sample(&plugin, "A28 (all 6 fields populated)"))
        .unwrap();
    assert!(out.ok);
    assert!(out.summary.contains("demo"), "{}", out.summary);
    let pid = out.output.lines().find(|l| l.starts_with("PID")).unwrap();
    for translated in ["|M|", "Mr", "eng", "REL22", "ETH33"] {
        assert!(pid.contains(translated), "{translated} missing from {pid}");
    }
}

#[test]
fn core_reference_leaves_an_out_of_scope_trigger_alone() {
    // §4.4.
    let plugin = CoreReferencePlugin;
    let input = sample(&plugin, "A04 (out of scope - passes through)");
    let out = plugin.run(input).unwrap();
    assert_eq!(
        out.summary,
        "ok: A04 is out of scope - passed through unchanged"
    );
    assert_eq!(out.output, input);
}

#[test]
fn proms_builds_a_bundle_for_every_supported_event_code() {
    // §4.5 — one sample per routed code of spec/16-proms-transformer.md §2.4.1,
    // the four `TEMP` placeholders included.
    let plugin = PromsPlugin;
    assert_eq!(plugin.samples().len(), 10);
    for (name, text) in plugin.samples() {
        let out = plugin.run(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            out.summary.contains("MessageHeader"),
            "{name}: {}",
            out.summary
        );
        assert!(out.summary.contains("Patient"), "{name}: {}", out.summary);
        let value: serde_json::Value = serde_json::from_str(&out.output).unwrap();
        assert_eq!(value["resourceType"], "Bundle", "{name}");
    }
}

#[test]
fn proms_rejects_an_unknown_event_code_as_a_tester_error() {
    // §2.3 — the transformer could not route it.
    let plugin = PromsPlugin;
    let xml =
        sample(&plugin, "REFERRAL (Referral)").replace("<eventCode>REFERRAL<", "<eventCode>NOPE<");
    assert!(plugin.run(&xml).is_err());
}

#[test]
fn hl7_server_accepts_a_valid_message_with_an_aa() {
    // §4.6 step 4.
    let plugin = Hl7ServerPlugin;
    let out = plugin.run(sample(&plugin, "Valid A28 (v2.5)")).unwrap();
    assert!(out.ok, "{}", out.summary);
    assert!(out.output.contains("PARSED MESSAGE SUMMARY"));
    assert!(
        out.output.contains("MSA|AA|202505052323326666666666"),
        "{}",
        out.output
    );
}

#[test]
fn hl7_server_answers_a_wrong_version_with_an_ar() {
    // §4.6 step 3 and spec/07-hl7-server.md §4.3: validation is AR.
    let plugin = Hl7ServerPlugin;
    let out = plugin
        .run(sample(&plugin, "Wrong version (A31 v2.3) -> AR NACK"))
        .unwrap();
    assert!(!out.ok);
    assert!(out.summary.contains("AR"), "{}", out.summary);
    assert!(
        out.output.contains("Message has wrong version"),
        "{}",
        out.output
    );
    assert!(out.output.contains("MSA|AR|"), "{}", out.output);
}

#[test]
fn hl7_server_answers_an_unparsable_message_with_a_generic_ae() {
    // §4.6 step 1 and spec/07-hl7-server.md §4.3.2.
    let plugin = Hl7ServerPlugin;
    let out = plugin
        .run(sample(&plugin, "Unparsable -> AE NACK (generic)"))
        .unwrap();
    assert!(!out.ok);
    assert!(out.output.contains("PARSE FAILURE"), "{}", out.output);
    assert!(out.output.contains("MSA|AE|"), "{}", out.output);
}

#[test]
fn hl7_server_refuses_a_message_type_it_does_not_handle() {
    // §4.6 step 3 — the allow-list of spec/07-hl7-server.md §7.1 is applied
    // as the pipeline applies it, which the Python preview did not.
    let plugin = Hl7ServerPlugin;
    let a01 = "MSH|^~\\&|1|1|2|2|20250101||ADT^A01^ADT_A01|M1|P|2.5\rPID|1";
    let out = plugin.run(a01).unwrap();
    assert!(!out.ok);
    assert!(
        out.output.contains("Unsupported Message Type"),
        "{}",
        out.output
    );
    assert!(out.output.contains("MSA|AE|"), "{}", out.output);
}

#[test]
fn rest_server_creates_for_a_valid_message_and_shows_the_json_body() {
    // §4.7.
    let plugin = RestServerPlugin;
    let out = plugin.run(sample(&plugin, "Valid A28 (v2.5)")).unwrap();
    assert!(out.ok, "{}", out.summary);
    assert!(out.output.contains("\"messageContent\""));
    assert!(out.output.contains("201 Created - ACK"));
    assert!(out.output.contains("MSA|AA|"), "{}", out.output);
}

#[test]
fn rest_server_answers_a_malformed_message_with_a_500_nack() {
    // §4.7 and spec/19-rest-servers.md §2.
    let plugin = RestServerPlugin;
    let out = plugin
        .run(sample(&plugin, "Malformed (unparsable)"))
        .unwrap();
    assert!(!out.ok);
    assert!(out.summary.starts_with("failed: 500"), "{}", out.summary);
    assert!(out.output.contains("MSA|AE|"), "{}", out.output);
}

#[test]
fn rest_server_accepts_v2_xml_as_well_as_er7() {
    // §4.7 — `input::to_er7` is the real adapter.
    let xml = ihb_validation::er7_to_xml(&to_er7(Hl7ServerSample::A28)).unwrap();
    let out = RestServerPlugin.run(&xml).unwrap();
    assert!(out.ok, "{}\n{}", out.summary, out.output);
}

/// The valid A28, without reaching into the plugin's private constants.
struct Hl7ServerSample;
impl Hl7ServerSample {
    const A28: &'static str = "MSH|^~\\&|252|252|100|100|20250505232328||ADT^A28^ADT_A05|M1|P|2.5|||||GBR||EN\rEVN||20250502102000|20250505232328|||20250505232328\rPID|||8888888^^^252^PI~9990000018^^^NHS^NH||TESTSURNAME^TESTFIRST^TESTMIDDLE^^MR||19870101|M|||ADDRESS1^ADDRESS2^ADDRESS3^ADDRESS4^XX99 9XX^^H|||||||||||||||01\rPD1|||^^W99999^|G7777777\rPV1||U";
}

#[test]
fn soap_server_acknowledges_a_valid_message_with_a_200() {
    // §4.8.
    let plugin = Hl7SoapServerPlugin;
    let out = plugin
        .run(sample(&plugin, "Valid ADT_A05 (authority 328)"))
        .unwrap();
    assert!(out.ok, "{}\n{}", out.summary, out.output);
    assert!(
        out.output.contains("HTTP 200 - SOAP RESPONSE"),
        "{}",
        out.output
    );
}

#[test]
fn soap_server_pairs_each_failure_with_its_status() {
    // §4.8 and spec/08-hl7-soap-server.md §3.1: 400 do not retry, 403 talk
    // to someone.
    let plugin = Hl7SoapServerPlugin;
    let bad = plugin
        .run(sample(&plugin, "Malformed SOAP request"))
        .unwrap();
    assert!(!bad.ok);
    assert!(bad.summary.contains("400"), "{}", bad.summary);

    let unknown = plugin
        .run(sample(&plugin, "Unknown assigning authority (999)"))
        .unwrap();
    assert!(!unknown.ok);
    assert!(unknown.summary.contains("403"), "{}", unknown.summary);
    assert!(
        unknown.output.contains("not authorised"),
        "{}",
        unknown.output
    );
}

#[test]
fn mllp_frame_is_vt_payload_fs_cr() {
    // §4.9 and spec/10-senders.md §3.2.
    assert_eq!(mllp_frame(b"MSH"), vec![0x0B, b'M', b'S', b'H', 0x1C, 0x0D]);
    assert_eq!(mllp_frame(b""), vec![0x0B, 0x1C, 0x0D]);
}

#[test]
fn hl7_sender_previews_the_frame_without_connecting() {
    // §4.9 — the byte count is payload plus three.
    let plugin = Hl7SenderPlugin;
    let input = sample(&plugin, "ADT A01 (Inpatient admit)");
    let out = plugin.run(input).unwrap();
    let payload = to_er7(input).len();
    assert!(out.ok);
    assert_eq!(
        out.summary,
        format!("ok: MLLP frame preview - {} bytes total", payload + 3)
    );
    assert!(
        out.output
            .contains(&format!("total: {} bytes", payload + 3))
    );
    assert!(out.output.contains("Start : 0B"));
    assert!(out.output.contains("End   : 1C0D"));
}

#[test]
fn hl7_sender_wants_a_parsable_message() {
    // §4.9 — "The input must parse."
    assert!(Hl7SenderPlugin.run("NOT HL7").is_err());
}

#[test]
fn sender_ack_classifies_all_four_outcomes() {
    // §4.10 and spec/10-senders.md §3.7.1.
    let plugin = Hl7SenderAckPlugin;
    let expect = [
        ("AA - success", "Outcome        : SUCCESS", true),
        ("AE - recoverable, retried", "Outcome        : AE", false),
        (
            "AR - non-recoverable, dead-lettered",
            "Outcome        : AR",
            false,
        ),
        ("Malformed response", "Outcome        : INVALID", false),
    ];
    for (name, line, ok) in expect {
        let out = plugin.run(sample(&plugin, name)).unwrap();
        assert!(out.output.contains(line), "{name}: {}", out.output);
        assert_eq!(out.ok, ok, "{name}");
    }
    let ar = plugin
        .run(sample(&plugin, "AR - non-recoverable, dead-lettered"))
        .unwrap();
    assert!(
        ar.output.contains("dead-lettered, not retried"),
        "{}",
        ar.output
    );
    assert!(ar.output.contains("MSA-1 ack code : AR"), "{}", ar.output);
    assert!(
        ar.output.contains("Control ID     : MSG000002"),
        "{}",
        ar.output
    );
}

#[test]
fn input_normalisation_trims_and_uses_cr() {
    // §2.4.
    assert_eq!(to_er7("  A\r\nB\nC  \n"), "A\rB\rC");
    assert_eq!(integration_hub_tester::plugin::display("A\rB"), "A\nB");
}

#[test]
fn soap_sender_refuses_a_non_http_scheme_before_opening_anything() {
    // spec/integration-hub-tester-component-preview.md §4.11.1 — upstream's
    // Bandit B310 fix: `file://` and `ftp://` are never attempted.
    let plugin = SoapSenderPlugin::queue().with_url("ftp://example.invalid/soap");
    let (_, text) = plugin.samples()[0];

    let out = plugin.run(text).unwrap();

    assert!(!out.ok);
    assert!(
        out.output.contains(
            "Unsupported URL scheme (only http/https allowed): ftp://example.invalid/soap"
        ),
        "{}",
        out.output
    );
}

#[test]
fn every_plugin_describes_itself_for_its_tab() {
    // §4: the page is built from these six accessors, so a plugin with a
    // blank one would render an empty tab, and a repeated id or label would
    // make two tabs indistinguishable.
    let mut ids = Vec::new();
    let mut labels = Vec::new();
    for category in categories() {
        assert_ne!(category.name.len(), 0);
        for plugin in category.plugins {
            for (what, text) in [
                ("id", plugin.id()),
                ("tab label", plugin.tab_label()),
                ("description", plugin.description()),
                ("input label", plugin.input_label()),
                ("output label", plugin.output_label()),
            ] {
                assert!(
                    !text.trim().is_empty(),
                    "{} has a blank {what}",
                    plugin.id()
                );
            }
            assert!(
                !plugin.samples().is_empty(),
                "{} has no samples",
                plugin.id()
            );
            ids.push(plugin.id());
            labels.push(plugin.tab_label());
        }
    }
    for list in [&mut ids, &mut labels] {
        let before = list.len();
        list.sort_unstable();
        list.dedup();
        assert_eq!(list.len(), before, "a plugin id or tab label is repeated");
    }
}
