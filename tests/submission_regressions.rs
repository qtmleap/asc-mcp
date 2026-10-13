//! Independent regressions for the missing tools and the actual lost-error bug.
use asc_mcp::{
    api_tools::{call, prepare_request},
    catalog,
    client::ApiClient,
    protocol::{Server, Session},
};
use httpmock::{
    Method::{GET, PATCH, POST},
    MockServer,
};
use serde_json::json;

#[test]
fn content_rights_declaration_compiles_only_official_values() {
    let request = prepare_request(
        "set_app_content_rights",
        &json!({"app_id":"app1", "content_rights_declaration":"DOES_NOT_USE_THIRD_PARTY_CONTENT"}),
    )
    .unwrap();
    assert_eq!(request.method, "PATCH");
    assert_eq!(request.path, "/v1/apps/app1");
    assert_eq!(
        request.body.unwrap(),
        json!({"data":{"type":"apps","id":"app1","attributes":{"contentRightsDeclaration":"DOES_NOT_USE_THIRD_PARTY_CONTENT"}}})
    );
    for value in ["", "UNKNOWN", "USES_THIRD_PARTY_CONTENT "] {
        assert!(
            prepare_request(
                "set_app_content_rights",
                &json!({"app_id":"app1", "content_rights_declaration":value})
            )
            .is_err()
        );
    }
    assert!(
        prepare_request(
            "set_app_content_rights",
            &json!({"app_id":" ", "content_rights_declaration":"USES_THIRD_PARTY_CONTENT"})
        )
        .is_err()
    );
}

#[test]
fn submission_tools_are_advertised_and_compile_official_requests() {
    for name in [
        "get_app_store_version_build",
        "set_app_store_version_build",
        "create_screenshot_set",
    ] {
        assert!(catalog::find(name).is_some(), "missing tool {name}");
    }
    let read = prepare_request(
        "get_app_store_version_build",
        &json!({"version_id":"version1"}),
    )
    .unwrap();
    assert_eq!(
        (read.method.as_str(), read.path.as_str()),
        ("GET", "/v1/appStoreVersions/version1/build")
    );
    let set = prepare_request(
        "set_app_store_version_build",
        &json!({"version_id":"version1", "build_id":"build1"}),
    )
    .unwrap();
    assert_eq!(
        (set.method.as_str(), set.path.as_str()),
        ("PATCH", "/v1/appStoreVersions/version1/relationships/build")
    );
    assert_eq!(
        set.body.unwrap(),
        json!({"data":{"type":"builds","id":"build1"}})
    );
    let set = prepare_request(
        "create_screenshot_set",
        &json!({"localization_id":"locale1", "screenshot_display_type":"APP_IPAD_PRO_3GEN_129"}),
    )
    .unwrap();
    assert_eq!(set.path, "/v1/appScreenshotSets");
    assert_eq!(
        set.body.unwrap()["data"]["relationships"]["appStoreVersionLocalization"]["data"],
        json!({"type":"appStoreVersionLocalizations","id":"locale1"})
    );
}

#[tokio::test]
async fn selection_and_screenshot_tools_work_over_http() {
    let host = MockServer::start_async().await;
    let get = host
        .mock_async(|when, then| {
            when.method(GET).path("/v1/appStoreVersions/v1/build");
            then.status(200).json_body(json!({"data":null}));
        })
        .await;
    let set = host
        .mock_async(|when, then| {
            when.method(PATCH)
                .path("/v1/appStoreVersions/v1/relationships/build")
                .json_body(json!({"data":{"type":"builds","id":"b1"}}));
            then.status(204);
        })
        .await;
    let create = host.mock_async(|when, then| {
        when.method(POST).path("/v1/appScreenshotSets").json_body(json!({"data":{"type":"appScreenshotSets","attributes":{"screenshotDisplayType":"APP_IPAD_PRO_3GEN_129"},"relationships":{"appStoreVersionLocalization":{"data":{"type":"appStoreVersionLocalizations","id":"l1"}}}}}));
        then.status(201).json_body(json!({"data":{"type":"appScreenshotSets","id":"set1"}}));
    }).await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    assert_eq!(
        call(
            &client,
            "get_app_store_version_build",
            &json!({"version_id":"v1"})
        )
        .await
        .unwrap()["data"],
        serde_json::Value::Null
    );
    assert_eq!(
        call(
            &client,
            "set_app_store_version_build",
            &json!({"version_id":"v1","build_id":"b1"})
        )
        .await
        .unwrap()["status"],
        "success"
    );
    assert_eq!(
        call(
            &client,
            "create_screenshot_set",
            &json!({"localization_id":"l1","screenshot_display_type":"APP_IPAD_PRO_3GEN_129"})
        )
        .await
        .unwrap()["data"]["id"],
        "set1"
    );
    get.assert_calls_async(1).await;
    set.assert_calls_async(1).await;
    create.assert_calls_async(1).await;
}

