//! Beta distribution activation must send one notification and reconcile uncertainty.
use asc_mcp::{
    api_tools::{call, prepare_request},
    catalog,
    client::ApiClient,
};
use httpmock::{Method::POST, MockServer};
use serde_json::{Value, json};
use std::time::Duration;

const TOOL: &str = "create_beta_build_notification";
const BUILD: &str = "df3441b7-0b7c-4085-b2e0-431932eeb81b";

fn body() -> Value {
    json!({"data":{"type":"buildBetaNotifications","relationships":{"build":{"data":{"type":"builds","id":BUILD}}}}})
}

#[test]
fn advertises_non_idempotent_notification_with_explicit_build() {
    let t = catalog::find(TOOL).expect("notification tool must be advertised");
    assert_eq!(t["inputSchema"]["required"], json!(["build_id"]));
    assert_eq!(t["inputSchema"]["properties"]["build_id"]["type"], "string");
    assert_eq!(t["annotations"]["readOnlyHint"], false);
    assert_eq!(t["annotations"]["destructiveHint"], false);
    assert_eq!(t["annotations"]["idempotentHint"], false);
    assert_eq!(t["annotations"]["openWorldHint"], true);
}

#[test]
fn exact_apple_post_and_build_relationship() {
    let r = prepare_request(TOOL, &json!({"build_id":BUILD})).unwrap();
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/v1/buildBetaNotifications");
    assert!(r.query.is_empty());
    assert_eq!(r.body.unwrap(), body());
}

#[tokio::test]
async fn rejects_invalid_ids_without_http_requests() {
    let host = MockServer::start_async().await;
    let mock = host
        .mock_async(|when, then| {
            when.any_request();
            then.status(201);
        })
        .await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    for args in [
        json!({}),
        json!({"build_id":null}),
        json!({"build_id":7}),
        json!({"build_id":[]}),
        json!({"build_id":""}),
        json!({"build_id":" \n"}),
        json!({"build_id":"."}),
        json!({"build_id":".."}),
        json!({"build_id":"a/b"}),
        json!({"build_id":"a\\b"}),
    ] {
        assert!(call(&client, TOOL, &args).await.is_err(), "accepted {args}");
    }
    mock.assert_calls_async(0).await;
}

#[tokio::test]
async fn returns_created_notification_after_one_post() {
    let host = MockServer::start_async().await;
    let response = json!({"data":{"type":"buildBetaNotifications","id":"notification-1"},"links":{"self":"https://api.appstoreconnect.apple.com/v1/buildBetaNotifications/notification-1"}});
    let mock = host
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/buildBetaNotifications")
                .json_body(body());
            then.status(201).json_body(response.clone());
        })
        .await;
    let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    assert_eq!(
        call(&client, TOOL, &json!({"build_id":BUILD}))
            .await
            .unwrap(),
        response
    );
    mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn malformed_or_unexpected_success_is_uncertain_and_not_retried() {
    for (status, response) in [
        (201, json!({})),
        (201, json!({"data":{"type":"builds","id":"wrong-type"}})),
        (201, json!({"data":{"type":"buildBetaNotifications"}})),
        (
            201,
            json!({"data":{"type":"buildBetaNotifications","id":" "}}),
        ),
        (
            201,
            json!({"data":{"type":"buildBetaNotifications","id":7}}),
        ),
        (
            200,
            json!({"data":{"type":"buildBetaNotifications","id":"notification-1"}}),
        ),
        (204, Value::Null),
    ] {
        let host = MockServer::start_async().await;
        let mock = host
            .mock_async(|when, then| {
                when.method(POST).path("/v1/buildBetaNotifications");
                then.status(status).json_body(response);
            })
            .await;
        let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
        let err = call(&client, TOOL, &json!({"build_id":BUILD}))
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").to_lowercase().contains("uncertain"),
            "{err:#}"
        );
        mock.assert_calls_async(1).await;
    }
}

#[tokio::test]
async fn apple_rejections_and_failures_are_never_retried() {
    for status in [409, 422, 429, 500] {
        let host = MockServer::start_async().await;
        let mock = host
            .mock_async(|when, then| {
                when.method(POST).path("/v1/buildBetaNotifications");
                then.status(status).json_body(
                    json!({"errors":[{"code":"STATE_ERROR","detail":"build not eligible"}]}),
                );
            })
            .await;
        let client = ApiClient::with_endpoint(&host.base_url()).unwrap();
        let err = call(&client, TOOL, &json!({"build_id":BUILD}))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains(&status.to_string()), "{err:#}");
        mock.assert_calls_async(1).await;
    }
}

#[tokio::test]
async fn transport_timeout_is_an_uncertain_single_attempt() {
    let host = MockServer::start_async().await;
    let mock = host
        .mock_async(|when, then| {
            when.method(POST).path("/v1/buildBetaNotifications");
            then.status(201)
                .delay(Duration::from_millis(200))
                .json_body(json!({"data":{"type":"buildBetaNotifications","id":"notification-1"}}));
        })
        .await;
    let mut client = ApiClient::with_endpoint(&host.base_url()).unwrap();
    client.http = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .timeout(Duration::from_millis(30))
        .build()
        .unwrap();
    let err = call(&client, TOOL, &json!({"build_id":BUILD}))
        .await
        .unwrap_err();
    let text = format!("{err:#}").to_lowercase();
    assert!(text.contains("uncertain"), "{text}");
    mock.assert_calls_async(1).await;
}
