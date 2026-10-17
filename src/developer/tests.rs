use super::*;
use httpmock::{Method::POST, MockServer};
use serde_json::json;
use std::fs;

fn seed() -> PortalCookie {
    PortalCookie {
        name: "myacinfo".into(),
        value: "synthetic-secret".into(),
        origin_host: "idmsa.apple.com".into(),
        domain: "apple.com".into(),
        path: "/".into(),
        host_only: false,
        secure: true,
        expires: None,
    }
}

fn team(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path("/services-account/QH65B2/account/listTeams.action");
        then.header("content-type","application/json").json_body(json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ","name":"Synthetic","status":"active"}]}));
    })
}

fn portal(server: &MockServer, root: &std::path::Path) -> DeveloperPortal {
    DeveloperPortal::for_test(&server.base_url(), root, &root.join("session.cookie")).unwrap()
}

#[tokio::test]
async fn developer_empty_form_sends_user_agent_and_content_length() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.method(POST)
            .path("/services-account/QH65B2/account/listTeams.action")
            .header("content-length", "0")
            .header("user-agent", concat!("asc-mcp/", env!("CARGO_PKG_VERSION")));
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}));
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    request.assert_calls(1);
}

fn shared_server(server: &MockServer, root: &std::path::Path) -> crate::protocol::Server {
    shared_service(&server.base_url(), root)
}

#[tokio::test]
async fn developer_http_errors_report_status_without_response_secrets() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.method(POST)
            .path("/services-account/QH65B2/account/listTeams.action");
        then.status(403).body("private-response-sentinel");
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    let error = client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "developer_http_failed:403");
    request.assert_calls(1);
}

#[tokio::test]
async fn developer_decodes_gzip_and_bounds_decompressed_responses() {
    use std::io::Write;

    for size in [None, Some(MAX_RESPONSE_BYTES), Some(MAX_RESPONSE_BYTES + 1)] {
        let server = MockServer::start();
        let mut body = json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}).to_string();
        if let Some(size) = size {
            body.push_str(&" ".repeat(size - body.len()));
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(body.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        let request = server.mock(|when, then| {
            when.method(POST)
                .path("/services-account/QH65B2/account/listTeams.action");
            then.header("content-type", "application/json")
                .header("content-encoding", "gzip")
                .body(compressed.clone());
        });
        let root = tempfile::tempdir().unwrap();
        let result = portal(&server, root.path())
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await;
        if size.is_some_and(|size| size > MAX_RESPONSE_BYTES) {
            assert_eq!(
                result.unwrap_err().to_string(),
                "developer_response_too_large"
            );
        } else {
            result.unwrap();
        }
        request.assert_calls(1);
    }
}

#[tokio::test]
async fn developer_rejects_malformed_or_unsupported_response_encoding() {
    for encoding in ["gzip", "br", "gzip, gzip", ""] {
        let server = MockServer::start();
        let request = server.mock(|when, then| {
            when.method(POST)
                .path("/services-account/QH65B2/account/listTeams.action");
            then.header("content-type", "application/json")
                .header("content-encoding", encoding)
                .body("private-response-sentinel");
        });
        let root = tempfile::tempdir().unwrap();
        let error = portal(&server, root.path())
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "developer_response_encoding_invalid");
        request.assert_calls(1);
    }
}