#[test]
fn new_tool_arguments_reject_missing_blank_invalid_types_and_paths() {
    for args in [
        json!({}),
        json!({"version_id":" "}),
        json!({"version_id":5}),
        json!({"version_id":"../v1"}),
    ] {
        assert!(prepare_request("get_app_store_version_build", &args).is_err());
    }
    for args in [
        json!({"version_id":"v1"}),
        json!({"version_id":"v1","build_id":null}),
        json!({"version_id":"v1","build_id":" "}),
    ] {
        assert!(prepare_request("set_app_store_version_build", &args).is_err());
    }
    assert!(
        prepare_request(
            "create_screenshot_set",
            &json!({"localization_id":"l1","screenshot_display_type":"UNKNOWN"})
        )
        .is_err()
    );
    let encoded =
        prepare_request("get_app_store_version_build", &json!({"version_id":"v?1"})).unwrap();
    assert_eq!(encoded.path, "/v1/appStoreVersions/v%3F1/build");
    let spec: serde_json::Value =
        serde_json::from_str(include_str!("../doc/references/apple-asc/openapi.oas.json")).unwrap();
    assert_eq!(
        catalog::find("create_screenshot_set").unwrap()["inputSchema"]["properties"]["screenshot_display_type"]
            ["enum"],
        spec["components"]["schemas"]["ScreenshotDisplayType"]["enum"]
    );
}

#[tokio::test]
async fn every_submission_stage_returns_safe_recovery_state_without_write_retries() {
    for failed in 0..3 {
        let host = MockServer::start_async().await;
        let error =
            json!({"errors":[{"code":"STATE_ERROR","detail":"Required information missing"}]});
        let create = host
            .mock_async(|when, then| {
                when.method(POST).path("/v1/reviewSubmissions");
                if failed == 0 {
                    then.status(409)
                        .header("Content-Type", "application/json")
                        .json_body(error.clone());
                } else {
                    then.status(201).json_body(json!({"data":{"id":"draft1"}}));
                }
            })
            .await;
        let attach = host
            .mock_async(|when, then| {
                when.method(POST).path("/v1/reviewSubmissionItems");
                if failed == 1 {
                    then.status(409)
                        .header("Content-Type", "application/json")
                        .json_body(error.clone());
                } else {
                    then.status(201).json_body(json!({"data":{"id":"item1"}}));
                }
            })
            .await;
        let submit = host
            .mock_async(|when, then| {
                when.method(PATCH).path("/v1/reviewSubmissions/draft1");
                then.status(503)
                    .header("Content-Type", "application/json")
                    .json_body(error);
            })
            .await;
        let server = Server {
            api: Some(ApiClient::with_endpoint(&host.base_url()).unwrap()),
            reviewer: None,
        };
        let reply = server.dispatch(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"submit_app_for_review","arguments":{"app_id":"app1","version_id":"v1"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}),&Session::default()).await.unwrap();
        let result = &reply["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["resultType"], "complete");
        let state = &result["structuredContent"];
        assert_eq!(
            state["failed_step"],
            [
                "create_review_submission",
                "attach_version",
                "submit_review_submission"
            ][failed]
        );
        assert_eq!(state["completed_steps"].as_array().unwrap().len(), failed);
        assert_eq!(state["cause"]["errors"][0]["code"], "STATE_ERROR");
        assert_eq!(state["outcome_unknown"], failed == 2);
        assert_eq!(
            state["submission_id"],
            if failed == 0 {
                serde_json::Value::Null
            } else {
                json!("draft1")
            }
        );
        create.assert_calls_async(1).await;
        attach.assert_calls_async(usize::from(failed > 0)).await;
        submit.assert_calls_async(usize::from(failed > 1)).await;
    }
}

