use super::*;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
    routing::any,
};
use std::sync::{Arc, Mutex};

struct Data {
    usages: Vec<Value>,
    published: bool,
    writes: Vec<(Method, String, Value)>,
    fail_at: Option<usize>,
    hide_at: Option<usize>,
    origin: String,
    paginate: bool,
    unsafe_next: bool,
    usage_reads: usize,
    change_at_read: Option<usize>,
    swap_after_write: Option<PathBuf>,
    publish_after_write: Option<usize>,
}
fn no_data() -> Value {
    json!({"type":"appDataUsages","id":"old1","relationships":{"app":{"data":{"type":"apps","id":"123"}},"dataProtection":{"data":{"type":"appDataUsageDataProtections","id":"DATA_NOT_COLLECTED"}}}})
}
async fn handler(
    State(state): State<Arc<Mutex<Data>>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> (StatusCode, String) {
    let mut s = state.lock().unwrap();
    let path = uri.path();
    if method == Method::GET
        && !s.writes.is_empty()
        && let Some(path) = s.swap_after_write.take()
    {
        atomic_json(&path,&json!({"cookies":[{"name":"session","value":"new-session-source","domain":"apple.com","path":"/"}]})).unwrap();
    }
    if method != Method::GET {
        s.writes.push((
            method.clone(),
            path.into(),
            serde_json::from_slice(&body).unwrap_or(Value::Null),
        ));
        if s.fail_at == Some(s.writes.len()) {
            return (StatusCode::INTERNAL_SERVER_ERROR, "{}".into());
        }
        if s.hide_at == Some(s.writes.len()) {
            return (StatusCode::NO_CONTENT, String::new());
        }
    }
    let result = match (method.clone(), path) {
        (Method::GET, "/iris/v1/apps/123") => json!({"data":{"type":"apps","id":"123"}}),
        (Method::GET, "/iris/v1/apps/123/dataUsages") => {
            s.usage_reads += 1;
            if s.change_at_read == Some(s.usage_reads) {
                let mut row = no_data();
                row["id"] = json!("external");
                s.usages.push(row);
            }
            if s.paginate && s.usages.len() > 3 {
                if uri.query().is_some_and(|q| q.contains("cursor=next")) {
                    json!({"data":s.usages[3..].to_vec()})
                } else {
                    json!({"data":s.usages[..3].to_vec(),"links":{"next":format!("{}/iris/v1/apps/123/dataUsages?cursor=next",if s.unsafe_next { "http://foreign.invalid" } else {&s.origin})}})
                }
            } else {
                json!({"data":s.usages})
            }
        }
        (Method::GET, "/iris/v1/apps/123/dataUsagePublishState") => {
            if s.publish_after_write == Some(s.writes.len()) {
                s.published = true;
            }
            json!({"data":{"type":"appDataUsagesPublishState","id":"pub1","attributes":{"published":s.published}}})
        }
        (Method::POST, "/iris/v1/appDataUsages") => {
            let mut item = s.writes.last().unwrap().2["data"].clone();
            item["id"] = json!(format!("new{}", s.writes.len()));
            s.usages.push(item.clone());
            s.published = false;
            json!({"data":item})
        }
        (Method::DELETE, p) if p.starts_with("/iris/v1/appDataUsages/") => {
            let id = p.rsplit('/').next().unwrap();
            s.usages.retain(|r| r["id"] != id);
            s.published = false;
            return (StatusCode::NO_CONTENT, String::new());
        }
        (Method::PATCH, "/iris/v1/appDataUsagesPublishState/pub1") => {
            s.published = true;
            return (StatusCode::NO_CONTENT, String::new());
        }
        _ => return (StatusCode::NOT_FOUND, "{}".into()),
    };
    (StatusCode::OK, result.to_string())
}
async fn setup() -> (
    tempfile::TempDir,
    Reviewer,
    Arc<Mutex<Data>>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(Mutex::new(Data {
        usages: vec![no_data()],
        published: true,
        writes: vec![],
        fail_at: None,
        hide_at: None,
        origin: String::new(),
        paginate: false,
        unsafe_next: false,
        usage_reads: 0,
        change_at_read: None,
        swap_after_write: None,
        publish_after_write: None,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    state.lock().unwrap().origin = origin.clone();
    let app = Router::new()
        .fallback(any(handler))
        .with_state(state.clone());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&origin, dir.path());
    (dir, reviewer, state, handle)
}
fn disclosures() -> Value {
    json!([
        {"category":"DEVICE_ID","purposes":["ANALYTICS","APP_FUNCTIONALITY"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"PRODUCT_INTERACTION","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"COARSE_LOCATION","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"CRASH_DATA","purposes":["APP_FUNCTIONALITY"],"linked_to_user":false,"used_for_tracking":false},
        {"category":"PERFORMANCE_DATA","purposes":["APP_FUNCTIONALITY"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"OTHER_DIAGNOSTIC_DATA","purposes":["ANALYTICS","APP_FUNCTIONALITY"],"linked_to_user":false,"used_for_tracking":false}
    ])
}
async fn args(r: &Reviewer, matrix: Value) -> Value {
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    json!({"app_id":"123","disclosures":matrix,"expected_snapshot_digest":read["snapshot_digest"],"confirmation":"REPLACE_APP_PRIVACY_DISCLOSURES","exclusive_editing_confirmed":true})
}
#[tokio::test]
async fn collection_replaces_no_data_with_eight_exact_rows_then_publishes() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    assert_eq!(saved["status"], "draft_verified");
    {
        let st = s.lock().unwrap();
        assert_eq!(st.writes.len(), 9);
        assert_eq!(st.writes[0].0, Method::DELETE);
        assert_eq!(st.writes[0].1, "/iris/v1/appDataUsages/old1");
        let mut tuples = st
            .writes
            .iter()
            .skip(1)
            .map(|w| {
                (
                    w.2["data"]["relationships"]["category"]["data"]["id"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                    w.2["data"]["relationships"]["purpose"]["data"]["id"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                    w.2["data"]["relationships"]["dataProtection"]["data"]["id"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        tuples.sort();
        assert_eq!(
            tuples,
            vec![
                (
                    "COARSE_LOCATION".into(),
                    "ANALYTICS".into(),
                    "DATA_LINKED_TO_YOU".into()
                ),
                (
                    "CRASH_DATA".into(),
                    "APP_FUNCTIONALITY".into(),
                    "DATA_NOT_LINKED_TO_YOU".into()
                ),
                (
                    "DEVICE_ID".into(),
                    "ANALYTICS".into(),
                    "DATA_LINKED_TO_YOU".into()
                ),
                (
                    "DEVICE_ID".into(),
                    "APP_FUNCTIONALITY".into(),
                    "DATA_LINKED_TO_YOU".into()
                ),
                (
                    "OTHER_DIAGNOSTIC_DATA".into(),
                    "ANALYTICS".into(),
                    "DATA_NOT_LINKED_TO_YOU".into()
                ),
                (
                    "OTHER_DIAGNOSTIC_DATA".into(),
                    "APP_FUNCTIONALITY".into(),
                    "DATA_NOT_LINKED_TO_YOU".into()
                ),
                (
                    "PERFORMANCE_DATA".into(),
                    "APP_FUNCTIONALITY".into(),
                    "DATA_LINKED_TO_YOU".into()
                ),
                (
                    "PRODUCT_INTERACTION".into(),
                    "ANALYTICS".into(),
                    "DATA_LINKED_TO_YOU".into()
                )
            ]
        );
    }
    let p = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":saved["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
    let published = r.call("publish_app_privacy_collection", &p).await.unwrap();
    assert_eq!(published["status"], "published");
    assert!(s.lock().unwrap().published);
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}
#[tokio::test]
async fn collection_rejects_stale_digest_missing_booleans_unknowns_and_duplicates_without_writes() {
    for mode in 0..6 {
        let (_d, r, s, h) = setup().await;
        let mut a = args(&r, disclosures()).await;
        match mode {
            0 => a["expected_snapshot_digest"] = json!("0".repeat(64)),
            1 => {
                a["disclosures"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("linked_to_user");
            }
            2 => a["disclosures"][0]["category"] = json!("UNKNOWN"),
            3 => a["disclosures"][0]["purposes"] = json!(["ANALYTICS", "ANALYTICS"]),
            4 => a["unexpected"] = json!(true),
            _ => a["confirmation"] = json!("DATA_NOT_COLLECTED"),
        }
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        assert!(s.lock().unwrap().writes.is_empty());
        h.abort();
    }
}
#[tokio::test]
async fn collection_partial_failure_blocks_replay_and_old_tools_after_restart() {
    let (d, r, s, h) = setup().await;
    s.lock().unwrap().fail_at = Some(3);
    let a = args(&r, disclosures()).await;
    assert!(
        r.call("set_app_privacy_collection", &a)
            .await
            .unwrap_err()
            .to_string()
            .contains("outcome_unknown")
    );
    let next = Reviewer::for_test(r.origin.as_str(), d.path());
    let read = next
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    assert!(
        next.call(
            "declare_app_privacy_no_data",
            &json!({"app_id":"123","confirmation":"DATA_NOT_COLLECTED"})
        )
        .await
        .is_err()
    );
    assert!(next.call("set_app_privacy_collection", &a).await.is_err());
    assert_eq!(s.lock().unwrap().writes.len(), 3);
    h.abort();
}

#[tokio::test]
async fn collection_repeat_preserves_rows_and_edit_removes_only_obsolete_tuples() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    let ids = s
        .lock()
        .unwrap()
        .usages
        .iter()
        .map(|r| r["id"].clone())
        .collect::<Vec<_>>();
    let repeat = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &repeat).await.unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 9);
    assert_eq!(
        s.lock()
            .unwrap()
            .usages
            .iter()
            .map(|r| r["id"].clone())
            .collect::<Vec<_>>(),
        ids
    );
    let mut changed = disclosures();
    changed[3]["linked_to_user"] = json!(true);
    let edit = args(&r, changed).await;
    r.call("set_app_privacy_collection", &edit).await.unwrap();
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 11);
    assert_eq!(st.writes[9].0, Method::DELETE);
    assert_eq!(st.writes[10].0, Method::POST);
    assert_eq!(
        st.usages.iter().filter(|r| ids.contains(&r["id"])).count(),
        7
    );
    h.abort();
}

#[tokio::test]
async fn collection_publish_requires_full_matching_matrix_and_all_attestations() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    let p = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":saved["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
    for field in [
        "answers_accurate",
        "complies_with_app_store_guidelines_and_law",
        "will_update_if_changed",
    ] {
        let mut bad = p.clone();
        bad[field] = json!(false);
        assert!(
            r.call("publish_app_privacy_collection", &bad)
                .await
                .is_err()
        );
    }
    let mut bad = p.clone();
    bad["disclosures"][0]["used_for_tracking"] = json!(true);
    assert!(
        r.call("publish_app_privacy_collection", &bad)
            .await
            .unwrap_err()
            .to_string()
            .contains("disclosures_mismatch")
    );
    let mut bad = p;
    bad["expected_snapshot_digest"] = json!("f".repeat(64));
    assert!(
        r.call("publish_app_privacy_collection", &bad)
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 9);
    assert!(!s.lock().unwrap().published);
    h.abort();
}

#[tokio::test]
async fn collection_tracking_requires_linkage_and_adds_separate_tracking_rows() {
    let (_d, r, s, h) = setup().await;
    let matrix = json!([{"category":"DEVICE_ID","purposes":["ANALYTICS","APP_FUNCTIONALITY"],"linked_to_user":false,"used_for_tracking":true}]);
    let a = args(&r, matrix).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    let st = s.lock().unwrap();
    assert_eq!(st.usages.len(), 4);
    assert_eq!(st.writes.len(), 5);
    assert_eq!(
        st.usages
            .iter()
            .filter(
                |u| u["relationships"]["dataProtection"]["data"]["id"] == "DATA_USED_TO_TRACK_YOU"
            )
            .count(),
        2
    );
    assert_eq!(saved["disclosures"][0]["linked_to_user"], false);
    assert_eq!(saved["disclosures"][0]["used_for_tracking"], true);
    h.abort();
}

#[tokio::test]
async fn collection_unconfirmed_delete_blocks_all_later_writes() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().hide_at = Some(1);
    let a = args(&r, disclosures()).await;
    assert!(
        r.call("set_app_privacy_collection", &a)
            .await
            .unwrap_err()
            .to_string()
            .contains("outcome_unknown")
    );
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    h.abort();
}

#[tokio::test]
async fn collection_foreign_owner_and_duplicate_existing_rows_fail_closed() {
    for mode in 0..2 {
        let (_d, r, s, h) = setup().await;
        if mode == 0 {
            s.lock().unwrap().usages[0]["relationships"]["app"]["data"]["id"] = json!("456");
            assert!(
                r.call("get_app_privacy_details", &json!({"app_id":"123"}))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("target_mismatch")
            );
        } else {
            let a = args(&r, disclosures()).await;
            r.call("set_app_privacy_collection", &a).await.unwrap();
            let duplicate = s.lock().unwrap().usages[0].clone();
            s.lock().unwrap().usages.push(duplicate);
            let a = args(&r, disclosures()).await;
            assert!(r.call("set_app_privacy_collection", &a).await.is_err());
            assert_eq!(s.lock().unwrap().writes.len(), 9);
        }
        h.abort();
    }
}

#[tokio::test]
async fn collection_reads_every_page_and_refuses_foreign_pagination_before_writes() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().paginate = true;
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["usage_count"], 8);
    assert_eq!(read["disclosures"].as_array().unwrap().len(), 6);
    let repeat = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &repeat).await.unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 9);
    s.lock().unwrap().unsafe_next = true;
    assert!(
        r.call("set_app_privacy_collection", &repeat)
            .await
            .unwrap_err()
            .to_string()
            .contains("unsafe_pagination")
    );
    assert_eq!(s.lock().unwrap().writes.len(), 9);
    h.abort();
}

#[tokio::test]
async fn collection_external_change_between_steps_stops_before_any_post() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    {
        let mut st = s.lock().unwrap();
        st.change_at_read = Some(st.usage_reads + 4);
    }
    assert!(
        r.call("set_app_privacy_collection", &a)
            .await
            .unwrap_err()
            .to_string()
            .contains("snapshot_changed")
    );
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    h.abort();
}

