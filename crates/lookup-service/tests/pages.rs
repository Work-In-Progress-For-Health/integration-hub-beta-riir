//! The pages, ported from `test_gui.py`.
//!
//! Implements `spec/20-lookup-service.md` §9 and §11.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{
    Field, SeedDir, app, codes_definition, get, local_config, make_row, memory, post_form,
    post_multipart,
};
use lookup_service::config::{Config, SeedLocation};
use lookup_service::file_store::{FileTableStore, SeedSource};
use lookup_service::memory_store::MemoryTableStore;

async fn read_only() -> common::App<FileTableStore> {
    let store =
        FileTableStore::load(&SeedSource::from_dir(&common::repo_seed_dir()).unwrap()).unwrap();
    app(local_config(), store).await
}

async fn writable() -> common::App<MemoryTableStore> {
    app(local_config(), memory()).await
}

mod overview {
    use super::*;

    #[tokio::test]
    async fn lists_tables_with_links() {
        let app = read_only().await;
        let reply = get(&app.router, "/").await;
        assert_eq!(reply.status, 200);
        assert!(
            reply.body.contains("href=\"/tables/ward_map\""),
            "{}",
            reply.body
        );
        assert!(reply.body.contains("preloaded"));
    }

    #[tokio::test]
    async fn sets_security_headers() {
        let app = read_only().await;
        let reply = get(&app.router, "/").await;
        assert!(
            reply
                .header("content-security-policy")
                .contains("default-src 'self'")
        );
        assert_eq!(reply.header("x-content-type-options"), "nosniff");
        assert_eq!(reply.header("referrer-policy"), "no-referrer");
    }

    #[tokio::test]
    async fn hides_upload_for_the_read_only_store() {
        let app = read_only().await;
        assert!(
            !get(&app.router, "/")
                .await
                .body
                .contains("btn-dash--primary\" href=\"/upload\"")
        );
        let page = get(&app.router, "/upload").await;
        assert!(page.body.contains("configure COSMOS_ENDPOINT"));
        assert!(!page.body.contains("enctype=\"multipart/form-data\""));
    }

    #[tokio::test]
    async fn static_assets_are_served() {
        let app = read_only().await;
        assert!(
            get(&app.router, "/static/lookup.css")
                .await
                .body
                .contains("#325083")
        );
        assert_eq!(get(&app.router, "/static/upload.js").await.status, 200);
        assert_eq!(
            get(&app.router, "/static/DHCW-logo-RGB-White.png")
                .await
                .header("content-type"),
            "image/png"
        );
        assert_eq!(get(&app.router, "/static/nope.js").await.status, 404);
    }

    #[tokio::test]
    async fn empty_state() {
        let dir = SeedDir::new();
        let config = Config {
            seed: SeedLocation::Dir(dir.path.clone()),
            ..local_config()
        };
        let store = FileTableStore::load(&SeedSource::from_dir(&dir.path).unwrap()).unwrap();
        let app = app(config, store).await;
        assert!(get(&app.router, "/").await.body.contains("No tables yet"));
    }
}

mod table_page {
    use super::*;

    #[tokio::test]
    async fn shows_definition_and_rows() {
        let app = read_only().await;
        let reply = get(&app.router, "/tables/ward_map").await;
        assert_eq!(reply.status, 200);
        assert!(reply.body.contains("name=\"key.sending_facility\""));
        assert!(reply.body.contains("7A2-W001"));
        assert!(reply.body.contains("3 row(s)"));
    }

    #[tokio::test]
    async fn search_filters_rows() {
        let app = read_only().await;
        let reply = get(&app.router, "/tables/ward_map?search=fac2").await;
        assert!(reply.body.contains("7A2-W001"));
        assert!(!reply.body.contains("7A1-W002"));
        assert!(
            get(&app.router, "/tables/ward_map?search=zzz")
                .await
                .body
                .contains("No rows match")
        );
    }

    #[tokio::test]
    async fn lookup_tester_shows_the_result() {
        let app = read_only().await;
        let reply = get(
            &app.router,
            "/tables/ward_map?key.sending_facility=fac1&key.ward_code=w1",
        )
        .await;
        assert!(reply.body.contains("FAC1 :: W1"));
        assert!(reply.body.contains("<strong>7A1-W001</strong>"));
    }

    #[tokio::test]
    async fn lookup_tester_shows_the_error() {
        let app = read_only().await;
        let reply = get(&app.router, "/tables/health_board_mapping?key.msh3=000").await;
        assert_eq!(reply.status, 200);
        assert!(reply.body.contains("key_not_found"));
        assert!(!reply.body.contains(">000<"));
    }

    #[tokio::test]
    async fn unknown_table_page() {
        let app = read_only().await;
        let reply = get(&app.router, "/tables/nope").await;
        assert_eq!(reply.status, 404);
        assert!(reply.body.contains("does not exist"));
    }