#[tokio::test]
async fn invalid_created_submission_id_is_not_echoed_or_used_in_next_request() {
    let host = MockServer::start_async().await;
    host.mock_async(|when, then| {
        when.method(POST).path("/v1/reviewSubmissions");
        then.status(201)
            .json_body(json!({"data":{"id":"\nBearer unsafe-secret"}}));
    })
    .await;
    let attach = host
        .mock_async(|when, then| {
            when.method(POST).path("/v1/reviewSubmissionItems");
            then.status(201);
        })
        .await;
    let e = call(
        &ApiClient::with_endpoint(&host.base_url()).unwrap(),
        "submit_app_for_review",
        &json!({"app_id":"app1","version_id":"v1"}),
    )
    .await
    .unwrap_err();
    let (text, state) = asc_mcp::api_error::public_error(&e);
    assert!(!text.contains("unsafe-secret"));
    let state = state.unwrap();
    assert!(state["submission_id"].is_null());
    assert_eq!(state["outcome_unknown"], true);
    assert_eq!(state["failed_step"], "read_created_submission");
    attach.assert_calls_async(0).await;
}

#[tokio::test]
async fn chunked_oversize_and_incomplete_error_stream_preserve_status_without_body() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    for chunked in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let body = if chunked {
                let payload = format!(
                    "{{\"errors\":[{{\"detail\":\"stream-secret{}\"}}]}}",
                    "x".repeat(70_000)
                );
                format!(
                    "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{payload}\r\n0\r\n\r\n",
                    payload.len()
                )
            } else {
                "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{\"errors\":[\"stream-secret\"".into()
            };
            // An oversize reader may close early. That is the expected behavior.
            let _ = stream.write_all(body.as_bytes()).await;
        });
        let e = ApiClient::with_endpoint(&endpoint)
            .unwrap()
            .request("POST", "/v1/apps", vec![], None)
            .await
            .unwrap_err();
        let text = e.to_string();
        assert!(text.contains("409"), "{text}");
        assert!(!text.contains("stream-secret"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn public_api_reports_apple_conflict_reason_without_replaying_write() {
    let host = MockServer::start_async().await;
    let failure = host.mock_async(|when, then| {
        when.method(PATCH).path("/v1/appStoreVersions/version1/relationships/build");
        then.status(409).header("Content-Type", "application/json").json_body(json!({"errors":[{"code":"STATE_ERROR.BUILD","title":"Invalid build","detail":"Select an eligible build","source":{"pointer":"/data/id"}}]}));
    }).await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    let error = client
        .request(
            "PATCH",
            "/v1/appStoreVersions/version1/relationships/build",
            vec![],
            Some(json!({"data":{"id":"build1","type":"builds"}})),
        )
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    for expected in [
        "409",
        "STATE_ERROR.BUILD",
        "Select an eligible build",
        "/data/id",
    ] {
        assert!(
            message.contains(expected),
            "missing {expected} in {message}"
        );
    }
    failure.assert_calls_async(1).await;
}

#[tokio::test]
async fn protocol_retains_created_draft_and_apple_cause_at_attachment_failure() {
    let host = MockServer::start_async().await;
    let create = host
        .mock_async(|when, then| {
            when.method(POST).path("/v1/reviewSubmissions");
            then.status(201).json_body(json!({"data":{"id":"draft1"}}));
        })
        .await;
    let attach = host.mock_async(|when, then| {
        when.method(POST).path("/v1/reviewSubmissionItems");
        then.status(409).header("Content-Type", "application/json").json_body(json!({"errors":[{"code":"STATE_ERROR.MISSING_METADATA","detail":"App privacy information is missing"}]}));
    }).await;
    let submit = host
        .mock_async(|when, then| {
            when.method(PATCH).path("/v1/reviewSubmissions/draft1");
            then.status(204);
        })
        .await;
    let server = Server {
        api: Some(ApiClient::with_endpoint(&host.base_url()).unwrap()),
        reviewer: None,
    };
    let session = Session::default();
    server.dispatch(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"regression","version":"1"}}}), &session).await.unwrap();
    let reply = server.dispatch(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"submit_app_for_review","arguments":{"app_id":"app1","version_id":"version1"}}}), &session).await.unwrap();
    assert_eq!(reply["result"]["isError"], true);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    for expected in [
        "draft1",
        "409",
        "STATE_ERROR.MISSING_METADATA",
        "App privacy information is missing",
    ] {
        assert!(text.contains(expected), "missing {expected} in {text}");
    }
    create.assert_calls_async(1).await;
    attach.assert_calls_async(1).await;
    submit.assert_calls_async(0).await;
}

