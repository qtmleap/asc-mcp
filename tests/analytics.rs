//! Offline contract tests for the read-only Apple usage/crash report tools.
//! Everything runs against an httpmock loopback server through `Server::call`.
use asc_mcp::{client::ApiClient, protocol::Server};
use flate2::{Compression, write::GzEncoder};
use httpmock::{Method::GET, MockServer};
use serde_json::{Value, json};
use std::io::Write;

const SECRET: &str = "SECRET-SIG";

fn server_for(mock: &MockServer) -> Server {
    Server {
        api: Some(ApiClient::with_endpoint(&mock.base_url()).unwrap()),
        reviewer: None,
    }
}
fn gzip(text: &str) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(text.as_bytes()).unwrap();
    e.finish().unwrap()
}
fn md5_hex(bytes: &[u8]) -> String {
    format!("{:x}", md5::compute(bytes))
}
/// Segment record as Apple returns it: md5 of the gzip bytes, size, signed URL.
fn segment(id: &str, url: &str, gz: &[u8]) -> Value {
    json!({"type":"analyticsReportSegments","id":id,"attributes":{
        "checksum":md5_hex(gz),"sizeInBytes":gz.len(),"url":url}})
}
fn instance(id: &str, granularity: &str, processing: &str) -> Value {
    json!({"type":"analyticsReportInstances","id":id,
        "attributes":{"granularity":granularity,"processingDate":processing}})
}
/// ONGOING request req1 with the two APP_USAGE reports r1 (sessions) / r2 (crashes).
async fn mock_request_and_reports(m: &MockServer) {
    m.mock_async(|when, then| {
        when.method(GET).path("/v1/apps/1/analyticsReportRequests");
        then.status(200).json_body(json!({"data":[
            {"type":"analyticsReportRequests","id":"req1","attributes":{"accessType":"ONGOING"}}]}));
    })
    .await;
    m.mock_async(|when, then| {
        when.method(GET).path("/v1/analyticsReportRequests/req1/reports");
        then.status(200).json_body(json!({"data":[
            {"type":"analyticsReports","id":"r1","attributes":{"name":"App Sessions Standard","category":"APP_USAGE"}},
            {"type":"analyticsReports","id":"r2","attributes":{"name":"App Crashes","category":"APP_USAGE"}}]}));
    })
    .await;
}
async fn mock_instances(m: &MockServer, report: &str, instances: Vec<Value>) {
    let path = format!("/v1/analyticsReports/{report}/instances");
    m.mock_async(move |when, then| {
        when.method(GET).path(path);
        then.status(200).json_body(json!({"data":instances}));
    })
    .await;
}
async fn mock_segments(m: &MockServer, instance: &str, segments: Vec<Value>) {
    let path = format!("/v1/analyticsReportInstances/{instance}/segments");
    m.mock_async(move |when, then| {
        when.method(GET).path(path);
        then.status(200).json_body(json!({"data":segments}));
    })
    .await;
}
/// Signed download: must carry neither Authorization nor Cookie.
async fn mock_download<'a>(m: &'a MockServer, name: &str, gz: Vec<u8>) -> httpmock::Mock<'a> {
    let path = format!("/download/{name}");
    m.mock_async(move |when, then| {
        when.method(GET)
            .path(path)
            .query_param("sig", SECRET)
            .header_missing("authorization")
            .header_missing("cookie");
        then.status(200).body(gz);
    })
    .await
}
fn signed(m: &MockServer, name: &str) -> String {
    m.url(format!("/download/{name}?sig={SECRET}"))
}
/// Sessions fixture with exactly one segment holding `tsv` (or raw bytes).
async fn one_segment_setup(m: &MockServer, url: String, gz: &[u8], checksum_of: &[u8]) {
    mock_request_and_reports(m).await;
    mock_instances(m, "r1", vec![instance("i1", "DAILY", "2026-10-10")]).await;
    let mut seg = segment("s1", &url, gz);
    seg["attributes"]["checksum"] = json!(md5_hex(checksum_of));
    mock_segments(m, "i1", vec![seg]).await;
}
const SESSIONS_HEADER: &str =
    "Date\tApp Apple Identifier\tDevice\tTerritory\tSessions\tUnique Devices\n";

