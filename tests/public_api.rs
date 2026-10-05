use asc_mcp::{
    api_tools::{build_request, call, prepare_request, validate_supported_arguments},
    client::ApiClient,
};
use httpmock::{
    Method::{GET, PATCH, POST},
    MockServer,
};
use serde_json::json;
#[tokio::test]
async fn submit_review_performs_all_three_steps() {
    let server = MockServer::start_async().await;
    let create=server.mock_async(|when,then|{when.method(POST).path("/v1/reviewSubmissions").json_body(json!({"data":{"type":"reviewSubmissions","attributes":{"platform":"IOS"},"relationships":{"app":{"data":{"type":"apps","id":"app1"}}}}}));then.status(201).json_body(json!({"data":{"type":"reviewSubmissions","id":"sub1"}}));}).await;
    let attach=server.mock_async(|when,then|{when.method(POST).path("/v1/reviewSubmissionItems").json_body(json!({"data":{"type":"reviewSubmissionItems","relationships":{"reviewSubmission":{"data":{"type":"reviewSubmissions","id":"sub1"}},"appStoreVersion":{"data":{"type":"appStoreVersions","id":"version1"}}}}}));then.status(201).json_body(json!({"data":{"id":"item1"}}));}).await;
    let submit=server.mock_async(|when,then|{when.method(PATCH).path("/v1/reviewSubmissions/sub1").json_body(json!({"data":{"type":"reviewSubmissions","id":"sub1","attributes":{"submitted":true}}}));then.status(200).json_body(json!({"data":{"id":"sub1","attributes":{"submitted":true}}}));}).await;
    let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
    let response = call(
        &client,
        "submit_app_for_review",
        &json!({"app_id":"app1","version_id":"version1"}),
    )
    .await
    .unwrap();
    assert_eq!(response["data"]["attributes"]["submitted"], true);
    create.assert_calls_async(1).await;
    attach.assert_calls_async(1).await;
    submit.assert_calls_async(1).await;
}
#[tokio::test]
async fn submission_failure_stops_and_reports_partial_progress() {
    let server = MockServer::start_async().await;
    let create = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/reviewSubmissions");
            then.status(201).json_body(json!({"data":{"id":"sub1"}}));
        })
        .await;
    let attach = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/reviewSubmissionItems");
            then.status(400)
                .json_body(json!({"errors":[{"detail":"invalid version"}]}));
        })
        .await;
    let submit = server
        .mock_async(|when, then| {
            when.method(PATCH).path("/v1/reviewSubmissions/sub1");
            then.status(200).json_body(json!({"data":{"id":"sub1"}}));
        })
        .await;
    let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
    let error = call(
        &client,
        "submit_app_for_review",
        &json!({"app_id":"app1","version_id":"version1"}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("sub1 created; failed to attach"), "{error}");
    create.assert_calls_async(1).await;
    attach.assert_calls_async(1).await;
    submit.assert_calls_async(0).await;
}
#[tokio::test]
async fn cursor_follows_same_origin_without_initial_query() {
    let server = MockServer::start_async().await;
    let next = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/v1/apps")
                .query_param("cursor", "next")
                .query_param_missing("limit");
            then.status(200).json_body(json!({"data":[]}));
        })
        .await;
    let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
    call(
        &client,
        "list_apps",
        &json!({"limit":50,"cursor":format!("{}/v1/apps?cursor=next",server.base_url())}),
    )
    .await
    .unwrap();
    next.assert_calls_async(1).await;
    assert!(
        call(
            &client,
            "list_apps",
            &json!({"cursor":"https://evil.example/v1/apps"})
        )
        .await
        .is_err()
    );
}
#[test]
fn unsupported_legacy_arguments_are_explicit_errors() {
    assert!(
        validate_supported_arguments(
            "list_app_store_versions",
            &json!({"app_id":"1","sort":["name"]})
        )
        .unwrap_err()
        .to_string()
        .contains("never implemented")
    );
    assert_eq!(
        prepare_request(
            "update_subscription_offer_code",
            &json!({"offer_code_id":"1","is_active":false})
        )
        .unwrap()
        .body
        .unwrap()["data"]["attributes"]["active"],
        false
    );
    assert!(
        validate_supported_arguments(
            "update_subscription_offer_code",
            &json!({"offer_code_id":"1","active":false})
        )
        .is_ok()
    );
    assert!(
        validate_supported_arguments(
            "add_review_submission_item",
            &json!({"review_submission_id":"1","app_event_id":"2","app_store_version_id":"3"})
        )
        .is_err()
    );
}
#[test]
fn false_patch_and_empty_string_omission_follow_go_contract() {
    let request =
        build_request("update_webhook", &json!({"webhook_id":"1","enabled":false})).unwrap();
    assert_eq!(
        request.body.unwrap()["data"]["attributes"]["enabled"],
        false
    );
    let request = build_request("update_webhook", &json!({"webhook_id":"1"})).unwrap();
    assert!(
        request.body.unwrap()["data"]["attributes"]
            .get("enabled")
            .is_none()
    );
    let request = build_request(
        "update_app_info_localization",
        &json!({"localization_id":"1","subtitle":""}),
    )
    .unwrap();
    assert!(
        request.body.unwrap()["data"]["attributes"]
            .get("subtitle")
            .is_none()
    );
}
#[test]
fn schema_repairs_follow_bundled_apple_openapi() {
    let spec: serde_json::Value =
        serde_json::from_str(include_str!("../doc/references/apple-asc/openapi.oas.json")).unwrap();
    assert!(spec["components"]["schemas"]["AgeRatingDeclarationUpdateRequest"]["properties"]["data"]["properties"]["attributes"]["properties"].get("horrorOrFearThemes").is_some());
    let r = prepare_request(
        "update_age_rating_declaration",
        &json!({"declaration_id":"1","horror_or_fear_themes":"INFREQUENT_OR_MILD"}),
    )
    .unwrap();
    assert_eq!(
        r.body.unwrap()["data"]["attributes"]["horrorOrFearThemes"],
        "INFREQUENT_OR_MILD"
    );
    for (alias, expected) in [
        ("SHOW_FOR_ALL_USERS", true),
        ("APP_STORE_CONNECT_ONLY", false),
    ] {
        let r = prepare_request(
            "update_promoted_purchase",
            &json!({"promoted_purchase_id":"1","visibility_type":alias}),
        )
        .unwrap();
        assert_eq!(
            r.body.unwrap()["data"]["attributes"]["visibleForAllUsers"],
            expected
        );
    }
    let r = prepare_request(
        "create_promoted_purchase",
        &json!({"app_id":"1","in_app_purchase_id":"2","visibility_type":"APP_STORE_CONNECT_ONLY"}),
    )
    .unwrap();
    assert_eq!(
        r.body.unwrap()["data"]["attributes"]["visibleForAllUsers"],
        false
    );
    assert!(
        prepare_request(
            "update_promoted_purchase",
            &json!({"promoted_purchase_id":"1","visibility_type":"UNKNOWN"})
        )
        .is_err()
    );
    assert!(
        prepare_request(
            "update_subscription_offer_code",
            &json!({"offer_code_id":"1","is_active":false,"active":true})
        )
        .is_err()
    );
}
fn offer_args() -> serde_json::Value {
    json!({"subscription_id":"sub1","name":"offer","customer_eligibility":"NEW","offer_eligibility_type":"STACK_WITH_INTRO_OFFERS","duration":"ONE_MONTH","offer_mode":"FREE_TRIAL","number_of_periods":1,"prices":[{"territory_id":"USA","price_point_id":"point1"},{"territory_id":"JPN","price_point_id":"point2"}]})
}
#[test]
fn subscription_offer_has_all_apple_required_fields_and_inline_prices() {
    let body = prepare_request("create_subscription_offer_code", &offer_args())
        .unwrap()
        .body
        .unwrap();
    assert_eq!(
        body["data"]["attributes"]["customerEligibilities"],
        json!(["NEW"])
    );
    assert_eq!(
        body["data"]["attributes"]["offerEligibility"],
        "STACK_WITH_INTRO_OFFERS"
    );
    assert_eq!(
        body["data"]["relationships"]["prices"]["data"],
        json!([{"type":"subscriptionOfferCodePrices","id":"price-1"},{"type":"subscriptionOfferCodePrices","id":"price-2"}])
    );
    assert_eq!(
        body["included"][1]["relationships"]["territory"]["data"]["id"],
        "JPN"
    );
    assert_eq!(
        body["included"][1]["relationships"]["subscriptionPricePoint"]["data"]["type"],
        "subscriptionPricePoints"
    );
    let spec: serde_json::Value =
        serde_json::from_str(include_str!("../doc/references/apple-asc/openapi.oas.json")).unwrap();
    let data = &spec["components"]["schemas"]["SubscriptionOfferCodeCreateRequest"]["properties"]["data"]
        ["properties"];
    for required in data["attributes"]["required"].as_array().unwrap() {
        assert!(
            body["data"]["attributes"]
                .get(required.as_str().unwrap())
                .is_some()
        );
    }
    for required in data["relationships"]["required"].as_array().unwrap() {
        assert!(
            body["data"]["relationships"]
                .get(required.as_str().unwrap())
                .is_some()
        );
    }
    let mut args = offer_args();
    args.as_object_mut().unwrap().remove("prices");
    args["price_ids"] = json!(["existing1"]);
    let body = prepare_request("create_subscription_offer_code", &args)
        .unwrap()
        .body
        .unwrap();
    assert_eq!(
        body["data"]["relationships"]["prices"]["data"][0]["id"],
        "existing1"
    );
    assert!(body.get("included").is_none());
    for key in ["duration", "offer_mode", "number_of_periods", "prices"] {
        let mut args = offer_args();
        args.as_object_mut().unwrap().remove(key);
        assert!(
            prepare_request("create_subscription_offer_code", &args).is_err(),
            "{key}"
        );
    }
}
#[tokio::test]
async fn repaired_offer_alias_is_sent_to_http() {
    let server = MockServer::start_async().await;
    let request = prepare_request("create_subscription_offer_code", &offer_args()).unwrap();
    let create = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/subscriptionOfferCodes")
                .json_body(request.body.unwrap());
            then.status(201).json_body(json!({"data":{"id":"offer1"}}));
        })
        .await;
    let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
    assert_eq!(
        call(&client, "create_subscription_offer_code", &offer_args())
            .await
            .unwrap()["data"]["id"],
        "offer1"
    );
    create.assert_calls_async(1).await;
}
#[test]
fn resource_ids_are_encoded_and_ambiguous_paths_rejected() {
    let r = build_request("get_app", &json!({"app_id":"a?include=keys#fragment"})).unwrap();
    assert_eq!(r.path, "/v1/apps/a%3Finclude%3Dkeys%23fragment");
    for id in ["../apps", "..", ".", "a\\b", "a/b"] {
        assert!(
            build_request("get_app", &json!({"app_id":id})).is_err(),
            "{id}"
        );
    }
    assert_eq!(
        build_request("get_app", &json!({"app_id":"%2e%2e"}))
            .unwrap()
            .path,
        "/v1/apps/%252e%252e"
    );
}
