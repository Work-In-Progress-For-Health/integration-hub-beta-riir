//! The JSON API, ported from `test_api.py`.
//!
//! Implements `spec/20-lookup-service.md` §8 and §11.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{
    Field, app, codes_definition, get, json_request, local_config, make_row, memory,
    post_multipart, unstarted,
};
use lookup_service::config::Config;
use lookup_service::file_store::{FileTableStore, SeedSource};
use lookup_service::memory_store::MemoryTableStore;
use serde_json::json;

const HEALTH_BOARD_CSV: &str = "msh3,health_board\n224,VCC-NEW\n999,TEST\n";

/// The read-only file store over the repository seed, as a deployment runs.
async fn read_only() -> common::App<FileTableStore> {
    let store =
        FileTableStore::load(&SeedSource::from_dir(&common::repo_seed_dir()).unwrap()).unwrap();
    app(local_config(), store).await
}

async fn writable() -> common::App<MemoryTableStore> {
    app(local_config(), memory()).await
}

mod lookup {
    use super::*;

    #[tokio::test]
    async fn single_key_lookup() {
        let app = read_only().await;
        let reply = get(&app.router, "/api/v1/lookup/health_board_mapping?k=224").await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.json(),
            json!({"table": "health_board_mapping", "key": ["224"], "value": "VCC",
                   "values": {"health_board": "VCC"}, "source": "file", "cached": true})
        );
    }

    /// `spec/20-lookup-service.md` §4.3, RCA-IN-2: a query that is not valid
    /// percent-encoding is a 422 (`invalid_request`), not a lookup of a repaired key.
    #[tokio::test]
    async fn a_malformed_query_is_refused_not_repaired() {
        let app = read_only().await;
        for path in [
            "/api/v1/lookup/health_board_mapping?k=22%",
            "/api/v1/lookup/health_board_mapping?k=%zz",
            "/api/v1/lookup/health_board_mapping?k=%FF",
        ] {
            let reply = get(&app.router, path).await;
            assert_eq!(reply.status, 422, "{path}");
            assert!(
                reply.body.contains("not valid percent-encoding"),
                "{path}: {}",
                reply.body
            );
            assert!(!reply.body.contains("%zz"), "the input is not echoed");
        }
    }

    #[tokio::test]
    async fn composite_key_positional() {
        let app = read_only().await;
        let reply = get(&app.router, "/api/v1/lookup/ward_map?k=FAC1&k=W2").await;
        assert_eq!(reply.status, 200);
        let body = reply.json();
        assert_eq!(body["value"], "7A1-W002");
        assert_eq!(body["values"]["description"], "Example ward two");
    }

    #[tokio::test]
    async fn composite_key_named_and_normalised() {
        let app = read_only().await;
        let reply = get(
            &app.router,
            "/api/v1/lookup/ward_map?key.ward_code=w2&key.sending_facility=%20fac1%20",
        )
        .await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.json()["key"], json!(["FAC1", "W2"]));
    }

    #[tokio::test]
    async fn values_keep_the_order_of_the_value_columns() {
        let app = read_only().await;
        let reply = get(&app.router, "/api/v1/lookup/ward_map?k=FAC1&k=W2").await;
        let national = reply.body.find("national_ward_code").unwrap();
        let description = reply.body.rfind("description").unwrap();
        assert!(national < description, "{}", reply.body);
    }

    #[tokio::test]
    async fn unknown_table() {
        let app = read_only().await;
        let reply = get(&app.router, "/api/v1/lookup/nope?k=1").await;
        assert_eq!(reply.status, 404);
        assert_eq!(
            reply.json(),
            json!({"error": "table_not_found", "detail": "Table 'nope' does not exist", "table": "nope"})
        );
    }

    /// §1.3, §4.4.
    #[tokio::test]
    async fn unknown_key_does_not_echo_the_key() {
        let app = read_only().await;
        let reply = get(
            &app.router,
            "/api/v1/lookup/health_board_mapping?k=SECRET999",
        )
        .await;
        assert_eq!(reply.status, 404);
        assert_eq!(reply.json()["error"], "key_not_found");
        assert!(!reply.body.contains("SECRET999"));
    }

    /// `spec/index.md` §0.8.4: a marker through every failure path of the
    /// lookup surface must not come back in the body.
    #[tokio::test]
    async fn no_failure_path_echoes_a_key_part() {
        let app = read_only().await;
        for url in [
            "/api/v1/lookup/health_board_mapping?k=PIIMARKER&k=PIIMARKER",
            "/api/v1/lookup/ward_map?k=PIIMARKER",
            "/api/v1/lookup/ward_map?key.ward_code=PIIMARKER&key.nope=PIIMARKER",
            "/api/v1/lookup/ward_map?key.ward_code=PIIMARKER&key.sending_facility=PIIMARKER",
            "/api/v1/lookup/ward_map?key.ward_code=PIIMARKER&key.ward_code=PIIMARKER",
            "/api/v1/lookup/ward_map?k=PIIMARKER&key.ward_code=PIIMARKER",
            "/api/v1/lookup/PIIMARKER?k=PIIMARKER",
        ] {
            let reply = get(&app.router, url).await;
            assert!(reply.status.is_client_error(), "{url}: {}", reply.status);
            // The unknown table's *name* is the one thing a path may echo
            // (it is not a key), so only the key-part cases are asserted.
            if !url.contains("/lookup/PIIMARKER") {
                assert!(!reply.body.contains("PIIMARKER"), "{url}: {}", reply.body);
            }
        }
    }

    #[tokio::test]
    async fn invalid_keys() {
        let app = read_only().await;
        for url in [
            "/api/v1/lookup/ward_map?k=FAC1",
            "/api/v1/lookup/ward_map",
            "/api/v1/lookup/ward_map?k=FAC1&key.ward_code=W2",
            "/api/v1/lookup/ward_map?key.ward_code=W1&key.ward_code=W2&key.sending_facility=F",
            "/api/v1/lookup/health_board_mapping?k=%20%20",
        ] {
            let reply = get(&app.router, url).await;
            assert_eq!(reply.status, 422, "{url}");
            assert_eq!(reply.json()["error"], "invalid_key", "{url}");
        }
    }

    #[tokio::test]
    async fn list_tables() {
        let app = read_only().await;
        let reply = get(&app.router, "/api/v1/tables").await;
        assert_eq!(reply.status, 200);
        let body = reply.json();
        let tables = body.as_array().unwrap();
        let find = |name: &str| tables.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(tables.len(), 2);
        assert_eq!(find("ward_map")["row_count"], 3);
        assert_eq!(
            find("ward_map")["key_columns"],
            json!(["sending_facility", "ward_code"])
        );
        assert_eq!(find("health_board_mapping")["cache"]["mode"], "preloaded");
        assert_eq!(find("ward_map")["cache"]["mode"], "ttl");
        assert_eq!(find("ward_map")["ttl_seconds"], 300);
    }

    #[tokio::test]
    async fn table_detail() {
        let app = read_only().await;
        let reply = get(&app.router, "/api/v1/tables/ward_map").await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.json()["key_normalisation"]["ward_code"]["case"],
            "upper"
        );
        assert_eq!(get(&app.router, "/api/v1/tables/nope").await.status, 404);
    }

    /// RCA-IN-2: a rows query the API cannot read is refused, naming the
    /// parameter; none of them falls back to a default.
    #[tokio::test]
    async fn unreadable_rows_queries_are_refused() {
        let app = read_only().await;
        let long = "a".repeat(201);
        for (query, parameter) in [
            ("offset=-1".to_owned(), "offset"),
            ("offset=x".to_owned(), "offset"),
            ("limit=0".to_owned(), "limit"),
            ("limit=x".to_owned(), "limit"),
            (format!("search={long}"), "search"),
        ] {
            let reply = get(
                &app.router,
                &format!("/api/v1/tables/ward_map/rows?{query}"),
            )
            .await;
            assert_eq!(reply.status, 422, "{query}");
            assert!(
                reply.json()["detail"].as_str().unwrap().contains(parameter),
                "{query}"
            );
        }
    }

    #[tokio::test]
    async fn rows_paging_and_search() {
        let app = read_only().await;
        let reply = get(
            &app.router,
            "/api/v1/tables/ward_map/rows?search=fac1&limit=1",
        )
        .await;
        assert_eq!(reply.status, 200);
        let body = reply.json();
        assert_eq!(
            (
                body["total"].as_u64(),
                body["rows"].as_array().unwrap().len()
            ),
            (Some(2), 1)
        );
        assert_eq!(body["rows"][0]["key"], json!(["FAC1", "W1"]));
        let too_big = get(&app.router, "/api/v1/tables/ward_map/rows?limit=999").await;
        assert_eq!(
            (too_big.status.as_u16(), too_big.json()["error"].clone()),
            (422, json!("invalid_request"))
        );
        assert_eq!(
            get(&app.router, "/api/v1/tables/nope/rows").await.status,
            404
        );
    }

    #[tokio::test]
    async fn upload_rejected_for_the_read_only_file_store() {
        let app = read_only().await;
        let reply = post_multipart(
            &app.router,
            "/api/v1/tables/health_board_mapping/uploads",
            vec![Field::File(
                "file",
                "hb.csv",
                HEALTH_BOARD_CSV.as_bytes().to_vec(),
            )],
        )
        .await;
        assert_eq!(reply.status, 409);
        assert_eq!(reply.json()["error"], "read_only");
    }
}

