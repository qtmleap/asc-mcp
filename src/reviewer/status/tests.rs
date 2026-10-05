use super::*;
use httpmock::{Method::GET, MockServer};
use serde_json::json;

fn setup(server: &MockServer) -> (tempfile::TempDir, Reviewer) {
    let dir = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test(&server.base_url(), dir.path());
    (dir, r)
}
fn get<'a>(server: &'a MockServer, path: &str, body: Value) -> httpmock::Mock<'a> {
    let path = path.to_owned();
    server.mock(move |when, then| {
        when.method(GET).path(path);
        then.json_body(body);
    })
}
fn app(id: &str) -> Value {
    json!({"type":"apps","id":id,"attributes":{"name":format!("App {id}"),"bundleId":format!("com.example.{id}"),"sku":"SKU","secret":"drop-me"}})
}
fn version(id: &str, platform: &str, state: Value, vstate: Value) -> Value {
    json!({"type":"appStoreVersions","id":id,"attributes":{"platform":platform,"versionString":"1.2","appStoreState":state,"appVersionState":vstate,"createdDate":"2026-01-01T00:00:00Z","extra":"drop"}})
}
fn dated(mut v: Value, date: &str) -> Value {
    v["attributes"]["createdDate"] = json!(date);
    v
}
fn submission(id: &str, state: &str, date: Value, version_id: Option<&str>) -> Value {
    let rel = match version_id {
        Some(v) => json!({"appStoreVersionForReview":{"data":{"type":"appStoreVersions","id":v}}}),
        None => json!({"appStoreVersionForReview":{"data":null}}),
    };
    json!({"type":"reviewSubmissions","id":id,"attributes":{"platform":"IOS","state":state,"submittedDate":date},"relationships":rel})
}
fn app_paths(id: &str) -> (String, String) {
    (
        format!("/iris/v1/apps/{id}/appStoreVersions"),
        format!("/iris/v1/apps/{id}/reviewSubmissions"),
    )
}
fn mount_app(server: &MockServer, id: &str, versions: Vec<Value>, subs: Vec<Value>) {
    let (v, s) = app_paths(id);
    get(server, &v, json!({"data":versions}));
    get(server, &s, json!({"data":subs}));
}
fn catch_all<'a>(server: &'a MockServer) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.any_request();
        then.json_body(json!({"data":[]}));
    })
}
fn files(dir: &tempfile::TempDir) -> Vec<String> {
    let mut v: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}
const NEW: [&str; 5] = [
    "list_review_apps",
    "get_app_review_status",
    "get_all_app_review_statuses",
    "get_app_rejection_details",
    "get_review_connection_status",
];

#[test]
fn schemas_are_strict_readonly_and_digits_only() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    let tools = r.tools();
    for name in NEW {
        let t = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(t["annotations"]["readOnlyHint"], true);
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
    }
    for name in ["get_app_review_status", "get_app_rejection_details"] {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(t["inputSchema"]["required"], json!(["app_id"]));
        assert_eq!(
            t["inputSchema"]["properties"]["app_id"]["pattern"],
            "^[0-9]+$"
        );
    }
    for name in [
        "list_review_apps",
        "get_all_app_review_statuses",
        "get_review_connection_status",
    ] {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(t["inputSchema"]["required"], json!([]));
    }
}

#[tokio::test]
async fn invalid_ids_and_extra_args_make_no_http() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    let any = catch_all(&server);
    for bad in ["", "12a", "../1", "1/2", " 1", "１２", "1?x=1"] {
        for tool in ["get_app_review_status", "get_app_rejection_details"] {
            assert!(
                r.call(tool, &json!({"app_id":bad})).await.is_err(),
                "{tool} {bad}"
            );
        }
    }
    for (tool, args) in [
        ("get_app_review_status", json!({"app_id":"1","extra":1})),
        ("get_app_rejection_details", json!({"app_id":"1","extra":1})),
        ("get_app_review_status", json!({"app_id":1})),
        ("get_app_review_status", json!({})),
        ("list_review_apps", json!({"x":1})),
        ("get_all_app_review_statuses", json!({"x":1})),
        ("get_review_connection_status", json!({"x":1})),
    ] {
        assert!(r.call(tool, &args).await.is_err(), "{tool} {args}");
    }
    any.assert_calls(0);
}

#[tokio::test]
async fn list_apps_follows_inherited_pagination_beyond_200() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    let next = format!("{}/iris/v1/apps?cursor=2", server.base_url());
    let p1: Vec<_> = (0..200).map(|i| app(&format!("{}", 1000 + i))).collect();
    let first = server.mock(|when, then| {
        when.method(GET)
            .path("/iris/v1/apps")
            .query_param("limit", "200")
            .query_param_missing("cursor");
        then.json_body(json!({"data":p1,"links":{"next":next}}));
    });
    let second = server.mock(|when, then| {
        when.method(GET)
            .path("/iris/v1/apps")
            .query_param("cursor", "2");
        then.json_body(json!({"data":[app("5000")]}));
    });
    let out = r.call("list_review_apps", &json!({})).await.unwrap();
    assert_eq!(out["apps"].as_array().unwrap().len(), 201);
    assert_eq!(out["complete"], true);
    assert_eq!(out["apps"][200]["id"], "5000");
    assert_eq!(out["apps"][0]["name"], "App 1000");
    assert_eq!(out["apps"][0]["bundle_id"], "com.example.1000");
    assert!(!out.to_string().contains("drop-me"));
    first.assert_calls(1);
    second.assert_calls(1);
}