#[tokio::test]
async fn collection_page_limit_never_fetches_an_eleventh_page() {
    let m = MockServer::start_async().await;
    let path = "/v1/apps/1/analyticsReportRequests";
    for page in 1..=11 {
        let next = m.url(format!("{path}?page={}", page + 1));
        let mock = m
            .mock_async(move |when, then| {
                let when = when.method(GET).path(path);
                if page == 1 {
                    when.query_param_missing("page");
                } else {
                    when.query_param("page", page.to_string());
                }
                then.status(200)
                    .json_body(json!({"data":[],"links":{"next":next}}));
            })
            .await;
        if page == 11 {
            let out = server_for(&m)
                .call("get_app_usage_report", &json!({"app_id":"1"}))
                .await
                .unwrap();
            assert_eq!(out["status"], "unavailable", "{out}");
            mock.assert_calls_async(0).await;
        }
    }
}

#[tokio::test]
async fn collection_item_limit_rejects_first_page_and_cumulative_overflow() {
    for (first_count, second_count) in [(201, 0), (150, 51)] {
        let m = MockServer::start_async().await;
        let path = "/v1/apps/1/analyticsReportRequests";
        let next = m.url(format!("{path}?page=2"));
        m.mock_async(move |when, then| {
            when.method(GET).path(path).query_param_missing("page");
            then.status(200)
                .json_body(json!({"data":vec![json!({});first_count],"links":{"next":next}}));
        })
        .await;
        let second = m
            .mock_async(move |when, then| {
                when.method(GET).path(path).query_param("page", "2");
                then.status(200)
                    .json_body(json!({"data":vec![json!({});second_count]}));
            })
            .await;
        let out = server_for(&m)
            .call("get_app_usage_report", &json!({"app_id":"1"}))
            .await
            .unwrap();
        assert_eq!(out["status"], "unavailable", "{out}");
        second
            .assert_calls_async(usize::from(first_count <= 200))
            .await;
    }
}

#[tokio::test]
async fn explicit_date_does_not_arbitrarily_choose_between_equal_processing_dates() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(
        &m,
        "r1",
        vec![
            instance("a", "DAILY", "2026-10-12"),
            instance("b", "DAILY", "2026-10-12"),
        ],
    )
    .await;
    for (id, count) in [("a", 10), ("b", 20)] {
        let bytes = gzip(&format!(
            "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t{count}\t5\n"
        ));
        mock_segments(&m, id, vec![segment(id, &signed(&m, id), &bytes)]).await;
        mock_download(&m, id, bytes).await;
    }
    let out = server_for(&m)
        .call(
            "get_app_usage_report",
            &json!({"app_id":"1","report_date":"2026-10-09"}),
        )
        .await
        .unwrap();
    assert_eq!(out["status"], "unavailable", "{out}");
    assert!(out["sessions_total"].is_null());
}

#[tokio::test]
async fn correction_replaces_the_entire_date_partition_including_removed_dimensions() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(
        &m,
        "r1",
        vec![
            instance("old", "DAILY", "2026-10-10"),
            instance("new", "DAILY", "2026-10-12"),
        ],
    )
    .await;
    let old = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t20\t15\n2026-10-09\t1\tiPad\tJP\t4\t3\n"
    ));
    let new = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t25\t18\n"
    ));
    for (id, bytes) in [("old", old), ("new", new)] {
        mock_segments(&m, id, vec![segment(id, &signed(&m, id), &bytes)]).await;
        mock_download(&m, id, bytes).await;
    }
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["processing_date"], "2026-10-12");
    assert_eq!(out["rows"].as_array().unwrap().len(), 1);
    assert_eq!(out["sessions_total"], 25);
    assert!(!out["rows"].to_string().contains("iPad"));
}

#[tokio::test]
async fn tools_are_listed_read_only_with_app_id_required_and_legacy_catalogue_kept() {
    let m = MockServer::start_async().await;
    let tools = server_for(&m).tools();
    assert_eq!(tools.len(), 366);
    for name in ["get_app_usage_report", "get_app_crash_report"] {
        let t = tools.iter().find(|t| t["name"] == name).expect(name);
        assert_eq!(t["inputSchema"]["required"], json!(["app_id"]));
        assert_eq!(t["annotations"]["readOnlyHint"], true);
    }
}