#[test]
fn developer_response_encoding_validation_is_strict() {
    use reqwest::header::{CONTENT_ENCODING, HeaderMap, HeaderValue};

    let compressed = hex::decode("1f8b0800000000000003ab562a4a2d2ecd2971ce4f4955b232d0512a494dcc2d56b28aae06b33c5394ac941c9d9c5d5cdddc3d3cbd946a636b017e5883a832000000").unwrap();
    let decoded = br#"{"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}"#;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_ENCODING, HeaderValue::from_static(" GZip "));
    assert_eq!(
        decode_response(&headers, compressed.clone()).unwrap(),
        decoded
    );
    let truncated = compressed[..compressed.len() - 1].to_vec();
    let mut trailing = compressed.clone();
    trailing.extend_from_slice(b"private-trailing-sentinel");
    for malformed in [truncated, trailing] {
        assert_eq!(
            decode_response(&headers, malformed)
                .unwrap_err()
                .to_string(),
            "developer_response_encoding_invalid"
        );
    }
    headers.append(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    assert_eq!(
        decode_response(&headers, compressed)
            .unwrap_err()
            .to_string(),
        "developer_response_encoding_invalid"
    );
    headers.insert(CONTENT_ENCODING, HeaderValue::from_static("identity"));
    assert_eq!(
        decode_response(&headers, decoded.to_vec()).unwrap(),
        decoded
    );
    assert_eq!(
        decode_response(&headers, vec![0; MAX_RESPONSE_BYTES + 1])
            .unwrap_err()
            .to_string(),
        "developer_response_too_large"
    );
}

fn shared_service(endpoint: &str, root: &std::path::Path) -> crate::protocol::Server {
    let review_root = root.join("reviewer");
    let reviewer = crate::reviewer::Reviewer::for_test(endpoint, &review_root);
    crate::reviewer::atomic_json(
        &review_root.join("session.cookie"),
        &json!({"cookies":[
            {"name":"myacinfo","value":"shared-synthetic-sso","domain":"apple.com","path":"/"},
            {"name":"asc_only","value":"asc-private-sentinel","domain":"appstoreconnect.apple.com","path":"/"}
        ]}),
    ).unwrap();
    crate::protocol::Server {
        api: None,
        reviewer: Some(reviewer),
        developer: Some(
            DeveloperPortal::for_test(
                endpoint,
                &root.join("developer"),
                &root.join("developer/session.cookie"),
            )
            .unwrap(),
        ),
    }
}

#[tokio::test]
async fn shared_authentication_respects_present_invalid_dedicated_source() {
    for invalid in [b"invalid".as_slice(), b"{}".as_slice()] {
        let server = MockServer::start();
        let request = team(&server);
        let root = tempfile::tempdir().unwrap();
        let service = shared_server(&server, root.path());
        fs_safe::atomic_write(&root.path().join("developer/session.cookie"), invalid).unwrap();
        let status = service
            .call("get_developer_connection_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(status["status"], "authentication_required");
        request.assert_calls(0);
    }
}

#[tokio::test]
async fn shared_authentication_never_falls_back_through_dedicated_symlinks() {
    use std::os::unix::fs::symlink;
    for dangling in [false, true] {
        let server = MockServer::start();
        let request = team(&server);
        let root = tempfile::tempdir().unwrap();
        let service = shared_server(&server, root.path());
        let target = root.path().join("private-dedicated");
        if !dangling {
            fs_safe::atomic_write(&target, b"invalid").unwrap();
        }
        fs::create_dir_all(root.path().join("developer")).unwrap();
        symlink(target, root.path().join("developer/session.cookie")).unwrap();
        let status = service
            .call("get_developer_connection_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(status["status"], "authentication_required");
        request.assert_calls(0);
    }
}

#[tokio::test]
async fn shared_suppression_cannot_override_a_newer_reviewer_key() {
    let server = MockServer::start();
    let removed = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/listTeams.action")
            .header(
                "cookie",
                "dsid=old-shared-aux; myacinfo=shared-synthetic-sso",
            );
        then.header("content-type", "application/json")
            .header(
                "set-cookie",
                "dsid=deleted; Domain=apple.com; Path=/; Secure; Max-Age=0",
            )
            .json_body(json!({"resultCode":0,"teams":[]}));
    });
    let newer = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/listTeams.action")
            .header(
                "cookie",
                "dsid=newer-shared-aux; myacinfo=shared-synthetic-sso",
            );
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"teams":[]}));
    });
    let root = tempfile::tempdir().unwrap();
    let service = shared_server(&server, root.path());
    let source = root.path().join("reviewer/session.cookie");
    let mut value: Value = serde_json::from_slice(&fs::read(&source).unwrap()).unwrap();
    value["cookies"].as_array_mut().unwrap().push(json!({
        "name":"dsid","value":"old-shared-aux","domain":"apple.com","path":"/"
    }));
    crate::reviewer::atomic_json(&source, &value).unwrap();
    let status = service
        .call("get_developer_connection_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(status["status"], "connected");
    let handle = service.reviewer.as_ref().unwrap().shared_session();
    let snapshot = handle.snapshot().unwrap();
    assert!(
        snapshot
            .cookies
            .iter()
            .any(|cookie| cookie.name == "dsid" && cookie.value == "old-shared-aux")
    );
    handle
        .rotate(
            &snapshot,
            &[crate::reviewer::SharedCookie {
                name: "dsid".into(),
                value: "newer-shared-aux".into(),
                expires: None,
            }],
        )
        .unwrap();
    let status = service
        .call("get_developer_connection_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(status["status"], "connected");
    removed.assert_calls(1);
    newer.assert_calls(1);
}

#[tokio::test]
async fn shared_revocation_is_durable_when_auxiliary_rotation_races_persistence() {
    for rotate_before_response in [false, true] {
        let server = MockServer::start();
        let request = team(&server);
        let root = tempfile::tempdir().unwrap();
        let service = shared_server(&server, root.path());
        let source = root.path().join("reviewer/session.cookie");
        let mut value: Value = serde_json::from_slice(&fs::read(&source).unwrap()).unwrap();
        value["cookies"].as_array_mut().unwrap().push(
            json!({"name":"auxiliary","value":"auxiliary-before","domain":"apple.com","path":"/"}),
        );
        crate::reviewer::atomic_json(&source, &value).unwrap();
        let handle = service.reviewer.as_ref().unwrap().shared_session();
        let mut transaction = state::Transaction::load_with_shared(
            &root.path().join("developer"),
            &root.path().join("developer/session.cookie"),
            Some(handle.clone()),
        )
        .unwrap();
        let rotate = || {
            let snapshot = handle.snapshot().unwrap();
            handle
                .rotate(
                    &snapshot,
                    &[crate::reviewer::SharedCookie {
                        name: "auxiliary".into(),
                        value: "auxiliary-after".into(),
                        expires: None,
                    }],
                )
                .unwrap();
        };
        if rotate_before_response {
            rotate();
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "set-cookie",
            "myacinfo=deleted; Domain=apple.com; Path=/; Secure; Max-Age=0"
                .parse()
                .unwrap(),
        );
        let state::Authentication::Shared(shared) = &mut transaction.auth else {
            panic!("shared authentication expected");
        };
        assert!(
            shared
                .rotate(
                    &mut transaction.cookies,
                    &headers,
                    &Url::parse("https://developer.apple.com/").unwrap()
                )
                .is_err()
        );
        if !rotate_before_response {
            rotate();
        }
        assert!(transaction.persist_overlay().is_err());
        drop(transaction);
        let status = service
            .call("get_developer_connection_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(status["status"], "authentication_required");
        request.assert_calls(0);
    }
}

#[tokio::test]
async fn suppressed_unsent_cookie_rotation_stays_developer_local() {
    let server = MockServer::start();
    let remove = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/listTeams.action")
            .header(
                "cookie",
                "dsid=original-auxiliary; myacinfo=shared-synthetic-sso",
            );
        then.header("content-type", "application/json")
            .header(
                "set-cookie",
                "dsid=deleted; Domain=apple.com; Path=/; Secure; Max-Age=0",
            )
            .json_body(json!({"resultCode":0,"teams":[]}));
    });
    let replace = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/listTeams.action")
            .header("cookie", "myacinfo=shared-synthetic-sso");
        then.header("content-type", "application/json")
            .header(
                "set-cookie",
                "dsid=developer-local-auxiliary; Domain=apple.com; Path=/; Secure",
            )
            .json_body(json!({"resultCode":0,"teams":[]}));
    });
    let local = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/listTeams.action")
            .header(
                "cookie",
                "myacinfo=shared-synthetic-sso; dsid=developer-local-auxiliary",
            );
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"teams":[]}));
    });
    let rebased = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/listTeams.action")
            .header(
                "cookie",
                "dsid=reviewer-advanced-auxiliary; myacinfo=shared-synthetic-sso",
            );
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"teams":[]}));
    });
    let root = tempfile::tempdir().unwrap();
    let service = shared_server(&server, root.path());
    let source = root.path().join("reviewer/session.cookie");
    let mut value: Value = serde_json::from_slice(&fs::read(&source).unwrap()).unwrap();
    value["cookies"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"dsid","value":"original-auxiliary","domain":"apple.com","path":"/"}));
    crate::reviewer::atomic_json(&source, &value).unwrap();
    for _ in 0..3 {
        let status = service
            .call("get_developer_connection_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(status["status"], "connected");
        let snapshot = service
            .reviewer
            .as_ref()
            .unwrap()
            .shared_session()
            .snapshot()
            .unwrap();
        assert!(
            snapshot
                .cookies
                .iter()
                .any(|cookie| cookie.name == "dsid" && cookie.value == "original-auxiliary")
        );
    }
    remove.assert_calls(1);
    replace.assert_calls(1);
    local.assert_calls(1);
    let handle = service.reviewer.as_ref().unwrap().shared_session();
    let snapshot = handle.snapshot().unwrap();
    handle
        .rotate(
            &snapshot,
            &[crate::reviewer::SharedCookie {
                name: "dsid".into(),
                value: "reviewer-advanced-auxiliary".into(),
                expires: None,
            }],
        )
        .unwrap();
    let status = service
        .call("get_developer_connection_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(status["status"], "connected");
    rebased.assert_calls(1);
}

#[tokio::test]
async fn shared_authentication_requires_explicit_live_team_membership() {
    let server = MockServer::start();
    let request = team(&server);
    let listed = groups(&server);
    let root = tempfile::tempdir().unwrap();
    let service = shared_server(&server, root.path());
    assert!(
        service
            .call("list_app_groups", &json!({"team_id":"1234567890"}))
            .await
            .is_err()
    );
    listed.assert_calls(0);
    let result = service
        .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
        .await
        .unwrap();
    assert_eq!(result["team_id"], "ABCDEFGHIJ");
    assert_eq!(result["groups"][0]["identifier"], "group.example.old");
    request.assert_calls(2);
    listed.assert_calls(1);
}

#[tokio::test]
async fn shared_sso_rotation_is_reused_without_copying_authentication_source() {
    let server = MockServer::start();
    let first = server.mock(|when, then| {
        when.method(POST)
            .path("/services-account/QH65B2/account/listTeams.action")
            .header("cookie", "myacinfo=shared-synthetic-sso");
        then.header("content-type", "application/json")
            .header(
                "set-cookie",
                "myacinfo=rotated-shared-sso; Domain=apple.com; Path=/; Secure",
            )
            .json_body(json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}));
    });
    let second = server.mock(|when, then| {
        when.method(POST)
            .path("/services-account/QH65B2/account/listTeams.action")
            .header("cookie", "myacinfo=rotated-shared-sso");
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}));
    });
    let root = tempfile::tempdir().unwrap();
    let service = shared_server(&server, root.path());
    let source_path = root.path().join("reviewer/session.cookie");
    let original = fs::read(&source_path).unwrap();
    for _ in 0..2 {
        let status = service
            .call("get_developer_connection_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(status["status"], "connected");
        assert_eq!(status["auth_source"], "shared_reviewer_session");
        assert!(status.get("team_id").is_none());
    }
    first.assert_calls(1);
    second.assert_calls(1);
    assert_eq!(original, fs::read(&source_path).unwrap());
    assert!(!root.path().join("developer/session.cookie").exists());
    assert!(!root.path().join("developer/overlay.json").exists());
    let overlay = fs::read_to_string(root.path().join("developer/shared-overlay.json")).unwrap();
    assert!(!overlay.contains("shared-synthetic-sso"));
    assert!(!overlay.contains("rotated-shared-sso"));
    assert!(!overlay.contains("asc-private-sentinel"));
}