#[tokio::test]
async fn pagination_failures_and_auth_are_errors() {
    for case in ["cross_origin", "page2_500", "auth"] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        match case {
            "cross_origin" => {
                get(
                    &server,
                    "/iris/v1/apps",
                    json!({"data":[app("1")],"links":{"next":"https://evil.example/iris/v1/apps?c=2"}}),
                );
            }
            "page2_500" => {
                let next = format!("{}/iris/v1/apps?c=2", server.base_url());
                server.mock(|w, t| {
                    w.method(GET)
                        .path("/iris/v1/apps")
                        .query_param("limit", "200");
                    t.json_body(json!({"data":[app("1")],"links":{"next":next}}));
                });
                server.mock(|w, t| {
                    w.method(GET).path("/iris/v1/apps").query_param("c", "2");
                    t.status(500).body("secret-sentinel");
                });
            }
            _ => {
                server.mock(|w, t| {
                    w.method(GET);
                    t.status(401).body("secret-sentinel");
                });
            }
        }
        let e = r
            .call("list_review_apps", &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(!e.contains("secret-sentinel"));
        match case {
            "cross_origin" => assert!(e.contains("unsafe_pagination")),
            "page2_500" => assert!(e.contains("api_unavailable")),
            _ => assert!(e.contains("auth_required")),
        }
    }
}

#[tokio::test]
async fn status_preserves_mixed_platforms_raw_states_and_does_not_infer_approval() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    let (v, s) = app_paths("111");
    server.mock(|w, t| {
        w.method(GET).path(v.clone()).query_param("limit", "200");
        t.json_body(json!({"data":[
            version("v1", "IOS", json!("SOME_NEW_APPLE_STATE"), json!("FUTURE_STATE")),
            version("v2", "MAC_OS", json!("READY_FOR_SALE"), Value::Null),
        ]}));
    });
    server.mock(|w, t| {
        w.method(GET)
            .path(s.clone())
            .query_param("limit", "200")
            .query_param("include", "appStoreVersionForReview");
        t.json_body(json!({"data":[
            submission("s1", "COMPLETE", json!("2026-02-01T00:00:00Z"), Some("v1")),
            submission("s2", "WEIRD_RAW", Value::Null, None),
        ],"included":[version("v1", "IOS", json!("x"), json!("y"))]}));
    });
    let out = r
        .call("get_app_review_status", &json!({"app_id":"111"}))
        .await
        .unwrap();
    assert_eq!(out["app_id"], "111");
    let vs = out["versions"].as_array().unwrap();
    assert_eq!(vs.len(), 2);
    assert_eq!(
        vs[0],
        json!({"id":"v1","platform":"IOS","version_string":"1.2","app_store_state":"SOME_NEW_APPLE_STATE","app_version_state":"FUTURE_STATE","created_date":"2026-01-01T00:00:00Z"})
    );
    assert_eq!(vs[1]["platform"], "MAC_OS");
    assert!(vs[1]["app_version_state"].is_null());
    let ss = out["review_submissions"].as_array().unwrap();
    assert_eq!(
        ss[0],
        json!({"id":"s1","platform":"IOS","state":"COMPLETE","submitted_date":"2026-02-01T00:00:00Z","version_id":"v1"})
    );
    assert_eq!(ss[1]["state"], "WEIRD_RAW");
    assert!(ss[1]["submitted_date"].is_null() && ss[1]["version_id"].is_null());
    assert!(out["notes"].to_string().contains("not approval"));
    assert!(!out.to_string().contains("drop"));
    assert_eq!(out["complete"], true);
}

#[tokio::test]
async fn status_with_no_submissions_is_explicitly_empty() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    mount_app(&server, "111", vec![], vec![]);
    let out = r
        .call("get_app_review_status", &json!({"app_id":"111"}))
        .await
        .unwrap();
    assert_eq!(out["versions"], json!([]));
    assert_eq!(out["review_submissions"], json!([]));
    assert_eq!(out["complete"], true);
}

#[tokio::test]
async fn status_fails_if_either_resource_fails_or_paginates_unsafely() {
    for broken in ["versions", "submissions", "cursor"] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        let (v, s) = app_paths("111");
        if broken == "versions" {
            server.mock(|w, t| {
                w.method(GET).path(v.clone());
                t.status(500);
            });
        } else if broken == "cursor" {
            get(
                &server,
                &v,
                json!({"data":[],"links":{"next":format!("{}/iris/v1/other",server.base_url())}}),
            );
        } else {
            get(&server, &v, json!({"data":[]}));
        }
        if broken == "submissions" {
            server.mock(|w, t| {
                w.method(GET).path(s.clone());
                t.status(503);
            });
        } else {
            get(&server, &s, json!({"data":[]}));
        }
        assert!(
            r.call("get_app_review_status", &json!({"app_id":"111"}))
                .await
                .is_err(),
            "{broken}"
        );
    }
}