#[tokio::test]
async fn forbidden_requests_listing_is_permission_denied_not_zero() {
    let m = MockServer::start_async().await;
    m.mock_async(|when, then| {
        when.method(GET).path("/v1/apps/1/analyticsReportRequests");
        then.status(403)
            .json_body(json!({"errors":[{"code":"FORBIDDEN"}]}));
    })
    .await;
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "permission_denied");
    assert!(
        out.get("rows")
            .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty))
    );
    assert!(out["sessions_total"].is_null());
}

#[tokio::test]
async fn no_report_request_yet_is_pending_not_zero() {
    let m = MockServer::start_async().await;
    m.mock_async(|when, then| {
        when.method(GET).path("/v1/apps/1/analyticsReportRequests");
        then.status(200).json_body(json!({"data":[]}));
    })
    .await;
    let out = server_for(&m)
        .call("get_app_crash_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "pending");
    assert!(out["crashes_total"].is_null());
    assert!(
        out.get("rows")
            .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty))
    );
}

#[tokio::test]
async fn usage_report_merges_segments_pages_and_newest_correction_without_summing_devices() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    // i1 (2026-10-10) is paged over two segments; i2 (2026-10-12) corrects 10-09;
    // the WEEKLY instance must be ignored for the default DAILY granularity.
    mock_instances(
        &m,
        "r1",
        vec![
            instance("i1", "DAILY", "2026-10-10"),
            instance("i2", "DAILY", "2026-10-12"),
            instance("i3", "WEEKLY", "2026-10-13"),
        ],
    )
    .await;
    let a = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-08\t1\tiPhone\tJP\t10\t7\n2026-10-08\t1\tiPad\tJP\t4\t3\n"
    ));
    let b = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t20\t15\n"
    ));
    let c = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t25\t18\n2026-10-09\t1\tiPad\tJP\t5\t4\n"
    ));
    let w = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-05\t1\tiPhone\tJP\t999\t999\n"
    ));
    let (url_a, url_b) = (signed(&m, "a"), signed(&m, "b"));
    m.mock_async(|when, then| {
        when.method(GET)
            .path("/v1/analyticsReportInstances/i1/segments")
            .query_param_missing("cursor");
        then.status(200).json_body(json!({
            "data":[segment("sa", &url_a, &a)],
            "links":{"next":m.url("/v1/analyticsReportInstances/i1/segments?cursor=2")}}));
    })
    .await;
    m.mock_async(|when, then| {
        when.method(GET)
            .path("/v1/analyticsReportInstances/i1/segments")
            .query_param("cursor", "2");
        then.status(200)
            .json_body(json!({"data":[segment("sb", &url_b, &b)]}));
    })
    .await;
    mock_segments(&m, "i2", vec![segment("sc", &signed(&m, "c"), &c)]).await;
    mock_segments(&m, "i3", vec![segment("sw", &signed(&m, "w"), &w)]).await;
    for (name, bytes) in [("a", a), ("b", b), ("c", c)] {
        mock_download(&m, name, bytes).await;
    }
    let weekly = mock_download(&m, "w", w).await;
    let server = server_for(&m);

    // Default date = latest event date available (10-09); correction wins per row.
    let out = server
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["app_id"], "1");
    assert_eq!(out["report_name"], "App Sessions Standard");
    assert_eq!(out["granularity"], "DAILY");
    assert_eq!(out["report_date"], "2026-10-09");
    assert_eq!(out["processing_date"], "2026-10-12");
    let rows = out["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let mut sessions: Vec<_> = rows
        .iter()
        .map(|r| r["sessions"].as_u64().unwrap())
        .collect();
    sessions.sort();
    assert_eq!(sessions, [5, 25]);
    assert!(
        rows.iter()
            .any(|r| r["unique_devices"] == 18 && r["dimensions"].to_string().contains("iPhone"))
    );
    assert_eq!(out["sessions_total"], 30);
    assert!(out["unique_devices_total"].is_null());

    // Explicit earlier date comes from i1's first page, with i1's processing date.
    let out = server
        .call(
            "get_app_usage_report",
            &json!({"app_id":"1","report_date":"2026-10-08"}),
        )
        .await
        .unwrap();
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["report_date"], "2026-10-08");
    assert_eq!(out["processing_date"], "2026-10-10");
    assert_eq!(out["rows"].as_array().unwrap().len(), 2);
    assert_eq!(out["sessions_total"], 14);
    assert!(out["unique_devices_total"].is_null());

    // Reports exist but not for this event date: no_data, never zero totals.
    let out = server
        .call(
            "get_app_usage_report",
            &json!({"app_id":"1","report_date":"2026-09-01"}),
        )
        .await
        .unwrap();
    assert_eq!(out["status"], "no_data");
    assert!(out["sessions_total"].is_null());
    weekly.assert_calls_async(0).await;
}

