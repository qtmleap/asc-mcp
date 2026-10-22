#![cfg(unix)]
mod release_common;
use asc_mcp::{client::ApiClient, release};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use release_common::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::net::TcpListener;

#[derive(Clone, Copy, PartialEq)]
enum Disruption {
    Never,
    ConfirmGet,
    Post,
    HoldPost,
}

struct Fixture {
    receipt: PathBuf,
    disruption: Disruption,
    gets: AtomicUsize,
    posts: AtomicUsize,
}

fn receipt_path(root: &Path) -> PathBuf {
    root.join(format!(
        "release-{}.json",
        hex::encode(Sha256::digest(b"v1"))
    ))
}

fn saved_path(receipt: &Path) -> PathBuf {
    receipt.with_extension("saved")
}

fn disrupt(receipt: &Path) {
    std::fs::rename(receipt, saved_path(receipt)).unwrap();
    std::fs::create_dir(receipt).unwrap();
}

fn restore(receipt: &Path) {
    std::fs::remove_dir(receipt).unwrap();
    std::fs::rename(saved_path(receipt), receipt).unwrap();
}

fn stored_receipt(receipt: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap()
}

async fn read_version(State(fixture): State<Arc<Fixture>>) -> Json<Value> {
    let seen = fixture.gets.fetch_add(1, Ordering::SeqCst) + 1;
    if fixture.disruption == Disruption::ConfirmGet && seen == 2 {
        disrupt(&fixture.receipt);
    }
    Json(ready("v1"))
}

async fn create_request(State(fixture): State<Arc<Fixture>>) -> (StatusCode, Json<Value>) {
    fixture.posts.fetch_add(1, Ordering::SeqCst);
    if fixture.disruption == Disruption::Post {
        disrupt(&fixture.receipt);
    }
    if fixture.disruption == Disruption::HoldPost {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    (StatusCode::CREATED, Json(created()))
}

async fn start(root: &Path, disruption: Disruption) -> (Arc<Fixture>, String) {
    let fixture = Arc::new(Fixture {
        receipt: receipt_path(root),
        disruption,
        gets: AtomicUsize::new(0),
        posts: AtomicUsize::new(0),
    });
    let router = Router::new()
        .route("/v1/appStoreVersions/{id}", get(read_version))
        .route("/v1/appStoreVersionReleaseRequests", post(create_request))
        .with_state(fixture.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await });
    (fixture, url)
}

#[tokio::test]
async fn attempt_save_failure_before_post_sends_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (fixture, url) = start(root.path(), Disruption::ConfirmGet).await;
    let client = ApiClient::with_endpoint(&url).unwrap();
    let token = prepare(&client, root.path()).await;
    let outcome = release::call(&client, root.path(), &confirm_args(&token)).await;
    assert!(outcome.is_err());
    assert_eq!(fixture.gets.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 0);
    restore(&fixture.receipt);
    let receipt = stored_receipt(&fixture.receipt);
    assert_eq!(receipt["attempted"], false);
    assert!(receipt["outcome"].is_null());
}

#[tokio::test]
async fn outcome_save_failure_after_post_stays_unknown_and_blocks_replay() {
    let root = tempfile::tempdir().unwrap();
    let (fixture, url) = start(root.path(), Disruption::Post).await;
    let client = ApiClient::with_endpoint(&url).unwrap();
    let token = prepare(&client, root.path()).await;
    let result = release::call(&client, root.path(), &confirm_args(&token))
        .await
        .unwrap();
    assert_eq!(result["status"], "outcome_unknown");
    assert_eq!(result["reason"], "outcome_not_persisted");
    assert_eq!(result["request_id"], "req1");
    assert_eq!(result["acceptance_observed"], true);
    assert_eq!(result["automatic_retry"], false);
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 1);
    restore(&fixture.receipt);
    let receipt = stored_receipt(&fixture.receipt);
    assert_eq!(receipt["attempted"], true);
    assert!(receipt["outcome"].is_null());
    let restarted = ApiClient::with_endpoint(&url).unwrap();
    let replay = release::call(&restarted, root.path(), &confirm_args(&token))
        .await
        .unwrap();
    assert_eq!(replay["status"], "release_attempt_recorded");
    assert_eq!(replay["last_outcome"]["status"], "outcome_unknown");
    let observation = release::call(&restarted, root.path(), &preview_args())
        .await
        .unwrap();
    assert_eq!(observation["status"], "release_attempt_recorded");
    assert!(observation.get("confirmation_token").is_none());
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore]
async fn confirm_child_process() {
    let Some(root) = std::env::var_os("ASC_RELEASE_CHILD_ROOT") else {
        return;
    };
    let url = std::env::var("ASC_RELEASE_CHILD_URL").unwrap();
    let token = std::env::var("ASC_RELEASE_CHILD_TOKEN").unwrap();
    let expected = std::env::var("ASC_RELEASE_CHILD_EXPECT").unwrap();
    let client = ApiClient::with_endpoint(&url).unwrap();
    let result = release::call(&client, Path::new(&root), &confirm_args(&token))
        .await
        .unwrap();
    assert_eq!(result["status"], expected.as_str());
}

fn child_command(root: &Path, url: &str, token: &str, expected: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "confirm_child_process", "--ignored"])
        .env("ASC_RELEASE_CHILD_ROOT", root)
        .env("ASC_RELEASE_CHILD_URL", url)
        .env("ASC_RELEASE_CHILD_TOKEN", token)
        .env("ASC_RELEASE_CHILD_EXPECT", expected)
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

async fn run_child(root: &Path, url: &str, token: &str, expected: &str) -> ExitStatus {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        child_command(root, url, token, expected).status(),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn real_process_restart_refuses_replay_after_a_completed_release() {
    let root = tempfile::tempdir().unwrap();
    let (fixture, url) = start(root.path(), Disruption::Never).await;
    let client = ApiClient::with_endpoint(&url).unwrap();
    let token = prepare(&client, root.path()).await;
    drop(client);
    let first = run_child(root.path(), &url, &token, "release_request_created").await;
    assert!(first.success());
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 1);
    let second = run_child(root.path(), &url, &token, "release_attempt_recorded").await;
    assert!(second.success());
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 1);
    assert_eq!(stored_receipt(&fixture.receipt)["attempted"], true);
}

#[tokio::test]
async fn process_termination_during_post_preserves_the_attempt_guard() {
    let root = tempfile::tempdir().unwrap();
    let (fixture, url) = start(root.path(), Disruption::HoldPost).await;
    let client = ApiClient::with_endpoint(&url).unwrap();
    let token = prepare(&client, root.path()).await;
    let mut sender = child_command(root.path(), &url, &token, "release_request_created")
        .spawn()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fixture.posts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stored_receipt(&fixture.receipt)["attempted"], true);
    assert!(stored_receipt(&fixture.receipt)["outcome"].is_null());
    sender.kill().await.unwrap();
    sender.wait().await.unwrap();
    assert!(
        run_child(root.path(), &url, &token, "release_attempt_recorded")
            .await
            .success()
    );
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 1);
    let observation = release::call(&client, root.path(), &preview_args())
        .await
        .unwrap();
    assert_eq!(observation["status"], "release_attempt_recorded");
    assert!(observation.get("confirmation_token").is_none());
}