#[tokio::test]
async fn shared_revocation_survives_restart_and_unrelated_reviewer_rotation() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.method(POST)
            .path("/services-account/QH65B2/account/listTeams.action");
        then.header("content-type", "application/json")
            .header(
                "set-cookie",
                "myacinfo=deleted; Domain=apple.com; Path=/; Secure; Max-Age=0",
            )
            .json_body(json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}));
    });
    let root = tempfile::tempdir().unwrap();
    let mut service = shared_server(&server, root.path());
    let status = service
        .call("get_developer_connection_status", &json!({}))
        .await
        .unwrap();
    assert_ne!(status["status"], "connected");
    service.developer = Some(portal(&server, &root.path().join("developer")));
    let source = fs::read(root.path().join("reviewer/session.cookie")).unwrap();
    crate::reviewer::atomic_json(
        &root.path().join("reviewer/session.state.json"),
        &json!({
            "generation":state::digest(&source),
            "cookies":[
                {"name":"myacinfo","value":"shared-synthetic-sso","domain":"apple.com","path":"/"},
                {"name":"other","value":"new-unrelated-value","domain":"apple.com","path":"/"}
            ]
        }),
    )
    .unwrap();
    let status = service
        .call("get_developer_connection_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(status["status"], "authentication_required");
    request.assert_calls(1);
}

#[test]
fn developer_cookie_scope_is_not_asc_scope() {
    let url = url::Url::parse(
        "https://developer.apple.com/services-account/QH65B2/account/listTeams.action",
    )
    .unwrap();
    assert!(cookies::header(&[seed()], &url).is_ok());
    for change in ["host", "insecure", "expired", "path", "prefix"] {
        let mut item = seed();
        match change {
            "host" => {
                item.domain = "appstoreconnect.apple.com".into();
                item.origin_host = item.domain.clone();
                item.host_only = true;
            }
            "insecure" => item.secure = false,
            "expired" => item.expires = Some("2000-01-01T00:00:00Z".into()),
            "path" => item.path = "/different".into(),
            _ => item.name = "__Host-myacinfo".into(),
        }
        assert!(cookies::header(&[item], &url).is_err(), "{change}");
    }
    assert!(cookies::header(&[seed(), seed()], &url).is_err());
}

#[tokio::test]
async fn developer_team_is_explicit() {
    let server = MockServer::start();
    let mock = team(&server);
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    assert!(
        client
            .call("list_app_groups", &json!({"team_id":"1234567890"}))
            .await
            .is_err()
    );
    assert!(
        client
            .call("list_app_groups", &json!({"team_id":"abcdefghij"}))
            .await
            .is_err()
    );
    mock.assert_calls(1);
}

#[tokio::test]
async fn developer_overlay_is_generation_bound() {
    let server = MockServer::start();
    team(&server);
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let before = fs::read(root.path().join("session.cookie")).unwrap();
    let mut changed = seed();
    changed.value = "new-generation".into();
    client
        .publish_login("ABCDEFGHIJ", vec![changed])
        .await
        .unwrap();
    assert_ne!(
        before,
        fs::read(root.path().join("session.cookie")).unwrap()
    );
    fs::remove_file(root.path().join("session.cookie")).unwrap();
    assert!(
        client
            .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
            .await
            .is_err()
    );
    assert!(!root.path().join("session.cookie").exists());
}

#[tokio::test]
async fn developer_transport_never_redirects_retries_or_leaks() {
    let server = MockServer::start();
    let sink = MockServer::start();
    let redirect = server.mock(|when, then| {
        when.method(POST)
            .path("/services-account/QH65B2/account/listTeams.action");
        then.status(307).header("Location", sink.base_url());
    });
    let unexpected = sink.mock(|_, then| {
        then.status(200);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    let failure = client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap_err()
        .to_string();
    assert!(!failure.contains("synthetic-secret"));
    redirect.assert_calls(1);
    unexpected.assert_calls(0);
    assert!(!root.path().join("session.cookie").exists());
}

#[test]
#[cfg(unix)]
fn developer_private_state_is_bounded_and_symlink_safe() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("private");
    fs_safe::atomic_write(&file, b"valid").unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(fs_safe::private_read(&file, 2).is_err());
    let link = root.path().join("link");
    symlink(&file, &link).unwrap();
    assert!(fs_safe::private_read(&link, 100).is_err());
    let directory = root.path().join("directory");
    symlink(root.path(), &directory).unwrap();
    assert!(fs_safe::atomic_write(&directory.join("escape"), b"no").is_err());
}

fn groups(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path("/services-account/QH65B2/account/ios/identifiers/listApplicationGroups.action").form_urlencoded_tuple("sort", "name=asc");
        then.header("csrf", "synthetic-csrf").header("csrf_ts", "12345")
            .header("content-type","application/json").json_body(json!({"resultCode":0,"applicationGroupList":[{"identifier":"group.example.old","applicationGroup":"GROUPOLD1","name":"Old"}],"totalRecords":1}));
    })
}

fn detail(server: &MockServer, count: usize) -> httpmock::Mock<'_> {
    server.mock(move |when, then| {
        when.method(POST).path("/services-account/QH65B2/account/ios/identifiers/getAppIdDetail.action");
        then.header("content-type","application/json").json_body(json!({"resultCode":0,"appId":{"appIdId":"APPID123","identifier":"com.example.app","appIdPlatform":"ios","features":{"APG3427HIY":true},"enabledFeatures":["APG3427HIY"],"associatedApplicationGroupsCount":count,"associatedApplicationGroups":[{"identifier":"group.example.old","applicationGroup":"GROUPOLD1","name":"Old"}]}}));
    })
}

fn apps(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path("/services-account/QH65B2/account/ios/identifiers/listAppIds.action").form_urlencoded_tuple("sort", "name=asc");
        then.header("content-type","application/json").json_body(json!({"resultCode":0,"appIds":[{"appIdId":"APPID123","identifier":"com.example.app","appIdPlatform":"ios"}],"totalRecords":1}));
    })
}