mod uploads {
    use super::*;

    async fn upload(
        app: &common::App<MemoryTableStore>,
        table: &str,
        content: &[u8],
        extra: Vec<Field<'_>>,
    ) -> common::Reply {
        let mut fields = vec![Field::File("file", "data.csv", content.to_vec())];
        fields.extend(extra);
        post_multipart(
            &app.router,
            &format!("/api/v1/tables/{table}/uploads"),
            fields,
        )
        .await
    }

    fn text(name: &str, value: &serde_json::Value) -> Field<'static> {
        // A leaked name keeps the helper's signature simple in a test.
        Field::Text(
            Box::leak(name.to_owned().into_boxed_str()),
            value.to_string(),
        )
    }

    #[tokio::test]
    async fn seed_tables_are_imported_into_a_writable_store() {
        let app = writable().await;
        assert_eq!(
            app.state.store.table_names(),
            ["health_board_mapping", "ward_map"]
        );
    }

    #[tokio::test]
    async fn replace_existing_table_is_visible_immediately() {
        let app = writable().await;
        let reply = upload(
            &app,
            "health_board_mapping",
            HEALTH_BOARD_CSV.as_bytes(),
            vec![],
        )
        .await;
        assert_eq!(reply.status, 201, "{}", reply.body);
        let body = reply.json();
        assert_eq!(
            (body["row_count"].as_u64(), body["removed"].as_u64()),
            (Some(2), Some(3))
        );
        assert_eq!(body["created"], false);
        let lookup = get(&app.router, "/api/v1/lookup/health_board_mapping?k=224")
            .await
            .json();
        assert_eq!(lookup["value"], "VCC-NEW");
        assert_eq!(
            get(&app.router, "/api/v1/lookup/health_board_mapping?k=212")
                .await
                .status,
            404
        );
        let record = app.state.store.record("health_board_mapping").unwrap();
        assert_eq!(record.stats.last_upload_file.as_deref(), Some("data.csv"));
    }

    #[tokio::test]
    async fn ttl_table_upload_drops_the_stale_cache() {
        let app = writable().await;
        let url = "/api/v1/lookup/ward_map?k=FAC1&k=W1";
        assert_eq!(get(&app.router, url).await.json()["value"], "7A1-W001");
        let csv = "sending_facility,ward_code,national_ward_code,description\nFAC1,W1,NEW,d\n";
        assert_eq!(
            upload(&app, "ward_map", csv.as_bytes(), vec![])
                .await
                .status,
            201
        );
        assert_eq!(get(&app.router, url).await.json()["value"], "NEW");
    }

    #[tokio::test]
    async fn create_table_with_definition_and_mapping() {
        let app = writable().await;
        let reply = upload(
            &app,
            "discharge_reason",
            b"Code,Text\n a ,Home\nB,Other\n",
            vec![
                text("definition", &json!({"key_columns": ["code"], "value_columns": ["text"], "ttl_seconds": 60,
                    "key_normalisation": {"code": {"case": "upper"}}})),
                text("mapping", &json!({"key": {"code": {"path": "Code"}}, "values": {"text": {"path": "Text"}}})),
            ],
        )
        .await;
        assert_eq!(reply.status, 201, "{}", reply.body);
        assert_eq!(reply.json()["created"], true);
        let lookup = get(&app.router, "/api/v1/lookup/discharge_reason?key.code=a").await;
        assert_eq!(lookup.json()["value"], "Home");
        assert_eq!(
            get(&app.router, "/api/v1/tables/discharge_reason")
                .await
                .json()["ttl_seconds"],
            60
        );
    }

    #[tokio::test]
    async fn new_table_without_a_definition_is_rejected() {
        let app = writable().await;
        let reply = upload(&app, "brand_new", b"code,label\nA,B\n", vec![]).await;
        assert_eq!(reply.status, 422);
        assert!(
            reply.json()["detail"]
                .as_str()
                .unwrap()
                .contains("supply a table definition")
        );
    }

    #[tokio::test]
    async fn invalid_rows_change_nothing_and_list_line_numbers() {
        let app = writable().await;
        let reply = upload(
            &app,
            "health_board_mapping",
            b"msh3,health_board\n224,X\n224,Y\n,Z\n",
            vec![],
        )
        .await;
        assert_eq!(reply.status, 422);
        let body = reply.json();
        assert_eq!(body["error"], "invalid_upload");
        assert_eq!(
            body["errors"],
            json!([
                "line 3: duplicate key (first seen on line 2)",
                "line 4: empty key part 'msh3'"
            ])
        );
        let row = app
            .state
            .store
            .row("health_board_mapping", &["224"])
            .unwrap();
        assert_eq!(row.values.get("health_board"), Some("VCC"));
    }

    #[tokio::test]
    async fn invalid_definition_and_mapping() {
        let app = writable().await;
        let csv = b"code,label\nA,B\n";
        let reply = upload(
            &app,
            "codes",
            csv,
            vec![text(
                "definition",
                &json!({"key_columns": ["missing column"]}),
            )],
        )
        .await;
        assert_eq!(reply.status, 422);
        assert!(reply.json().get("errors").is_some());
        let bad_json = upload(
            &app,
            "codes",
            csv,
            vec![Field::Text("definition", "{bad".to_owned())],
        )
        .await;
        assert_eq!(bad_json.status, 422);
        let bad_mapping = upload(
            &app,
            "codes",
            csv,
            vec![
                text("definition", &json!({})),
                text("mapping", &json!({"key": "nope"})),
            ],
        )
        .await;
        assert_eq!(bad_mapping.status, 422);
    }

    /// The `mapping` field must be JSON and an object, and each way it is
    /// not is named; nothing is created from a refused upload.
    #[tokio::test]
    async fn an_unreadable_mapping_names_the_reason() {
        let app = writable().await;
        let csv = b"code,label\nA,B\n";
        for (mapping, expected) in [
            ("{nope", "mapping is not valid JSON"),
            ("[1]", "mapping must be a JSON object"),
            ("{\"key\": {}}", "mapping is invalid"),
        ] {
            let reply = upload(
                &app,
                "codes",
                csv,
                vec![
                    text("definition", &json!({})),
                    Field::Text("mapping", mapping.to_owned()),
                ],
            )
            .await;
            assert_eq!(reply.status, 422, "{mapping}");
            assert_eq!(reply.json()["detail"], expected, "{mapping}");
        }
        assert!(app.state.store.record("codes").is_none());
    }

    #[tokio::test]
    async fn invalid_table_name() {
        let app = writable().await;
        let reply = upload(
            &app,
            "Bad_Name",
            b"code,label\nA,B\n",
            vec![text("definition", &json!({}))],
        )
        .await;
        assert_eq!(reply.status, 422);
    }

    #[tokio::test]
    async fn empty_and_non_utf8_files() {
        let app = writable().await;
        assert_eq!(
            upload(&app, "health_board_mapping", b"msh3,health_board\n", vec![])
                .await
                .status,
            422
        );
        assert_eq!(
            upload(&app, "health_board_mapping", b"\xff\xfe\x00", vec![])
                .await
                .status,
            422
        );
    }

    #[tokio::test]
    async fn too_large() {
        let config = Config {
            max_upload_mb: 1,
            ..local_config()
        };
        let app = app(config, memory()).await;
        let mut big = b"msh3,health_board\n".to_vec();
        big.extend(b"1,x\n".repeat(300_000));
        let reply = upload(&app, "health_board_mapping", &big, vec![]).await;
        assert_eq!(reply.status, 413);
        assert_eq!(reply.json()["error"], "upload_too_large");
    }

    #[tokio::test]
    async fn a_missing_file_is_a_422() {
        let app = writable().await;
        let reply = post_multipart(
            &app.router,
            "/api/v1/tables/codes/uploads",
            vec![Field::Text("definition", "{}".to_owned())],
        )
        .await;
        assert_eq!(reply.status, 422);
    }
}