#[tokio::test]
async fn weekly_granularity_selects_weekly_instances_and_crash_report_is_used() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    let sessions = mock_instances_counting(&m, "r1").await;
    mock_instances(
        &m,
        "r2",
        vec![
            instance("c1", "DAILY", "2026-10-11"),
            instance("c2", "WEEKLY", "2026-10-12"),
        ],
    )
    .await;
    let gz = gzip(
        "Date\tApp Apple Identifier\tDevice\tTerritory\tCrashes\tUnique Devices\n2026-10-05\t1\tiPhone\tJP\t3\t2\n2026-10-05\t1\tiPad\tJP\t1\t1\n",
    );
    mock_segments(&m, "c2", vec![segment("sc", &signed(&m, "crash"), &gz)]).await;
    mock_download(&m, "crash", gz).await;
    let out = server_for(&m)
        .call(
            "get_app_crash_report",
            &json!({"app_id":"1","granularity":"WEEKLY"}),
        )
        .await
        .unwrap();
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["report_name"], "App Crashes");
    assert_eq!(out["granularity"], "WEEKLY");
    assert_eq!(out["report_date"], "2026-10-05");
    assert_eq!(out["crashes_total"], 4);
    assert!(out["unique_devices_total"].is_null());
    assert!(out["rows"][0].get("crashes").is_some());
    sessions.assert_calls_async(0).await;
}
async fn mock_instances_counting<'a>(m: &'a MockServer, report: &str) -> httpmock::Mock<'a> {
    let path = format!("/v1/analyticsReports/{report}/instances");
    m.mock_async(move |when, then| {
        when.method(GET).path(path);
        then.status(200).json_body(json!({"data":[]}));
    })
    .await
}

#[tokio::test]
async fn invalid_arguments_are_rejected_before_any_request() {
    let m = MockServer::start_async().await;
    let any = m
        .mock_async(|when, then| {
            when.any_request();
            then.status(200).json_body(json!({"data":[]}));
        })
        .await;
    let server = server_for(&m);
    for args in [
        json!({}),
        json!({"app_id":""}),
        json!({"app_id":"1","report_date":"2026-02-30"}),
        json!({"app_id":"1","report_date":"10/09/2026"}),
        json!({"app_id":"1","report_date":"2026-10-09T00:00:00Z"}),
        json!({"app_id":"1","granularity":"HOURLY"}),
        json!({"app_id":"1","granularity":"daily"}),
        json!({"app_id":"1","extra":true}),
    ] {
        let err = server
            .call("get_app_usage_report", &args)
            .await
            .unwrap_err();
        assert!(!err.to_string().contains("unknown tool"), "{args}: {err}");
    }
    any.assert_calls_async(0).await;
}

#[tokio::test]
async fn corrupt_segments_are_unavailable_not_partial_or_zero() {
    let good = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t5\t4\n"
    ));
    let cases: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        ("not gzip", b"plain text".to_vec(), b"plain text".to_vec()),
        ("md5 mismatch", good.clone(), b"other".to_vec()),
        (
            "bad sessions",
            gzip(&format!(
                "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\tmany\t4\n"
            )),
            vec![],
        ),
        (
            "negative devices",
            gzip(&format!(
                "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t5\t-4\n"
            )),
            vec![],
        ),
        (
            "ragged row",
            gzip(&format!("{SESSIONS_HEADER}2026-10-09\t1\tiPhone\n")),
            vec![],
        ),
        (
            "missing column",
            gzip("Date\tDevice\n2026-10-09\tiPhone\n"),
            vec![],
        ),
        (
            "bad date",
            gzip(&format!(
                "{SESSIONS_HEADER}yesterday\t1\tiPhone\tJP\t5\t4\n"
            )),
            vec![],
        ),
    ];
    for (label, gz, checksum_of) in cases {
        let m = MockServer::start_async().await;
        let checksum_of = if checksum_of.is_empty() {
            gz.clone()
        } else {
            checksum_of
        };
        one_segment_setup(&m, signed(&m, "x"), &gz, &checksum_of).await;
        mock_download(&m, "x", gz).await;
        let out = server_for(&m)
            .call("get_app_usage_report", &json!({"app_id":"1"}))
            .await
            .unwrap_or_else(|e| panic!("{label}: expected unavailable result, got error {e}"));
        assert_eq!(out["status"], "unavailable", "{label}: {out}");
        assert!(
            out.get("rows")
                .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)),
            "{label}"
        );
        assert!(!out.to_string().contains(SECRET), "{label}");
    }
    // Sanity: the same harness with a valid segment is available.
    let m = MockServer::start_async().await;
    one_segment_setup(&m, signed(&m, "x"), &good, &good).await;
    mock_download(&m, "x", good).await;
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "available", "{out}");
}