#[tokio::test]
async fn html_errors_never_echo_response_body() {
    let host = MockServer::start_async().await;
    host.mock_async(|when, then| {
        when.method(GET).path("/v1/apps");
        then.status(403)
            .header("Content-Type", "text/html")
            .body("<html>private-session-value</html>");
    })
    .await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    let text = format!(
        "{:#}",
        client
            .request("GET", "/v1/apps", vec![], None)
            .await
            .unwrap_err()
    );
    assert!(text.contains("403"));
    assert!(!text.contains("private-session-value"));
}

#[tokio::test]
async fn error_details_redact_request_password_and_authorization_token() {
    let host = MockServer::start_async().await;
    host.mock_async(|when, then| {
        when.method(POST).path("/v1/appStoreReviewDetails");
        then.status(409).header("Content-Type", "application/json").json_body(json!({"errors":[{"code":"STATE_ERROR","detail":"Invalid demo account fixture-password-123; bearer offline-test-token","source":{"parameter":"fixture-password-123"}}]}));
    }).await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    let text = format!(
        "{:#}",
        client
            .request(
                "POST",
                "/v1/appStoreReviewDetails",
                vec![],
                Some(json!({"data":{"attributes":{"demoAccountPassword":"fixture-password-123"}}}))
            )
            .await
            .unwrap_err()
    );
    assert!(text.contains("STATE_ERROR"), "{text}");
    for secret in ["fixture-password-123", "offline-test-token"] {
        assert!(!text.contains(secret), "leaked sensitive fixture value");
    }
}

#[tokio::test]
async fn oversized_error_body_keeps_status_but_does_not_echo_details() {
    let host = MockServer::start_async().await;
    host.mock_async(|when, then| {
        when.method(POST).path("/v1/appScreenshotSets");
        then.status(409)
            .header("Content-Type", "application/json")
            .json_body(
                json!({"errors":[{"detail":format!("sensitive-marker{}", "x".repeat(70_000))}]}),
            );
    })
    .await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    let text = format!(
        "{:#}",
        client
            .request("POST", "/v1/appScreenshotSets", vec![], Some(json!({})))
            .await
            .unwrap_err()
    );
    assert!(text.contains("409"), "{text}");
    assert!(!text.contains("sensitive-marker"));
    assert!(text.len() < 2_000);
}

fn args_err(name: &str, args: serde_json::Value) -> String {
    format!("{:#}", prepare_request(name, &args).unwrap_err())
}