#[tokio::test]
async fn collection_replaced_authentication_after_delete_cannot_continue_or_reconcile() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    s.lock().unwrap().swap_after_write = Some(r.cookie_path.clone());
    assert!(
        r.call("set_app_privacy_collection", &a)
            .await
            .unwrap_err()
            .to_string()
            .contains("outcome_unknown")
    );
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    assert_eq!(read["authentication_source_matches_intent"], false);
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    h.abort();
}

#[tokio::test]
async fn collection_external_publication_during_draft_blocks_continuation_and_reconciliation() {
    for stop_at in [2, 9] {
        let (_d, r, s, h) = setup().await;
        let a = args(&r, disclosures()).await;
        s.lock().unwrap().publish_after_write = Some(stop_at);
        assert!(
            r.call("set_app_privacy_collection", &a)
                .await
                .unwrap_err()
                .to_string()
                .contains("outcome_unknown")
        );
        assert_eq!(s.lock().unwrap().writes.len(), stop_at);
        let read = r
            .call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap();
        assert_eq!(read["published"], true);
        assert_eq!(read["outcome_unknown"], true);
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        assert_eq!(s.lock().unwrap().writes.len(), stop_at);
        h.abort();
    }
}

#[tokio::test]
async fn collection_malformed_intent_action_and_desired_matrix_cannot_reconcile() {
    for mode in 0..6 {
        let (_d, r, s, h) = setup().await;
        let a = args(&r, disclosures()).await;
        s.lock().unwrap().fail_at = Some(2);
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        let path = r.root.join("privacy-123.json");
        let mut record: Value =
            serde_json::from_slice(&private_read(&path, MAX_BODY).unwrap()).unwrap();
        match mode {
            0 => record["desired"] = Value::Null,
            1 => record["action"] = json!("declare"),
            2 => record["desired"].as_array_mut().unwrap().swap(0, 1),
            3 => record["desired"][0]["category"] = json!("UNKNOWN_CATEGORY"),
            4 => record["desired"][0]["protection"] = json!("DATA_USED_TO_TRACK_YOU"),
            _ => {
                let item = record["desired"][0].clone();
                record["desired"].as_array_mut().unwrap().push(item);
            }
        }
        atomic_json(&path, &record).unwrap();
        assert!(
            r.call("get_app_privacy_details", &json!({"app_id":"123"}))
                .await
                .unwrap_err()
                .to_string()
                .contains("record_invalid")
        );
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        assert_eq!(s.lock().unwrap().writes.len(), 2);
        h.abort();
    }
}