mod settings {
    use super::*;

    #[tokio::test]
    async fn patch_changes_ttl_and_preload_and_keeps_rows() {
        let app = writable().await;
        let reply = json_request(
            &app.router,
            "PATCH",
            "/api/v1/tables/ward_map",
            &json!({"ttl_seconds": 7200, "preload": true}),
        )
        .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body = reply.json();
        assert_eq!(
            (
                body["preload"].clone(),
                body["cache"]["mode"].clone(),
                body["row_count"].clone()
            ),
            (json!(true), json!("preloaded"), json!(3))
        );
        assert_eq!(
            app.state
                .store
                .record("ward_map")
                .unwrap()
                .definition
                .ttl_seconds,
            Some(7200)
        );
        let lookup = get(&app.router, "/api/v1/lookup/ward_map?k=FAC1&k=W1").await;
        assert_eq!(lookup.json()["value"], "7A1-W001");
    }

    #[tokio::test]
    async fn patch_only_changes_supplied_fields() {
        let app = writable().await;
        json_request(
            &app.router,
            "PATCH",
            "/api/v1/tables/health_board_mapping",
            &json!({"ttl_seconds": 60}),
        )
        .await;
        let definition = app
            .state
            .store
            .record("health_board_mapping")
            .unwrap()
            .definition;
        assert_eq!(
            (definition.ttl_seconds, definition.preload),
            (Some(60), true)
        );
        json_request(
            &app.router,
            "PATCH",
            "/api/v1/tables/health_board_mapping",
            &json!({"ttl_seconds": null}),
        )
        .await;
        assert_eq!(
            app.state
                .store
                .record("health_board_mapping")
                .unwrap()
                .definition
                .ttl_seconds,
            None
        );
    }