#[tokio::test]
async fn disallowed_download_urls_and_redirects_are_refused_without_leaking_the_signature() {
    let good = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t5\t4\n"
    ));
    let foreign = [
        format!("https://example.com/seg?X-Amz-Signature={SECRET}"),
        format!("http://127.0.0.1:1/seg?sig={SECRET}"),
        format!("file:///etc/passwd?sig={SECRET}"),
        format!("https://user:pw@example.com/seg?sig={SECRET}"),
    ];
    for url in foreign {
        let m = MockServer::start_async().await;
        one_segment_setup(&m, url.clone(), &good, &good).await;
        let out = server_for(&m)
            .call("get_app_usage_report", &json!({"app_id":"1"}))
            .await
            .unwrap_or_else(|e| panic!("{url}: expected unavailable result, got error {e}"));
        assert_eq!(out["status"], "unavailable", "{url}: {out}");
        let text = out.to_string();
        assert!(
            !text.contains(SECRET) && !text.contains("pw@"),
            "{url}: {text}"
        );
    }
    // A same-origin download that redirects must not be followed.
    let m = MockServer::start_async().await;
    one_segment_setup(&m, signed(&m, "hop"), &good, &good).await;
    m.mock_async(|when, then| {
        when.method(GET).path("/download/hop");
        then.status(302)
            .header("location", m.url(format!("/elsewhere?sig={SECRET}")));
    })
    .await;
    let target = m
        .mock_async(|when, then| {
            when.method(GET).path("/elsewhere");
            then.status(200).body(good.clone());
        })
        .await;
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "unavailable", "{out}");
    assert!(!out.to_string().contains(SECRET));
    target.assert_calls_async(0).await;
}

#[tokio::test]
async fn quoted_tsv_cells_with_tabs_quotes_and_newlines_are_parsed_from_a_gzip_segment() {
    let m = MockServer::start_async().await;
    let text = String::from(
        "\u{feff}Date\tApp Apple Identifier\tApp Version\tDevice\tSessions\tUnique Devices\r\n2026-10-09\t1\t\"1.0\t\"\"beta\"\"\nx\"\tiPhone\t5\t4\r\n",
    );
    let gz = gzip(&text);
    one_segment_setup(&m, signed(&m, "q"), &gz, &gz).await;
    mock_download(&m, "q", gz).await;
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(
        out["rows"][0]["dimensions"]["App Version"],
        "1.0\t\"beta\"\nx"
    );
    assert_eq!(out["sessions_total"], 5);
    assert_eq!(out["provenance"]["segment_ids"], json!(["s1"]));
    assert!(!out.to_string().contains(SECRET));
}

#[tokio::test]
async fn size_metadata_mismatch_and_foreign_app_rows_are_unavailable() {
    let good = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t5\t4\n"
    ));
    let m = MockServer::start_async().await;
    one_segment_setup(&m, signed(&m, "x"), &good, &good).await;
    let mut longer = good.clone();
    longer.push(0);
    mock_download(&m, "x", longer).await;
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "unavailable", "{out}");

    let other = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t2\tiPhone\tJP\t5\t4\n"
    ));
    let m = MockServer::start_async().await;
    one_segment_setup(&m, signed(&m, "x"), &other, &other).await;
    mock_download(&m, "x", other).await;
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "unavailable", "{out}");
    assert!(out["sessions_total"].is_null());
}

