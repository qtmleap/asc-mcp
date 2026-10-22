#![cfg(unix)]
mod release_common;
use asc_mcp::release;
use httpmock::{Method::GET, MockServer};
use release_common::*;
use serde_json::json;

#[tokio::test]
async fn modern_state_is_supported_and_conflicting_or_missing_states_block_post() {
    let mut modern = ready("v1");
    modern["data"]["attributes"]
        .as_object_mut()
        .unwrap()
        .remove("appStoreState");
    modern["data"]["attributes"]["appVersionState"] = json!("PENDING_DEVELOPER_RELEASE");
    let server = MockServer::start_async().await;
    get(&server, "v1", modern).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let accepted = release::call(&client, root.path(), &confirm_args(&token))
        .await
        .unwrap();
    assert_eq!(accepted["status"], "release_request_created");
    post.assert_calls_async(1).await;

    for state in [json!(null), json!("READY_FOR_DISTRIBUTION"), json!(false)] {
        let mut conflicting = ready("v1");
        conflicting["data"]["attributes"]["appVersionState"] = state;
        let refused = MockServer::start_async().await;
        get(&refused, "v1", conflicting).await;
        let refused_post = release_common::post(&refused, 201, Some(created())).await;
        let private = tempfile::tempdir().unwrap();
        assert!(
            release::call(&api_client(&refused), private.path(), &preview_args())
                .await
                .is_err()
        );
        refused_post.assert_calls_async(0).await;
    }
}

#[tokio::test]
async fn preflight_http_failure_never_sends_a_release_request() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/v1/appStoreVersions/v1");
            then.status(403);
        })
        .await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    assert!(
        release::call(&api_client(&server), root.path(), &preview_args())
            .await
            .is_err()
    );
    post.assert_calls_async(0).await;
}
