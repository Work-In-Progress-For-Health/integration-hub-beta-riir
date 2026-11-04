//! Ported from `test_models.py`. `spec/ultra7-test-message-sender.md` `§3.1`, `§3.2`.
#![allow(missing_docs, clippy::unwrap_used)]

use ultra7::model::{
    Endpoint, EndpointKind, IterationMode, IterationSpec, Message, MessageFormat, Project,
};

fn round<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    serde_json::from_str(&serde_json::to_string(v).unwrap()).unwrap()
}

#[test]
fn message_round_trip() {
    let m = Message::new("A01", MessageFormat::Hl7, "MSH|^~\\&|...");
    assert_eq!(round(&m), m);
}

#[test]
fn default_iteration_is_none_and_enabled() {
    let m = Message::new("A01", MessageFormat::Json, "{}");
    assert!(m.iteration.is_none());
    assert!(m.enabled);
}

#[test]
fn round_trip_preserves_disabled_state() {
    let mut m = Message::new("A01", MessageFormat::Json, "{}");
    m.enabled = false;
    assert!(!round(&m).enabled);
}

#[test]
fn round_trip_with_iteration() {
    let mut m = Message::new("A01", MessageFormat::Hl7, "MSG000001");
    let mut spec = IterationSpec::new(3, 9, IterationMode::Increment);
    spec.step = 2;
    spec.pad_width = 6;
    m.iteration = Some(spec);
    assert_eq!(round(&m), m);
}

#[test]
fn message_id_is_eight_hex_when_absent() {
    let m: Message =
        serde_json::from_str(r#"{"name":"n","format":"xml","content":"<a/>"}"#).unwrap();
    assert_eq!(m.id.len(), 8);
    assert!(m.id.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn iteration_round_trip_list_mode() {
    let mut spec = IterationSpec::new(0, 3, IterationMode::List);
    spec.values = vec!["AAA".into(), "BBB".into()];
    assert_eq!(round(&spec), spec);
}

#[test]
fn iteration_from_dict_defaults() {
    let spec: IterationSpec = serde_json::from_str(r#"{"start":1,"end":2}"#).unwrap();
    assert_eq!(spec.mode, IterationMode::Increment);
    assert_eq!(spec.step, 1);
    assert_eq!(spec.pad_width, 0);
    assert_eq!(spec.timestamp_format, "%Y%m%d%H%M%S");
}

#[test]
fn endpoint_round_trip_mllp() {
    let e = Endpoint {
        host: "127.0.0.1".into(),
        port: Some(2575),
        ..Endpoint::default()
    };
    assert_eq!(round(&e), e);
}

#[test]
fn endpoint_round_trip_rest_with_headers() {
    let mut e = Endpoint {
        kind: EndpointKind::Rest,
        url: "https://example.test/ingest".into(),
        ..Endpoint::default()
    };
    e.headers.insert("X-Test".into(), "1".into());
    assert_eq!(round(&e), e);
}

#[test]
fn endpoint_from_dict_defaults() {
    let e: Endpoint = serde_json::from_str("{}").unwrap();
    assert_eq!(e.kind, EndpointKind::Mllp);
    assert!((e.timeout_seconds - 5.0).abs() < f64::EPSILON);
    assert_eq!(e.port, None);
}

#[test]
fn project_round_trip_with_messages() {
    let mut p = Project::new("Demo");
    p.endpoint = Endpoint {
        kind: EndpointKind::Rest,
        url: "https://example.test".into(),
        ..Endpoint::default()
    };
    p.messages
        .push(Message::new("m1", MessageFormat::Json, "{}"));
    p.repeat_count = 3;
    p.delay_ms = 250;
    assert_eq!(round(&p), p);
}

#[test]
fn python_written_project_file_loads() {
    // The shape `Project.to_dict()` writes, with `null` port and iteration.
    let text = r#"{"name":"Demo","endpoint":{"kind":"mllp","host":"h","port":null,"url":"",
      "headers":{},"soap_action":"","timeout_seconds":5.0},
      "messages":[{"id":"ab12cd34","name":"m","format":"hl7","content":"MSH|x","iteration":null,"enabled":true}],
      "repeat_count":2,"delay_ms":10}"#;
    let p: Project = serde_json::from_str(text).unwrap();
    assert_eq!(p.messages[0].id, "ab12cd34");
    assert_eq!(p.repeat_count, 2);
}