#[tokio::test]
async fn developer_prepare_never_mutates() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.status(500);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let prepared = client
        .call(
            "create_app_group",
            &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"}),
        )
        .await
        .unwrap();
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["confirmation_token"].as_str().unwrap().len(), 64);
    mutation.assert_calls(0);
    let receipt = fs::read_to_string(root.path().join("receipts.json")).unwrap();
    assert!(!receipt.contains(prepared["confirmation_token"].as_str().unwrap()));
    assert!(!receipt.contains("synthetic-secret"));
    assert!(!receipt.contains("synthetic-csrf"));
}

#[tokio::test]
async fn developer_create_requires_exact_stored_plan() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.status(500);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let prepared = client
        .call(
            "create_app_group",
            &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"}),
        )
        .await
        .unwrap();
    for (name, token) in [
        ("Changed", prepared["confirmation_token"].as_str().unwrap()),
        ("New", "bad"),
    ] {
        assert!(client.call("create_app_group", &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":name,"dry_run":false,"operation_id":prepared["operation_id"],"confirmation_token":token})).await.is_err());
    }
    mutation.assert_calls(0);
}

#[tokio::test]
async fn developer_association_preserves_complete_union() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    apps(&server);
    detail(&server, 1);
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let result = client.call("associate_app_group", &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.old","bundle_id":"com.example.app"})).await.unwrap();
    assert_eq!(result["state"], "no_op");
    assert_eq!(result["group_ids"], json!(["GROUPOLD1"]));
}

#[tokio::test]
async fn developer_incomplete_association_is_not_empty() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    apps(&server);
    detail(&server, 0);
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    assert_eq!(client.call("associate_app_group", &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.old","bundle_id":"com.example.app"})).await.unwrap_err().to_string(),"developer_associations_incomplete");
}

#[tokio::test]
async fn developer_unknown_blocks_target_across_restart_and_relogin() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.status(503);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let prepared = client
        .call(
            "create_app_group",
            &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"}),
        )
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New","dry_run":false,"operation_id":prepared["operation_id"],"confirmation_token":prepared["confirmation_token"]});
    let result = client.call("create_app_group", &args).await.unwrap();
    assert_eq!(result["state"], "delivery_unknown");
    let restarted = portal(&server, root.path());
    restarted
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    assert!(
        restarted
            .call(
                "create_app_group",
                &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"Different"})
            )
            .await
            .is_err()
    );
    assert!(restarted.call("create_app_group", &args).await.is_err());
    mutation.assert_calls(1);
    let status = restarted
        .call(
            "get_app_group_write_status",
            &json!({"operation_id":prepared["operation_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "delivery_unknown");
    assert_eq!(status["observation"], "desired_state_absent");
}

#[tokio::test]
async fn developer_attempt_record_failure_sends_nothing() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.status(503);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let prepared = client
        .call(
            "create_app_group",
            &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"}),
        )
        .await
        .unwrap();
    client.fail_next_receipt_write();
    assert!(client.call("create_app_group", &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New","dry_run":false,"operation_id":prepared["operation_id"],"confirmation_token":prepared["confirmation_token"]})).await.is_err());
    mutation.assert_calls(0);
}

#[tokio::test]
async fn developer_cancellation_never_resends() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.delay(std::time::Duration::from_secs(2)).status(503);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let prepared = client
        .call(
            "create_app_group",
            &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"}),
        )
        .await
        .unwrap();
    let arguments = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New","dry_run":false,"operation_id":prepared["operation_id"],"confirmation_token":prepared["confirmation_token"]});
    let execution = client.call("create_app_group", &arguments);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), execution)
            .await
            .is_err()
    );
    assert!(
        client
            .call(
                "create_app_group",
                &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"})
            )
            .await
            .is_err()
    );
    mutation.assert_calls(1);
    assert!(
        fs::read_to_string(root.path().join("receipts.json"))
            .unwrap()
            .contains("delivery_unknown")
    );
}