#[tokio::test]
async fn forbidden_segment_listing_is_permission_denied_and_other_api_errors_propagate() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(&m, "r1", vec![instance("i1", "DAILY", "2026-10-10")]).await;
    m.mock_async(|when, then| {
        when.method(GET)
            .path("/v1/analyticsReportInstances/i1/segments");
        then.status(403)
            .json_body(json!({"errors":[{"code":"FORBIDDEN"}]}));
    })
    .await;
    let out = server_for(&m)
        .call("get_app_usage_report", &json!({"app_id":"1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], "permission_denied");

    let m = MockServer::start_async().await;
    m.mock_async(|when, then| {
        when.method(GET).path("/v1/apps/1/analyticsReportRequests");
        then.status(404)
            .json_body(json!({"errors":[{"code":"NOT_FOUND"}]}));
    })
    .await;
    assert!(
        server_for(&m)
            .call("get_app_usage_report", &json!({"app_id":"1"}))
            .await
            .is_err()
    );
}

// ---- Latest-event search, checksum provenance and optional duration ----

/// Registers segments plus the signed download for `id`; returns the download mock.
async fn mount_instance<'a>(m: &'a MockServer, id: &str, bytes: Vec<u8>) -> httpmock::Mock<'a> {
    mock_segments(m, id, vec![segment(id, &signed(m, id), &bytes)]).await;
    mock_download(m, id, bytes).await
}
fn sessions_gz(event: &str, count: u32) -> Vec<u8> {
    gzip(&format!(
        "{SESSIONS_HEADER}{event}\t1\tiPhone\tJP\t{count}\t5\n"
    ))
}
async fn usage(m: &MockServer, args: Value) -> Value {
    server_for(m)
        .call("get_app_usage_report", &args)
        .await
        .unwrap()
}

#[tokio::test]
async fn default_latest_event_skips_old_corrupt_downloads_once_next_processing_is_older_than_latest_event()
 {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(
        &m,
        "r1",
        vec![
            instance("new", "DAILY", "2026-10-12"),
            instance("old", "DAILY", "2026-10-10"),
        ],
    )
    .await;
    mount_instance(&m, "new", sessions_gz("2026-10-11", 7)).await;
    // processingDate 10-10 < latest eventDate 10-11: can never hold a newer event date.
    let good = sessions_gz("2026-10-09", 1);
    mock_segments(&m, "old", vec![segment("old", &signed(&m, "old"), &good)]).await;
    let corrupt = mock_download(&m, "old", b"not a gzip stream".to_vec()).await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["report_date"], "2026-10-11");
    assert_eq!(out["sessions_total"], 7);
    corrupt.assert_calls_async(0).await;
}

#[tokio::test]
async fn newest_processing_may_be_a_historical_correction_so_the_search_continues() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(
        &m,
        "r1",
        vec![
            instance("fix", "DAILY", "2026-10-14"),
            instance("fresh", "DAILY", "2026-10-12"),
            instance("stale", "DAILY", "2026-10-09"),
        ],
    )
    .await;
    mount_instance(&m, "fix", sessions_gz("2026-10-05", 3)).await;
    mount_instance(&m, "fresh", sessions_gz("2026-10-11", 9)).await;
    mock_segments(
        &m,
        "stale",
        vec![segment("stale", &signed(&m, "stale"), b"x")],
    )
    .await;
    let stale = mock_download(&m, "stale", b"corrupt".to_vec()).await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["report_date"], "2026-10-11");
    assert_eq!(out["processing_date"], "2026-10-12");
    assert_eq!(out["sessions_total"], 9);
    stale.assert_calls_async(0).await;
}

#[tokio::test]
async fn more_than_two_hundred_daily_instances_resolve_latest_without_fetching_every_segment() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let instances: Vec<_> = (0..250)
        .map(|n| {
            let day = start + chrono::Duration::days(n);
            instance(&format!("i{n:03}"), "DAILY", &day.to_string())
        })
        .collect();
    let newest_day = (start + chrono::Duration::days(249)).to_string();
    // 200 + 50 over two pages, newest instance on the later page.
    let path = "/v1/analyticsReports/r1/instances";
    let next = m.url(format!("{path}?page=2"));
    let first = instances[..200].to_vec();
    let second = instances[200..].to_vec();
    let p1 = m
        .mock_async(move |when, then| {
            when.method(GET).path(path).query_param_missing("page");
            then.status(200)
                .json_body(json!({"data":first,"links":{"next":next}}));
        })
        .await;
    let p2 = m
        .mock_async(move |when, then| {
            when.method(GET).path(path).query_param("page", "2");
            then.status(200).json_body(json!({"data":second}));
        })
        .await;
    // Only the newest instance has segments; fetching any other would 404 -> unavailable.
    let dl = mount_instance(&m, "i249", sessions_gz(&newest_day, 4)).await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["report_date"], newest_day.as_str());
    assert_eq!(out["sessions_total"], 4);
    assert_eq!(out["limits"]["instances"], 2000);
    p1.assert_calls_async(1).await;
    p2.assert_calls_async(1).await;
    dl.assert_calls_async(1).await;
}