    #[tokio::test]
    async fn upload_banner() {
        let app = read_only().await;
        assert!(
            get(&app.router, "/tables/ward_map?uploaded=7")
                .await
                .body
                .contains("Upload complete: 7 row(s)")
        );
    }

    #[tokio::test]
    async fn pages_through_rows() {
        let store = memory();
        let rows = (0..120)
            .map(|i| make_row(&[&format!("K{i:03}")], &[("label", &format!("v{i}"))]))
            .collect();
        store.put_table(codes_definition(), rows);
        let dir = SeedDir::new();
        let config = Config {
            seed: SeedLocation::Dir(dir.path.join("missing")),
            ..local_config()
        };
        let app = app(config, store).await;
        let first = get(&app.router, "/tables/codes").await.body;
        let last = get(&app.router, "/tables/codes?page=3").await.body;
        assert!(first.contains("page 1 of 3"));
        assert!(first.contains("K000"));
        assert!(!first.contains("K050"));
        assert!(last.contains("K119"));
        assert!(!last.contains(">Next<"));
        assert!(first.contains(">Next<"));
        assert!(first.contains("href=\"/tables/codes?page=2\""));
    }

    /// §9.1 and RCA-IN-2: a query the page cannot read is refused, naming
    /// the parameter, and is never replaced by a default.
    #[tokio::test]
    async fn unreadable_table_queries_are_refused() {
        let app = read_only().await;
        let long = "a".repeat(201);
        for (query, parameter) in [
            ("page=0".to_owned(), "page"),
            ("page=abc".to_owned(), "page"),
            ("page=-1".to_owned(), "page"),
            (format!("search={long}"), "search"),
        ] {
            let reply = get(&app.router, &format!("/tables/ward_map?{query}")).await;
            assert_eq!(reply.status, 422, "{query}");
            assert!(reply.body.contains(parameter), "{query}");
        }
    }

    /// §9.1.
    #[tokio::test]
    async fn values_and_names_are_html_escaped() {
        let dir = SeedDir::new();
        dir.write_csv("codes", "code,label\nA,\"<script>alert(1)</script>\"\n");
        let config = Config {
            seed: SeedLocation::Dir(dir.path.clone()),
            ..local_config()
        };
        let store = FileTableStore::load(&SeedSource::from_dir(&dir.path).unwrap()).unwrap();
        let app = app(config, store).await;
        let reply = get(&app.router, "/tables/codes?key.code=A").await;
        assert!(!reply.body.contains("<script>alert(1)</script>"));
        assert!(
            reply.body.contains("&lt;script&gt;") || reply.body.contains("&#60;script&#62;"),
            "{}",
            reply.body
        );
        let searched = get(&app.router, "/tables/codes?search=%3Cb%3Ex%3C%2Fb%3E").await;
        assert!(!searched.body.contains("<b>x</b>"));
    }
}

mod upload_page {
    use super::*;

    #[tokio::test]
    async fn form_rendered() {
        let app = writable().await;
        let reply = get(&app.router, "/upload?table=ward_map").await;
        assert_eq!(reply.status, 200);
        assert!(reply.body.contains("enctype=\"multipart/form-data\""));
        assert!(reply.body.contains("value=\"ward_map\""));
    }

    #[tokio::test]
    async fn create_table_redirects_to_the_table_page() {
        let app = writable().await;
        let reply = post_multipart(
            &app.router,
            "/upload",
            vec![
                Field::Text("table", "discharge_reason".to_owned()),
                Field::Text("key_columns", "code".to_owned()),
                Field::Text("key_case", "upper".to_owned()),
                Field::Text("ttl_value", "2".to_owned()),
                Field::Text("ttl_unit", "minutes".to_owned()),
                Field::Text("preload", "on".to_owned()),
                Field::Text("description", "Discharge reasons".to_owned()),
                Field::File("file", "reasons.csv", b"code,text\na,Home\n".to_vec()),
            ],
        )
        .await;
        assert_eq!(reply.status, 303, "{}", reply.body);
        assert!(
            reply
                .header("location")
                .ends_with("/tables/discharge_reason?uploaded=1")
        );
        let definition = app
            .state
            .store
            .record("discharge_reason")
            .unwrap()
            .definition;
        assert!(definition.preload);
        assert_eq!(definition.ttl_seconds, Some(120));
        assert_eq!(
            definition.rule_for("code").case,
            lookup_service::model::Case::Upper
        );
        assert!(app.state.store.row("discharge_reason", &["A"]).is_some());
    }

    #[tokio::test]
    async fn replace_existing_keeps_the_definition_unless_redefine() {
        let app = writable().await;
        let reply = post_multipart(
            &app.router,
            "/upload",
            vec![
                Field::Text("table", "health_board_mapping".to_owned()),
                Field::Text("key_columns", "ignored".to_owned()),
                Field::File("file", "hb.csv", b"msh3,health_board\n224,X\n".to_vec()),
            ],
        )
        .await;
        assert_eq!(reply.status, 303, "{}", reply.body);
        let definition = app
            .state
            .store
            .record("health_board_mapping")
            .unwrap()
            .definition;
        assert_eq!(definition.key_columns, ["msh3"]);
    }