#[test]
fn app_price_schedule_uses_apple_local_ids_and_requires_prices() {
    let r = prepare_request(
        "set_app_price_schedule",
        &json!({"app_id":"a1","base_territory_id":"USA","prices":[{"price_point_id":"zero","start_date":"2026-01-01"},{"price_point_id":"p2"}]}),
    )
    .unwrap();
    let body = r.body.unwrap();
    let refs = &body["data"]["relationships"]["manualPrices"]["data"];
    assert_eq!(refs[0], json!({"type":"appPrices","id":"${price-1}"}));
    assert_eq!(refs[1]["id"], "${price-2}");
    assert_eq!(body["included"][0]["id"], "${price-1}");
    assert_eq!(body["included"][1]["id"], "${price-2}");
    assert_eq!(
        body["included"][0]["relationships"]["appPricePoint"]["data"],
        json!({"type":"appPricePoints","id":"zero"})
    );
    for args in [
        json!({"app_id":"a1","base_territory_id":"USA"}),
        json!({"app_id":"a1","base_territory_id":"USA","prices":[]}),
        json!({"app_id":"a1","base_territory_id":"USA","prices":null}),
    ] {
        assert!(
            prepare_request("set_app_price_schedule", &args).is_err(),
            "{args}"
        );
    }
    let tool = catalog::find("set_app_price_schedule").unwrap();
    assert!(
        tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("prices"))
    );
    let text = tool.to_string();
    assert!(
        text.contains("zero") && !text.contains("pass an empty prices array to keep the app free")
    );
}

#[test]
fn app_info_read_and_category_tools_compile_apple_shapes() {
    let r = prepare_request(
        "get_app_info",
        &json!({"app_info_id":"i1","include":["primaryCategory","ageRatingDeclaration"],"fields":{"appInfos":["primaryCategory"]}}),
    )
    .unwrap();
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("GET", "/v1/appInfos/i1")
    );
    assert!(r.query.contains(&(
        "include".into(),
        "primaryCategory,ageRatingDeclaration".into()
    )));
    assert!(
        r.query
            .contains(&("fields[appInfos]".into(), "primaryCategory".into()))
    );
    assert!(prepare_request("get_app_info", &json!({"app_info_id":" "})).is_err());

    let r = prepare_request(
        "update_app_info_categories",
        &json!({"app_info_id":"i1","primary_category_id":"GAMES"}),
    )
    .unwrap();
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("PATCH", "/v1/appInfos/i1")
    );
    assert_eq!(
        r.body.unwrap(),
        json!({"data":{"type":"appInfos","id":"i1","relationships":{"primaryCategory":{"data":{"type":"appCategories","id":"GAMES"}}}}})
    );
    let r = prepare_request(
        "update_app_info_categories",
        &json!({"app_info_id":"i1","primary_category_id":"GAMES","secondary_category_id":"EDUCATION"}),
    )
    .unwrap();
    assert_eq!(
        r.body.unwrap()["data"]["relationships"]["secondaryCategory"]["data"],
        json!({"type":"appCategories","id":"EDUCATION"})
    );
    for args in [
        json!({"app_info_id":"i1"}),
        json!({"app_info_id":"i1","primary_category_id":""}),
        json!({"app_info_id":"i1","primary_category_id":"  "}),
        json!({"app_info_id":"i1","primary_category_id":"GAMES","secondary_category_id":" "}),
        json!({"primary_category_id":"GAMES"}),
    ] {
        assert!(
            prepare_request("update_app_info_categories", &args).is_err(),
            "{args}"
        );
    }
}

