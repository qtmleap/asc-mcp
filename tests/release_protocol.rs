use asc_mcp::{client::ApiClient, protocol::Server};
use serde_json::json;

fn names(server: &Server) -> Vec<String> {
    server
        .tools()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn release_tool_is_listed_only_with_api() {
    let bare = Server {
        api: None,
        reviewer: None,
        developer: None,
    };
    assert_eq!(bare.tools().len(), 356);
    assert!(
        !names(&bare)
            .iter()
            .any(|name| name == "release_app_store_version")
    );
    let api = Server {
        api: Some(ApiClient::with_endpoint("http://127.0.0.1:9").unwrap()),
        reviewer: None,
        developer: None,
    };
    let listed = names(&api);
    assert_eq!(listed.len(), 356 + usize::from(cfg!(unix)));
    assert_eq!(
        listed
            .iter()
            .filter(|name| *name == "release_app_store_version")
            .count(),
        usize::from(cfg!(unix))
    );
}

#[tokio::test]
async fn release_tool_rejects_invalid_arguments_before_network() {
    let api = Server {
        api: Some(ApiClient::with_endpoint("http://127.0.0.1:9").unwrap()),
        reviewer: None,
        developer: None,
    };
    assert!(
        api.call("release_app_store_version", &json!({"app_id":"a"}))
            .await
            .is_err()
    );
}
