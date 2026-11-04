use super::*;

async fn original_unknown(client: &DeveloperPortal, fixture: &StatefulFixture) -> Value {
    original_unknown_for(client, fixture, "com.example.app").await
}

async fn original_unknown_for(
    client: &DeveloperPortal,
    fixture: &StatefulFixture,
    bundle: &str,
) -> Value {
    let args = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":bundle});
    let prepared = client.call("associate_app_group", &args).await.unwrap();
    fixture.state.lock().unwrap().ignore_association = true;
    let original = client
        .call("associate_app_group", &execution(&prepared, args))
        .await
        .unwrap();
    fixture.state.lock().unwrap().ignore_association = false;
    assert_eq!(original["state"], "delivery_unknown");
    original
}

fn repair_args(original: &Value) -> Value {
    json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app","recovery_of":original["operation_id"]})
}

#[tokio::test]
async fn explicit_repair_adds_exact_set_once_and_preserves_original_block() {
    for empty_baseline in [false, true] {
        let fixture = StatefulFixture::start().await;
        {
            let mut state = fixture.state.lock().unwrap();
            state.created = true;
            state.empty_baseline = empty_baseline;
        }
        let root = tempfile::tempdir().unwrap();
        let client = fixture.client(root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let original = original_unknown(&client, &fixture).await;
        let args = repair_args(&original);
        let prepared = client
            .call("repair_app_group_association", &args)
            .await
            .unwrap();
        assert_eq!(prepared["recovery_of"], original["operation_id"]);
        assert_eq!(
            prepared["existing_group_ids"],
            if empty_baseline {
                json!([])
            } else {
                json!(["GROUPOLD1"])
            }
        );
        let request = execution(&prepared, args.clone());
        let repaired = client
            .call("repair_app_group_association", &request)
            .await
            .unwrap();
        assert_eq!(repaired["state"], "verified");
        assert_eq!(repaired["normal_writes_blocked"], true);
        assert_eq!(repaired["original_delivery_unknown"], true);
        assert!(
            client
                .call("repair_app_group_association", &request)
                .await
                .is_err()
        );
        assert!(
            client
                .call("repair_app_group_association", &args)
                .await
                .is_err()
        );
        let normal = json!({"team_id":"ABCDEFGHIJ","identifier":"group.example.new","bundle_id":"com.example.app"});
        assert!(client.call("associate_app_group", &normal).await.is_err());
        let status = client
            .call(
                "get_app_group_write_status",
                &json!({"operation_id":original["operation_id"]}),
            )
            .await
            .unwrap();
        assert_eq!(status["state"], "delivery_unknown");
        assert_eq!(status["observation"], "desired_state_present");
        assert_eq!(status["write_blocked"], true);
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 2);
    }
}