#[test]
fn app_price_schedule_prices_list_manual_and_automatic() {
    let manual = prepare_request(
        "list_app_price_schedule_prices",
        &json!({"price_schedule_id":"s1","price_type":"MANUAL","include":["appPricePoint","territory"],"limit":500,"fields":{"appPricePoints":["customerPrice"]}}),
    )
    .unwrap();
    assert_eq!(manual.path, "/v1/appPriceSchedules/s1/manualPrices");
    assert!(
        manual
            .query
            .contains(&("include".into(), "appPricePoint,territory".into()))
    );
    assert!(manual.query.contains(&("limit".into(), "200".into())));
    assert!(
        manual
            .query
            .contains(&("fields[appPricePoints]".into(), "customerPrice".into()))
    );
    let auto = prepare_request(
        "list_app_price_schedule_prices",
        &json!({"price_schedule_id":"s1","price_type":"AUTOMATIC"}),
    )
    .unwrap();
    assert_eq!(auto.path, "/v1/appPriceSchedules/s1/automaticPrices");
    let next = prepare_request(
        "list_app_price_schedule_prices",
        &json!({"price_schedule_id":"s1","price_type":"MANUAL","cursor":"https://api.appstoreconnect.apple.com/v1/appPriceSchedules/s1/manualPrices?cursor=abc"}),
    )
    .unwrap();
    assert_eq!(next.query, vec![("cursor".to_string(), "abc".to_string())]);
    for args in [
        json!({"price_schedule_id":"s1"}),
        json!({"price_schedule_id":"s1","price_type":"manual"}),
        json!({"price_schedule_id":"s1","price_type":"OTHER"}),
        json!({"price_type":"MANUAL"}),
    ] {
        assert!(
            prepare_request("list_app_price_schedule_prices", &args).is_err(),
            "{args}"
        );
    }
}

#[test]
fn app_availability_uses_v2_app_relationship() {
    let r = prepare_request("get_app_availability", &json!({"app_id":"a1"})).unwrap();
    assert_eq!(r.path, "/v1/apps/a1/appAvailabilityV2");
}

#[test]
fn review_attachment_list_accepts_review_detail_id_and_legacy_alias() {
    let new = prepare_request(
        "list_app_store_review_attachments",
        &json!({"review_detail_id":"d1"}),
    )
    .unwrap();
    assert_eq!(
        new.path,
        "/v1/appStoreReviewDetails/d1/appStoreReviewAttachments"
    );
    let old = prepare_request(
        "list_app_store_review_attachments",
        &json!({"version_id":"d1"}),
    )
    .unwrap();
    assert_eq!(old.path, new.path);
    let both = prepare_request(
        "list_app_store_review_attachments",
        &json!({"version_id":"d1","review_detail_id":"d1"}),
    )
    .unwrap();
    assert_eq!(both.path, new.path);
    assert!(
        args_err(
            "list_app_store_review_attachments",
            json!({"version_id":"d1","review_detail_id":"d2"})
        )
        .contains("conflicting")
    );
    assert!(prepare_request("list_app_store_review_attachments", &json!({})).is_err());
    assert!(
        prepare_request(
            "list_app_store_review_attachments",
            &json!({"review_detail_id":" "})
        )
        .is_err()
    );
    let tool = catalog::find("list_app_store_review_attachments").unwrap();
    assert!(
        tool["inputSchema"]["properties"]
            .get("review_detail_id")
            .is_some()
    );
}

#[test]
fn age_rating_catalogue_matches_apple_schema_and_forwards_new_fields() {
    let spec: serde_json::Value =
        serde_json::from_str(include_str!("../doc/references/apple-asc/openapi.oas.json")).unwrap();
    let apple = &spec["components"]["schemas"]["AgeRatingDeclarationUpdateRequest"]["properties"]["data"]
        ["properties"]["attributes"]["properties"];
    let tool = catalog::find("update_age_rating_declaration").unwrap();
    for (arg, name, kind) in catalog::AGE_RATING_EXTRA {
        let mut property = &apple[*name];
        if let Some(reference) = property["$ref"].as_str() {
            property = spec.pointer(reference.trim_start_matches('#')).unwrap();
        }
        assert!(property.is_object(), "{name} not in Apple schema");
        let advertised = &tool["inputSchema"]["properties"][*arg];
        assert!(advertised.is_object(), "{arg} not advertised");
        match catalog::age_rating_enum(kind) {
            Some(values) => {
                assert_eq!(property["enum"], json!(values), "{name}");
                assert_eq!(advertised["enum"], json!(values), "{arg}");
            }
            None if *kind == "bool" => assert_eq!(advertised["type"], "boolean"),
            None => assert_eq!(advertised["type"], "string"),
        }
    }
    let r = prepare_request(
        "update_age_rating_declaration",
        &json!({"declaration_id":"d1","loot_box":false,"guns_or_other_weapons":"INFREQUENT","kids_age_band":"SIX_TO_EIGHT","age_rating_override_v2":"EIGHTEEN_PLUS","korea_age_rating_override":"NONE","developer_age_rating_info_url":"https://example.com/rating","gambling":false,"contests":"NONE"}),
    )
    .unwrap();
    let a = &r.body.unwrap()["data"]["attributes"];
    assert_eq!(a["lootBox"], false);
    assert_eq!(a["gunsOrOtherWeapons"], "INFREQUENT");
    assert_eq!(a["kidsAgeBand"], "SIX_TO_EIGHT");
    assert_eq!(a["ageRatingOverrideV2"], "EIGHTEEN_PLUS");
    assert_eq!(a["koreaAgeRatingOverride"], "NONE");
    assert_eq!(a["developerAgeRatingInfoUrl"], "https://example.com/rating");
    assert_eq!(a["gambling"], false);
    assert_eq!(a["contests"], "NONE");
}