#[tokio::test]
async fn default_event_date_after_processing_date_is_unavailable() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(&m, "r1", vec![instance("i1", "DAILY", "2026-10-10")]).await;
    mount_instance(&m, "i1", sessions_gz("2026-10-11", 4)).await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "unavailable", "{out}");
    assert!(out["sessions_total"].is_null());
}

#[tokio::test]
async fn explicit_date_after_every_processing_date_is_no_data_without_segment_requests() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(&m, "r1", vec![instance("i1", "DAILY", "2026-10-10")]).await;
    let segs = m
        .mock_async(|when, then| {
            when.method(GET)
                .path("/v1/analyticsReportInstances/i1/segments");
            then.status(200).json_body(json!({"data":[]}));
        })
        .await;
    let out = usage(&m, json!({"app_id":"1","report_date":"2026-10-11"})).await;
    assert_eq!(out["status"], "no_data", "{out}");
    assert!(out["sessions_total"].is_null());
    segs.assert_calls_async(0).await;
}

#[tokio::test]
async fn obsolete_equal_processing_ambiguity_does_not_block_the_greatest_event_date() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(
        &m,
        "r1",
        vec![
            instance("a", "DAILY", "2026-10-12"),
            instance("b", "DAILY", "2026-10-12"),
            instance("old", "DAILY", "2026-10-10"),
        ],
    )
    .await;
    mount_instance(&m, "a", sessions_gz("2026-10-05", 10)).await;
    mount_instance(&m, "b", sessions_gz("2026-10-05", 20)).await;
    mount_instance(&m, "old", sessions_gz("2026-10-09", 3)).await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["report_date"], "2026-10-09");
    assert_eq!(out["sessions_total"], 3);
}

#[tokio::test]
async fn present_but_wrong_checksum_is_still_unavailable() {
    let m = MockServer::start_async().await;
    let bytes = sessions_gz("2026-10-09", 6);
    one_segment_setup(&m, signed(&m, "bad"), &bytes, b"other").await;
    mock_download(&m, "bad", bytes).await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "unavailable", "{out}");
}

#[tokio::test]
async fn default_equal_processing_duplicates_remain_unavailable() {
    for processing in ["2026-10-12", "2026-10-09"] {
        let m = MockServer::start_async().await;
        mock_request_and_reports(&m).await;
        mock_instances(
            &m,
            "r1",
            vec![
                instance("a", "DAILY", processing),
                instance("b", "DAILY", processing),
            ],
        )
        .await;
        mount_instance(&m, "a", sessions_gz("2026-10-09", 10)).await;
        mount_instance(&m, "b", sessions_gz("2026-10-09", 20)).await;
        let out = usage(&m, json!({"app_id":"1"})).await;
        assert_eq!(out["status"], "unavailable", "{out}");
        assert!(out["sessions_total"].is_null());
    }
}

#[tokio::test]
async fn missing_or_empty_checksum_is_reported_unverified_not_silently_available() {
    for checksum in [None, Some("")] {
        let m = MockServer::start_async().await;
        mock_request_and_reports(&m).await;
        mock_instances(&m, "r1", vec![instance("i1", "DAILY", "2026-10-10")]).await;
        let bytes = sessions_gz("2026-10-09", 6);
        let mut seg = segment("s1", &signed(&m, "d"), &bytes);
        match checksum {
            Some(c) => seg["attributes"]["checksum"] = json!(c),
            None => {
                seg["attributes"]
                    .as_object_mut()
                    .unwrap()
                    .remove("checksum");
            }
        }
        mock_segments(&m, "i1", vec![seg]).await;
        mock_download(&m, "d", bytes).await;
        let out = usage(&m, json!({"app_id":"1"})).await;
        assert_eq!(out["status"], "available", "{out}");
        assert_eq!(out["sessions_total"], 6);
        assert_eq!(out["provenance"]["checksum_verified"], false, "{out}");
        assert!(
            out["warnings"].as_array().unwrap().iter().any(|w| w
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("checksum")),
            "{out}"
        );
    }
    // A present, correct checksum is reported as verified.
    let m = MockServer::start_async().await;
    let bytes = sessions_gz("2026-10-09", 6);
    let url = signed(&m, "ok");
    one_segment_setup(&m, url, &bytes, &bytes).await;
    mock_download(&m, "ok", bytes).await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["provenance"]["checksum_verified"], true, "{out}");
}

