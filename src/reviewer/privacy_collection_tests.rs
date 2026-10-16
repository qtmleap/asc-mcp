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
    last_published: Option<Value>,
    advance_publish_time: bool,
    keep_published_on_draft: bool,
    fail_after_apply_at: Option<usize>,
    fail_after_delete_at: Option<usize>,
    publish_response: Option<(StatusCode, String)>,
    fail_readback_after_patch: bool,
    publish_id: String,
    row_id_after_patch: Option<String>,
    publish_id_after_patch: Option<String>,
    timestamp_after_patch: Option<Value>,
    swap_after_patch: Option<PathBuf>,
    block_receipt_save: Option<PathBuf>,
    record_before_save_failure: Option<Vec<u8>>,
    // (write number, status, readback shape): the POST applies per shape, then replies
    // with that status and a non-JSON body. Shapes: exact, absent, wrong, extra.
    non_json: Option<(usize, StatusCode, &'static str)>,
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
        && s.fail_readback_after_patch
        && s.writes.last().is_some_and(|w| w.0 == Method::PATCH)
    {
        s.fail_readback_after_patch = false;
        return (StatusCode::INTERNAL_SERVER_ERROR, "{}".into());
    }
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
            let mut attrs = json!({"published":s.published});
            if let Some(value) = &s.last_published {
                attrs["lastPublished"] = value.clone();
            }
            json!({"data":{"type":"appDataUsagesPublishState","id":s.publish_id,"attributes":attrs}})
        }
        (Method::POST, "/iris/v1/appDataUsages") => {
            let mut item = s.writes.last().unwrap().2["data"].clone();
            item["id"] = json!(format!("new{}", s.writes.len()));
            s.usages.push(item.clone());
            if !s.keep_published_on_draft {
                s.published = false;
            }
            // Genuine uncertainty: committed, then HTTP 200 with a non-JSON body.
            if s.fail_after_apply_at == Some(s.writes.len()) {
                return (StatusCode::OK, "not-json-after-committing".into());
            }
            if let Some((_, status, shape)) = s.non_json.filter(|n| n.0 == s.writes.len()) {
                match shape {
                    "absent" => {
                        s.usages.pop();
                    }
                    "wrong" => {
                        let last = s.usages.last_mut().unwrap();
                        last["relationships"]["purpose"]["data"]["id"] = json!("OTHER_PURPOSES");
                    }
                    "extra" => {
                        let mut extra = item.clone();
                        extra["id"] = json!("extra-row");
                        extra["relationships"]["category"]["data"]["id"] = json!("HEALTH");
                        s.usages.push(extra);
                    }
                    _ => {}
                }
                return (status, "not-json-after-committing".into());
            }
            json!({"data":item})
        }
        (Method::DELETE, p) if p.starts_with("/iris/v1/appDataUsages/") => {
            let id = p.rsplit('/').next().unwrap();
            s.usages.retain(|r| r["id"] != id);
            if !s.keep_published_on_draft {
                s.published = false;
            }
            if s.fail_after_delete_at == Some(s.writes.len()) {
                return (StatusCode::INTERNAL_SERVER_ERROR, "{}".into());
            }
            return (StatusCode::NO_CONTENT, String::new());
        }
        (Method::PATCH, "/iris/v1/appDataUsagesPublishState/pub1") => {
            s.published = true;
            if s.advance_publish_time {
                s.last_published = Some(json!("2026-10-15T12:00:00Z"));
            }
            if let Some(id) = s.row_id_after_patch.take() {
                s.usages[0]["id"] = json!(id);
            }
            if let Some(id) = s.publish_id_after_patch.take() {
                s.publish_id = id;
            }
            if let Some(timestamp) = s.timestamp_after_patch.take() {
                s.last_published = Some(timestamp);
            }
            if let Some(path) = s.swap_after_patch.take() {
                atomic_json(&path,&json!({"cookies":[{"name":"session","value":"new-post-patch-source","domain":"apple.com","path":"/"}]})).unwrap();
            }
            if let Some(path) = s.block_receipt_save.take() {
                s.record_before_save_failure = Some(private_read(&path, MAX_BODY).unwrap());
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            if s.fail_after_apply_at == Some(s.writes.len()) {
                return (StatusCode::INTERNAL_SERVER_ERROR, "{}".into());
            }
            if let Some(response) = &s.publish_response {
                return response.clone();
            }
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
        last_published: Some(json!("2026-10-01T00:00:00Z")),
        advance_publish_time: true,
        keep_published_on_draft: false,
        fail_after_apply_at: None,
        fail_after_delete_at: None,
        publish_response: None,
        fail_readback_after_patch: false,
        publish_id: "pub1".into(),
        row_id_after_patch: None,
        publish_id_after_patch: None,
        timestamp_after_patch: None,
        swap_after_patch: None,
        block_receipt_save: None,
        record_before_save_failure: None,
        non_json: None,
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

async fn acknowledge_args(r: &Reviewer, original: &[u8]) -> Value {
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    json!({"app_id":"123","disclosures":disclosures(),"expected_intent_fingerprint":digest(original),"expected_snapshot_digest":read["snapshot_digest"],"confirmation":"ACKNOWLEDGE_CONFIRMED_APP_PRIVACY_PARTIAL","exclusive_editing_confirmed":true})
}
#[tokio::test]
async fn recovery_acknowledges_applied_prefix_without_remote_writes_and_preserves_archive() {
    let (_d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.keep_published_on_draft = true;
        st.fail_after_apply_at = Some(2);
    }
    let a = args(&r, disclosures()).await;
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let first_id = s.lock().unwrap().usages[0]["id"].clone();
    let ack = acknowledge_args(&r, &original).await;
    let result = r
        .call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    assert_eq!(result["status"], "partial_confirmed");
    assert_eq!(result["collection_complete"], false);
    assert_eq!(result["remote_writes"], 0);
    assert_eq!(s.lock().unwrap().writes.len(), 2);
    assert_eq!(
        private_read(
            &r.root
                .join(format!("privacy-123-partial-{}.json", digest(&original))),
            MAX_BODY
        )
        .unwrap(),
        original
    );
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 9);
    assert_eq!(st.usages.len(), 8);
    assert_eq!(
        st.usages
            .iter()
            .filter(|u| u["relationships"]["category"]["data"]["id"] == "COARSE_LOCATION")
            .count(),
        1
    );
    assert!(st.usages.iter().any(|u| u["id"] == first_id));
    h.abort();
}
#[tokio::test]
async fn publication_true_old_flag_requires_one_patch_and_a_new_publication_proof() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    let p = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":saved["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
    r.call("publish_app_privacy_collection", &p).await.unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    let mut repeat = p;
    repeat["expected_snapshot_digest"] = read["snapshot_digest"].clone();
    r.call("publish_app_privacy_collection", &repeat)
        .await
        .unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn recovery_archive_conflict_preserves_original_pending_bytes_and_remote_state() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().fail_after_apply_at = Some(2);
    let a = args(&r, disclosures()).await;
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let archive = r
        .root
        .join(format!("privacy-123-partial-{}.json", digest(&original)));
    write_private_atomic(&archive, b"conflicting archive").unwrap();
    let ack = acknowledge_args(&r, &original).await;
    assert!(
        r.call("acknowledge_app_privacy_partial", &ack)
            .await
            .unwrap_err()
            .to_string()
            .contains("archive_conflict")
    );
    assert_eq!(
        private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
        original
    );
    assert_eq!(s.lock().unwrap().writes.len(), 2);
    h.abort();
}

#[tokio::test]
async fn recovery_refuses_stale_hashes_changed_matrix_and_absent_uncertain_post() {
    for mode in 0..4 {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().fail_after_apply_at = Some(2);
        let a = args(&r, disclosures()).await;
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
        if mode == 3 {
            s.lock().unwrap().usages.clear();
        }
        let mut ack = acknowledge_args(&r, &original).await;
        match mode {
            0 => ack["expected_intent_fingerprint"] = json!("a".repeat(64)),
            1 => ack["expected_snapshot_digest"] = json!("b".repeat(64)),
            2 => ack["disclosures"][0]["linked_to_user"] = json!(false),
            _ => {}
        }
        assert!(
            r.call("acknowledge_app_privacy_partial", &ack)
                .await
                .is_err()
        );
        assert_eq!(
            private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
            original
        );
        assert_eq!(s.lock().unwrap().writes.len(), 2);
        h.abort();
    }
}