struct StatefulFixture {
    endpoint: String,
    state: std::sync::Arc<std::sync::Mutex<FixtureState>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct FixtureState {
    created: bool,
    associated: bool,
    readback_rejection: bool,
    mutation_count: usize,
    captured: Vec<(String, String, bool, bool)>,
    detail_count: usize,
    old_name: Option<String>,
    fail_detail_on: Option<usize>,
    change_detail_on: Option<usize>,
    replace_on_detail: Option<(usize, std::path::PathBuf, Vec<u8>)>,
    replace_on_mutation: Option<(std::path::PathBuf, Vec<u8>)>,
    block_overlay_on_mutation: Option<std::path::PathBuf>,
    mutation_cookie: Option<String>,
}

impl Drop for StatefulFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl StatefulFixture {
    async fn start() -> Self {
        use axum::{Router, routing::post};
        let state = std::sync::Arc::new(std::sync::Mutex::new(FixtureState::default()));
        let shared = state.clone();
        let router = Router::new().fallback(post(move |request: axum::extract::Request| {
            let shared = shared.clone();
            async move {
                let path = request.uri().path().to_owned();
                let authorization = request.headers().contains_key("authorization");
                let csrf = request.headers().get("csrf").is_some_and(|value| value == "fixture-csrf") && request.headers().get("csrf_ts").is_some_and(|value| value == "123");
                let bytes = axum::body::to_bytes(request.into_body(), 65536).await.unwrap();
                let form = String::from_utf8(bytes.to_vec()).unwrap();
                let mut state = shared.lock().unwrap();
                state.captured.push((path.clone(),form,authorization,csrf));
                if path.ends_with("getAppIdDetail.action") {
                    state.detail_count += 1;
                    if state.change_detail_on == Some(state.detail_count) { state.associated = true; }
                    if let Some((count,path,bytes)) = &state.replace_on_detail
                        && state.detail_count == *count { fs_safe::atomic_write(path,bytes).unwrap(); }
                }
                let old = json!({"identifier":"group.example.old","applicationGroup":"GROUPOLD1","name":state.old_name.as_deref().unwrap_or("Old")});
                let new = json!({"identifier":"group.example.new","applicationGroup":"GROUPNEW1","name":"New"});
                let detail = |associated| json!({"appIdId":"APPID123","identifier":"com.example.app","appIdPlatform":"ios","features":{"APG3427HIY":true},"enabledFeatures":["APG3427HIY"],"associatedApplicationGroupsCount":if associated {2} else {1},"associatedApplicationGroups":if associated {vec![old.clone(),new.clone()]} else {vec![old.clone()]}});
                let body = if path.ends_with("listTeams.action") { json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}) }
                else if path.ends_with("listApplicationGroups.action") {
                    if state.created && state.readback_rejection { json!({"resultCode":35}) }
                    else { let groups = if state.created {vec![old.clone(),new.clone()]} else {vec![old.clone()]}; json!({"resultCode":0,"totalRecords":groups.len(),"applicationGroupList":groups}) }
                } else if path.ends_with("listAppIds.action") { json!({"resultCode":0,"totalRecords":1,"appIds":[{"appIdId":"APPID123","identifier":"com.example.app","appIdPlatform":"ios"}]}) }
                else if path.ends_with("getAppIdDetail.action") {
                    if state.fail_detail_on == Some(state.detail_count) { json!({"resultCode":35}) }
                    else { json!({"resultCode":0,"appId":detail(state.associated)}) }
                }
                else if path.ends_with("addApplicationGroup.action") { state.mutation_count += 1; state.created = true; json!({"resultCode":0,"applicationGroup":new}) }
                else if path.ends_with("assignApplicationGroupToAppId.action") { state.mutation_count += 1; state.associated = true; json!({"resultCode":0,"appId":detail(true)}) }
                else { json!({"resultCode":404}) };
                let token = if path.ends_with("listApplicationGroups.action") { "fixture-csrf" } else { "unrelated-token" };
                let mutation = path.ends_with("addApplicationGroup.action") || path.ends_with("assignApplicationGroupToAppId.action");
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert("csrf", token.parse().unwrap());
                headers.insert("csrf_ts", "123".parse().unwrap());
                if mutation {
                    if let Some((path, bytes)) = &state.replace_on_mutation { fs_safe::atomic_write(path, bytes).unwrap(); }
                    if let Some(path) = &state.block_overlay_on_mutation {
                        fs::remove_file(path).unwrap();
                        fs::create_dir(path).unwrap();
                    }
                    if let Some(cookie) = &state.mutation_cookie { headers.insert("set-cookie", cookie.parse().unwrap()); }
                }
                (headers, axum::Json(body))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
    fn client(&self, root: &std::path::Path) -> DeveloperPortal {
        DeveloperPortal::for_test(&self.endpoint, root, &root.join("session.cookie")).unwrap()
    }
}

fn execution(prepared: &serde_json::Value, mut args: serde_json::Value) -> serde_json::Value {
    args["dry_run"] = json!(false);
    args["operation_id"] = prepared["operation_id"].clone();
    args["confirmation_token"] = prepared["confirmation_token"].clone();
    args
}

#[tokio::test]
async fn shared_create_and_associate_preserve_confirmation_across_rotation() {
    let fixture = StatefulFixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let service = shared_service(&fixture.endpoint, root.path());
    let create = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = service.call("create_app_group", &create).await.unwrap();
    let handle = service.reviewer.as_ref().unwrap().shared_session();
    let snapshot = handle.snapshot().unwrap();
    handle
        .rotate(
            &snapshot,
            &[crate::reviewer::SharedCookie {
                name: "myacinfo".into(),
                value: "same-source-rotated-sso".into(),
                expires: None,
            }],
        )
        .unwrap();
    let result = service
        .call("create_app_group", &execution(&prepared, create))
        .await
        .unwrap();
    assert_eq!(result["state"], "verified");
    let associate = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app"});
    let prepared = service
        .call("associate_app_group", &associate)
        .await
        .unwrap();
    let result = service
        .call("associate_app_group", &execution(&prepared, associate))
        .await
        .unwrap();
    assert_eq!(result["state"], "verified");
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 2);
}

#[tokio::test]
async fn shared_confirmation_is_revoked_by_source_replacement_or_dedicated_appearance() {
    for dedicated in [false, true] {
        let fixture = StatefulFixture::start().await;
        let root = tempfile::tempdir().unwrap();
        let service = shared_service(&fixture.endpoint, root.path());
        let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
        let prepared = service.call("create_app_group", &args).await.unwrap();
        if dedicated {
            service
                .developer
                .as_ref()
                .unwrap()
                .publish_login("ABCDEFGHIJ", vec![seed()])
                .await
                .unwrap();
        } else {
            crate::reviewer::atomic_json(&root.path().join("reviewer/session.cookie"), &json!({"cookies":[
                {"name":"myacinfo","value":"replacement-account-sso","domain":"apple.com","path":"/"}
            ]})).unwrap();
        }
        assert!(
            service
                .call("create_app_group", &execution(&prepared, args))
                .await
                .is_err()
        );
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 0);
    }
}

#[tokio::test]
async fn shared_post_dispatch_cas_or_storage_failure_blocks_after_mode_switch() {
    for storage_failure in [false, true] {
        let fixture = StatefulFixture::start().await;
        let root = tempfile::tempdir().unwrap();
        let service = shared_service(&fixture.endpoint, root.path());
        let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
        let prepared = service.call("create_app_group", &args).await.unwrap();
        {
            let mut state = fixture.state.lock().unwrap();
            if storage_failure {
                state.block_overlay_on_mutation =
                    Some(root.path().join("developer/shared-overlay.json"));
            } else {
                let source = fs::read(root.path().join("reviewer/session.cookie")).unwrap();
                state.replace_on_mutation = Some((root.path().join("reviewer/session.state.json"), serde_json::to_vec(&json!({
                    "generation":state::digest(&source),
                    "cookies":[{"name":"myacinfo","value":"concurrent-newer-sso","domain":"apple.com","path":"/"}]
                })).unwrap()));
                state.mutation_cookie =
                    Some("myacinfo=stale-response-sso; Domain=apple.com; Path=/; Secure".into());
            }
        }
        let result = service
            .call("create_app_group", &execution(&prepared, args.clone()))
            .await
            .unwrap();
        assert_eq!(result["state"], "delivery_unknown");
        if !storage_failure {
            let snapshot = service
                .reviewer
                .as_ref()
                .unwrap()
                .shared_session()
                .snapshot()
                .unwrap();
            assert!(
                snapshot
                    .cookies
                    .iter()
                    .any(|cookie| cookie.name == "myacinfo"
                        && cookie.value == "concurrent-newer-sso")
            );
        }
        service
            .developer
            .as_ref()
            .unwrap()
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        assert_eq!(
            service
                .call("create_app_group", &args)
                .await
                .unwrap_err()
                .to_string(),
            "developer_target_delivery_unknown"
        );
        let status = service
            .call(
                "get_app_group_write_status",
                &json!({"operation_id":prepared["operation_id"]}),
            )
            .await
            .unwrap();
        assert_eq!(status["state"], "delivery_unknown");
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
    }
}

#[tokio::test]
async fn dedicated_unknown_receipts_remain_blocking_in_shared_mode() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.status(503);
    });
    let root = tempfile::tempdir().unwrap();
    let service = shared_server(&server, root.path());
    let client = service.developer.as_ref().unwrap();
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = service.call("create_app_group", &args).await.unwrap();
    let result = service
        .call("create_app_group", &execution(&prepared, args.clone()))
        .await
        .unwrap();
    assert_eq!(result["state"], "delivery_unknown");
    fs::remove_file(root.path().join("developer/session.cookie")).unwrap();
    assert_eq!(
        service
            .call("create_app_group", &args)
            .await
            .unwrap_err()
            .to_string(),
        "developer_target_delivery_unknown"
    );
    mutation.assert_calls(1);
}

