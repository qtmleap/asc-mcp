mod release_common;
use asc_mcp::release;
use httpmock::{Method::POST, MockServer};
use release_common::*;
use std::time::Duration;

#[tokio::test]
async fn cancellation_after_post_blocks_replay_from_a_new_client() {
    let server = MockServer::start_async().await;
    get(&server, "v1", ready("v1")).await;
    let post = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/appStoreVersionReleaseRequests");
            then.status(201)
                .delay(Duration::from_secs(3))
                .json_body(created());
        })
        .await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let task_root = root.path().to_path_buf();
    let args = confirm_args(&token);
    let task = tokio::spawn(async move { release::call(&client, &task_root, &args).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while post.calls_async().await == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let restarted = api_client(&server);
    let refused = release::call(&restarted, root.path(), &confirm_args(&token))
        .await
        .unwrap();
    assert_eq!(refused["status"], "release_attempt_recorded");
    let observation = release::call(&restarted, root.path(), &preview_args())
        .await
        .unwrap();
    assert_eq!(observation["status"], "release_attempt_recorded");
    assert!(observation.get("confirmation_token").is_none());
    post.assert_calls_async(1).await;
}

#[tokio::test]
async fn corrupted_receipt_blocks_confirmation_without_posting() {
    let server = MockServer::start_async().await;
    get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let receipt = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    std::fs::write(receipt, b"{invalid-json").unwrap();
    assert!(
        release::call(&client, root.path(), &confirm_args(&token))
            .await
            .is_err()
    );
    post.assert_calls_async(0).await;
}

#[tokio::test]
async fn expired_preview_never_sends_a_release_request() {
    let server = MockServer::start_async().await;
    let get = get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let receipt_path = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    receipt["issued_at"] = serde_json::json!(0);
    receipt["expires_at"] = serde_json::json!(900);
    std::fs::write(receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let failure = release::call(&client, root.path(), &confirm_args(&token))
        .await
        .unwrap_err();
    assert_eq!(failure.to_string(), "release_token_expired");
    get.assert_calls_async(1).await;
    post.assert_calls_async(0).await;
}
