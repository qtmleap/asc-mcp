#![cfg(unix)]
mod release_common;
use asc_mcp::release;
use httpmock::MockServer;
use release_common::*;
use serde_json::json;

#[tokio::test]
async fn repeated_confirm_and_fresh_preview_after_attempt_never_post() {
    let server = MockServer::start_async().await;
    let get = get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let first = release::call(&client, root.path(), &confirm_args(&token))
        .await
        .unwrap();
    assert_eq!(first["status"], "release_request_created");
    let again = release::call(&client, root.path(), &confirm_args(&token)).await;
    if let Ok(value) = &again {
        assert_ne!(value["status"], "release_request_created");
    }
    let fresh = release::call(&api_client(&server), root.path(), &preview_args())
        .await
        .unwrap();
    assert_eq!(fresh["status"], "release_attempt_recorded");
    assert!(fresh.get("confirmation_token").is_none());
    assert!(fresh.to_string().contains("PENDING_DEVELOPER_RELEASE"));
    assert!(get.calls_async().await >= 3);
    post.assert_calls_async(1).await;
}

#[tokio::test]
async fn failed_posts_are_outcome_unknown_and_block_all_future_posts() {
    let cases: [(u16, Option<serde_json::Value>); 4] = [
        (500, Some(json!({"errors":[{"detail":"boom"}]}))),
        (409, Some(json!({"errors":[{"detail":"conflict"}]}))),
        (201, None),
        (
            201,
            Some(json!({"data":{"type":"appStoreVersionReleaseRequests"}})),
        ),
    ];
    for (status, body) in cases {
        let server = MockServer::start_async().await;
        get(&server, "v1", ready("v1")).await;
        let post = post(&server, status, body).await;
        let root = tempfile::tempdir().unwrap();
        let client = api_client(&server);
        let token = prepare(&client, root.path()).await;
        let result = release::call(&client, root.path(), &confirm_args(&token))
            .await
            .unwrap();
        assert_eq!(result["status"], "outcome_unknown", "{status}");
        assert!(result.get("request_id").is_none());
        post.assert_calls_async(1).await;
        let _ = release::call(&client, root.path(), &confirm_args(&token)).await;
        let preview = release::call(&client, root.path(), &preview_args())
            .await
            .unwrap();
        assert_eq!(preview["status"], "release_attempt_recorded");
        assert!(preview.get("confirmation_token").is_none());
        let other = MockServer::start_async().await;
        get(&other, "v1", ready("v1")).await;
        let other_post = self::post(&other, 201, Some(created())).await;
        let _ = release::call(&api_client(&other), root.path(), &confirm_args(&token)).await;
        let _ = release::call(&api_client(&other), root.path(), &preview_args()).await;
        other_post.assert_calls_async(0).await;
        post.assert_calls_async(1).await;
    }
}

#[tokio::test]
async fn concurrent_confirms_post_once() {
    let server = MockServer::start_async().await;
    get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let args = confirm_args(&token);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        let root = root.path().to_path_buf();
        let args = args.clone();
        tasks.push(tokio::spawn(async move {
            release::call(&client, &root, &args).await
        }));
    }
    let mut created_count = 0;
    let mut busy_count = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(value) if value["status"] == "release_request_created" => created_count += 1,
            Err(error) if error.to_string() == "release_busy" => busy_count += 1,
            _ => (),
        }
    }
    assert_eq!(created_count, 1);
    assert!(busy_count > 0);
    post.assert_calls_async(1).await;
}