#[tokio::test]
async fn shared_fallback_requires_configured_reviewer_and_rejects_secret_echo() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/listTeams.action");
        then.header("content-type", "application/json").json_body(
            json!({"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ","name":"asc-private-sentinel"}]}),
        );
    });
    let root = tempfile::tempdir().unwrap();
    let mut service = shared_server(&server, root.path());
    let reviewer = service.reviewer.take();
    let status = service
        .call("get_developer_connection_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(status["status"], "authentication_required");
    request.assert_calls(0);
    service.reviewer = reviewer;
    let status = service
        .call("get_developer_connection_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(status["status"], "access_unverified");
    assert!(!status.to_string().contains("asc-private-sentinel"));
    request.assert_calls(1);
}

#[tokio::test]
async fn developer_successful_create_is_verified_once_and_never_replayed() {
    let fixture = StatefulFixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = client.call("create_app_group", &args).await.unwrap();
    let execution = execution(&prepared, args);
    let result = client.call("create_app_group", &execution).await.unwrap();
    assert_eq!(result["state"], "verified");
    assert!(client.call("create_app_group", &execution).await.is_err());
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.mutation_count, 1);
    assert!(
        state
            .captured
            .iter()
            .all(|(_, _, authorization, _)| !authorization)
    );
    assert!(
        state
            .captured
            .iter()
            .filter(|(path, _, _, _)| path.ends_with("addApplicationGroup.action"))
            .all(|(_, _, _, csrf)| *csrf)
    );
}

#[tokio::test]
async fn developer_association_posts_complete_repeated_union() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app"});
    let prepared = client.call("associate_app_group", &args).await.unwrap();
    assert_eq!(prepared["existing_group_ids"], json!(["GROUPOLD1"]));
    assert_eq!(
        prepared["proposed_group_ids"],
        json!(["GROUPNEW1", "GROUPOLD1"])
    );
    assert_eq!(
        client
            .call("associate_app_group", &execution(&prepared, args))
            .await
            .unwrap()["state"],
        "verified"
    );
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.mutation_count, 1);
    let sent = &state
        .captured
        .iter()
        .find(|(path, _, _, _)| path.ends_with("assignApplicationGroupToAppId.action"))
        .unwrap()
        .1;
    let values: Vec<_> = url::form_urlencoded::parse(sent.as_bytes())
        .filter(|(key, _)| key == "applicationGroups[]")
        .map(|(_, value)| value.into_owned())
        .collect();
    assert_eq!(values, vec!["GROUPNEW1", "GROUPOLD1"]);
    assert!(
        state
            .captured
            .iter()
            .filter(|(path, _, _, _)| path.ends_with("assignApplicationGroupToAppId.action"))
            .all(|(_, _, _, csrf)| *csrf)
    );
}

#[tokio::test]
async fn developer_readback_rejection_is_unknown_not_rejected() {
    let fixture = StatefulFixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = client.call("create_app_group", &args).await.unwrap();
    fixture.state.lock().unwrap().readback_rejection = true;
    let result = client
        .call("create_app_group", &execution(&prepared, args.clone()))
        .await
        .unwrap();
    assert_eq!(result["state"], "delivery_unknown");
    assert!(client.call("create_app_group", &args).await.is_err());
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

#[tokio::test]
async fn developer_strict_schemas_and_validation_precede_session_read() {
    let server = MockServer::start();
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    let tools = client.tools();
    assert_eq!(tools.len(), 6);
    for tool in &tools {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        if matches!(
            tool["name"].as_str(),
            Some("create_app_group" | "associate_app_group")
        ) {
            assert_eq!(
                tool["inputSchema"]["properties"]["dry_run"]["default"],
                true
            );
            assert_eq!(tool["annotations"]["readOnlyHint"], false);
            assert_eq!(tool["annotations"]["idempotentHint"], false);
        }
    }
    for args in [
        json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New","unexpected":true}),
        json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New","dry_run":"false"}),
        json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New","operation_id":null}),
        json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.*","name":"New"}),
    ] {
        let error = client
            .call("create_app_group", &args)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("session_required"));
    }
}

#[tokio::test]
async fn developer_response_requires_json_media_and_exact_platform() {
    let server = MockServer::start();
    let mut malformed = server.mock(|when, then| {
        when.method(POST);
        then.header("content-type", "text/html")
            .body(r#"{"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}"#);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    assert!(
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .is_err()
    );
    malformed.delete();
    team(&server);
    groups(&server);
    server.mock(|when,then| { when.path("/services-account/QH65B2/account/ios/identifiers/listAppIds.action"); then.header("content-type","application/json").json_body(json!({"resultCode":0,"totalRecords":1,"appIds":[{"appIdId":"APPID123","identifier":"com.example.app","appIdPlatform":"osx"}]})); });
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    assert!(
        client
            .call(
                "get_bundle_id_app_groups",
                &json!({"team_id":"ABCDEFGHIJ","bundle_id":"com.example.app"})
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn developer_source_change_during_final_baseline_prevents_dispatch() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app"});
    let prepared = client.call("associate_app_group", &args).await.unwrap();
    let mut source: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("session.cookie")).unwrap()).unwrap();
    source["login_id"] = json!(uuid::Uuid::new_v4().to_string());
    fixture.state.lock().unwrap().replace_on_detail = Some((
        3,
        root.path().join("session.cookie"),
        serde_json::to_vec(&source).unwrap(),
    ));
    let error = client
        .call("associate_app_group", &execution(&prepared, args.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "developer_session_changed");
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 0);
    assert_eq!(
        client.call("associate_app_group", &args).await.unwrap()["state"],
        "prepared"
    );
}

#[tokio::test]
async fn developer_complete_pages_duplicates_and_totals_are_verified() {
    for (records, total) in [
        (json!([]), 1),
        (
            json!([{"identifier":"group.example.old","applicationGroup":"GROUPOLD1","name":"Old"},{"identifier":"group.example.other","applicationGroup":"GROUPOLD1","name":"Other"}]),
            2,
        ),
    ] {
        let server = MockServer::start();
        team(&server);
        server.mock(|when, then| {
            when.path(
                "/services-account/QH65B2/account/ios/identifiers/listApplicationGroups.action",
            );
            then.header("content-type", "application/json").json_body(
                json!({"resultCode":0,"totalRecords":total,"applicationGroupList":records}),
            );
        });
        let root = tempfile::tempdir().unwrap();
        let client = portal(&server, root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        assert!(
            client
                .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
                .await
                .is_err()
        );
    }
    let server = MockServer::start();
    team(&server);
    let first: Vec<_> = (0..100).map(|index| json!({"identifier":format!("group.example.g{index}"),"applicationGroup":format!("GROUP{index}"),"name":format!("Group{index}")})).collect();
    server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/listApplicationGroups.action")
            .body_includes("pageNumber=1");
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"totalRecords":101,"applicationGroupList":first}));
    });
    server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/listApplicationGroups.action")
            .body_includes("pageNumber=2");
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"totalRecords":100,"applicationGroupList":[]}));
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    assert_eq!(
        client
            .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
            .await
            .unwrap_err()
            .to_string(),
        "developer_page_changed"
    );
}