#[tokio::test]
async fn blank_optional_duration_is_null_but_malformed_nonempty_duration_is_rejected() {
    let header = "Date\tApp Apple Identifier\tDevice\tTerritory\tSessions\tUnique Devices\tTotal Session Duration\n";
    for (cell, ok) in [("", true), ("abc", false), ("-1", false), ("1.5", false)] {
        let m = MockServer::start_async().await;
        mock_request_and_reports(&m).await;
        mock_instances(&m, "r1", vec![instance("i1", "DAILY", "2026-10-10")]).await;
        let bytes = gzip(&format!(
            "{header}2026-10-09\t1\tiPhone\tJP\t5\t3\t{cell}\n"
        ));
        mount_instance(&m, "i1", bytes).await;
        let out = usage(&m, json!({"app_id":"1"})).await;
        if ok {
            assert_eq!(out["status"], "available", "{out}");
            assert_eq!(out["sessions_total"], 5);
            let row = &out["rows"][0];
            assert!(
                row.get("total_session_duration").is_none_or(Value::is_null),
                "{out}"
            );
        } else {
            assert_eq!(out["status"], "unavailable", "{cell}: {out}");
        }
    }
}

#[tokio::test]
async fn default_skips_same_event_day_when_processed_earlier_than_the_winner() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(
        &m,
        "r1",
        vec![
            instance("new", "DAILY", "2026-10-12"),
            instance("old", "DAILY", "2026-10-11"),
        ],
    )
    .await;
    mount_instance(&m, "new", sessions_gz("2026-10-11", 7)).await;
    let old = m
        .mock_async(|when, then| {
            when.method(GET)
                .path("/v1/analyticsReportInstances/old/segments");
            then.status(500);
        })
        .await;
    let out = usage(&m, json!({"app_id":"1"})).await;
    assert_eq!(out["status"], "available", "{out}");
    assert_eq!(out["sessions_total"], 7);
    old.assert_calls_async(0).await;
}

#[tokio::test]
async fn explicit_date_stops_after_its_newest_correction_or_before_older_processing() {
    for (new_event, expected_status) in [("2026-10-09", "available"), ("2026-10-08", "no_data")] {
        let m = MockServer::start_async().await;
        mock_request_and_reports(&m).await;
        let old_processing = if expected_status == "available" {
            "2026-10-11"
        } else {
            "2026-10-08"
        };
        mock_instances(
            &m,
            "r1",
            vec![
                instance("new", "DAILY", "2026-10-12"),
                instance("old", "DAILY", old_processing),
            ],
        )
        .await;
        mount_instance(&m, "new", sessions_gz(new_event, 7)).await;
        let old = m
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/v1/analyticsReportInstances/old/segments");
                then.status(500);
            })
            .await;
        let out = usage(&m, json!({"app_id":"1","report_date":"2026-10-09"})).await;
        assert_eq!(out["status"], expected_status, "{out}");
        if expected_status == "available" {
            assert_eq!(out["sessions_total"], 7);
            assert_eq!(out["processing_date"], "2026-10-12");
        } else {
            assert!(out["sessions_total"].is_null());
        }
        old.assert_calls_async(0).await;
    }
}

#[tokio::test]
async fn explicit_date_rejects_invalid_other_rows_after_processing_date() {
    let m = MockServer::start_async().await;
    mock_request_and_reports(&m).await;
    mock_instances(&m, "r1", vec![instance("i1", "DAILY", "2026-10-10")]).await;
    let gz = gzip(&format!(
        "{SESSIONS_HEADER}2026-10-09\t1\tiPhone\tJP\t5\t3\n2026-10-11\t1\tiPhone\tJP\t2\t2\n"
    ));
    mount_instance(&m, "i1", gz).await;
    let out = usage(&m, json!({"app_id":"1","report_date":"2026-10-09"})).await;
    assert_eq!(out["status"], "unavailable", "{out}");
    assert!(out["sessions_total"].is_null());
}