    #[tokio::test]
    async fn patch_validation() {
        let app = writable().await;
        for body in [
            json!({"ttl_seconds": 0}),
            json!({"ttl_seconds": 8 * 86_400}),
            json!({"key_columns": ["x"]}),
        ] {
            let reply = json_request(&app.router, "PATCH", "/api/v1/tables/ward_map", &body).await;
            assert_eq!(reply.status, 422, "{body}");
        }
        let missing = json_request(
            &app.router,
            "PATCH",
            "/api/v1/tables/nope",
            &json!({"preload": true}),
        )
        .await;
        assert_eq!(missing.status, 404);
    }

    #[tokio::test]
    async fn patch_rejected_for_the_read_only_store() {
        let app = read_only().await;
        let reply = json_request(
            &app.router,
            "PATCH",
            "/api/v1/tables/ward_map",
            &json!({"preload": true}),
        )
        .await;
        assert_eq!(
            (reply.status.as_u16(), reply.json()["error"].clone()),
            (409, json!("read_only"))
        );
    }
}

mod health {
    use super::*;

    #[tokio::test]
    async fn live() {
        let app = read_only().await;
        assert_eq!(
            get(&app.router, "/health/live").await.json(),
            json!({"status": "ok"})
        );
    }

    #[tokio::test]
    async fn ready_after_startup() {
        let app = writable().await;
        let reply = get(&app.router, "/health/ready").await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.json(),
            json!({"status": "ready", "tables": 2, "source": "memory"})
        );
        assert!(app.state.store.was_opened());
        assert!(!app.state.store.was_closed());
    }

    /// §5.1.
    #[tokio::test]
    async fn not_ready_before_startup() {
        let router = unstarted(local_config(), memory());
        assert_eq!(get(&router, "/health/live").await.status, 200);
        assert_eq!(get(&router, "/health/ready").await.status, 503);
        let reply = get(&router, "/api/v1/lookup/health_board_mapping?k=224").await;
        assert_eq!(reply.status, 503);
        assert_eq!(reply.json()["error"], "not_ready");
    }

    #[tokio::test]
    async fn stop_closes_the_store_and_stops_serving() {
        let app = writable().await;
        app.state.stop().await;
        assert!(app.state.store.was_closed());
        assert_eq!(get(&app.router, "/health/ready").await.status, 503);
    }
}