#[tokio::test]
async fn developer_confirmations_expire_and_change_with_authentication() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = client.call("create_app_group", &args).await.unwrap();
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    assert_eq!(
        client
            .call("create_app_group", &execution(&prepared, args.clone()))
            .await
            .unwrap_err()
            .to_string(),
        "developer_confirmation_invalid"
    );
    let prepared = client.call("create_app_group", &args).await.unwrap();
    let mut receipts: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("receipts.json")).unwrap()).unwrap();
    for record in receipts["records"].as_array_mut().unwrap() {
        record["created_at"] = json!(chrono::Utc::now().timestamp() - 1000);
        record["expires_at"] = json!(chrono::Utc::now().timestamp() - 100);
    }
    fs_safe::atomic_write(
        &root.path().join("receipts.json"),
        &serde_json::to_vec(&receipts).unwrap(),
    )
    .unwrap();
    assert!(
        client
            .call("create_app_group", &execution(&prepared, args))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn developer_corrupt_receipt_store_denies_writes_without_mutation() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.status(500);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    fs_safe::atomic_write(
        &root.path().join("receipts.json"),
        br#"{"schema":1,"records":[{"state":"delivery_unknown"}]}"#,
    )
    .unwrap();
    assert_eq!(
        client
            .call(
                "create_app_group",
                &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"})
            )
            .await
            .unwrap_err()
            .to_string(),
        "developer_receipt_invalid"
    );
    mutation.assert_calls(0);
}

#[tokio::test]
async fn developer_parallel_operations_report_busy_without_hidden_wait() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let transaction =
        state::Transaction::load(root.path(), &root.path().join("session.cookie")).unwrap();
    assert_eq!(
        client
            .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
            .await
            .unwrap_err()
            .to_string(),
        "developer_busy"
    );
    assert_eq!(
        client
            .call("get_developer_connection_status", &json!({}))
            .await
            .unwrap_err()
            .to_string(),
        "developer_busy"
    );
    drop(transaction);
}

#[test]
fn developer_response_rotation_rejects_scope_confusion_and_keeps_private_overlay() {
    let target = url::Url::parse(
        "https://developer.apple.com/services-account/QH65B2/account/listTeams.action",
    )
    .unwrap();
    let mut cookies = vec![seed()];
    let original = cookies.clone();
    for raw in [
        "myacinfo=x; Domain=appstoreconnect.apple.com; Path=/; Secure",
        "myacinfo=host; Path=/; Secure",
        "__Host-invalid=x; Domain=apple.com; Path=/; Secure",
    ] {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("set-cookie", raw.parse().unwrap());
        assert!(cookies::rotate(&mut cookies, &headers, &target).is_err());
        assert_eq!(cookies, original);
    }
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "set-cookie",
        "myacinfo=rotated; Domain=apple.com; Path=/; Secure"
            .parse()
            .unwrap(),
    );
    cookies::rotate(&mut cookies, &headers, &target).unwrap();
    assert_eq!(cookies[0].value, "rotated");
    assert_eq!(cookies[0].origin_host, "developer.apple.com");
}

#[tokio::test]
async fn developer_final_receipt_failure_blocks_after_restart() {
    let fixture = StatefulFixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = client.call("create_app_group", &args).await.unwrap();
    client
        .fail_receipt
        .store(2, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        client
            .call("create_app_group", &execution(&prepared, args.clone()))
            .await
            .unwrap_err()
            .to_string(),
        "developer_delivery_unknown_receipt_unverified"
    );
    let restarted = fixture.client(root.path());
    assert_eq!(
        restarted
            .call("create_app_group", &args)
            .await
            .unwrap_err()
            .to_string(),
        "developer_target_delivery_unknown"
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

#[tokio::test]
async fn developer_typed_mutation_rejection_and_missing_csrf_are_distinct() {
    let server = MockServer::start();
    team(&server);
    let mut read = groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":35}));
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = client.call("create_app_group", &args).await.unwrap();
    let result = client
        .call("create_app_group", &execution(&prepared, args.clone()))
        .await
        .unwrap();
    assert_eq!(result["state"], "rejected");
    let prepared = client.call("create_app_group", &args).await.unwrap();
    read.delete();
    server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/listApplicationGroups.action");
        then.header("content-type", "application/json")
            .json_body(json!({"resultCode":0,"totalRecords":0,"applicationGroupList":[]}));
    });
    assert_eq!(
        client
            .call("create_app_group", &execution(&prepared, args))
            .await
            .unwrap_err()
            .to_string(),
        "developer_csrf_required"
    );
    mutation.assert_calls(1);
}

#[tokio::test]
async fn developer_response_cap_and_malformed_mutation_never_retry() {
    let server = MockServer::start();
    team(&server);
    let mut read = groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.header("content-type", "application/json")
            .body("not-json");
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"});
    let prepared = client.call("create_app_group", &args).await.unwrap();
    assert_eq!(
        client
            .call("create_app_group", &execution(&prepared, args.clone()))
            .await
            .unwrap()["state"],
        "delivery_unknown"
    );
    assert!(client.call("create_app_group", &args).await.is_err());
    mutation.assert_calls(1);
    read.delete();
    server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/listApplicationGroups.action");
        then.header("content-type", "application/json")
            .body(" ".repeat((8 << 20) + 1));
    });
    assert_eq!(
        client
            .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
            .await
            .unwrap_err()
            .to_string(),
        "developer_response_too_large"
    );
}