#[tokio::test]
async fn status_pagination_inherits_cursor_for_both_resources() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    let (v, s) = app_paths("111");
    let next = format!("{}{}?cursor=2", server.base_url(), v);
    server.mock(|w, t| {
        w.method(GET).path(v.clone()).query_param("limit", "200");
        t.json_body(
            json!({"data":[version("a","IOS",json!("X"),Value::Null)],"links":{"next":next}}),
        );
    });
    server.mock(|w, t| {
        w.method(GET).path(v.clone()).query_param("cursor", "2");
        t.json_body(json!({"data":[version("b","IOS",json!("X"),Value::Null)]}));
    });
    get(&server, &s, json!({"data":[]}));
    let out = r
        .call("get_app_review_status", &json!({"app_id":"111"}))
        .await
        .unwrap();
    assert_eq!(out["versions"].as_array().unwrap().len(), 2);
}

fn setup_batch(server: &MockServer, ids: &[&str]) {
    get(
        server,
        "/iris/v1/apps",
        json!({"data":ids.iter().map(|i| app(i)).collect::<Vec<_>>()}),
    );
}

#[tokio::test]
async fn batch_continues_after_500_and_reports_it() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    setup_batch(&server, &["1", "2", "3"]);
    mount_app(
        &server,
        "1",
        vec![version("v", "IOS", json!("READY_FOR_SALE"), Value::Null)],
        vec![],
    );
    server.mock(|w, t| {
        w.method(GET).path("/iris/v1/apps/2/appStoreVersions");
        t.status(500).body("secret-sentinel");
    });
    mount_app(&server, "3", vec![], vec![]);
    let out = r
        .call("get_all_app_review_statuses", &json!({}))
        .await
        .unwrap();
    assert_eq!(out["complete"], false);
    assert_eq!(out["truncated"], false);
    let apps = out["apps"].as_array().unwrap();
    assert_eq!(
        apps.iter()
            .map(|a| a["app_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["1", "3"]
    );
    assert_eq!(apps[0]["name"], "App 1");
    assert_eq!(apps[0]["status"]["versions"][0]["id"], "v");
    let errors = out["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["app_id"], "2");
    assert!(!errors[0]["error"].as_str().unwrap().is_empty());
    assert!(!out.to_string().contains("secret-sentinel"));
    assert_eq!(out["not_checked"], json!([]));
}

#[tokio::test]
async fn batch_stops_remaining_after_auth_failure() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    setup_batch(&server, &["1", "2", "3"]);
    mount_app(&server, "1", vec![], vec![]);
    server.mock(|w, t| {
        w.method(GET).path("/iris/v1/apps/2/appStoreVersions");
        t.status(401);
    });
    let third = catch_all_for(&server, "/iris/v1/apps/3/appStoreVersions");
    let out = r
        .call("get_all_app_review_statuses", &json!({}))
        .await
        .unwrap();
    assert_eq!(out["complete"], false);
    assert_eq!(out["apps"].as_array().unwrap().len(), 1);
    assert_eq!(out["errors"][0]["app_id"], "2");
    assert!(
        out["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("auth_required")
    );
    assert_eq!(out["not_checked"], json!(["3"]));
    third.assert_calls(0);
}
fn catch_all_for<'a>(server: &'a MockServer, path: &str) -> httpmock::Mock<'a> {
    let path = path.to_owned();
    server.mock(move |w, t| {
        w.method(GET).path(path);
        t.json_body(json!({"data":[]}));
    })
}

#[tokio::test]
async fn batch_preserves_auth_failure_from_either_resource() {
    for slow_versions in [false, true] {
        let server = MockServer::start();
        let (_dir, mut reviewer) = setup(&server);
        reviewer.limits.item = Duration::from_millis(100);
        setup_batch(&server, &["1", "2"]);
        server.mock(|when, then| {
            when.method(GET).path("/iris/v1/apps/1/appStoreVersions");
            if slow_versions {
                then.delay(Duration::from_millis(500))
                    .json_body(json!({"data":[]}));
            } else {
                then.status(500);
            }
        });
        server.mock(|when, then| {
            when.method(GET).path("/iris/v1/apps/1/reviewSubmissions");
            then.status(401);
        });
        let later = catch_all_for(&server, "/iris/v1/apps/2/appStoreVersions");
        let out = reviewer
            .call("get_all_app_review_statuses", &json!({}))
            .await
            .unwrap();
        assert_eq!(out["complete"], false);
        assert!(
            out["errors"][0]["error"]
                .as_str()
                .unwrap()
                .contains("reviewer_auth_required")
        );
        assert_eq!(out["not_checked"], json!(["2"]));
        later.assert_calls(0);
    }
}

