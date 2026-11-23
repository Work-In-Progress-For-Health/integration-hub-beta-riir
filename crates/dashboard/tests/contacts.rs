//! The Contacts routes, end to end through the router.
//!
//! Implements `spec/15-dashboard.md` §10.3.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::app;
use dashboard::alarm::store::{Disabled, InMemory};

fn contact() -> serde_json::Value {
    serde_json::json!({
        "first_name": "Ada",
        "last_name": "Lovelace",
        "email": "ada@example.org",
        "organisation_type": "Supplier",
        "organisation_name": "Analytical Engines",
        "flow_ids": ["phw-to-mpi"],
    })
}

#[tokio::test]
async fn a_contact_goes_through_its_whole_life() {
    // §10.3.9: create, read, update, soft-delete, restore.
    let app = app(InMemory::new());

    let (status, created) = app.json_request("POST", "/api/contacts", &contact()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (status, fetched) = app.get_json(&format!("/api/contacts/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["email"], "ada@example.org");

    let mut change = contact();
    change["first_name"] = "Augusta".into();
    let (status, updated) = app
        .json_request("PUT", &format!("/api/contacts/{id}"), &change)
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["first_name"], "Augusta");

    let (status, deleted) = app
        .send("DELETE", &format!("/api/contacts/{id}"), None, Vec::new())
        .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");

    let (_, listed) = app.get_json("/api/contacts").await;
    assert_eq!(
        listed["total"], 1,
        "deleted contacts stay visible by default"
    );
    assert_eq!(listed["contacts"][0]["deleted"], true);
    let (_, hidden) = app.get_json("/api/contacts?include_deleted=false").await;
    assert_eq!(hidden["total"], 0);

    let (status, restored) = app
        .json_request(
            "POST",
            &format!("/api/contacts/{id}/restore"),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["deleted"], false);
}

#[tokio::test]
async fn the_statuses_are_the_ones_the_script_expects() {
    // §10.3.9.
    let app = app(InMemory::new());

    let (status, _) = app.get_json("/api/contacts/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = app
        .json_request("PUT", "/api/contacts/ghost", &contact())
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "upstream's behaviour: {body}"
    );
    assert_eq!(body["error"], "Contact not found.");

    let (status, _) = app
        .json_request(
            "POST",
            "/api/contacts/ghost/restore",
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = app
        .send("DELETE", "/api/contacts/ghost", None, Vec::new())
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "deleting what is not there is a no-op"
    );

    let (status, body) = app
        .json_request("POST", "/api/contacts", &serde_json::json!([1]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "JSON body must be an object");

    let mut bad = contact();
    bad["organisation_type"] = "Charity".into();
    let (status, body) = app.json_request("POST", "/api/contacts", &bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("must be one of"));
}

#[tokio::test]
async fn nothing_persisted_means_503_and_the_page_still_renders() {
    // §10.3.7, §1.2.
    let app = app(Disabled);

    let (status, body) = app.json_request("POST", "/api/contacts", &contact()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "Contact persistence is not configured.");

    let (status, page) = app.get("/contacts").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("COSMOS_ENDPOINT"));
}

#[tokio::test]
async fn a_csv_import_reports_what_it_did() {
    // §10.3.6.
    let app = app(InMemory::new());
    let csv = "first_name,last_name,organisation_type,organisation_name,email\n\
               Ada,Lovelace,Supplier,AE,ada@example.org\n\
               Bad,Row,Nope,X,b@example.org\n";

    let (status, text) = app
        .send(
            "POST",
            "/api/contacts/import",
            Some("text/csv"),
            csv.as_bytes().to_vec(),
        )
        .await;
    let result: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(result["created"], 1);
    assert_eq!(result["updated"], 0);
    assert!(result["errors"][0].as_str().unwrap().starts_with("Row 3:"));
    assert_eq!(result["persistence_failed"], false);

    let (status, _) = app
        .send("POST", "/api/contacts/import", Some("text/csv"), Vec::new())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, text) = app
        .send(
            "POST",
            "/api/contacts/import",
            Some("text/csv"),
            vec![0xff, 0xfe, 0x00],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("UTF-8"));
    let (status, text) = app
        .send(
            "POST",
            "/api/contacts/import",
            Some("text/csv"),
            vec![b'a'; 1_000_001],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("too large"));
}

#[tokio::test]
async fn an_import_with_nowhere_to_store_is_a_503() {
    let app = app(Disabled);

    let (status, _) = app
        .send(
            "POST",
            "/api/contacts/import",
            Some("text/csv"),
            b"first_name,last_name,organisation_type,organisation_name,email\nA,B,Team,C,a@b.co\n"
                .to_vec(),
        )
        .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_page_lists_filters_and_escapes() {
    let app = app(InMemory::new());
    let mut hostile = contact();
    hostile["first_name"] = "<img src=x onerror=alert(1)>".into();
    hostile["last_name"] = "Zed".into();
    app.json_request("POST", "/api/contacts", &hostile).await;
    let mut other = contact();
    other["email"] = "o@example.org".into();
    other["flow_ids"] = serde_json::json!(["paris-to-mpi"]);
    app.json_request("POST", "/api/contacts", &other).await;

    let (_, page) = app.get("/contacts").await;
    assert!(!page.contains("<img src=x"), "unescaped markup in the page");
    assert!(page.contains("Lovelace"));

    let (_, filtered) = app.get("/contacts?flow=paris-to-mpi").await;
    assert!(filtered.contains("Contacts (1)"), "{filtered}");
    let (_, none) = app.get("/contacts?q=nobody-matches").await;
    assert!(none.contains("No contacts match"));
}

/// §10.3.5, RCA-IN-2/-3: a `page` or `per_page` that is supplied and is not a
/// whole number of at least 1 is refused, naming the parameter. It is never
/// read as 1 (the Flask behaviour this replaces), and absent still defaults.
#[tokio::test]
async fn an_invalid_page_or_page_size_is_refused_not_replaced() {
    let app = app(InMemory::new());
    for (query, name) in [
        ("page=0", "page"),
        ("page=abc", "page"),
        ("page=", "page"),
        ("page=-1", "page"),
        ("per_page=0", "per_page"),
        ("per_page=lots", "per_page"),
    ] {
        let (status, body) = app.get_json(&format!("/api/contacts?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(message.starts_with(name), "{query}: {message}");

        let (status, _) = app.get(&format!("/contacts?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "page {query}");
    }
    let (status, body) = app.get_json("/api/contacts").await;
    assert_eq!(status, StatusCode::OK, "absent defaults");
    assert_eq!(body["page"], 1);
    let (status, _) = app.get_json("/api/contacts?page=2&per_page=5").await;
    assert_eq!(status, StatusCode::OK);
}