#[tokio::test]
async fn recovery_checkpoint_survives_restart_and_blocks_other_mutations_or_changed_state() {
    for mode in 0..3 {
        let (d, r, s, h) = setup().await;
        s.lock().unwrap().fail_after_apply_at = Some(2);
        let a = args(&r, disclosures()).await;
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
        let ack = acknowledge_args(&r, &original).await;
        r.call("acknowledge_app_privacy_partial", &ack)
            .await
            .unwrap();
        let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
        for name in ["declare_app_privacy_no_data", "publish_app_privacy_no_data"] {
            assert!(
                restarted
                    .call(
                        name,
                        &json!({"app_id":"123","confirmation":"DATA_NOT_COLLECTED"})
                    )
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("partial_collection")
            );
        }
        let mut matrix = disclosures();
        if mode == 0 {
            matrix[0]["linked_to_user"] = json!(false);
        }
        if mode == 1 {
            s.lock().unwrap().last_published = Some(json!("2026-10-02T00:00:00Z"));
        }
        if mode == 2 {
            atomic_json(&restarted.cookie_path,&json!({"cookies":[{"name":"session","value":"replaced-source","domain":"apple.com","path":"/"}]})).unwrap();
        }
        let a = args(&restarted, matrix).await;
        assert!(
            restarted
                .call("set_app_privacy_collection", &a)
                .await
                .is_err()
        );
        assert_eq!(s.lock().unwrap().writes.len(), 2);
        h.abort();
    }
}

#[tokio::test]
async fn recovery_repeated_applied_post_failure_archives_subset_without_duplicate_creation() {
    let (_d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.keep_published_on_draft = true;
        st.fail_after_apply_at = Some(2);
    }
    let a = args(&r, disclosures()).await;
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    let first = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let ack = acknowledge_args(&r, &first).await;
    r.call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    s.lock().unwrap().fail_after_apply_at = Some(3);
    let a = args(&r, disclosures()).await;
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["last_error"]["kind"], "non_json");
    assert_eq!(read["last_error"]["http_status"], 200);
    assert!(!read.to_string().contains("not-json-after-committing"));
    let second = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let ack = acknowledge_args(&r, &second).await;
    r.call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 9);
    assert_eq!(st.usages.len(), 8);
    let tuples = st
        .usages
        .iter()
        .map(|u| u["relationships"].to_string())
        .collect::<HashSet<_>>();
    assert_eq!(tuples.len(), 8);
    h.abort();
}

fn tracking_disclosures() -> Value {
    json!([
        {"category":"DEVICE_ID","purposes":["ANALYTICS","APP_FUNCTIONALITY"],"linked_to_user":true,"used_for_tracking":true},
        {"category":"PRODUCT_INTERACTION","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false}
    ])
}

#[tokio::test]
async fn recovery_tracking_prefix_resumes_after_restart_and_another_unknown_post() {
    let (d, r, s, h) = setup().await;
    s.lock().unwrap().fail_after_apply_at = Some(4);
    let a = args(&r, tracking_disclosures()).await;
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    assert_eq!(s.lock().unwrap().usages.len(), 3);
    let first = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let mut ack = acknowledge_args(&r, &first).await;
    ack["disclosures"] = tracking_disclosures();
    r.call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 4);

    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    s.lock().unwrap().fail_after_apply_at = Some(5);
    let a = args(&restarted, tracking_disclosures()).await;
    let error = restarted
        .call("set_app_privacy_collection", &a)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("outcome_unknown"), "{error}");
    assert_eq!(s.lock().unwrap().writes.len(), 5);
    let second = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let mut ack = acknowledge_args(&restarted, &second).await;
    ack["disclosures"] = tracking_disclosures();
    restarted
        .call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 5);
    for original in [&first, &second] {
        assert_eq!(
            private_read(
                &r.root
                    .join(format!("privacy-123-partial-{}.json", digest(original))),
                MAX_BODY
            )
            .unwrap(),
            *original
        );
    }
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let a = args(&restarted, tracking_disclosures()).await;
    restarted
        .call("set_app_privacy_collection", &a)
        .await
        .unwrap();
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 6);
    assert_eq!(st.usages.len(), 5);
    assert_eq!(
        st.usages
            .iter()
            .map(|u| u["relationships"].to_string())
            .collect::<HashSet<_>>()
            .len(),
        5
    );
    h.abort();
}

#[tokio::test]
async fn recovery_external_tracking_prefix_without_checkpoint_is_refused() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().fail_after_apply_at = Some(4);
    let a = args(&r, tracking_disclosures()).await;
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    // Identical external rows are not authority to continue an unrecorded batch.
    let external = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test(r.origin.as_str(), external.path());
    let a = args(&r, tracking_disclosures()).await;
    assert!(
        r.call("set_app_privacy_collection", &a)
            .await
            .unwrap_err()
            .to_string()
            .contains("collection_schema_invalid")
    );
    assert_eq!(s.lock().unwrap().writes.len(), 4);
    h.abort();
}

#[tokio::test]
async fn recovery_tracking_continuation_rejects_corrupt_or_foreign_archive_provenance() {
    for mode in 0..6 {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().fail_after_apply_at = Some(4);
        let a = args(&r, tracking_disclosures()).await;
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        let first = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
        let mut ack = acknowledge_args(&r, &first).await;
        ack["disclosures"] = tracking_disclosures();
        r.call("acknowledge_app_privacy_partial", &ack)
            .await
            .unwrap();
        s.lock().unwrap().fail_after_apply_at = Some(5);
        let a = args(&r, tracking_disclosures()).await;
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        let second = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
        let mut ack = acknowledge_args(&r, &second).await;
        ack["disclosures"] = tracking_disclosures();
        let mut current: Value = serde_json::from_slice(&second).unwrap();
        if mode == 0 {
            write_private_atomic(
                &r.root
                    .join(format!("privacy-123-partial-{}.json", digest(&first))),
                b"corrupt archive",
            )
            .unwrap();
        } else if mode == 5 {
            current["recovery_snapshot_digest"] = json!("a".repeat(64));
        } else {
            let mut ancestor: Value = serde_json::from_slice(&first).unwrap();
            match mode {
                1 => ancestor["app_id"] = json!("456"),
                2 => ancestor["generation"] = json!("foreign-source"),
                3 => ancestor["publish_state_id"] = json!("pub2"),
                4 => ancestor["desired"][0]["purpose"] = json!("OTHER_PURPOSES"),
                _ => unreachable!(),
            }
            let bytes = serde_json::to_vec(&ancestor).unwrap();
            let fingerprint = digest(&bytes);
            write_private_atomic(
                &r.root
                    .join(format!("privacy-123-partial-{fingerprint}.json")),
                &bytes,
            )
            .unwrap();
            current["archived_intent_fingerprint"] = json!(fingerprint);
        }
        let current = serde_json::to_vec(&current).unwrap();
        write_private_atomic(&r.root.join("privacy-123.json"), &current).unwrap();
        ack["expected_intent_fingerprint"] = json!(digest(&current));
        assert!(
            r.call("acknowledge_app_privacy_partial", &ack)
                .await
                .is_err()
        );
        assert_eq!(
            private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
            current
        );
        assert_eq!(s.lock().unwrap().writes.len(), 5);
        h.abort();
    }
}

#[tokio::test]
async fn recovery_archive_chain_bound_stops_before_any_more_remote_writes() {
    let (_d, r, s, h) = setup().await;
    let matrix = json!(["DEVICE_ID", "PRODUCT_INTERACTION", "OTHER_USAGE_DATA"])
        .as_array()
        .unwrap()
        .iter()
        .map(|category| json!({"category":category,"purposes":["THIRD_PARTY_ADVERTISING","DEVELOPERS_ADVERTISING","ANALYTICS","PRODUCT_PERSONALIZATION","APP_FUNCTIONALITY","OTHER_PURPOSES"],"linked_to_user":true,"used_for_tracking":true}))
        .collect::<Vec<_>>();
    let matrix = json!(matrix);
    for completed in 1..=33 {
        s.lock().unwrap().fail_after_apply_at = Some(completed + 1);
        let a = args(&r, matrix.clone()).await;
        assert!(r.call("set_app_privacy_collection", &a).await.is_err());
        assert_eq!(s.lock().unwrap().writes.len(), completed + 1);
        let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
        let mut ack = acknowledge_args(&r, &original).await;
        ack["disclosures"] = matrix.clone();
        r.call("acknowledge_app_privacy_partial", &ack)
            .await
            .unwrap();
    }
    let before = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let a = args(&r, matrix).await;
    assert!(
        r.call("set_app_privacy_collection", &a)
            .await
            .unwrap_err()
            .to_string()
            .contains("recovery_provenance_invalid")
    );
    assert_eq!(s.lock().unwrap().writes.len(), 34);
    assert_eq!(
        private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
        before
    );
    h.abort();
}

