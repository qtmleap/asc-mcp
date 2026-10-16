//! add_builds_to_beta_group: additive POST relationship, validated before any network call.
use asc_mcp::{
    api_tools::{call, prepare_request},
    catalog,
    client::ApiClient,
};
use httpmock::{Method::POST, MockServer};
use serde_json::{Value, json};

const TOOL: &str = "add_builds_to_beta_group";

fn err(args: Value) -> String {
    format!("{:#}", prepare_request(TOOL, &args).unwrap_err())
}

#[test]
fn catalogue_exposes_additive_tool_with_required_arguments() {
    let tool = catalog::find(TOOL).expect("tool is advertised");
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["beta_group_id", "build_ids"])
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["beta_group_id"]["type"],
        "string"
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["build_ids"]["type"],
        "array"
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["build_ids"]["items"]["type"],
        "string"
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["build_ids"]["minItems"],
        1
    );
    assert!(tool["inputSchema"]["properties"]["build_ids"]["maxItems"].is_null());
    let a = &tool["annotations"];
    assert_eq!(a["readOnlyHint"], false);
    assert_eq!(a["destructiveHint"], false);
    assert_eq!(a["idempotentHint"], true);
    let description = tool["description"].as_str().unwrap();
    for word in ["POST", "list_beta_groups", "list_builds", "betaGroups"] {
        assert!(description.contains(word), "description lacks {word}");
    }
}

#[test]
fn compiles_exact_post_path_and_body() {
    let request = prepare_request(
        TOOL,
        &json!({"beta_group_id":"group1","build_ids":["b1","b2"]}),
    )
    .unwrap();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/betaGroups/group1/relationships/builds");
    assert!(request.query.is_empty());
    assert_eq!(
        request.body.unwrap(),
        json!({"data":[{"type":"builds","id":"b1"},{"type":"builds","id":"b2"}]})
    );
}

#[test]
fn group_id_is_path_encoded() {
    let request =
        prepare_request(TOOL, &json!({"beta_group_id":"a b?x","build_ids":["b1"]})).unwrap();
    assert_eq!(
        request.path,
        "/v1/betaGroups/a%20b%3Fx/relationships/builds"
    );
}

#[test]
fn rejects_invalid_arguments_before_network() {
    let cases = [
        json!({}),
        json!({"build_ids":["b1"]}),
        json!({"beta_group_id":"g"}),
        json!({"beta_group_id":null,"build_ids":["b1"]}),
        json!({"beta_group_id":"g","build_ids":null}),
        json!({"beta_group_id":"","build_ids":["b1"]}),
        json!({"beta_group_id":"  ","build_ids":["b1"]}),
        json!({"beta_group_id":7,"build_ids":["b1"]}),
        json!({"beta_group_id":"g","build_ids":[]}),
        json!({"beta_group_id":"g","build_ids":"b1"}),
        json!({"beta_group_id":"g","build_ids":[""]}),
        json!({"beta_group_id":"g","build_ids":["b1"," "]}),
        json!({"beta_group_id":"g","build_ids":["b1",null]}),
        json!({"beta_group_id":"g","build_ids":["b1",2]}),
        json!({"beta_group_id":"g","build_ids":[["b1"]]}),
        json!({"beta_group_id":"g/x","build_ids":["b1"]}),
        json!({"beta_group_id":"..","build_ids":["b1"]}),
        json!({"beta_group_id":"g","build_ids":["b/1"]}),
        json!({"beta_group_id":"g","build_ids":[".."]}),
        json!({"beta_group_id":"g","build_ids":["b\\1"]}),
    ];
    for args in cases {
        assert!(
            prepare_request(TOOL, &args).is_err(),
            "accepted invalid {args}"
        );
    }
    assert!(err(json!({"beta_group_id":"g","build_ids":[]})).contains("build_ids"));
}

#[tokio::test]
async fn http_204_is_success_with_one_call() {
    let host = MockServer::start_async().await;
    let mock = host
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/betaGroups/g1/relationships/builds")
                .json_body(json!({"data":[{"type":"builds","id":"b1"}]}));
            then.status(204);
        })
        .await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    let out = call(
        &client,
        TOOL,
        &json!({"beta_group_id":"g1","build_ids":["b1"]}),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "success");
    mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn invalid_arguments_make_no_http_call() {
    let host = MockServer::start_async().await;
    let mock = host
        .mock_async(|when, then| {
            when.any_request();
            then.status(204);
        })
        .await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    for args in [
        json!({"beta_group_id":"g1","build_ids":[]}),
        json!({"beta_group_id":"g1","build_ids":["b1",""]}),
        json!({"beta_group_id":" ","build_ids":["b1"]}),
    ] {
        assert!(call(&client, TOOL, &args).await.is_err());
    }
    mock.assert_calls_async(0).await;
}

#[tokio::test]
async fn http_errors_are_attempted_once_and_report_apple_error() {
    for status in [409, 429, 500] {
        let host = MockServer::start_async().await;
        let mock = host
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1/betaGroups/g1/relationships/builds");
                then.status(status)
                    .header("Content-Type", "application/json")
                    .json_body(
                        json!({"errors":[{"code":"STATE_ERROR","detail":"build not eligible"}]}),
                    );
            })
            .await;
        let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
        let error = call(
            &client,
            TOOL,
            &json!({"beta_group_id":"g1","build_ids":["b1"]}),
        )
        .await
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains(&status.to_string()) && message.contains("STATE_ERROR"),
            "{message}"
        );
        mock.assert_calls_async(1).await;
    }
}