#[tokio::test]
async fn batch_item_timeout_is_partial_and_continues() {
    let server = MockServer::start();
    let (_d, mut r) = setup(&server);
    r.limits.item = Duration::from_millis(300);
    setup_batch(&server, &["1", "2"]);
    server.mock(|w, t| {
        w.method(GET).path("/iris/v1/apps/1/appStoreVersions");
        t.delay(Duration::from_secs(2))
            .json_body(json!({"data":[]}));
    });
    get(
        &server,
        "/iris/v1/apps/1/reviewSubmissions",
        json!({"data":[]}),
    );
    mount_app(&server, "2", vec![], vec![]);
    let out = r
        .call("get_all_app_review_statuses", &json!({}))
        .await
        .unwrap();
    assert_eq!(out["complete"], false);
    assert_eq!(out["apps"].as_array().unwrap().len(), 1);
    assert_eq!(out["apps"][0]["app_id"], "2");
    assert!(
        out["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("timeout")
    );
}

#[tokio::test]
async fn batch_deadline_returns_partial_result() {
    let server = MockServer::start();
    let (_d, mut r) = setup(&server);
    r.limits.item = Duration::from_secs(5);
    r.limits.batch = Duration::from_millis(700);
    setup_batch(&server, &["1", "2", "3"]);
    for id in ["1", "2", "3"] {
        server.mock(move |w, t| {
            w.method(GET)
                .path(format!("/iris/v1/apps/{id}/appStoreVersions"));
            t.delay(Duration::from_millis(500))
                .json_body(json!({"data":[]}));
        });
        get(
            &server,
            &format!("/iris/v1/apps/{id}/reviewSubmissions"),
            json!({"data":[]}),
        );
    }
    let started = std::time::Instant::now();
    let out = r
        .call("get_all_app_review_statuses", &json!({}))
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(1500));
    assert_eq!(out["complete"], false);
    assert_eq!(out["apps"].as_array().unwrap().len(), 1);
    let accounted =
        out["errors"].as_array().unwrap().len() + out["not_checked"].as_array().unwrap().len();
    assert_eq!(accounted, 2);
    assert!(!out["errors"].as_array().unwrap().is_empty());
}

#[test]
fn deadlines_are_below_protocol_timeout() {
    let l = Limits::default();
    assert_eq!(l.item, Duration::from_secs(55));
    assert_eq!(l.batch, Duration::from_secs(150));
    assert_eq!(l.output, 8 * 1024 * 1024);
}

#[tokio::test]
async fn batch_output_is_bounded_and_marked_truncated() {
    let server = MockServer::start();
    let (_d, mut r) = setup(&server);
    r.limits.output = 4096;
    let ids: Vec<String> = (1..=30).map(|i| i.to_string()).collect();
    setup_batch(&server, &ids.iter().map(String::as_str).collect::<Vec<_>>());
    for id in &ids {
        mount_app(
            &server,
            id,
            vec![version("v", "IOS", json!("READY_FOR_SALE"), Value::Null)],
            vec![],
        );
    }
    let out = r
        .call("get_all_app_review_statuses", &json!({}))
        .await
        .unwrap();
    assert_eq!(out["truncated"], true);
    assert_eq!(out["complete"], false);
    assert!(serde_json::to_vec(&out).unwrap().len() <= 4096);
    let shown = out["apps"].as_array().unwrap().len();
    assert!(shown > 0 && shown < 30);
    assert_eq!(out["omitted_count"], 30 - shown);
}