#[tokio::test]
async fn publication_unknown_patch_cannot_reconcile_unchanged_old_true_flag() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().fail_at = Some(10);
    let p = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":saved["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn publication_requires_valid_baseline_before_any_patch() {
    for mode in 0..3 {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().keep_published_on_draft = true;
        let a = args(&r, disclosures()).await;
        r.call("set_app_privacy_collection", &a).await.unwrap();
        {
            let mut st = s.lock().unwrap();
            match mode {
                0 => st.last_published = None,
                1 => st.last_published = Some(json!("secret-sentinel")),
                2 => st.last_published = Some(Value::Null),
                _ => st.advance_publish_time = false,
            }
        }
        let read = r
            .call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap();
        assert!(!read.to_string().contains("secret-sentinel"));
        let p = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":read["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
        assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
        assert_eq!(s.lock().unwrap().writes.len(), 9);
        h.abort();
    }
}

#[tokio::test]
async fn publication_explicit_null_unpublished_baseline_supports_first_publish() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().last_published = Some(Value::Null);
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    let p = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":read["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
    assert_eq!(
        r.call("publish_app_privacy_collection", &p).await.unwrap()["status"],
        "published"
    );
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn publication_applied_error_reconciles_only_advancing_timestamp_without_retry() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().fail_after_apply_at = Some(10);
    let mut p = json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":saved["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], false);
    assert_eq!(read["reconciliation"], "desired_state_confirmed");
    p["expected_snapshot_digest"] = read["snapshot_digest"].clone();
    r.call("publish_app_privacy_collection", &p).await.unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

