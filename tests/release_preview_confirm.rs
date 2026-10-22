#![cfg(unix)]
mod release_common;
use asc_mcp::release;
use httpmock::MockServer;
use release_common::*;
use serde_json::json;

#[test]
fn tool_schema_declares_contract() {
    let tool = release::tool();
    assert_eq!(tool["name"], "release_app_store_version");
    let required = tool["inputSchema"]["required"].as_array().unwrap();
    for name in [
        "app_id",
        "version_id",
        "expected_version_string",
        "expected_build_id",
    ] {
        assert!(required.iter().any(|value| value == name), "{name}");
    }
    assert!(
        !required
            .iter()
            .any(|value| value == "dry_run" || value == "confirmation_token")
    );
}

#[tokio::test]
async fn preview_makes_get_only_and_stores_private_receipt() {
    let server = MockServer::start_async().await;
    let get = get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let result = release::call(&api_client(&server), root.path(), &preview_args())
        .await
        .unwrap();
    assert_eq!(result["status"], "prepared");
    assert!(!result["confirmation_token"].as_str().unwrap().is_empty());
    assert_eq!(result["target"]["version_id"], "v1");
    get.assert_calls_async(1).await;
    post.assert_calls_async(0).await;
    let explicit = release::call(&api_client(&server), root.path(), &json!({"app_id":"app1","version_id":"v1","expected_version_string":"1.2.3","expected_build_id":"build1","dry_run":true})).await.unwrap();
    assert_eq!(explicit["status"], "prepared");
    post.assert_calls_async(0).await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut files = 0;
        let mut stack = vec![root.path().to_path_buf()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                assert_eq!(meta.permissions().mode() & 0o077, 0, "{:?}", entry.path());
                if meta.is_dir() {
                    stack.push(entry.path())
                } else {
                    files += 1
                }
            }
        }
        assert!(files > 0);
    }
}

#[tokio::test]
async fn confirm_sends_exact_minimal_post_once() {
    let server = MockServer::start_async().await;
    let get = get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let result = release::call(&client, root.path(), &confirm_args(&token))
        .await
        .unwrap();
    assert_eq!(result["status"], "release_request_created");
    assert_eq!(result["request_id"], "req1");
    assert!(!result.to_string().contains("READY_FOR_SALE"));
    get.assert_calls_async(2).await;
    post.assert_calls_async(1).await;
}

#[tokio::test]
async fn mismatch_matrix_never_posts() {
    let cases = [
        ready("other"),
        version_body(
            "v1",
            "MAC_OS",
            "MANUAL",
            "PENDING_DEVELOPER_RELEASE",
            Some("build1"),
        ),
        version_body(
            "v1",
            "IOS",
            "AFTER_APPROVAL",
            "PENDING_DEVELOPER_RELEASE",
            Some("build1"),
        ),
        version_body("v1", "IOS", "MANUAL", "READY_FOR_SALE", Some("build1")),
        version_body(
            "v1",
            "IOS",
            "MANUAL",
            "PENDING_APPLE_RELEASE",
            Some("build1"),
        ),
        version_body(
            "v1",
            "IOS",
            "MANUAL",
            "PENDING_DEVELOPER_RELEASE",
            Some("build2"),
        ),
        version_body("v1", "IOS", "MANUAL", "PENDING_DEVELOPER_RELEASE", None),
        {
            let mut body = ready("v1");
            body["data"]["relationships"]["app"]["data"]["id"] = json!("app2");
            body
        },
        {
            let mut body = ready("v1");
            body["data"]["attributes"]["versionString"] = json!("9.9.9");
            body
        },
        {
            let mut body = ready("v1");
            body["data"]["type"] = json!("apps");
            body
        },
    ];
    for body in cases {
        let server = MockServer::start_async().await;
        get(&server, "v1", body).await;
        let post = post(&server, 201, Some(created())).await;
        let root = tempfile::tempdir().unwrap();
        assert!(
            release::call(&api_client(&server), root.path(), &preview_args())
                .await
                .is_err()
        );
        post.assert_calls_async(0).await;
        let ok = MockServer::start_async().await;
        get(&ok, "v1", ready("v1")).await;
        let token = prepare(&api_client(&ok), root.path()).await;
        assert!(
            release::call(&api_client(&server), root.path(), &confirm_args(&token))
                .await
                .is_err()
        );
        post.assert_calls_async(0).await;
    }
}

#[tokio::test]
async fn invalid_arguments_make_no_network_calls() {
    let server = MockServer::start_async().await;
    let get = get(&server, "v1", ready("v1")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let mut bad = vec![];
    for key in [
        "app_id",
        "version_id",
        "expected_version_string",
        "expected_build_id",
    ] {
        let mut args = preview_args();
        args.as_object_mut().unwrap().remove(key);
        bad.push(args);
        let mut args = preview_args();
        args[key] = json!(7);
        bad.push(args);
    }
    let mut args = preview_args();
    args["extra"] = json!(true);
    bad.push(args);
    let mut args = preview_args();
    args["dry_run"] = json!("false");
    bad.push(args);
    let mut args = preview_args();
    args["confirmation_token"] = json!(5);
    bad.push(args);
    let mut args = preview_args();
    args["dry_run"] = json!(false);
    bad.push(args);
    let mut args = confirm_args("not-a-real-token");
    args["dry_run"] = json!(false);
    bad.push(args);
    bad.push(json!([]));
    for args in bad {
        assert!(
            release::call(&client, root.path(), &args).await.is_err(),
            "{args}"
        );
    }
    get.assert_calls_async(0).await;
    post.assert_calls_async(0).await;
}

#[tokio::test]
async fn token_is_bound_to_exact_target_and_args() {
    let server = MockServer::start_async().await;
    get(&server, "v1", ready("v1")).await;
    get(&server, "v2", ready("v2")).await;
    let post = post(&server, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let client = api_client(&server);
    let token = prepare(&client, root.path()).await;
    let mut other_version = confirm_args(&token);
    other_version["version_id"] = json!("v2");
    let mut other_build = confirm_args(&token);
    other_build["expected_build_id"] = json!("build2");
    let mut other_string = confirm_args(&token);
    other_string["expected_version_string"] = json!("1.2.4");
    let mut other_app = confirm_args(&token);
    other_app["app_id"] = json!("app2");
    for args in [
        other_version,
        other_build,
        other_string,
        other_app,
        confirm_args("wrong"),
    ] {
        assert!(
            release::call(&client, root.path(), &args).await.is_err(),
            "{args}"
        );
    }
    post.assert_calls_async(0).await;
}

#[tokio::test]
async fn drift_between_preview_and_confirm_blocks_post() {
    let first = MockServer::start_async().await;
    get(&first, "v1", ready("v1")).await;
    let second = MockServer::start_async().await;
    get(
        &second,
        "v1",
        version_body("v1", "IOS", "MANUAL", "READY_FOR_SALE", Some("build1")),
    )
    .await;
    let post = post(&second, 201, Some(created())).await;
    let root = tempfile::tempdir().unwrap();
    let token = prepare(&api_client(&first), root.path()).await;
    assert!(
        release::call(&api_client(&second), root.path(), &confirm_args(&token))
            .await
            .is_err()
    );
    post.assert_calls_async(0).await;
}