#[tokio::test]
async fn batch_list_failure_is_a_hard_error() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    server.mock(|w, t| {
        w.method(GET).path("/iris/v1/apps");
        t.status(500);
    });
    assert!(
        r.call("get_all_app_review_statuses", &json!({}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn connection_status_whitelists_provider_and_hides_identity() {
    let server = MockServer::start();
    let (dir, r) = setup(&server);
    let m = server.mock(|w, t| {
        w.method(GET).path("/olympus/v1/session");
        t.json_body(json!({"user":{"emailAddress":"dev@example.com","fullName":"Secret Person"},
            "provider":{"providerId":123456,"name":"Example Team","publicProviderId":"x","contentTypes":["a"]},
            "availableProviders":[{"providerId":9,"name":"Other"}],"cookies":"secret-sentinel"}));
    });
    let out = r
        .call_with_api("get_review_connection_status", &json!({}), Some(true))
        .await
        .unwrap();
    m.assert_calls(1);
    assert_eq!(out["authentication"], "ok");
    assert_eq!(
        out["provider"],
        json!({"providerId":123456,"name":"Example Team"})
    );
    assert_eq!(out["scope"], "current_provider_session");
    assert!(out["capability"].is_object());
    assert_eq!(out["public_api_configured"], true);
    let text = out.to_string();
    for banned in [
        "dev@example.com",
        "Secret Person",
        "Other",
        "secret-sentinel",
        "publicProviderId",
    ] {
        assert!(!text.contains(banned), "{banned}");
    }
    assert_eq!(files(&dir), ["session.cookie"]);
}

#[tokio::test]
async fn connection_status_provider_missing_or_malformed_is_unknown() {
    for body in [
        json!({"user":{}}),
        json!({"provider":{"providerId":"123","name":"x"}}),
        json!({"provider":{"providerId":1}}),
        json!({"provider":{"name":"x"}}),
        json!({"provider":{},"user":null}),
    ] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        get(&server, "/olympus/v1/session", body.clone());
        let out = r
            .call_with_api("get_review_connection_status", &json!({}), Some(false))
            .await
            .unwrap();
        assert_eq!(out["authentication"], "ok", "{body}");
        assert_eq!(out["provider"], "unknown", "{body}");
        assert_eq!(out["public_api_configured"], false);
    }
}

#[tokio::test]
async fn connection_status_classifies_failures_without_leaking() {
    for (status, expected) in [
        (401, "rejected"),
        (403, "rejected"),
        (302, "rejected"),
        (500, "unreachable"),
        (503, "unreachable"),
    ] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        server.mock(|w, t| {
            w.method(GET).path("/olympus/v1/session");
            t.status(status).body("secret-sentinel");
        });
        let out = r
            .call("get_review_connection_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(out["authentication"], expected, "{status}");
        assert!(out["provider"] == "unknown");
        assert!(out["public_api_configured"].is_null());
        assert!(!out.to_string().contains("secret-sentinel"));
    }
}

#[tokio::test]
async fn connection_status_missing_session_makes_no_request() {
    let server = MockServer::start();
    let (dir, r) = setup(&server);
    std::fs::remove_file(dir.path().join("session.cookie")).unwrap();
    let any = catch_all(&server);
    let out = r
        .call("get_review_connection_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(out["authentication"], "missing");
    any.assert_calls(0);
}

fn rejected_fixture(server: &MockServer) {
    mount_app(
        server,
        "111",
        vec![
            version("v1", "IOS", json!("REJECTED"), json!("REJECTED")),
            version("v2", "MAC_OS", json!("REJECTED"), json!("READY_FOR_REVIEW")),
            version("v3", "TV_OS", json!("METADATA_REJECTED"), Value::Null),
            dated(
                version("v4", "IOS", json!("READY_FOR_SALE"), Value::Null),
                "2025-12-01T00:00:00Z",
            ),
        ],
        vec![
            submission(
                "s_old",
                "COMPLETE",
                json!("2026-01-01T00:00:00Z"),
                Some("v1"),
            ),
            submission(
                "s_new",
                "UNRESOLVED_ISSUES",
                json!("2026-02-01T00:00:00Z"),
                Some("v1"),
            ),
            submission(
                "s_other",
                "COMPLETE",
                json!("2026-03-01T00:00:00Z"),
                Some("v4"),
            ),
            submission(
                "s_unlinked",
                "COMPLETE",
                json!("2026-04-01T00:00:00Z"),
                None,
            ),
        ],
    );
}
fn threads_mock<'a>(server: &'a MockServer, sid: &str, threads: &[&str]) -> httpmock::Mock<'a> {
    let sid = sid.to_owned();
    let data: Vec<_> = threads.iter().map(|t| json!({"id":t})).collect();
    server.mock(move |w, t| {
        w.method(GET)
            .path("/iris/v1/resolutionCenterThreads")
            .query_param("filter[reviewSubmission]", sid);
        t.json_body(json!({"data":data}));
    })
}
fn messages_mock<'a>(server: &'a MockServer, thread: &str, body: Value) -> httpmock::Mock<'a> {
    let path = format!("/iris/v1/resolutionCenterThreads/{thread}/resolutionCenterMessages");
    server.mock(move |w, t| {
        w.method(GET)
            .path(path)
            .query_param(
                "include",
                "fromActor,rejections,resolutionCenterMessageAttachments",
            )
            .query_param("limit", "200");
        t.json_body(body);
    })
}
fn apple_messages() -> Value {
    json!({"data":[{"id":"m1","type":"resolutionCenterMessages",
        "attributes":{"createdDate":"2026-02-02T00:00:00Z","messageBody":"<p>Guideline 2.1</p>"},
        "relationships":{"fromActor":{"data":{"type":"actors","id":"a1"}},
          "rejections":{"data":[{"type":"reviewRejections","id":"r1"}]},
          "resolutionCenterMessageAttachments":{"data":[{"type":"resolutionCenterMessageAttachments","id":"att1"}]}}}],
      "included":[
        {"type":"actors","id":"a1","attributes":{"actorType":"APPLE","userEmail":"hidden@example.com"}},
        {"type":"reviewRejections","id":"r1","attributes":{"reasons":[{"reasonCode":"5.2.5","reasonDescription":"Legal: Intellectual Property - Apple Products","reasonSection":"5.2.5","private":"drop"}],"internal":"drop"}},
        {"type":"resolutionCenterMessageAttachments","id":"att1","attributes":{"fileName":"shot.png","fileSize":12,"downloadUrl":"https://x"}}]})
}