async fn publication_args(r: &Reviewer) -> Value {
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    json!({"app_id":"123","disclosures":disclosures(),"expected_snapshot_digest":read["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true})
}

#[tokio::test]
async fn publication_accepted_unchanged_checkpoint_200_and_204_is_truthful_and_not_repeated() {
    for json_body in [false, true] {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().keep_published_on_draft = true;
        let a = args(&r, disclosures()).await;
        r.call("set_app_privacy_collection", &a).await.unwrap();
        {
            let mut st = s.lock().unwrap();
            st.advance_publish_time = false;
            if json_body {
                st.publish_response = Some((StatusCode::OK, json!({"data":{"type":"appDataUsagesPublishState","id":"pub1","attributes":{"published":true}}}).to_string()));
            }
        }
        let p = publication_args(&r).await;
        let result = r.call("publish_app_privacy_collection", &p).await.unwrap();
        assert_eq!(result["request_accepted"], true);
        assert_eq!(result["current_published"], true);
        assert_eq!(result["fresh_publication_verified"], false);
        assert_eq!(s.lock().unwrap().writes.len(), 10);
        let p = publication_args(&r).await;
        r.call("publish_app_privacy_collection", &p).await.unwrap();
        assert_eq!(s.lock().unwrap().writes.len(), 10);
        h.abort();
    }
}

#[tokio::test]
async fn publication_durable_receipt_reconciles_failed_readback_after_restart() {
    let (d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    {
        let mut st = s.lock().unwrap();
        st.advance_publish_time = false;
        st.fail_readback_after_patch = true;
    }
    let p = publication_args(&r).await;
    assert!(
        r.call("publish_app_privacy_collection", &p)
            .await
            .unwrap_err()
            .to_string()
            .contains("readback failed")
    );
    let intent: Value =
        serde_json::from_slice(&private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap())
            .unwrap();
    assert_eq!(intent["publication_protocol_version"], 1);
    assert_eq!(intent["publication_acceptance"]["http_status"], 204);
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let read = restarted
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], false);
    assert_eq!(read["request_accepted"], true);
    assert_eq!(read["fresh_publication_verified"], false);
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn publication_get_never_attributes_a_later_external_timestamp_to_an_accepted_nop() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().advance_publish_time = false;
    let p = publication_args(&r).await;
    r.call("publish_app_privacy_collection", &p).await.unwrap();
    s.lock().unwrap().last_published = Some(json!("2026-10-15T12:00:00Z"));
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["request_accepted"], true);
    assert_eq!(read["fresh_publication_verified"], false);
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

async fn legacy_goal_args(r: &Reviewer) -> (Vec<u8>, Value) {
    // This fixture represents the versionless pre-receipt pending format.
    let mut intent: Value =
        serde_json::from_slice(&private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap())
            .unwrap();
    for field in [
        "publication_protocol_version",
        "publication_acceptance",
        "publication_basis",
        "publication_goal_archive_fingerprint",
    ] {
        intent.as_object_mut().unwrap().remove(field);
    }
    intent["state"] = json!("pending");
    intent["publication_after"] = Value::Null;
    intent["confirmed_snapshot_digest"] = Value::Null;
    atomic_json(&r.root.join("privacy-123.json"), &intent).unwrap();
    let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    let a = json!({"app_id":"123","disclosures":disclosures(),"expected_intent_fingerprint":digest(&original),"expected_snapshot_digest":read["snapshot_digest"],"confirmation":"ACKNOWLEDGE_CURRENT_PUBLISHED_APP_PRIVACY_GOAL","exclusive_editing_confirmed":true,"current_published_goal_acknowledged":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true});
    (original, a)
}

#[tokio::test]
async fn publication_legacy_goal_acknowledgment_archives_without_an_apple_write() {
    let (d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().advance_publish_time = false;
    let p = publication_args(&r).await;
    let _ = r.call("publish_app_privacy_collection", &p).await;
    let (original, a) = legacy_goal_args(&r).await;
    let result = r
        .call("acknowledge_app_privacy_published_goal", &a)
        .await
        .unwrap();
    assert_eq!(result["request_accepted"], Value::Null);
    assert_eq!(result["current_published"], true);
    assert_eq!(result["fresh_publication_verified"], false);
    assert_eq!(result["remote_writes"], 0);
    assert_eq!(
        private_read(
            &r.root.join(format!(
                "privacy-123-published-goal-{}.json",
                digest(&original)
            )),
            MAX_BODY
        )
        .unwrap(),
        original
    );
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let read = restarted
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], false);
    assert_eq!(read["request_accepted"], Value::Null);
    assert_eq!(read["fresh_publication_verified"], false);
    let p = publication_args(&restarted).await;
    restarted
        .call("publish_app_privacy_collection", &p)
        .await
        .unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn publication_error_or_contradictory_response_never_creates_a_receipt() {
    for response in [
        (StatusCode::INTERNAL_SERVER_ERROR, "{}".into()),
        (StatusCode::OK, "not-json".into()),
        (
            StatusCode::OK,
            json!({"errors":[{"detail":"private-sentinel"}]}).to_string(),
        ),
        (
            StatusCode::OK,
            json!({"data":{"type":"otherType","id":"pub1"}}).to_string(),
        ),
        (
            StatusCode::OK,
            json!({"data":{"type":"appDataUsagesPublishState","id":"pub2"}}).to_string(),
        ),
        (
            StatusCode::OK,
            json!({"data":{"attributes":{"published":false}}}).to_string(),
        ),
    ] {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().keep_published_on_draft = true;
        let a = args(&r, disclosures()).await;
        r.call("set_app_privacy_collection", &a).await.unwrap();
        {
            let mut st = s.lock().unwrap();
            st.advance_publish_time = false;
            st.publish_response = Some(response);
        }
        let p = publication_args(&r).await;
        let error = r
            .call("publish_app_privacy_collection", &p)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-sentinel"));
        let read = r
            .call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap();
        assert_eq!(read["outcome_unknown"], true);
        assert_eq!(read["request_accepted"], Value::Null);
        assert_eq!(read["fresh_publication_verified"], false);
        assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
        assert_eq!(s.lock().unwrap().writes.len(), 10);
        h.abort();
    }
}

#[tokio::test]
async fn publication_applied_transport_failure_stays_pending_without_a_receipt_or_retry() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (d, original, s, h) = setup().await;
    h.abort();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    s.lock().unwrap().origin = origin.clone();
    let state = s.clone();
    let h = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let state = state.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let (header_end, length) = loop {
                    let mut chunk = [0u8; 1024];
                    let count = socket.read(&mut chunk).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break (end + 4, length);
                        }
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let mut line = headers.lines().next().unwrap().split_ascii_whitespace();
                let method: Method = line.next().unwrap().parse().unwrap();
                let uri: Uri = line.next().unwrap().parse().unwrap();
                let (status, body) = handler(
                    State(state),
                    method.clone(),
                    uri,
                    Bytes::copy_from_slice(&request[header_end..header_end + length]),
                )
                .await;
                // The PATCH reaches the mock service, then its connection drops
                // before a status or body can establish an acceptance receipt.
                if method == Method::PATCH {
                    return;
                }
                let response = format!(
                    "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status.as_u16(),
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    let r = Reviewer::for_test(&origin, d.path());
    drop(original);
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock().unwrap().advance_publish_time = false;
    let p = publication_args(&r).await;
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    assert_eq!(read["request_accepted"], Value::Null);
    assert_eq!(read["last_error"]["kind"], "transport");
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn publication_receipt_readback_requires_exact_rows_resource_source_and_timestamp() {
    for mode in 0..5 {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().keep_published_on_draft = true;
        let a = args(&r, disclosures()).await;
        r.call("set_app_privacy_collection", &a).await.unwrap();
        {
            let mut st = s.lock().unwrap();
            st.advance_publish_time = false;
            match mode {
                0 => st.row_id_after_patch = Some("changed-row".into()),
                1 => st.publish_id_after_patch = Some("pub2".into()),
                2 => st.timestamp_after_patch = Some(json!("2026-09-01T00:00:00Z")),
                3 => st.timestamp_after_patch = Some(Value::Null),
                4 => st.swap_after_patch = Some(r.cookie_path.clone()),
                _ => unreachable!(),
            }
        }
        let p = publication_args(&r).await;
        assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
        let read = r
            .call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap();
        assert_eq!(read["outcome_unknown"], true);
        assert_eq!(read["fresh_publication_verified"], false);
        assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
        assert_eq!(s.lock().unwrap().writes.len(), 10);
        h.abort();
    }
}

#[tokio::test]
async fn publication_false_to_true_never_accepts_a_regressing_timestamp() {
    for uncertain in [false, true] {
        let (_d, r, s, h) = setup().await;
        let a = args(&r, disclosures()).await;
        r.call("set_app_privacy_collection", &a).await.unwrap();
        {
            let mut st = s.lock().unwrap();
            st.advance_publish_time = false;
            st.timestamp_after_patch = Some(json!("2026-09-01T00:00:00Z"));
            if uncertain {
                st.fail_after_apply_at = Some(10);
            }
        }
        let p = publication_args(&r).await;
        assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
        let read = r
            .call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap();
        assert_eq!(read["outcome_unknown"], true);
        assert_eq!(read["fresh_publication_verified"], false);
        assert_eq!(s.lock().unwrap().writes.len(), 10);
        h.abort();
    }
}

#[tokio::test]
async fn publication_receipt_save_failure_keeps_versioned_pending_and_blocks_legacy_ack() {
    let (d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    let path = r.root.join("privacy-123.json");
    {
        let mut st = s.lock().unwrap();
        st.advance_publish_time = false;
        st.block_receipt_save = Some(path.clone());
    }
    let p = publication_args(&r).await;
    assert!(
        r.call("publish_app_privacy_collection", &p)
            .await
            .unwrap_err()
            .to_string()
            .contains("record_unwritable")
    );
    let original = s
        .lock()
        .unwrap()
        .record_before_save_failure
        .clone()
        .unwrap();
    let value: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(value["publication_protocol_version"], 1);
    assert_eq!(value["publication_acceptance"], Value::Null);
    fs::remove_dir(&path).unwrap();
    write_private_atomic(&path, &original).unwrap();
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let read = restarted
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    let mut ack = p.clone();
    ack["confirmation"] = json!("ACKNOWLEDGE_CURRENT_PUBLISHED_APP_PRIVACY_GOAL");
    ack["expected_intent_fingerprint"] = read["intent_fingerprint"].clone();
    ack["current_published_goal_acknowledged"] = json!(true);
    assert!(
        restarted
            .call("acknowledge_app_privacy_published_goal", &ack)
            .await
            .unwrap_err()
            .to_string()
            .contains("not_supported")
    );
    assert!(
        restarted
            .call("publish_app_privacy_collection", &p)
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn publication_legacy_ack_rejects_hash_version_archive_and_context_changes() {
    for mode in 0..11 {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().keep_published_on_draft = true;
        let a = args(&r, disclosures()).await;
        r.call("set_app_privacy_collection", &a).await.unwrap();
        s.lock().unwrap().advance_publish_time = false;
        let p = publication_args(&r).await;
        let _ = r.call("publish_app_privacy_collection", &p).await;
        let (original, mut ack) = legacy_goal_args(&r).await;
        match mode {
            0 => ack["expected_intent_fingerprint"] = json!("a".repeat(64)),
            1 => ack["expected_snapshot_digest"] = json!("b".repeat(64)),
            2 => ack["disclosures"][0]["linked_to_user"] = json!(false),
            3 => ack["current_published_goal_acknowledged"] = json!(false),
            4 => {
                let mut v: Value = serde_json::from_slice(&original).unwrap();
                v["publication_protocol_version"] = json!(1);
                atomic_json(&r.root.join("privacy-123.json"), &v).unwrap();
                ack["expected_intent_fingerprint"] = json!(digest(
                    &private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap()
                ));
            }
            5 => {
                let mut v: Value = serde_json::from_slice(&original).unwrap();
                v["last_error"] = json!({"kind":"non_json","http_status":200});
                atomic_json(&r.root.join("privacy-123.json"), &v).unwrap();
                ack["expected_intent_fingerprint"] = json!(digest(
                    &private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap()
                ));
            }
            6 => write_private_atomic(
                &r.root.join(format!(
                    "privacy-123-published-goal-{}.json",
                    digest(&original)
                )),
                b"conflicting archive",
            )
            .unwrap(),
            7 => s.lock().unwrap().usages[0]["id"] = json!("changed-row"),
            8 => s.lock().unwrap().publish_id = "pub2".into(),
            9 => s.lock().unwrap().last_published = Some(json!("2026-09-01T00:00:00Z")),
            10 => {
                atomic_json(&r.cookie_path,&json!({"cookies":[{"name":"session","value":"foreign-test-source","domain":"apple.com","path":"/"}]})).unwrap();
            }
            _ => unreachable!(),
        }
        if mode >= 7 {
            let read = r
                .call("get_app_privacy_details", &json!({"app_id":"123"}))
                .await
                .unwrap();
            ack["expected_snapshot_digest"] = read["snapshot_digest"].clone();
        }
        let before = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
        assert!(
            r.call("acknowledge_app_privacy_published_goal", &ack)
                .await
                .is_err()
        );
        assert_eq!(
            private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
            before
        );
        assert_eq!(s.lock().unwrap().writes.len(), 10);
        h.abort();
    }
}

// Observed legacy shape: category-wide DATA_USED_TO_TRACK_YOU rows carry no
// purpose relationship, beside ordinary purposeful linkage rows.
fn legacy_row(id: &str, category: &str, purpose: Option<&str>, protection: &str) -> Value {
    let mut row = json!({"type":"appDataUsages","id":id,"relationships":{
        "app":{"data":{"type":"apps","id":"123"}},
        "category":{"data":{"type":"appDataUsageCategories","id":category}},
        "dataProtection":{"data":{"type":"appDataUsageDataProtections","id":protection}}}});
    if let Some(purpose) = purpose {
        row["relationships"]["purpose"] =
            json!({"data":{"type":"appDataUsagePurposes","id":purpose}});
    }
    row
}
fn legacy_marker(id: &str, category: &str) -> Value {
    legacy_row(id, category, None, "DATA_USED_TO_TRACK_YOU")
}
fn legacy_rows() -> Vec<Value> {
    let linked = "DATA_LINKED_TO_YOU";
    vec![
        legacy_row("l1", "DEVICE_ID", Some("ANALYTICS"), linked),
        legacy_marker("t-device", "DEVICE_ID"),
        legacy_row("l2", "DEVICE_ID", Some("APP_FUNCTIONALITY"), linked),
        legacy_row("l3", "USER_ID", Some("APP_FUNCTIONALITY"), linked),
        legacy_marker("t-user", "USER_ID"),
        legacy_row("l4", "PRODUCT_INTERACTION", Some("ANALYTICS"), linked),
        legacy_marker("t-product", "PRODUCT_INTERACTION"),
        legacy_row("l5", "OTHER_USAGE_DATA", Some("ANALYTICS"), linked),
        legacy_marker("t-usage", "OTHER_USAGE_DATA"),
        legacy_row("l6", "CRASH_DATA", Some("APP_FUNCTIONALITY"), linked),
        legacy_marker("t-crash", "CRASH_DATA"),
        legacy_row("l7", "PERFORMANCE_DATA", Some("APP_FUNCTIONALITY"), linked),
        legacy_marker("t-perf", "PERFORMANCE_DATA"),
        legacy_row("l8", "COARSE_LOCATION", Some("ANALYTICS"), linked),
        legacy_row("l9", "OTHER_DIAGNOSTIC_DATA", Some("ANALYTICS"), linked),
        legacy_row(
            "l10",
            "OTHER_DIAGNOSTIC_DATA",
            Some("APP_FUNCTIONALITY"),
            linked,
        ),
        legacy_row("l11", "SEARCH_HISTORY", Some("ANALYTICS"), linked),
    ]
}
// Eight non-tracking categories. Against legacy_rows() this drops OTHER_USAGE_DATA
// and SEARCH_HISTORY, narrows OTHER_DIAGNOSTIC_DATA, flips CRASH_DATA to unlinked and
// adds NAME, so the obsolete/missing sets are not simply "the six markers".
fn legacy_desired() -> Value {
    json!([
        {"category":"DEVICE_ID","purposes":["ANALYTICS","APP_FUNCTIONALITY"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"USER_ID","purposes":["APP_FUNCTIONALITY"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"PRODUCT_INTERACTION","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"COARSE_LOCATION","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"CRASH_DATA","purposes":["APP_FUNCTIONALITY"],"linked_to_user":false,"used_for_tracking":false},
        {"category":"PERFORMANCE_DATA","purposes":["APP_FUNCTIONALITY"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"OTHER_DIAGNOSTIC_DATA","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false},
        {"category":"NAME","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false}
    ])
}
const LEGACY_REMOVED: [&str; 10] = [
    "l5",
    "l6",
    "l10",
    "l11",
    "t-device",
    "t-user",
    "t-product",
    "t-usage",
    "t-crash",
    "t-perf",
];
const LEGACY_RETAINED: [&str; 7] = ["l1", "l2", "l3", "l4", "l7", "l8", "l9"];
fn sorted_ids(ids: &[&str]) -> Vec<String> {
    let mut ids = ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    ids.sort();
    ids
}
fn row_ids(rows: &[Value]) -> Vec<String> {
    let mut ids = rows
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}
fn intent_json(r: &Reviewer) -> Value {
    serde_json::from_slice(&private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap())
        .unwrap()
}
async fn legacy_publish_args(r: &Reviewer, matrix: Value) -> Value {
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    json!({"app_id":"123","disclosures":matrix,"expected_snapshot_digest":read["snapshot_digest"],"confirmation":"PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE","exclusive_editing_confirmed":true,"answers_accurate":true,"complies_with_app_store_guidelines_and_law":true,"will_update_if_changed":true})
}

#[tokio::test]
async fn legacy_tracking_markers_are_deleted_by_replacement_without_inventing_purposes() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().usages = legacy_rows();
    assert_eq!(s.lock().unwrap().usages.len(), 17);
    let a = args(&r, legacy_desired()).await;
    let original_digest = a["expected_snapshot_digest"].clone();
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    assert_eq!(saved["status"], "draft_verified");
    {
        let st = s.lock().unwrap();
        assert_eq!(st.writes.len(), 12);
        let (deletes, posts) = st.writes.split_at(10);
        assert!(deletes.iter().all(|w| w.0 == Method::DELETE));
        let mut removed = deletes
            .iter()
            .map(|w| w.1.rsplit('/').next().unwrap().to_string())
            .collect::<Vec<_>>();
        removed.sort();
        assert_eq!(removed, sorted_ids(&LEGACY_REMOVED));
        assert!(posts.iter().all(|w| w.0 == Method::POST));
        // A fabricated purpose or null-purpose POST would show up here.
        let mut created = posts
            .iter()
            .map(|w| {
                let rel = &w.2["data"]["relationships"];
                (
                    rel["category"]["data"]["id"].as_str().unwrap().to_string(),
                    rel["purpose"]["data"]["id"].as_str().unwrap().to_string(),
                    rel["dataProtection"]["data"]["id"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        created.sort();
        assert_eq!(
            created,
            vec![
                (
                    "CRASH_DATA".into(),
                    "APP_FUNCTIONALITY".into(),
                    "DATA_NOT_LINKED_TO_YOU".into()
                ),
                (
                    "NAME".into(),
                    "ANALYTICS".into(),
                    "DATA_LINKED_TO_YOU".into()
                ),
            ]
        );
        let mut retained = st
            .usages
            .iter()
            .filter(|u| !u["id"].as_str().unwrap().starts_with("new"))
            .map(|u| u["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        retained.sort();
        assert_eq!(retained, sorted_ids(&LEGACY_RETAINED));
        assert_eq!(st.usages.len(), 9);
        for u in &st.usages {
            assert_ne!(
                u["relationships"]["dataProtection"]["data"]["id"],
                "DATA_USED_TO_TRACK_YOU"
            );
            assert!(u["relationships"]["category"]["data"]["id"].is_string());
            assert!(u["relationships"]["purpose"]["data"]["id"].is_string());
        }
    }
    let disclosures = saved["disclosures"].as_array().unwrap();
    assert_eq!(disclosures.len(), 8);
    assert!(disclosures.iter().all(|d| d["used_for_tracking"] == false));
    assert_ne!(saved["snapshot_digest"], original_digest);

    let intent = intent_json(&r);
    assert_eq!(intent["state"], "confirmed");
    assert_eq!(
        intent["previous_snapshot"]["snapshot_digest"],
        original_digest
    );
    let previous = intent["previous_snapshot"]["rows"].as_array().unwrap();
    assert_eq!(previous.len(), 17);
    assert_eq!(
        previous
            .iter()
            .filter(|row| row["purpose"].is_null())
            .count(),
        6
    );
    assert_eq!(row_ids(previous), row_ids(&legacy_rows()));
    let desired = intent["desired"].as_array().unwrap();
    assert_eq!(desired.len(), 9);
    assert!(
        desired
            .iter()
            .all(|t| { t["purpose"].is_string() && t["protection"] != "DATA_USED_TO_TRACK_YOU" })
    );
    h.abort();
}

#[tokio::test]
async fn legacy_explicit_null_purpose_and_matching_grouping_are_replaceable() {
    let (_d, r, s, h) = setup().await;
    let mut rows = legacy_rows();
    for row in &mut rows {
        let category = row["relationships"]["category"]["data"]["id"]
            .as_str()
            .unwrap();
        let grouping = match category {
            "DEVICE_ID" | "USER_ID" => "IDENTIFIERS",
            "CRASH_DATA" | "PERFORMANCE_DATA" | "OTHER_DIAGNOSTIC_DATA" => "DIAGNOSTICS",
            "COARSE_LOCATION" => "LOCATION",
            "SEARCH_HISTORY" => "SEARCH_HISTORY",
            _ => "USAGE_DATA",
        };
        row["relationships"]["grouping"] =
            json!({"data":{"type":"appDataUsageGroupings","id":grouping}});
        if row["relationships"].get("purpose").is_none() {
            row["relationships"]["purpose"] = json!({"data":null});
        }
    }
    s.lock().unwrap().usages = rows;
    let a = args(&r, legacy_desired()).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    assert_eq!(saved["status"], "draft_verified");
    assert_eq!(saved["disclosures"].as_array().unwrap().len(), 8);
    assert_eq!(s.lock().unwrap().writes.len(), 12);
    let intent = intent_json(&r);
    let previous = intent["previous_snapshot"]["rows"].as_array().unwrap();
    assert!(previous.iter().all(|row| row["grouping"].is_string()));
    assert_eq!(
        previous
            .iter()
            .filter(|row| row["purpose"].is_null())
            .count(),
        6
    );
    h.abort();
}

#[tokio::test]
async fn legacy_marker_is_removed_even_when_every_desired_row_is_already_present() {
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    let modern = row_ids(&s.lock().unwrap().usages);
    s.lock()
        .unwrap()
        .usages
        .push(legacy_marker("t-late", "DEVICE_ID"));
    let a = args(&r, disclosures()).await;
    let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
    assert_ne!(saved["already_present"], true);
    {
        let st = s.lock().unwrap();
        assert_eq!(st.writes.len(), 10);
        assert_eq!(st.writes[9].0, Method::DELETE);
        assert_eq!(st.writes[9].1, "/iris/v1/appDataUsages/t-late");
        assert_eq!(row_ids(&st.usages), modern);
    }
    let again = args(&r, disclosures()).await;
    let repeat = r.call("set_app_privacy_collection", &again).await.unwrap();
    assert_eq!(repeat["already_present"], true);
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();
}

#[tokio::test]
async fn legacy_malformed_orphan_duplicate_or_mixed_tracking_is_refused_without_writes() {
    let null_purpose = |protection: &str| {
        let mut row = legacy_row("x", "NAME", None, protection);
        row["relationships"]["purpose"] = json!({"data":null});
        row
    };
    let cases: Vec<(&str, Value)> = vec![
        (
            "absent purpose linked",
            legacy_row("x", "NAME", None, "DATA_LINKED_TO_YOU"),
        ),
        (
            "absent purpose unlinked",
            legacy_row("x", "NAME", None, "DATA_NOT_LINKED_TO_YOU"),
        ),
        ("null purpose linked", null_purpose("DATA_LINKED_TO_YOU")),
        (
            "null purpose unlinked",
            null_purpose("DATA_NOT_LINKED_TO_YOU"),
        ),
        (
            "orphan marker without linkage rows",
            legacy_marker("x", "FITNESS"),
        ),
        ("duplicate marker category", legacy_marker("x", "DEVICE_ID")),
        (
            "duplicate row id",
            legacy_row("l1", "NAME", Some("ANALYTICS"), "DATA_LINKED_TO_YOU"),
        ),
        (
            "marker mixed with purposeful tracking",
            legacy_row(
                "x",
                "DEVICE_ID",
                Some("ANALYTICS"),
                "DATA_USED_TO_TRACK_YOU",
            ),
        ),
        (
            "marker with unknown category",
            legacy_marker("x", "NOT_A_CATEGORY"),
        ),
        ("marker without category", {
            let mut row = legacy_marker("x", "NAME");
            row["relationships"]
                .as_object_mut()
                .unwrap()
                .remove("category");
            row
        }),
        ("marker with grouping", {
            let mut row = legacy_marker("x", "COARSE_LOCATION");
            row["relationships"]["grouping"] =
                json!({"data":{"type":"appDataUsageGroupings","id":"G"}});
            row
        }),
    ];
    for (name, extra) in cases {
        let (_d, r, s, h) = setup().await;
        let mut rows = legacy_rows();
        rows.push(extra);
        s.lock().unwrap().usages = rows.clone();
        let a = args(&r, legacy_desired()).await;
        assert!(
            r.call("set_app_privacy_collection", &a).await.is_err(),
            "{name}"
        );
        let st = s.lock().unwrap();
        assert!(st.writes.is_empty(), "{name}");
        assert_eq!(st.usages, rows, "{name}");
        assert!(!r.root.join("privacy-123.json").exists(), "{name}");
        h.abort();
    }
}

#[tokio::test]
async fn legacy_markers_never_allow_strict_publication() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().usages = legacy_rows();
    let p = legacy_publish_args(&r, legacy_desired()).await;
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    assert!(s.lock().unwrap().writes.is_empty());
    assert!(!r.root.join("privacy-123.json").exists());
    h.abort();

    // A complete modern matrix plus one stray marker must not publish either.
    let (_d, r, s, h) = setup().await;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    s.lock()
        .unwrap()
        .usages
        .push(legacy_marker("t-late", "DEVICE_ID"));
    let p = legacy_publish_args(&r, disclosures()).await;
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 9);
    assert!(st.writes.iter().all(|w| w.0 != Method::PATCH));
    assert!(!st.published);
    assert!(st.usages.iter().any(|u| u["id"] == "t-late"));
    h.abort();
}

#[tokio::test]
async fn legacy_delete_applied_then_failed_stays_pending_without_retry_or_partial_ack() {
    let (d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.usages = legacy_rows();
        st.fail_after_delete_at = Some(3);
    }
    let a = args(&r, legacy_desired()).await;
    let error = r.call("set_app_privacy_collection", &a).await.unwrap_err();
    assert!(error.to_string().contains("outcome_unknown"), "{error}");
    {
        let st = s.lock().unwrap();
        assert_eq!(st.writes.len(), 3);
        assert!(st.writes.iter().all(|w| w.0 == Method::DELETE));
        // The failed third DELETE was applied remotely before the error.
        assert_eq!(st.usages.len(), 14);
    }
    let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let pending = intent_json(&r);
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["step"], 3);

    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let read = restarted
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    let fresh = args(&restarted, legacy_desired()).await;
    assert!(
        restarted
            .call("set_app_privacy_collection", &fresh)
            .await
            .is_err()
    );
    assert!(r.call("set_app_privacy_collection", &a).await.is_err());
    assert!(
        restarted
            .call(
                "declare_app_privacy_no_data",
                &json!({"app_id":"123","confirmation":"DATA_NOT_COLLECTED"})
            )
            .await
            .is_err()
    );
    // Remaining markers keep this batch outside automatic partial recovery.
    let mut ack = acknowledge_args(&restarted, &original).await;
    ack["disclosures"] = legacy_desired();
    assert!(
        restarted
            .call("acknowledge_app_privacy_partial", &ack)
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 3);
    assert_eq!(s.lock().unwrap().usages.len(), 14);
    assert_eq!(
        private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
        original
    );
    assert!(
        !r.root
            .join(format!("privacy-123-partial-{}.json", digest(&original)))
            .exists()
    );
    h.abort();
}

// ---- Legacy replacement recovery: partial ACK after obsolete deletes + first POST ----

fn health_disclosure() -> Value {
    json!({"category":"HEALTH","purposes":["ANALYTICS"],"linked_to_user":true,"used_for_tracking":false})
}
fn legacy_desired_three_missing() -> Value {
    let mut matrix = legacy_desired().as_array().unwrap().clone();
    matrix.push(health_disclosure());
    json!(matrix)
}
fn rewrite_intent(r: &Reviewer, f: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v = intent_json(r);
    f(&mut v);
    let bytes = serde_json::to_vec(&v).unwrap();
    write_private_atomic(&r.root.join("privacy-123.json"), &bytes).unwrap();
    bytes
}
fn partial_archive_path(r: &Reviewer, bytes: &[u8]) -> PathBuf {
    r.root
        .join(format!("privacy-123-partial-{}.json", digest(bytes)))
}
// Ten obsolete DELETEs apply, then POST #1 (write 11) is applied remotely but answers
// HTTP 200 with a non-JSON body. The saved intent stays pending at step 11.
async fn legacy_partial(
    matrix: Value,
    total: usize,
) -> (
    tempfile::TempDir,
    Reviewer,
    Arc<Mutex<Data>>,
    tokio::task::JoinHandle<()>,
    Vec<u8>,
) {
    let (d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.usages = legacy_rows();
        st.fail_after_apply_at = Some(LEGACY_REMOVED.len() + 1);
    }
    let a = args(&r, matrix).await;
    let error = r.call("set_app_privacy_collection", &a).await.unwrap_err();
    assert!(error.to_string().contains("outcome_unknown"), "{error}");
    let pending = intent_json(&r);
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["step"], 11);
    assert_eq!(pending["total_steps"], total);
    assert_eq!(pending["last_error"]["kind"], "non_json");
    assert_eq!(pending["last_error"]["http_status"], 200);
    {
        let st = s.lock().unwrap();
        assert_eq!(st.writes.len(), 11);
        assert!(st.writes[..10].iter().all(|w| w.0 == Method::DELETE));
        assert_eq!(st.writes[10].0, Method::POST);
        assert_eq!(st.usages.len(), LEGACY_RETAINED.len() + 1);
    }
    let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    (d, r, s, h, original)
}
async fn legacy_ack(r: &Reviewer, original: &[u8], matrix: &Value) -> Value {
    let mut ack = acknowledge_args(r, original).await;
    ack["disclosures"] = matrix.clone();
    ack
}
fn created_category(write: &(Method, String, Value)) -> String {
    write.2["data"]["relationships"]["category"]["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}
fn assert_no_marker_and_unique_tuples(rows: &[Value]) {
    let mut tuples = HashSet::new();
    for u in rows {
        assert_ne!(
            u["relationships"]["dataProtection"]["data"]["id"],
            "DATA_USED_TO_TRACK_YOU"
        );
        assert!(u["relationships"]["purpose"]["data"]["id"].is_string());
        let t = (
            u["relationships"]["category"]["data"]["id"].to_string(),
            u["relationships"]["purpose"]["data"]["id"].to_string(),
            u["relationships"]["dataProtection"]["data"]["id"].to_string(),
        );
        assert!(tuples.insert(t));
    }
}

#[tokio::test]
async fn legacy_partial_ack_after_deletes_and_first_post_writes_nothing_then_posts_only_suffix() {
    let (d, r, s, h, original) = legacy_partial(legacy_desired(), 12).await;
    let (usages_before, published_before, stamp_before) = {
        let st = s.lock().unwrap();
        let mut removed = st.writes[..10]
            .iter()
            .map(|w| w.1.rsplit('/').next().unwrap().to_string())
            .collect::<Vec<_>>();
        removed.sort();
        assert_eq!(removed, sorted_ids(&LEGACY_REMOVED));
        assert_eq!(created_category(&st.writes[10]), "CRASH_DATA");
        (st.usages.clone(), st.published, st.last_published.clone())
    };
    let ack = legacy_ack(&r, &original, &legacy_desired()).await;
    let result = r
        .call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    assert_eq!(result["status"], "partial_confirmed");
    assert_eq!(result["collection_complete"], false);
    assert_eq!(result["remote_writes"], 0);
    assert_eq!(result["completed_steps"], 11);
    assert_eq!(result["total_steps"], 12);
    {
        let st = s.lock().unwrap();
        assert_eq!(st.writes.len(), 11);
        assert_eq!(st.usages, usages_before);
        assert_eq!(st.published, published_before);
        assert_eq!(st.last_published, stamp_before);
    }
    assert_eq!(
        private_read(&partial_archive_path(&r, &original), MAX_BODY).unwrap(),
        original
    );
    assert_eq!(intent_json(&r)["state"], "partial_confirmed");

    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let a = args(&restarted, legacy_desired()).await;
    let saved = restarted
        .call("set_app_privacy_collection", &a)
        .await
        .unwrap();
    assert_eq!(saved["status"], "draft_verified");
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 12);
    assert!(st.writes[11..].iter().all(|w| w.0 == Method::POST));
    assert_eq!(created_category(&st.writes[11]), "NAME");
    assert_eq!(st.usages.len(), 9);
    let mut retained = st
        .usages
        .iter()
        .filter(|u| !u["id"].as_str().unwrap().starts_with("new"))
        .map(|u| u["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    retained.sort();
    assert_eq!(retained, sorted_ids(&LEGACY_RETAINED));
    assert_no_marker_and_unique_tuples(&st.usages);
    assert_eq!(intent_json(&restarted)["state"], "confirmed");
    h.abort();
}

#[tokio::test]
async fn legacy_partial_ack_accepts_historical_http201_non_json_last_error() {
    let (d, r, s, h, saved) = legacy_partial(legacy_desired(), 12).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&saved).unwrap()["last_error"]["http_status"],
        200
    );
    // Test-only state: older builds recorded this uncertainty as HTTP 201.
    let original = rewrite_intent(&r, |v| {
        v["last_error"] = json!({"kind":"non_json","http_status":201});
    });
    assert_ne!(original, saved);
    let ack = legacy_ack(&r, &original, &legacy_desired()).await;
    let result = r
        .call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    assert_eq!(result["status"], "partial_confirmed");
    assert_eq!(result["remote_writes"], 0);
    assert_eq!(s.lock().unwrap().writes.len(), 11);
    assert_eq!(
        private_read(&partial_archive_path(&r, &original), MAX_BODY).unwrap(),
        original
    );
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let a = args(&restarted, legacy_desired()).await;
    restarted
        .call("set_app_privacy_collection", &a)
        .await
        .unwrap();
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 12);
    assert_eq!(created_category(&st.writes[11]), "NAME");
    assert_eq!(st.usages.len(), 9);
    h.abort();
}

#[tokio::test]
async fn legacy_partial_ack_chain_survives_a_second_uncertain_post_with_recursive_provenance() {
    let matrix = legacy_desired_three_missing();
    let (d, r, s, h, first) = legacy_partial(matrix.clone(), 13).await;
    let ack = legacy_ack(&r, &first, &matrix).await;
    r.call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    assert_eq!(s.lock().unwrap().writes.len(), 11);

    // CRASH_DATA, HEALTH, NAME is the missing order; write 12 is the HEALTH POST.
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    s.lock().unwrap().fail_after_apply_at = Some(12);
    let a = args(&restarted, matrix.clone()).await;
    assert!(
        restarted
            .call("set_app_privacy_collection", &a)
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 12);
    let second = private_read(&restarted.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let pending = intent_json(&restarted);
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["step"], 1);
    assert_eq!(pending["total_steps"], 2);
    assert_eq!(
        pending["archived_intent_fingerprint"],
        json!(digest(&first))
    );
    let ack = legacy_ack(&restarted, &second, &matrix).await;
    let result = restarted
        .call("acknowledge_app_privacy_partial", &ack)
        .await
        .unwrap();
    assert_eq!(result["remote_writes"], 0);
    assert_eq!(s.lock().unwrap().writes.len(), 12);
    assert_eq!(
        private_read(&partial_archive_path(&restarted, &second), MAX_BODY).unwrap(),
        second
    );
    assert_eq!(
        private_read(&partial_archive_path(&restarted, &first), MAX_BODY).unwrap(),
        first
    );

    let again = Reviewer::for_test(r.origin.as_str(), d.path());
    let a = args(&again, matrix).await;
    again.call("set_app_privacy_collection", &a).await.unwrap();
    let st = s.lock().unwrap();
    assert_eq!(st.writes.len(), 13);
    assert_eq!(st.writes[12].0, Method::POST);
    assert_eq!(created_category(&st.writes[12]), "NAME");
    assert!(st.writes[10..].iter().all(|w| w.0 == Method::POST));
    assert_eq!(st.usages.len(), 10);
    assert_no_marker_and_unique_tuples(&st.usages);
    h.abort();
}

#[tokio::test]
async fn legacy_partial_ack_stays_refused_for_every_unproven_prefix_or_guard_violation() {
    let modes = [
        "changed retained grouping",
        "reappeared obsolete id",
        "obsolete id reused for desired prefix",
        "reappeared obsolete marker",
        "retained row deleted",
        "uncertain post absent",
        "extra unique row",
        "wrong prefix tuple",
        "duplicated prefix tuple",
        "stale intent fingerprint",
        "stale snapshot digest",
        "changed matrix",
        "replaced authentication source",
        "publication changed",
        "tampered previous snapshot digest",
        "step at obsolete count",
        "step at total",
    ];
    for mode in modes {
        let (_d, r, s, h, original) = legacy_partial(legacy_desired(), 12).await;
        {
            let mut st = s.lock().unwrap();
            match mode {
                "changed retained grouping" => {
                    let row = st.usages.iter_mut().find(|u| u["id"] == "l1").unwrap();
                    row["relationships"]["grouping"] =
                        json!({"data":{"type":"appDataUsageGroupings","id":"IDENTIFIERS"}});
                }
                "reappeared obsolete id" => {
                    let row = legacy_rows().into_iter().find(|u| u["id"] == "l5").unwrap();
                    st.usages.push(row);
                }
                "reappeared obsolete marker" => {
                    st.usages.push(legacy_marker("t-device", "DEVICE_ID"))
                }
                "obsolete id reused for desired prefix" => {
                    let row = st.usages.iter_mut().find(|u| u["id"] == "new11").unwrap();
                    row["id"] = json!("l5");
                }
                "retained row deleted" => st.usages.retain(|u| u["id"] != "l1"),
                "uncertain post absent" => st.usages.retain(|u| u["id"] != "new11"),
                "extra unique row" => st.usages.push(legacy_row(
                    "zz",
                    "HEALTH",
                    Some("ANALYTICS"),
                    "DATA_LINKED_TO_YOU",
                )),
                "wrong prefix tuple" => {
                    let row = st.usages.iter_mut().find(|u| u["id"] == "new11").unwrap();
                    row["relationships"]["category"]["data"]["id"] = json!("NAME");
                    row["relationships"]["purpose"]["data"]["id"] = json!("ANALYTICS");
                    row["relationships"]["dataProtection"]["data"]["id"] =
                        json!("DATA_LINKED_TO_YOU");
                }
                "duplicated prefix tuple" => {
                    let mut row = st
                        .usages
                        .iter()
                        .find(|u| u["id"] == "new11")
                        .unwrap()
                        .clone();
                    row["id"] = json!("dup-of-prefix");
                    st.usages.push(row);
                }
                "publication changed" => st.published = true,
                _ => {}
            }
        }
        if mode == "replaced authentication source" {
            atomic_json(&r.cookie_path,&json!({"cookies":[{"name":"session","value":"replaced-source","domain":"apple.com","path":"/"}]})).unwrap();
        }
        let current = match mode {
            "tampered previous snapshot digest" => rewrite_intent(&r, |v| {
                v["previous_snapshot"]["snapshot_digest"] = json!("c".repeat(64));
            }),
            "step at obsolete count" => rewrite_intent(&r, |v| v["step"] = json!(10)),
            "step at total" => rewrite_intent(&r, |v| v["step"] = json!(12)),
            _ => original.clone(),
        };
        let mut ack = legacy_ack(&r, &current, &legacy_desired()).await;
        match mode {
            "stale intent fingerprint" => {
                ack["expected_intent_fingerprint"] = json!("a".repeat(64))
            }
            "stale snapshot digest" => ack["expected_snapshot_digest"] = json!("b".repeat(64)),
            "changed matrix" => ack["disclosures"][0]["linked_to_user"] = json!(false),
            _ => {}
        }
        assert!(
            r.call("acknowledge_app_privacy_partial", &ack)
                .await
                .is_err(),
            "{mode}"
        );
        assert_eq!(s.lock().unwrap().writes.len(), 11, "{mode}");
        assert_eq!(
            private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
            current,
            "{mode}"
        );
        assert!(!partial_archive_path(&r, &current).exists(), "{mode}");
        h.abort();
    }
}

#[tokio::test]
async fn legacy_partial_ack_refuses_uncertain_last_delete_before_any_post() {
    let (d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.usages = legacy_rows();
        st.fail_after_delete_at = Some(10);
    }
    let a = args(&r, legacy_desired()).await;
    let error = r.call("set_app_privacy_collection", &a).await.unwrap_err();
    assert!(error.to_string().contains("outcome_unknown"), "{error}");
    let pending = intent_json(&r);
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["step"], 10);
    assert_eq!(pending["total_steps"], 12);
    {
        let st = s.lock().unwrap();
        assert_eq!(st.writes.len(), 10);
        // Every obsolete row is already gone, but no POST has been attempted.
        assert_eq!(st.usages.len(), LEGACY_RETAINED.len());
    }
    let original = private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap();
    let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
    let ack = legacy_ack(&restarted, &original, &legacy_desired()).await;
    assert!(
        restarted
            .call("acknowledge_app_privacy_partial", &ack)
            .await
            .is_err()
    );
    let fresh = args(&restarted, legacy_desired()).await;
    assert!(
        restarted
            .call("set_app_privacy_collection", &fresh)
            .await
            .is_err()
    );
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    assert_eq!(
        private_read(&r.root.join("privacy-123.json"), MAX_BODY).unwrap(),
        original
    );
    assert!(!partial_archive_path(&r, &original).exists());
    h.abort();
}

// ---- HTTP 201 non-JSON POST: success only with an exact GET delta ----

#[tokio::test]
async fn transport_201_non_json_post_succeeds_only_with_exact_readback_delta() {
    for write in [2usize, 5, 9] {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().non_json = Some((write, StatusCode::CREATED, "exact"));
        let a = args(&r, disclosures()).await;
        let saved = r.call("set_app_privacy_collection", &a).await.unwrap();
        assert_eq!(saved["status"], "draft_verified", "{write}");
        {
            let st = s.lock().unwrap();
            // One DELETE plus exactly one POST per tuple: the 201 POST was not retried.
            assert_eq!(st.writes.len(), 9, "{write}");
            assert_eq!(st.writes.iter().filter(|w| w.0 == Method::POST).count(), 8);
            assert_eq!(st.usages.len(), 8, "{write}");
            assert_no_marker_and_unique_tuples(&st.usages);
        }
        assert_eq!(intent_json(&r)["state"], "confirmed", "{write}");
        let again = args(&r, disclosures()).await;
        let repeat = r.call("set_app_privacy_collection", &again).await.unwrap();
        assert_eq!(repeat["already_present"], true);
        assert_eq!(s.lock().unwrap().writes.len(), 9);
        h.abort();
    }
}

#[tokio::test]
async fn transport_201_non_json_post_with_absent_wrong_or_extra_readback_stays_unknown() {
    for shape in ["absent", "wrong", "extra"] {
        let (d, r, s, h) = setup().await;
        s.lock().unwrap().non_json = Some((2, StatusCode::CREATED, shape));
        let a = args(&r, disclosures()).await;
        let error = r.call("set_app_privacy_collection", &a).await.unwrap_err();
        assert!(
            error.to_string().contains("outcome_unknown"),
            "{shape}: {error}"
        );
        let pending = intent_json(&r);
        assert_eq!(pending["state"], "pending", "{shape}");
        // Transport acceptance is not completion: exact readback, rather than
        // JSON parsing, is what must now stop this pending write.
        assert!(
            error.to_string().contains("exact step outcome"),
            "{shape}: {error}"
        );
        assert_eq!(pending["last_error"], Value::Null, "{shape}");
        assert_eq!(pending["step"], 2, "{shape}");
        assert_eq!(s.lock().unwrap().writes.len(), 2, "{shape}");
        let restarted = Reviewer::for_test(r.origin.as_str(), d.path());
        let read = restarted
            .call("get_app_privacy_details", &json!({"app_id":"123"}))
            .await
            .unwrap();
        assert_eq!(read["outcome_unknown"], true, "{shape}");
        let fresh = args(&restarted, disclosures()).await;
        assert!(
            restarted
                .call("set_app_privacy_collection", &fresh)
                .await
                .is_err(),
            "{shape}"
        );
        assert_eq!(s.lock().unwrap().writes.len(), 2, "{shape}");
        h.abort();
    }
}

#[tokio::test]
async fn non_201_non_json_or_failed_post_statuses_remain_uncertain_even_with_exact_readback() {
    for status in [
        StatusCode::OK,
        StatusCode::ACCEPTED,
        StatusCode::NON_AUTHORITATIVE_INFORMATION,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let (_d, r, s, h) = setup().await;
        s.lock().unwrap().non_json = Some((2, status, "exact"));
        let a = args(&r, disclosures()).await;
        let error = r.call("set_app_privacy_collection", &a).await.unwrap_err();
        assert!(
            error.to_string().contains("outcome_unknown"),
            "{status}: {error}"
        );
        let pending = intent_json(&r);
        assert_eq!(pending["state"], "pending", "{status}");
        assert_eq!(
            pending["last_error"]["http_status"],
            status.as_u16(),
            "{status}"
        );
        assert_eq!(s.lock().unwrap().writes.len(), 2, "{status}");
        h.abort();
    }
}

// ---- Scope: the 201 tolerance covers collection POSTs only ----

#[tokio::test]
async fn transport_201_tolerance_does_not_cover_publication_patch_or_no_data_declaration() {
    let (_d, r, s, h) = setup().await;
    s.lock().unwrap().keep_published_on_draft = true;
    let a = args(&r, disclosures()).await;
    r.call("set_app_privacy_collection", &a).await.unwrap();
    {
        let mut st = s.lock().unwrap();
        st.advance_publish_time = false;
        st.publish_response = Some((StatusCode::CREATED, "not-json".into()));
    }
    let p = publication_args(&r).await;
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    let read = r
        .call("get_app_privacy_details", &json!({"app_id":"123"}))
        .await
        .unwrap();
    assert_eq!(read["outcome_unknown"], true);
    assert_eq!(read["request_accepted"], Value::Null);
    assert_eq!(read["fresh_publication_verified"], false);
    assert!(r.call("publish_app_privacy_collection", &p).await.is_err());
    assert_eq!(s.lock().unwrap().writes.len(), 10);
    h.abort();

    let (_d, r, s, h) = setup().await;
    {
        let mut st = s.lock().unwrap();
        st.usages.clear();
        st.non_json = Some((1, StatusCode::CREATED, "exact"));
    }
    let error = r
        .call(
            "declare_app_privacy_no_data",
            &json!({"app_id":"123","confirmation":"DATA_NOT_COLLECTED"}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("outcome_unknown"), "{error}");
    assert_eq!(intent_json(&r)["state"], "pending");
    assert_eq!(s.lock().unwrap().writes.len(), 1);
    h.abort();
}
