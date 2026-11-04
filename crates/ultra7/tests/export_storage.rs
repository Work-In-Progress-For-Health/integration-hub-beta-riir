//! Ported from `test_export.py` and `test_storage.py`.
//! `spec/ultra7-test-message-sender.md` `§3.3`, `§3.4`, `§6`.
#![allow(missing_docs, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ultra7::export::{extension_for, sanitize_filename, unique_filename};
use ultra7::model::MessageFormat::{Hl7, Json, Xml};
use ultra7::model::Project;
use ultra7::storage::{ProjectStore, StoreError, project_filename};

#[test]
fn known_extensions() {
    assert_eq!(extension_for(Hl7), ".hl7");
    assert_eq!(extension_for(Xml), ".xml");
    assert_eq!(extension_for(Json), ".json");
}

#[test]
fn sanitises_unsafe_characters() {
    assert_eq!(sanitize_filename("My Message"), "My Message");
    assert_eq!(sanitize_filename("a/b\\c"), "a_b_c");
    assert_eq!(sanitize_filename("../x"), "___x");
}

#[test]
fn blank_name_falls_back_to_message() {
    assert_eq!(sanitize_filename("   "), "message");
}

#[test]
fn first_use_is_unmodified_and_duplicates_are_disambiguated() {
    let mut used = BTreeSet::new();
    let names: Vec<String> = (0..3)
        .map(|_| unique_filename("A01", ".hl7", &mut used))
        .collect();
    assert_eq!(names, ["A01.hl7", "A01 (2).hl7", "A01 (3).hl7"]);
}

#[test]
fn project_filename_sanitises() {
    assert_eq!(project_filename("My Project").unwrap(), "My Project.json");
    assert_eq!(project_filename("a/b\\c").unwrap(), "a_b_c.json");
}

#[test]
fn project_filename_rejects_empty_name() {
    assert!(matches!(
        project_filename("   "),
        Err(StoreError::EmptyName)
    ));
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("ultra7-test-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn save_and_load_round_trip() {
    let tmp = Tmp::new();
    let store = ProjectStore::new(tmp.0.join("projects"));
    store.save(&Project::new("Demo")).unwrap();
    assert_eq!(store.load("Demo").unwrap().name, "Demo");
    // No temp file is left behind by the atomic write (`§3.4`).
    assert!(!tmp.0.join("projects/Demo.json.tmp").exists());
}

#[test]
fn list_projects_sorted() {
    let tmp = Tmp::new();
    let store = ProjectStore::new(tmp.0.join("projects"));
    store.save(&Project::new("Zeta")).unwrap();
    store.save(&Project::new("Alpha")).unwrap();
    assert_eq!(store.list_projects().unwrap(), ["Alpha", "Zeta"]);
}

#[test]
fn delete_removes_file_and_missing_is_a_no_op() {
    let tmp = Tmp::new();
    let store = ProjectStore::new(tmp.0.join("projects"));
    store.save(&Project::new("Demo")).unwrap();
    assert!(store.exists("Demo").unwrap());
    store.delete("Demo").unwrap();
    assert!(!store.exists("Demo").unwrap());
    store.delete("does-not-exist").unwrap();
}

#[test]
fn a_file_that_is_not_a_project_is_a_parse_error_without_its_content() {
    let tmp = Tmp::new();
    let store = ProjectStore::new(tmp.0.join("projects"));
    store.list_projects().unwrap();
    std::fs::write(tmp.0.join("projects/Bad.json"), "PID|SECRET-LOOKING").unwrap();
    let err = store.load("Bad").unwrap_err();
    assert!(matches!(err, StoreError::Parse(_)));
    assert!(!err.to_string().contains("SECRET"));
}