#[tokio::test]
async fn developer_auth_revocation_survives_errors_restart_and_new_login() {
    for (status, body, invalid_cookie) in [
        (
            200,
            r#"{"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}"#,
            false,
        ),
        (401, "not-json", false),
        (200, r#"{"resultCode":35}"#, false),
        (200, "not-json", false),
        (
            200,
            r#"{"resultCode":0,"teams":[{"teamId":"ABCDEFGHIJ"}]}"#,
            true,
        ),
    ] {
        let server = MockServer::start();
        let mut initial = team(&server);
        let root = tempfile::tempdir().unwrap();
        let client = portal(&server, root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let source = fs::read(root.path().join("session.cookie")).unwrap();
        initial.delete();
        let mut revoked = server.mock(|when, then| {
            when.path("/services-account/QH65B2/account/listTeams.action");
            let then = then
                .status(status)
                .header("content-type", "application/json")
                .header(
                    "set-cookie",
                    "myacinfo=; Domain=apple.com; Path=/; Secure; Max-Age=0",
                );
            let then = if invalid_cookie {
                then.header(
                    "set-cookie",
                    "foreign=bad; Domain=unrelated.example; Path=/; Secure",
                )
            } else {
                then
            };
            then.body(body);
        });
        assert!(
            client
                .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
                .await
                .is_err()
        );
        let restarted = portal(&server, root.path());
        let status = restarted
            .call("get_developer_connection_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(status["status"], "authentication_required");
        assert!(
            restarted
                .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
                .await
                .is_err()
        );
        revoked.assert_calls(1);
        assert_eq!(
            source,
            fs::read(root.path().join("session.cookie")).unwrap()
        );
        let overlay: serde_json::Value =
            serde_json::from_slice(&fs::read(root.path().join("overlay.json")).unwrap()).unwrap();
        assert_eq!(overlay["cookies"], json!([]));
        revoked.delete();
        let live = team(&server);
        restarted
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        assert_eq!(
            restarted
                .call("get_developer_connection_status", &json!({}))
                .await
                .unwrap()["status"],
            "connected"
        );
        live.assert_calls(2);
    }
}

#[tokio::test]
async fn developer_response_secret_echo_never_enters_output_or_receipts() {
    for scenario in ["name", "resource", "rotated", "csrf", "old_after_rotation"] {
        let server = MockServer::start();
        team(&server);
        let reflected = match scenario {
            "rotated" => "PORTALSECRETNEW",
            "csrf" => "PORTALCSRFSECRET",
            "name" => "synthetic-secret",
            _ => "PORTALSECRETOLD",
        };
        let group = json!({"identifier":"group.example.target","applicationGroup":if matches!(scenario,"resource"|"rotated") { reflected } else { "GROUPTARGET" },"name":if matches!(scenario,"name"|"csrf"|"old_after_rotation") { format!("Prefix {reflected}") } else { "Target".to_owned() }});
        server.mock(|when, then| {
            when.path(
                "/services-account/QH65B2/account/ios/identifiers/listApplicationGroups.action",
            );
            let then = then
                .header("content-type", "application/json")
                .header("csrf", "PORTALCSRFSECRET")
                .header("csrf_ts", "12345");
            let then = if matches!(scenario, "rotated" | "old_after_rotation") {
                then.header(
                    "set-cookie",
                    "myacinfo=PORTALSECRETNEW; Domain=apple.com; Path=/; Secure",
                )
            } else {
                then
            };
            then.json_body(json!({"resultCode":0,"totalRecords":1,"applicationGroupList":[group]}));
        });
        apps(&server);
        detail(&server, 1);
        let root = tempfile::tempdir().unwrap();
        let client = portal(&server, root.path());
        let mut cookie = seed();
        if scenario != "name" {
            cookie.value = "PORTALSECRETOLD".into();
        }
        client
            .publish_login("ABCDEFGHIJ", vec![cookie])
            .await
            .unwrap();
        let args = if scenario == "name" {
            json!({"team_id":"ABCDEFGHIJ"})
        } else {
            json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.target","bundle_id":"com.example.app"})
        };
        let name = if scenario == "name" {
            "list_app_groups"
        } else {
            "associate_app_group"
        };
        let error = client.call(name, &args).await.unwrap_err().to_string();
        assert_eq!(error, "developer_response_contains_secret");
        assert!(!error.contains(reflected));
        assert!(!root.path().join("receipts.json").exists());
    }
}

#[tokio::test]
async fn developer_existing_response_names_are_opaque_not_new_name_inputs() {
    for name in ["Bob's Group", "Café グループ"] {
        let fixture = StatefulFixture::start().await;
        {
            let mut state = fixture.state.lock().unwrap();
            state.created = true;
            state.old_name = Some(name.into());
        }
        let root = tempfile::tempdir().unwrap();
        let client = fixture.client(root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let listing = client
            .call("list_app_groups", &json!({"team_id":"ABCDEFGHIJ"}))
            .await
            .unwrap();
        assert_eq!(listing["groups"][0]["name"], name);
        let create =
            json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.third","name":"Third"});
        assert_eq!(
            client.call("create_app_group", &create).await.unwrap()["state"],
            "prepared"
        );
        let associate = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app"});
        let prepared = client
            .call("associate_app_group", &associate)
            .await
            .unwrap();
        assert_eq!(prepared["state"], "prepared");
        assert_eq!(
            client
                .call("associate_app_group", &execution(&prepared, associate))
                .await
                .unwrap()["state"],
            "verified"
        );
        for invalid in [name, "Bad\nName"] {
            let args =
                json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.fourth","name":invalid});
            assert_eq!(
                client
                    .call("create_app_group", &args)
                    .await
                    .unwrap_err()
                    .to_string(),
                "developer_name_invalid"
            );
        }
    }
}

#[tokio::test]
async fn developer_final_baseline_failure_is_not_an_uncertain_write() {
    for change in [false, true] {
        let fixture = StatefulFixture::start().await;
        fixture.state.lock().unwrap().created = true;
        let root = tempfile::tempdir().unwrap();
        let client = fixture.client(root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app"});
        let prepared = client.call("associate_app_group", &args).await.unwrap();
        {
            let mut state = fixture.state.lock().unwrap();
            if change {
                state.change_detail_on = Some(3);
            } else {
                state.fail_detail_on = Some(3);
            }
        }
        assert!(
            client
                .call("associate_app_group", &execution(&prepared, args.clone()))
                .await
                .is_err()
        );
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 0);
        assert!(
            client
                .call("associate_app_group", &execution(&prepared, args.clone()))
                .await
                .is_err()
        );
        let status = client
            .call(
                "get_app_group_write_status",
                &json!({"operation_id":prepared["operation_id"]}),
            )
            .await
            .unwrap();
        assert_eq!(status["state"], "rejected");
        assert_eq!(status["observation"], "not_dispatched");
        let fresh = client.call("associate_app_group", &args).await.unwrap();
        assert!(fresh["state"] == "prepared" || fresh["state"] == "no_op");
        assert!(
            !fs::read_to_string(root.path().join("receipts.json"))
                .unwrap()
                .contains("delivery_unknown")
        );
    }
}

#[tokio::test]
async fn developer_not_dispatched_receipt_persistence_failure_stays_blocked() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app"});
    let prepared = client.call("associate_app_group", &args).await.unwrap();
    fixture.state.lock().unwrap().fail_detail_on = Some(3);
    client.fail_next_receipt_write();
    assert_eq!(
        client
            .call("associate_app_group", &execution(&prepared, args.clone()))
            .await
            .unwrap_err()
            .to_string(),
        "developer_delivery_unknown_receipt_unverified"
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 0);
    let restarted = fixture.client(root.path());
    assert_eq!(
        restarted
            .call("associate_app_group", &args)
            .await
            .unwrap_err()
            .to_string(),
        "developer_target_delivery_unknown"
    );
    let stored = fs::read_to_string(root.path().join("receipts.json")).unwrap();
    assert!(stored.contains(prepared["operation_id"].as_str().unwrap()));
    assert!(stored.contains("delivery_unknown"));
}

#[tokio::test]
async fn developer_actual_dispatch_gate_returns_typed_not_dispatched() {
    let server = MockServer::start();
    team(&server);
    groups(&server);
    let mutation = server.mock(|when, then| {
        when.path("/services-account/QH65B2/account/ios/identifiers/addApplicationGroup.action");
        then.status(500);
    });
    let root = tempfile::tempdir().unwrap();
    let client = portal(&server, root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let mut transaction =
        state::Transaction::load(root.path(), &root.path().join("session.cookie")).unwrap();
    let request = groups::write_args(
        &json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","name":"New"}),
        groups::Kind::Create,
    )
    .unwrap();
    let (plan, _) = client
        .resolve_plan(&mut transaction, &request.plan)
        .await
        .unwrap();
    for expected in ["developer_preflight_deadline", "developer_session_changed"] {
        let started = if expected == "developer_preflight_deadline" {
            std::time::Instant::now() - std::time::Duration::from_secs(91)
        } else {
            let mut source: serde_json::Value =
                serde_json::from_slice(&fs::read(root.path().join("session.cookie")).unwrap())
                    .unwrap();
            source["login_id"] = json!(uuid::Uuid::new_v4().to_string());
            fs_safe::atomic_write(
                &root.path().join("session.cookie"),
                &serde_json::to_vec(&source).unwrap(),
            )
            .unwrap();
            std::time::Instant::now()
        };
        match client.mutate(&mut transaction, &plan, started).await {
            groups::MutationOutcome::NotDispatched(error) => {
                assert_eq!(error.to_string(), expected)
            }
            _ => panic!("pre-send validation must return the typed not-dispatched outcome"),
        }
    }
    mutation.assert_calls(0);
}