    #[tokio::test]
    async fn errors_rerender_the_form_with_values() {
        let app = writable().await;
        let reply = post_multipart(
            &app.router,
            "/upload",
            vec![
                Field::Text("table", "health_board_mapping".to_owned()),
                Field::File(
                    "file",
                    "hb.csv",
                    b"msh3,health_board\n224,X\n224,Y\n".to_vec(),
                ),
            ],
        )
        .await;
        assert_eq!(reply.status, 422);
        assert!(
            reply
                .body
                .contains("line 3: duplicate key (first seen on line 2)")
        );
        assert!(reply.body.contains("value=\"health_board_mapping\""));
    }

    #[tokio::test]
    async fn missing_file() {
        let app = writable().await;
        let reply = post_multipart(
            &app.router,
            "/upload",
            vec![Field::Text("table", "health_board_mapping".to_owned())],
        )
        .await;
        assert_eq!(reply.status, 422);
        assert!(reply.body.contains("Choose a CSV file"));
    }

    #[tokio::test]
    async fn invalid_ttl() {
        let app = writable().await;
        let reply = post_multipart(
            &app.router,
            "/upload",
            vec![
                Field::Text("table", "newtable".to_owned()),
                Field::Text("ttl_value", "soon".to_owned()),
                Field::File("file", "x.csv", b"a,b\n1,2\n".to_vec()),
            ],
        )
        .await;
        assert_eq!(reply.status, 422);
        assert!(reply.body.contains("TTL must be a whole number"));
    }
}

mod settings_form {
    use super::*;

    async fn save(app: &common::App<MemoryTableStore>, pairs: &[(&str, &str)]) -> common::Reply {
        post_form(&app.router, "/tables/ward_map/settings", pairs).await
    }

    #[tokio::test]
    async fn form_shown_prefilled_with_the_split_ttl() {
        let app = writable().await;
        save(&app, &[("ttl_value", "2"), ("ttl_unit", "hours")]).await;
        let page = get(&app.router, "/tables/ward_map").await.body;
        assert!(page.contains("action=\"/tables/ward_map/settings\""));
        assert!(page.contains("name=\"ttl_value\" value=\"2\""));
        assert!(page.contains("<option value=\"hours\" selected>"));
    }

    #[tokio::test]
    async fn each_unit_converts_to_seconds() {
        let app = writable().await;
        let mut last = None;
        for (unit, expected) in [
            ("seconds", 5),
            ("minutes", 300),
            ("hours", 18_000),
            ("days", 432_000),
        ] {
            let reply = save(&app, &[("ttl_value", "5"), ("ttl_unit", unit)]).await;
            assert_eq!(reply.status, 303, "{unit}: {}", reply.body);
            assert_eq!(
                app.state
                    .store
                    .record("ward_map")
                    .unwrap()
                    .definition
                    .ttl_seconds,
                Some(expected)
            );
            last = Some(reply);
        }
        assert!(
            last.unwrap()
                .header("location")
                .ends_with("/tables/ward_map?saved=1")
        );
    }

    #[tokio::test]
    async fn preload_and_blank_ttl() {
        let app = writable().await;
        save(
            &app,
            &[("ttl_value", ""), ("ttl_unit", "hours"), ("preload", "on")],
        )
        .await;
        let definition = app.state.store.record("ward_map").unwrap().definition;
        assert_eq!((definition.ttl_seconds, definition.preload), (None, true));
        let page = get(&app.router, "/tables/ward_map?saved=1").await.body;
        assert!(page.contains("Preloaded"));
        assert!(page.contains("Cache settings saved"));
    }

    #[tokio::test]
    async fn invalid_values_rerender_with_the_error() {
        let app = writable().await;
        for pairs in [
            vec![("ttl_value", "soon")],
            vec![("ttl_value", "0")],
            vec![("ttl_value", "8"), ("ttl_unit", "days")],
            vec![("ttl_value", "1"), ("ttl_unit", "weeks")],
        ] {
            let reply = save(&app, &pairs).await;
            assert_eq!(reply.status, 422, "{pairs:?}");
            assert!(reply.body.contains("invalid_settings"), "{pairs:?}");
        }
        assert_eq!(
            app.state
                .store
                .record("ward_map")
                .unwrap()
                .definition
                .ttl_seconds,
            None
        );
    }

    #[tokio::test]
    async fn hidden_and_rejected_when_read_only() {
        let app = read_only().await;
        assert!(
            !get(&app.router, "/tables/ward_map")
                .await
                .body
                .contains("Cache settings")
        );
        let reply = post_form(
            &app.router,
            "/tables/ward_map/settings",
            &[("preload", "on")],
        )
        .await;
        assert_eq!(reply.status, 409);
        assert_eq!(
            post_form(&app.router, "/tables/nope/settings", &[])
                .await
                .status,
            404
        );
    }
}
