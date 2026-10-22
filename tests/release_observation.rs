#![cfg(unix)]
mod release_common;
use asc_mcp::release;
use httpmock::{Method::GET, MockServer};
use release_common::*;

#[tokio::test]
async fn inspection_failure_still_reports_the_durable_attempt_guard() {
    let server = MockServer::start_async().await;
    let initial = get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    release::call(&client, root.path(), &confirm_args(&token))
        .await
        .unwrap();
    initial.delete_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/v1/appStoreVersions/v1");
            then.status(403);
        })
        .await;
    let observation = release::call(&client, root.path(), &preview_args())
        .await
        .unwrap();
    assert_eq!(observation["status"], "release_attempt_recorded");
    assert_eq!(observation["observation_error"], "read_failed");
    assert!(observation.get("confirmation_token").is_none());
    post.assert_calls_async(1).await;
}

#[tokio::test]
async fn symlinked_state_root_stops_before_any_api_request() {
    let server = MockServer::start_async().await;
    let get = get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("private");
    std::fs::create_dir(&target).unwrap();
    let alias = parent.path().join("alias");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    assert!(
        release::call(&api_client(&server), &alias, &preview_args())
            .await
            .is_err()
    );
    get.assert_calls_async(0).await;
    post.assert_calls_async(0).await;
}