#[test]
fn age_rating_rejects_unknown_arguments_invalid_enums_and_nulls() {
    let base = |extra: serde_json::Value| {
        let mut v = json!({"declaration_id":"d1"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    for extra in [
        json!({"app_privacy_data_collected":true}),
        json!({"guns_or_other_weapons":"SOMETIMES"}),
        json!({"contests":"SOMETIMES"}),
        json!({"contests":""}),
        json!({"kids_age_band":"TEN"}),
        json!({"age_rating_override_v2":"SEVENTEEN_PLUS"}),
        json!({"korea_age_rating_override":"EIGHTEEN_PLUS"}),
        json!({"developer_age_rating_info_url":"javascript:alert(1)"}),
        json!({"loot_box":null}),
        json!({"gambling":null}),
        json!({"horror_or_fear_themes":null}),
        json!({"loot_box":"true"}),
    ] {
        assert!(
            prepare_request("update_age_rating_declaration", &base(extra.clone())).is_err(),
            "{extra}"
        );
    }
    assert!(
        prepare_request(
            "update_age_rating_declaration",
            &json!({"declaration_id":"d1"})
        )
        .is_err()
    );
}

#[tokio::test]
async fn new_tools_work_over_http_and_mutations_are_attempted_once_on_409() {
    let host = MockServer::start_async().await;
    let read = host
        .mock_async(|when, then| {
            when.method(GET)
                .path("/v1/appPriceSchedules/s1/manualPrices")
                .query_param("include", "appPricePoint");
            then.status(200)
                .json_body(json!({"data":[{"type":"appPrices","id":"p1"}]}));
        })
        .await;
    let price = host
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/appPriceSchedules")
                .body_includes("${price-1}");
            then.status(409)
                .header("Content-Type", "application/json")
                .json_body(json!({"errors":[{"code":"ENTITY_ERROR","detail":"conflict"}]}));
        })
        .await;
    let categories = host
        .mock_async(|when, then| {
            when.method(PATCH).path("/v1/appInfos/i1");
            then.status(409)
                .header("Content-Type", "application/json")
                .json_body(json!({"errors":[{"code":"STATE_ERROR","detail":"locked"}]}));
        })
        .await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    call(
        &client,
        "list_app_price_schedule_prices",
        &json!({"price_schedule_id":"s1","price_type":"MANUAL","include":["appPricePoint"]}),
    )
    .await
    .unwrap();
    let error = call(
        &client,
        "set_app_price_schedule",
        &json!({"app_id":"a1","base_territory_id":"USA","prices":[{"price_point_id":"zero"}]}),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("409"));
    let error = call(
        &client,
        "update_app_info_categories",
        &json!({"app_info_id":"i1","primary_category_id":"GAMES"}),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("STATE_ERROR"));
    read.assert_calls_async(1).await;
    price.assert_calls_async(1).await;
    categories.assert_calls_async(1).await;
}
