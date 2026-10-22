#![allow(dead_code)]
use asc_mcp::client::ApiClient;
use httpmock::{
    Method::{GET, POST},
    Mock, MockServer,
};
use serde_json::{Value, json};
use std::path::Path;

pub fn version_body(
    id: &str,
    platform: &str,
    release: &str,
    state: &str,
    build: Option<&str>,
) -> Value {
    let build_data = build.map_or(
        Value::Null,
        |build_id| json!({"type":"builds","id":build_id}),
    );
    json!({
        "data":{
            "type":"appStoreVersions",
            "id":id,
            "attributes":{"platform":platform,"releaseType":release,"appStoreState":state,"versionString":"1.2.3"},
            "relationships":{"app":{"data":{"type":"apps","id":"app1"}},"build":{"data":build_data}}
        },
        "included":[{"type":"apps","id":"app1"}]
    })
}

pub fn ready(id: &str) -> Value {
    version_body(
        id,
        "IOS",
        "MANUAL",
        "PENDING_DEVELOPER_RELEASE",
        Some("build1"),
    )
}

pub async fn get<'a>(server: &'a MockServer, id: &str, body: Value) -> Mock<'a> {
    let path = format!("/v1/appStoreVersions/{id}");
    server
        .mock_async(move |when, then| {
            when.method(GET)
                .path(path)
                .query_param("include", "app,build");
            then.status(200).json_body(body);
        })
        .await
}

pub async fn post<'a>(server: &'a MockServer, status: u16, body: Option<Value>) -> Mock<'a> {
    server
        .mock_async(move |when, then| {
            when.method(POST)
                .path("/v1/appStoreVersionReleaseRequests")
                .json_body(json!({"data":{"type":"appStoreVersionReleaseRequests","relationships":{"appStoreVersion":{"data":{"type":"appStoreVersions","id":"v1"}}}}}));
            match body {
                Some(value) => then.status(status).json_body(value),
                None => then.status(status).body("not json"),
            };
        })
        .await
}

pub fn created() -> Value {
    json!({"data":{"type":"appStoreVersionReleaseRequests","id":"req1"}})
}

pub fn api_client(server: &MockServer) -> ApiClient {
    ApiClient::with_endpoint(&server.base_url()).unwrap()
}

pub fn preview_args() -> Value {
    json!({"app_id":"app1","version_id":"v1","expected_version_string":"1.2.3","expected_build_id":"build1"})
}

pub fn confirm_args(token: &str) -> Value {
    let mut value = preview_args();
    value["dry_run"] = json!(false);
    value["confirmation_token"] = json!(token);
    value
}

pub async fn prepare(client: &ApiClient, root: &Path) -> String {
    let result = asc_mcp::release::call(client, root, &preview_args())
        .await
        .unwrap();
    assert_eq!(result["status"], "prepared");
    result["confirmation_token"].as_str().unwrap().to_string()
}