#[tokio::test]
async fn rejection_missing_current_evidence_is_incomplete() {
    for broken in [
        "body",
        "linked_resource",
        "reasons_missing",
        "reasons_malformed",
        "reason_fields",
        "not_apple",
    ] {
        let server = MockServer::start();
        let (_dir, reviewer) = setup(&server);
        mount_app(
            &server,
            "111",
            vec![version("v1", "IOS", json!("REJECTED"), Value::Null)],
            vec![submission(
                "s1",
                "UNRESOLVED_ISSUES",
                json!("2026-02-01T00:00:00Z"),
                Some("v1"),
            )],
        );
        threads_mock(&server, "s1", &["t1"]);
        let mut messages = apple_messages();
        match broken {
            "body" => {
                messages["data"][0]["attributes"]
                    .as_object_mut()
                    .unwrap()
                    .remove("messageBody");
            }
            "linked_resource" => {
                messages["included"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|item| item["type"] != "reviewRejections");
            }
            "reasons_missing" => {
                messages["included"][1]["attributes"]
                    .as_object_mut()
                    .unwrap()
                    .remove("reasons");
            }
            "reasons_malformed" => {
                messages["included"][1]["attributes"]["reasons"] = json!("invalid");
            }
            "reason_fields" => {
                messages["included"][1]["attributes"]["reasons"] = json!([{}]);
            }
            "not_apple" => {
                messages["included"][0]["attributes"]["actorType"] = json!("DEVELOPER");
            }
            _ => unreachable!(),
        }
        messages_mock(&server, "t1", messages);
        let out = reviewer
            .call("get_app_rejection_details", &json!({"app_id":"111"}))
            .await
            .unwrap();
        assert_eq!(out["complete"], false, "{broken}");
        assert!(
            !out["incomplete_reasons"].as_array().unwrap().is_empty(),
            "{broken}"
        );
        if matches!(broken, "reasons_missing" | "reasons_malformed") {
            assert!(out["rejected_versions"][0]["current_messages"][0]["rejections"][0]["attributes"]["reasons"].is_null());
        }
    }
}

#[tokio::test]
async fn rejection_explicit_empty_reason_array_is_preserved() {
    let server = MockServer::start();
    let (_dir, reviewer) = setup(&server);
    mount_app(
        &server,
        "111",
        vec![version("v1", "IOS", json!("REJECTED"), Value::Null)],
        vec![submission(
            "s1",
            "UNRESOLVED_ISSUES",
            json!("2026-02-01T00:00:00Z"),
            Some("v1"),
        )],
    );
    threads_mock(&server, "s1", &["t1"]);
    let mut messages = apple_messages();
    messages["included"][1]["attributes"]["reasons"] = json!([]);
    messages_mock(&server, "t1", messages);
    let out = reviewer
        .call("get_app_rejection_details", &json!({"app_id":"111"}))
        .await
        .unwrap();
    assert_eq!(out["complete"], true);
    assert_eq!(
        out["rejected_versions"][0]["current_messages"][0]["rejections"][0]["attributes"]["reasons"],
        json!([])
    );
}