#[tokio::test]
async fn collection_requires_explicit_exclusive_editing_confirmation_before_writes() {
    for publish in [false, true] {
        for missing in [false, true] {
            let (_d, r, s, h) = setup().await;
            let mut a = args(&r, disclosures()).await;
            let before = if publish {
                let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
                a = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":saved["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
                9
            } else {
                0
            };
            if missing {
                a.as_object_mut()
                    .unwrap()
                    .remove("exclusive_editing_confirmed");
            } else {
                a["exclusive_editing_confirmed"] = json!(false);
            }
            let name = if publish {
                "publish_app_privacy_collection"
            } else {
                "set_app_privacy_collection"
            };
            assert!(r.call(name, &a).await.is_err());
            assert_eq!(s.lock().unwrap().writes.len(), before);
            h.abort();
        }
    }
}

#[tokio::test]
async fn collection_single_delete_postreadback_checkpoint_survives_pending_restart() {
    let (d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().published = true;
    let mut subset = disclosures();
    subset.as_array_mut().unwrap().remove(3);
    let a = args(&r, subset).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    assert_eq!(s.lock().unwrap().writes[9].0, Method::DELETE);
    // Recreate the durable state at interruption before final confirmation. This
    // retains the checkpoint that must already exist after the only readback.
    let path = r.root.join("privacy-123.json");
    let mut pending: Value =
        serde_json::from_slice(&private_read(&path, MAX_BODY).unwrap()).unwrap();
    pending["state"] = json!("pending");
    atomic_json(&path, &pending).unwrap();
    s.lock().unwrap().published = true;
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let read = restarted
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    assert_eq!(read["reconciliation"], Value::Null);
    assert!(
        restarted
            .call("set_app_privacy_collection", &a)
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}
