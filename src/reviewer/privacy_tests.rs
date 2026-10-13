use super::*;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    routing::any,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct StateData {
    usages: Vec<Value>,
    published: bool,
    writes: Vec<(Method, HeaderMap, Value)>,
    hide_write: bool,
    reject: bool,
    server_error: bool,
    malformed: bool,
    wrong_app: bool,
    swap: Option<PathBuf>,
}
async fn handler(
    State(state): State<Arc<Mutex<StateData>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let mut s = state.lock().unwrap();
    let path = uri.path();
    if method != Method::GET {
        s.writes.push((
            method.clone(),
            headers.clone(),
            serde_json::from_slice(&body).unwrap_or(Value::Null),
        ));
    }
    if let Some(p) = s.swap.take() {
        atomic_json(&p, &json!({"cookies":[{"name":"session","value":"other-secret","domain":"apple.com","path":"/"}]})).unwrap();
    }
    let result = match (method.clone(), path) {
        (Method::GET, "/iris/v1/apps/123") => {
            json!({"data":{"type":"apps","id":if s.wrong_app {"456"} else {"123"}}})
        }
        (Method::GET, "/iris/v1/apps/123/dataUsages") => json!({"data":s.usages}),
        (Method::GET, "/iris/v1/apps/123/dataUsagePublishState") => {
            if s.malformed {
                json!({"data":{"type":"appDataUsagesPublishState","id":"pub1","attributes":{"published":"false"}}})
            } else {
                json!({"data":{"type":"appDataUsagesPublishState","id":"pub1","attributes":{"published":s.published}}})
            }
        }
        (Method::POST, "/iris/v1/appDataUsages") => {
            if s.server_error {
                return (StatusCode::INTERNAL_SERVER_ERROR, "{}".into());
            }
            if s.reject {
                return (StatusCode::CONFLICT, "{}".into());
            }
            if !s.hide_write {
                s.usages = vec![no_data()];
            }
            json!({"data":no_data()})
        }
        (Method::PATCH, "/iris/v1/appDataUsagesPublishState/pub1") => {
            if s.server_error {
                return (StatusCode::INTERNAL_SERVER_ERROR, "{}".into());
            }
            if s.reject {
                return (StatusCode::CONFLICT, "{}".into());
            }
            if !s.hide_write {
                s.published = true;
            }
            return (StatusCode::NO_CONTENT, String::new());
        }
        _ => return (StatusCode::NOT_FOUND, "{}".into()),
    };
    (StatusCode::OK, result.to_string())
}
fn no_data() -> Value {
    json!({"type":"appDataUsages","id":"usage1","relationships":{"app":{"data":{"type":"apps","id":"123"}},"dataProtection":{"data":{"type":"appDataUsageDataProtections","id":"DATA_NOT_COLLECTED"}}}})
}
fn args() -> Value {
    json!({"app_id":"123","confirmation":"DATA_NOT_COLLECTED"})
}
async fn setup() -> (
    tempfile::TempDir,
    Reviewer,
    Arc<Mutex<StateData>>,
    tokio::task::JoinHandle<()>,
) {
    let st = Arc::new(Mutex::new(StateData::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(any(handler)).with_state(st.clone());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let d = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test(&origin, d.path());
    (d, r, st, handle)
}
#[tokio::test]
async fn privacy_registers_and_declares_without_deleting() {
    let (_d, r, s, h) = setup().await;
    let result = r
        .call("declare_app_privacy_no_data", &args())
        .await
        .unwrap();
    assert_eq!(result["status"], "declared");
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 1);
    assert_eq!(st.writes[0].0, Method::POST);
    assert_eq!(
        st.writes[0].2["data"]["relationships"]["dataProtection"]["data"]["id"],
        "DATA_NOT_COLLECTED"
    );
    h.abort();
}
#[tokio::test]
async fn privacy_publish_patch_204_has_headers_and_readback() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().usages = vec![no_data()];
    let result = r
        .call("publish_app_privacy_no_data", &args())
        .await
        .unwrap();
    assert_eq!(result["status"], "published");
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 1);
    let w = &st.writes[0];
    assert_eq!(w.0, Method::PATCH);
    assert!(w.1.contains_key("origin"));
    assert!(w.1.contains_key("referer"));
    assert_eq!(w.1["x-csrf-itc"], "[asc-ui]");
    assert_eq!(w.2["data"]["id"], "pub1");
    h.abort();
}
#[tokio::test]
async fn privacy_idempotent_existing_no_data() {
    let (_d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.usages = vec![no_data()];
        st.published = true;
    }
    assert_eq!(
        r.call("declare_app_privacy_no_data", &args())
            .await
            .unwrap()["status"],
        "declared"
    );
    assert_eq!(
        r.call("publish_app_privacy_no_data", &args())
            .await
            .unwrap()["status"],
        "published"
    );
    assert!(s.lock().unwrap().writes.is_empty());
    h.abort();
}
#[tokio::test]
async fn privacy_refuses_disclosures_duplicates_malformed_and_foreign_app() {
    for mode in 0..5 {
        let (_d, r, s, h) = setup().await;
        {
            let mut st = s.lock().unwrap();
            match mode {
                0 => {
                    let mut u = no_data();
                    u["relationships"]["category"] =
                        json!({"data":{"type":"appDataUsageCategories","id":"CONTACT_INFO"}});
                    st.usages = vec![u]
                }
                1 => st.usages = vec![no_data(), no_data()],
                2 => st.malformed = true,
                3 => st.wrong_app = true,
                _ => {
                    let mut u = no_data();
                    u["relationships"]["app"]["data"]["id"] = json!("456");
                    st.usages = vec![u]
                }
            }
        }
        let error = r
            .call("declare_app_privacy_no_data", &args())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(if mode == 2 {
                "schema_invalid"
            } else if mode >= 3 {
                "target_mismatch"
            } else {
                "conflict"
            }),
            "{error}"
        );
        assert!(s.lock().unwrap().writes.is_empty());
        h.abort();
    }
}
#[tokio::test]
async fn privacy_unknown_create_blocks_after_restart_until_conclusive() {
    let (d, r, s, h) = setup().await;
    s.lock().unwrap().hide_write = true;
    assert!(
        r.call("declare_app_privacy_no_data", &args())
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    assert!(
        restarted
            .call("declare_app_privacy_no_data", &args())
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    {
        let mut st = s.lock().unwrap();
        st.usages = vec![no_data()];
        st.hide_write = false;
    }
    assert_eq!(
        restarted
            .call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap()["outcome_unknown"],
        false
    );
    assert!(
        restarted
            .call("declare_app_privacy_no_data", &args())
            .await
            .is_ok()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    h.abort();
}
#[tokio::test]
async fn privacy_unknown_publish_blocks_second_write() {
    let (_d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.usages = vec![no_data()];
        st.hide_write = true;
    }
    assert!(
        r.call("publish_app_privacy_no_data", &args())
            .await
            .is_err()
    );
    assert!(
        r.call("publish_app_privacy_no_data", &args())
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    s.lock().unwrap().published = true;
    assert_eq!(
        r.call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap()["outcome_unknown"],
        false
    );
    h.abort();
}
#[tokio::test]
async fn privacy_definite_rejection_is_not_retried() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().reject = true;
    assert!(
        r.call("declare_app_privacy_no_data", &args())
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    h.abort();
}
#[tokio::test]
async fn privacy_lock_and_session_replacement_prevent_writes() {
    let (d, r, s, h) = setup().await;
    let lock = open_private_lock(d.path(), ".privacy-123.lock").unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    assert!(
        r.call("declare_app_privacy_no_data", &args())
            .await
            .is_err()
    );
    fs2::FileExt::unlock(&lock).unwrap();
    s.lock().unwrap().swap = Some(r.cookie_path.clone());
    assert!(
        r.call("declare_app_privacy_no_data", &args())
            .await
            .is_err()
    );
    assert!(s.lock().unwrap().writes.is_empty());
    h.abort();
}

#[tokio::test]
async fn privacy_ambiguous_5xx_never_retries_post_or_patch() {
    for publish in [false, true] {
        let (_d, r, s, h) = setup().await;
        {
            let mut st = s.lock().unwrap();
            st.server_error = true;
            if publish {
                st.usages = vec![no_data()];
            }
        }
        let name = if publish {
            "publish_app_privacy_no_data"
        } else {
            "declare_app_privacy_no_data"
        };
        for _ in 0..2 {
            assert!(
                r.call(name, &args())
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("outcome_unknown")
            );
        }
        assert_eq!(s.lock().unwrap().writes.len(), 1);
        h.abort();
    }
}
#[tokio::test]
async fn privacy_requires_confirmation_and_sanitizes_public_response() {
    let (_d, r, s, h) = setup().await;
    let mut invalid = args();
    invalid["confirmation"] = json!("YES");
    assert!(
        r.call("declare_app_privacy_no_data", &invalid)
            .await
            .unwrap_err()
            .to_string()
            .contains("confirmation_required")
    );
    invalid["app_id"] = json!("../123");
    assert!(
        r.call("get_app_privacy_details", &json!({"app_id":"../123"}))
            .await
            .is_err()
    );
    {
        let mut st = s.lock().unwrap();
        let mut u = no_data();
        u["attributes"] =
            json!({"secret":"secret-sentinel","lastPublishedBy":"person@example.invalid"});
        st.usages = vec![u];
    }
    let result = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap()
        .to_string();
    assert!(!result.contains("secret-sentinel"));
    assert!(!result.contains("person@example.invalid"));
    assert!(s.lock().unwrap().writes.is_empty());
    h.abort();
}

#[tokio::test]
async fn privacy_read_status_survives_source_change_without_clearing_unknown() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().hide_write = true;
    assert!(
        r.call("declare_app_privacy_no_data", &args())
            .await
            .unwrap_err()
            .to_string()
            .contains("outcome_unknown")
    );
    atomic_json(&r.cookie_path, &json!({"cookies":[{"name":"session","value":"other-account-sentinel","domain":"apple.com","path":"/"}]})).unwrap();
    s.lock().unwrap().usages = vec![no_data()];
    let result = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(result["data_not_collected"], true);
    assert_eq!(result["outcome_unknown"], true);
    assert_eq!(result["authentication_source_matches_intent"], false);
    assert_eq!(result["reconciliation"], Value::Null);
    assert!(
        r.call("declare_app_privacy_no_data", &args())
            .await
            .unwrap_err()
            .to_string()
            .contains("outcome_unknown")
    );
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    h.abort();
}