#[tokio::test]
async fn explicit_repair_unknown_child_is_queryable_and_blocks_future_attempts() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    let args = repair_args(&original);
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    fixture.state.lock().unwrap().ignore_association = true;
    let result = client
        .call(
            "repair_app_group_association",
            &execution(&prepared, args.clone()),
        )
        .await
        .unwrap();
    assert_eq!(result["state"], "delivery_unknown");
    let restarted = fixture.client(root.path());
    let status = restarted
        .call(
            "get_app_group_write_status",
            &json!({"operation_id":result["operation_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "delivery_unknown");
    assert_eq!(status["normal_writes_blocked"], true);
    assert!(
        restarted
            .call("repair_app_group_association", &args)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 2);
}

#[test]
fn legacy_plan_digest_serialization_stays_unchanged() {
    let legacy = r#"{"kind":"associate","team":"ABCDEFGHIJ","identifier":"group.example.new","name":null,"bundle_id":"com.example.app","app_id":"APPID123","group_id":"GROUPNEW1","generation":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","baseline":["GROUPOLD1"],"expected":["GROUPNEW1","GROUPOLD1"]}"#;
    let plan: groups::Plan = serde_json::from_str(legacy).unwrap();
    assert!(plan.valid());
    assert_eq!(serde_json::to_string(&plan).unwrap(), legacy);
    assert_eq!(
        state::digest(legacy.as_bytes()),
        "cb6ece7a72a257dfcf9c33ea738a0729f981987871f6a72da4f8a0b9a56fb37a"
    );
}

#[tokio::test]
async fn explicit_repair_final_baseline_read_observes_exact_completion_without_dispatch() {
    for exact in [true, false] {
        let fixture = StatefulFixture::start().await;
        fixture.state.lock().unwrap().created = true;
        let root = tempfile::tempdir().unwrap();
        let client = fixture.client(root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let original = original_unknown(&client, &fixture).await;
        let args = repair_args(&original);
        let prepared = client
            .call("repair_app_group_association", &args)
            .await
            .unwrap();
        {
            let mut state = fixture.state.lock().unwrap();
            let final_read = state.detail_count + 2;
            if exact {
                state.change_detail_on = Some(final_read);
            } else {
                state.extra_detail_on = Some(final_read);
            }
        }
        let result = client
            .call(
                "repair_app_group_association",
                &execution(&prepared, args.clone()),
            )
            .await;
        if exact {
            let result = result.expect("exact desired state at the final read must be a no-op");
            assert_eq!(result["state"], "no_op");
            assert_eq!(result["causation_verified"], false);
            assert_eq!(result["original_delivery_unknown"], true);
            assert_eq!(result["normal_writes_blocked"], true);
        } else {
            assert!(result.is_err());
        }
        assert!(
            client
                .call("repair_app_group_association", &args)
                .await
                .is_err()
        );
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
    }
}

#[tokio::test]
async fn explicit_repair_predispatch_failure_consumes_only_the_approved_confirmation() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    let args = repair_args(&original);
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    {
        let mut state = fixture.state.lock().unwrap();
        state.fail_detail_on = Some(state.detail_count + 1);
    }
    assert!(
        client
            .call(
                "repair_app_group_association",
                &execution(&prepared, args.clone())
            )
            .await
            .is_err()
    );
    assert!(
        client
            .call("repair_app_group_association", &args)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

#[tokio::test]
async fn explicit_repair_supports_separate_app_and_extension_empty_baselines() {
    for (bundle, app) in [
        ("com.example.app", "APPID123"),
        ("com.example.app.Extension", "APPID124"),
    ] {
        let fixture = StatefulFixture::start().await;
        {
            let mut state = fixture.state.lock().unwrap();
            state.created = true;
            state.empty_baseline = true;
            state.bundle_override = Some(bundle.into());
            state.app_override = Some(app.into());
        }
        let root = tempfile::tempdir().unwrap();
        let client = fixture.client(root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let original = original_unknown_for(&client, &fixture, bundle).await;
        let mut args = repair_args(&original);
        args["bundle_id"] = json!(bundle);
        let prepared = client
            .call("repair_app_group_association", &args)
            .await
            .unwrap();
        assert_eq!(prepared["existing_group_ids"], json!([]));
        assert_eq!(prepared["proposed_group_ids"], json!(["GROUPNEW1"]));
        assert_eq!(
            client
                .call("repair_app_group_association", &execution(&prepared, args))
                .await
                .unwrap()["state"],
            "verified"
        );
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 2);
    }
}

#[tokio::test]
async fn explicit_repair_interrupted_child_is_never_redispatched() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    edit_receipts(root.path(), |store| {
        store["records"][0]["state"] = json!("attempting");
        store["records"][0]["observation"] = json!("attempting");
    });
    let args = repair_args(&original);
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    edit_receipts(root.path(), |store| {
        store["records"][1]["state"] = json!("attempting");
        store["records"][1]["observation"] = json!("attempting");
    });
    let status = client
        .call(
            "get_app_group_write_status",
            &json!({"operation_id":prepared["operation_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "delivery_unknown");
    assert!(
        client
            .call(
                "repair_app_group_association",
                &execution(&prepared, args.clone())
            )
            .await
            .is_err()
    );
    assert!(
        client
            .call("repair_app_group_association", &args)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

fn edit_receipts(root: &std::path::Path, change: impl FnOnce(&mut Value)) {
    let filename = root.join("receipts.json");
    let mut store: Value = serde_json::from_slice(&fs::read(&filename).unwrap()).unwrap();
    change(&mut store);
    for record in store["records"].as_array_mut().unwrap() {
        let plan: groups::Plan = serde_json::from_value(record["plan"].clone()).unwrap();
        record["plan_digest"] = json!(state::digest(&serde_json::to_vec(&plan).unwrap()));
    }
    fs_safe::atomic_write(&filename, &serde_json::to_vec(&store).unwrap()).unwrap();
}

#[tokio::test]
async fn explicit_repair_exact_present_state_is_a_read_only_noop() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    fixture.state.lock().unwrap().associated = true;
    let result = client
        .call("repair_app_group_association", &repair_args(&original))
        .await
        .unwrap();
    assert_eq!(result["state"], "no_op");
    assert_eq!(result["causation_verified"], false);
    assert_eq!(result["original_delivery_unknown"], true);
    assert_eq!(result["normal_writes_blocked"], true);
    let records: Value =
        serde_json::from_slice(&fs::read(root.path().join("receipts.json")).unwrap()).unwrap();
    assert_eq!(records["records"].as_array().unwrap().len(), 1);
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

#[tokio::test]
async fn explicit_repair_refuses_superset_and_changed_baseline() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    let args = repair_args(&original);
    {
        let mut state = fixture.state.lock().unwrap();
        state.associated = true;
        state.extra_association = true;
    }
    assert_eq!(
        client
            .call("repair_app_group_association", &args)
            .await
            .unwrap_err()
            .to_string(),
        "developer_recovery_state_changed"
    );
    {
        let mut state = fixture.state.lock().unwrap();
        state.associated = false;
        state.extra_association = false;
    }
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    fixture.state.lock().unwrap().extra_association = true;
    assert!(
        client
            .call("repair_app_group_association", &execution(&prepared, args))
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

#[tokio::test]
async fn explicit_repair_late_original_completion_consumes_confirmation_without_dispatch() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    let args = repair_args(&original);
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    fixture.state.lock().unwrap().associated = true;
    let result = client
        .call(
            "repair_app_group_association",
            &execution(&prepared, args.clone()),
        )
        .await
        .unwrap();
    assert_eq!(result["state"], "no_op");
    assert_eq!(result["causation_verified"], false);
    assert!(
        client
            .call("repair_app_group_association", &args)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

#[tokio::test]
async fn explicit_repair_validates_references_and_keeps_normal_schemas_closed() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    for reference in [
        Value::Null,
        json!("not-a-uuid"),
        json!(uuid::Uuid::new_v4().to_string()),
    ] {
        let mut args = repair_args(&original);
        args["recovery_of"] = reference;
        assert!(
            client
                .call("repair_app_group_association", &args)
                .await
                .is_err()
        );
    }
    for (key, value) in [
        ("team_id", "ZYXWVUTSRQ"),
        ("bundle_id", "com.example.other"),
        ("identifier", "group.example.old"),
    ] {
        let mut args = repair_args(&original);
        args[key] = json!(value);
        assert!(
            client
                .call("repair_app_group_association", &args)
                .await
                .is_err()
        );
    }
    for reference in [Value::Null, original["operation_id"].clone()] {
        let mut args = repair_args(&original);
        args["recovery_of"] = reference;
        assert!(client.call("associate_app_group", &args).await.is_err());
    }
    let args = repair_args(&original);
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    assert!(
        client
            .call("repair_app_group_association", &args)
            .await
            .is_err()
    );
    let mut normal = execution(&prepared, args.clone());
    normal.as_object_mut().unwrap().remove("recovery_of");
    assert!(client.call("associate_app_group", &normal).await.is_err());
    let mut wrong_token = execution(&prepared, args.clone());
    wrong_token["confirmation_token"] = json!("f".repeat(64));
    assert!(
        client
            .call("repair_app_group_association", &wrong_token)
            .await
            .is_err()
    );
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    assert!(
        client
            .call("repair_app_group_association", &execution(&prepared, args))
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
}

#[tokio::test]
async fn explicit_repair_rejects_resource_replacement_and_other_unknown_conflicts() {
    for changed in ["app_id", "group_id", "same-key", "other-key"] {
        let fixture = StatefulFixture::start().await;
        fixture.state.lock().unwrap().created = true;
        let root = tempfile::tempdir().unwrap();
        let client = fixture.client(root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let original = original_unknown(&client, &fixture).await;
        edit_receipts(root.path(), |store| {
            if changed.ends_with("key") {
                let mut duplicate = store["records"][0].clone();
                duplicate["operation_id"] = json!(uuid::Uuid::new_v4().to_string());
                if changed == "other-key" {
                    duplicate["plan"]["bundle_id"] = json!("com.example.other");
                }
                store["records"].as_array_mut().unwrap().push(duplicate);
            } else {
                store["records"][0]["plan"][changed] = json!("REPLACED1");
                if changed == "group_id" {
                    store["records"][0]["plan"]["expected"] = json!(["GROUPOLD1", "REPLACED1"]);
                }
            }
        });
        let result = client
            .call("repair_app_group_association", &repair_args(&original))
            .await;
        assert_eq!(result.is_ok(), changed == "other-key", "{changed}");
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
    }
}

#[tokio::test]
async fn explicit_repair_rejects_self_dangling_and_mismatched_stored_relationships() {
    for corrupted in ["self", "dangling", "app_id", "baseline", "duplicate"] {
        let fixture = StatefulFixture::start().await;
        fixture.state.lock().unwrap().created = true;
        let root = tempfile::tempdir().unwrap();
        let client = fixture.client(root.path());
        client
            .publish_login("ABCDEFGHIJ", vec![seed()])
            .await
            .unwrap();
        let original = original_unknown(&client, &fixture).await;
        let prepared = client
            .call("repair_app_group_association", &repair_args(&original))
            .await
            .unwrap();
        edit_receipts(root.path(), |store| match corrupted {
            "self" => store["records"][1]["plan"]["recovery_of"] = prepared["operation_id"].clone(),
            "dangling" => {
                store["records"][1]["plan"]["recovery_of"] = json!(uuid::Uuid::new_v4().to_string())
            }
            "app_id" => store["records"][1]["plan"]["app_id"] = json!("REPLACED1"),
            "baseline" => store["records"][1]["plan"]["baseline"] = json!([]),
            _ => {
                let mut duplicate = store["records"][1].clone();
                duplicate["operation_id"] = json!(uuid::Uuid::new_v4().to_string());
                store["records"].as_array_mut().unwrap().push(duplicate);
            }
        });
        assert_eq!(
            client
                .call(
                    "get_app_group_write_status",
                    &json!({"operation_id":original["operation_id"]})
                )
                .await
                .unwrap_err()
                .to_string(),
            "developer_receipt_invalid"
        );
        assert_eq!(fixture.state.lock().unwrap().mutation_count, 1);
    }
}

#[tokio::test]
async fn explicit_repair_retains_terminal_children_and_blocks_chains() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    let args = repair_args(&original);
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    let result = client
        .call(
            "repair_app_group_association",
            &execution(&prepared, args.clone()),
        )
        .await
        .unwrap();
    edit_receipts(root.path(), |store| {
        let created = chrono::Utc::now().timestamp() - 40 * 86400;
        store["records"][1]["created_at"] = json!(created);
        store["records"][1]["expires_at"] = json!(created + 900);
    });
    let status = client
        .call(
            "get_app_group_write_status",
            &json!({"operation_id":result["operation_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "verified");
    assert_eq!(status["normal_writes_blocked"], true);
    assert!(
        client
            .call("repair_app_group_association", &repair_args(&result))
            .await
            .is_err()
    );
    fixture.state.lock().unwrap().associated = false;
    assert!(
        client
            .call("repair_app_group_association", &args)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 2);
}

#[tokio::test]
async fn explicit_repair_final_receipt_failure_stays_unknown_and_blocks_recovery() {
    let fixture = StatefulFixture::start().await;
    fixture.state.lock().unwrap().created = true;
    let root = tempfile::tempdir().unwrap();
    let client = fixture.client(root.path());
    client
        .publish_login("ABCDEFGHIJ", vec![seed()])
        .await
        .unwrap();
    let original = original_unknown(&client, &fixture).await;
    let args = repair_args(&original);
    let prepared = client
        .call("repair_app_group_association", &args)
        .await
        .unwrap();
    client
        .fail_receipt
        .store(2, std::sync::atomic::Ordering::SeqCst);
    assert!(
        client
            .call(
                "repair_app_group_association",
                &execution(&prepared, args.clone())
            )
            .await
            .is_err()
    );
    let status = client
        .call(
            "get_app_group_write_status",
            &json!({"operation_id":prepared["operation_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "delivery_unknown");
    assert_eq!(status["observation"], "desired_state_present");
    assert!(
        client
            .call("repair_app_group_association", &args)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().mutation_count, 2);
}