/// §5.5.
#[tokio::test]
async fn store_outage_is_a_503() {
    let app = writable().await;
    app.state.store.set_fail_reads(true);
    let reply = get(&app.router, "/api/v1/lookup/ward_map?k=FAC1&k=W1").await;
    assert_eq!(reply.status, 503);
    assert_eq!(reply.json()["error"], "backend_unavailable");
}

/// §2.1 / §9.2: the Python gates its API documentation by environment; this
/// port serves none anywhere.
#[tokio::test]
async fn api_documentation_is_never_served() {
    for environment in ["LOCAL", "PRD"] {
        let config = Config {
            environment: environment.to_owned(),
            ..local_config()
        };
        let app = app(config, memory()).await;
        assert_eq!(
            get(&app.router, "/openapi.json").await.status,
            404,
            "{environment}"
        );
        assert_eq!(get(&app.router, "/docs").await.status, 404, "{environment}");
    }
}

#[tokio::test]
async fn a_table_put_in_the_store_is_served() {
    let store = memory();
    store.put_table(
        codes_definition(),
        vec![make_row(&["A"], &[("label", "Alpha")])],
    );
    let app = app(local_config(), store).await;
    assert_eq!(
        get(&app.router, "/api/v1/lookup/codes?k=A").await.json()["value"],
        "Alpha"
    );
}