#[tokio::test]
async fn rejection_details_use_latest_linked_submission_and_label_history() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    rejected_fixture(&server);
    let new_threads = threads_mock(&server, "s_new", &["t_new"]);
    let old_threads = threads_mock(&server, "s_old", &["t_old"]);
    let other = threads_mock(&server, "s_other", &["t_other"]);
    let unlinked = threads_mock(&server, "s_unlinked", &["t_unlinked"]);
    let new_msgs = messages_mock(&server, "t_new", apple_messages());
    let old_msgs = messages_mock(&server, "t_old", json!({"data":[],"included":[]}));
    let out = r
        .call("get_app_rejection_details", &json!({"app_id":"111"}))
        .await
        .unwrap();
    other.assert_calls(0);
    unlinked.assert_calls(0);
    new_threads.assert_calls(1);
    old_threads.assert_calls(1);
    new_msgs.assert_calls(1);
    old_msgs.assert_calls(1);
    let rejected: Vec<_> = out["rejected_versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(rejected, ["v1", "v3"]);
    let v1 = &out["rejected_versions"][0];
    assert_eq!(v1["rejection_state"], "REJECTED");
    assert_eq!(v1["latest_submission"]["id"], "s_new");
    assert_eq!(v1["latest_status"], "latest_submission_identified");
    let msg = &v1["current_messages"][0];
    assert_eq!(msg["id"], "m1");
    assert_eq!(msg["submission_id"], "s_new");
    assert_eq!(msg["created_date"], "2026-02-02T00:00:00Z");
    assert_eq!(msg["body_html"], "<p>Guideline 2.1</p>");
    assert_eq!(msg["actor_type"], "APPLE");
    assert_eq!(msg["is_apple"], true);
    assert_eq!(msg["rejections"][0]["id"], "r1");
    assert_eq!(
        msg["rejections"][0]["attributes"],
        json!({"reasons":[{"reasonCode":"5.2.5","reasonDescription":"Legal: Intellectual Property - Apple Products","reasonSection":"5.2.5"}]})
    );
    assert_eq!(msg["actor_name"], Value::Null);
    assert!(!out.to_string().contains("drop"));
    assert_eq!(msg["attachments"][0]["id"], "att1");
    assert_eq!(msg["attachments"][0]["file_name"], "shot.png");
    assert!(
        v1["older_message_history"]["label"]
            .as_str()
            .unwrap()
            .contains("older")
    );
    assert_eq!(
        v1["older_message_history"]["submissions"][0]["submission_id"],
        "s_old"
    );
    assert!(!out.to_string().contains("hidden@example.com"));
    // v3 has no linked submission: unknown, never "no issues".
    let v3 = &out["rejected_versions"][1];
    assert_eq!(v3["rejection_state"], "METADATA_REJECTED");
    assert_eq!(v3["latest_status"], "unknown_no_linked_submission");
    assert_eq!(out["complete"], false);
    assert!(!out["incomplete_reasons"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn rejection_prefers_app_version_state_over_store_state() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    mount_app(
        &server,
        "111",
        vec![version(
            "v2",
            "MAC_OS",
            json!("REJECTED"),
            json!("READY_FOR_REVIEW"),
        )],
        vec![],
    );
    let out = r
        .call("get_app_rejection_details", &json!({"app_id":"111"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "no_current_rejection");
    assert_eq!(out["rejected_versions"], json!([]));
    assert_eq!(out["complete"], true);
}

#[tokio::test]
async fn rejection_ambiguous_or_undated_latest_is_unknown_without_inference() {
    for (name, subs) in [
        (
            "tie",
            vec![
                submission("a", "COMPLETE", json!("2026-01-01T00:00:00Z"), Some("v1")),
                submission("b", "COMPLETE", json!("2026-01-01T00:00:00Z"), Some("v1")),
            ],
        ),
        (
            "invalid",
            vec![
                submission("a", "COMPLETE", json!("2026-01-01T00:00:00Z"), Some("v1")),
                submission("b", "COMPLETE", json!("not-a-date"), Some("v1")),
            ],
        ),
        (
            "null",
            vec![submission("a", "COMPLETE", Value::Null, Some("v1"))],
        ),
        (
            "date_only_unlinked",
            vec![submission(
                "a",
                "COMPLETE",
                json!("2026-01-01T00:00:00Z"),
                None,
            )],
        ),
    ] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        mount_app(
            &server,
            "111",
            vec![version("v1", "IOS", json!("REJECTED"), Value::Null)],
            subs,
        );
        let threads = catch_all_for(&server, "/iris/v1/resolutionCenterThreads");
        let out = r
            .call("get_app_rejection_details", &json!({"app_id":"111"}))
            .await
            .unwrap();
        assert_eq!(out["complete"], false, "{name}");
        assert_ne!(out["status"], "no_current_rejection", "{name}");
        assert!(
            out["rejected_versions"][0]["latest_status"]
                .as_str()
                .unwrap()
                .starts_with("unknown"),
            "{name}"
        );
        assert!(
            out["rejected_versions"][0]["current_messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        threads.assert_calls(0);
    }
}

#[tokio::test]
async fn rejection_unknown_actor_is_never_assumed_apple() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    mount_app(
        &server,
        "111",
        vec![version("v1", "IOS", json!("REJECTED"), Value::Null)],
        vec![submission(
            "s",
            "COMPLETE",
            json!("2026-01-01T00:00:00Z"),
            Some("v1"),
        )],
    );
    threads_mock(&server, "s", &["t"]);
    messages_mock(
        &server,
        "t",
        json!({"data":[
        {"id":"m1","attributes":{"messageBody":"x","createdDate":"d"},"relationships":{"fromActor":{"data":{"id":"u"}}}},
        {"id":"m2","attributes":{"messageBody":"y"},"relationships":{}}],
      "included":[{"type":"actors","id":"u","attributes":{"actorType":"SOMETHING_NEW","name":"Apple Review"}}]}),
    );
    let out = r
        .call("get_app_rejection_details", &json!({"app_id":"111"}))
        .await
        .unwrap();
    let msgs = out["rejected_versions"][0]["current_messages"]
        .as_array()
        .unwrap();
    for m in msgs {
        assert_eq!(m["is_apple"], false);
        assert_eq!(m["actor_type"], "UNKNOWN");
        assert!(m["actor_name"].is_null());
    }
    assert_eq!(out["complete"], false);
    assert!(
        out["incomplete_reasons"]
            .to_string()
            .contains("unknown_actor")
    );
}

#[tokio::test]
async fn rejection_resource_failure_is_an_error() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    mount_app(
        &server,
        "111",
        vec![version("v1", "IOS", json!("REJECTED"), Value::Null)],
        vec![submission(
            "s",
            "COMPLETE",
            json!("2026-01-01T00:00:00Z"),
            Some("v1"),
        )],
    );
    server.mock(|w, t| {
        w.method(GET).path("/iris/v1/resolutionCenterThreads");
        t.status(500);
    });
    assert!(
        r.call("get_app_rejection_details", &json!({"app_id":"111"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn status_tools_never_write_monitor_state() {
    let server = MockServer::start();
    let (dir, r) = setup(&server);
    rejected_fixture(&server);
    get(&server, "/iris/v1/apps", json!({"data":[app("111")]}));
    threads_mock(&server, "s_new", &[]);
    threads_mock(&server, "s_old", &[]);
    get(&server, "/olympus/v1/session", json!({}));
    for (tool, args) in [
        ("list_review_apps", json!({})),
        ("get_app_review_status", json!({"app_id":"111"})),
        ("get_all_app_review_statuses", json!({})),
        ("get_app_rejection_details", json!({"app_id":"111"})),
        ("get_review_connection_status", json!({})),
    ] {
        let _ = r.call(tool, &args).await;
    }
    assert_eq!(files(&dir), ["session.cookie"]);
}

#[tokio::test]
async fn connection_status_requires_session_shape() {
    for body in [
        json!({}),
        json!([]),
        json!("ok"),
        json!({"user":null,"provider":null}),
        json!({"user":"x","provider":[1]}),
        json!({"availableProviders":[{"providerId":9,"name":"Other"}]}),
    ] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        get(&server, "/olympus/v1/session", body.clone());
        let out = r
            .call_with_api("get_review_connection_status", &json!({}), None)
            .await
            .unwrap();
        assert_eq!(out["authentication"], "unknown", "{body}");
        assert_eq!(out["provider"], "unknown", "{body}");
        assert!(!out.to_string().contains("Other"));
    }
}

#[tokio::test]
async fn rejection_missing_state_is_incomplete_not_no_rejection() {
    for (vs, st) in [
        (Value::Null, Value::Null),
        (json!(7), Value::Null),
        (Value::Null, json!({"x":1})),
    ] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        mount_app(&server, "111", vec![version("v1", "IOS", st, vs)], vec![]);
        let out = r
            .call("get_app_rejection_details", &json!({"app_id":"111"}))
            .await
            .unwrap();
        assert_eq!(out["status"], "unknown_current_state");
        assert_eq!(out["complete"], false);
        assert_eq!(out["unclassified_versions"][0]["id"], "v1");
        assert!(
            out["unclassified_versions"][0]
                .get("app_version_state")
                .is_some()
        );
    }
}

#[tokio::test]
async fn rejection_unknown_raw_state_is_incomplete_not_no_rejection() {
    let server = MockServer::start();
    let (_dir, reviewer) = setup(&server);
    mount_app(
        &server,
        "111",
        vec![version(
            "v1",
            "IOS",
            json!("READY_FOR_SALE"),
            json!("FUTURE_REVIEW_STATE"),
        )],
        vec![],
    );
    let out = reviewer
        .call("get_app_rejection_details", &json!({"app_id":"111"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "unknown_current_state");
    assert_eq!(out["complete"], false);
    assert_eq!(
        out["unclassified_versions"][0]["app_version_state"],
        "FUTURE_REVIEW_STATE"
    );
}

#[tokio::test]
async fn rejection_older_rejected_version_with_newer_accepted_is_historical() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    mount_app(
        &server,
        "111",
        vec![
            dated(
                version("old", "IOS", json!("REJECTED"), Value::Null),
                "2026-01-01T00:00:00Z",
            ),
            dated(
                version("new", "IOS", json!("READY_FOR_SALE"), Value::Null),
                "2026-02-01T00:00:00Z",
            ),
        ],
        vec![submission(
            "s",
            "COMPLETE",
            json!("2026-01-02T00:00:00Z"),
            Some("old"),
        )],
    );
    let threads = catch_all_for(&server, "/iris/v1/resolutionCenterThreads");
    let out = r
        .call("get_app_rejection_details", &json!({"app_id":"111"}))
        .await
        .unwrap();
    threads.assert_calls(0);
    assert_eq!(out["status"], "no_current_rejection");
    assert_eq!(out["complete"], true);
    assert_eq!(out["rejected_versions"], json!([]));
    assert_eq!(
        out["historical_rejected_versions"][0]["version"]["id"],
        "old"
    );
    assert_eq!(
        out["historical_rejected_versions"][0]["superseded_by"],
        "new"
    );
}

#[tokio::test]
async fn rejection_mixed_platforms_only_supersede_same_platform() {
    let server = MockServer::start();
    let (_d, r) = setup(&server);
    mount_app(
        &server,
        "111",
        vec![
            dated(
                version("ios_rej", "IOS", json!("REJECTED"), Value::Null),
                "2026-01-01T00:00:00Z",
            ),
            dated(
                version("mac_new", "MAC_OS", json!("READY_FOR_SALE"), Value::Null),
                "2026-03-01T00:00:00Z",
            ),
        ],
        vec![],
    );
    let out = r
        .call("get_app_rejection_details", &json!({"app_id":"111"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "rejected_versions_found");
    assert_eq!(out["rejected_versions"][0]["version"]["id"], "ios_rej");
    assert_eq!(out["historical_rejected_versions"], json!([]));
}

#[tokio::test]
async fn rejection_missing_chronology_is_incomplete_not_guessed() {
    for (name, rejected, other) in [
        (
            "bad_rejected_date",
            json!("nope"),
            json!("2026-02-01T00:00:00Z"),
        ),
        ("bad_other_date", json!("2026-01-01T00:00:00Z"), Value::Null),
        (
            "tie",
            json!("2026-01-01T00:00:00Z"),
            json!("2026-01-01T00:00:00Z"),
        ),
    ] {
        let server = MockServer::start();
        let (_d, r) = setup(&server);
        let mut a = version("a", "IOS", json!("REJECTED"), Value::Null);
        a["attributes"]["createdDate"] = rejected;
        let mut b = version("b", "IOS", json!("READY_FOR_SALE"), Value::Null);
        b["attributes"]["createdDate"] = other;
        mount_app(&server, "111", vec![a, b], vec![]);
        let out = r
            .call("get_app_rejection_details", &json!({"app_id":"111"}))
            .await
            .unwrap();
        assert_eq!(out["complete"], false, "{name}");
        assert_eq!(out["status"], "rejected_versions_found", "{name}");
        assert_eq!(out["rejected_versions"][0]["version"]["id"], "a", "{name}");
        assert!(
            out["incomplete_reasons"][0]
                .as_str()
                .unwrap()
                .starts_with("unknown_chronology"),
            "{name}"
        );
    }
}
