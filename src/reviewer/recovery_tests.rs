use super::*;
use httpmock::prelude::{GET, MockServer, POST};

fn renewed_session(path: &Path) {
    atomic_json(path, &json!({"cookies":[{"name":"session","value":"renewed-fixture","domain":"apple.com","path":"/"}]})).unwrap();
}

#[tokio::test]
async fn expired_get_waits_for_host_and_replays_once() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let mut reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    reviewer.recovery_wait = Duration::from_millis(500);
    let expired = server.mock(|when, then| {
        when.method(GET)
            .path("/review")
            .header("cookie", "session=secret-sentinel");
        then.status(401);
    });
    let ready = server.mock(|when, then| {
        when.method(GET)
            .path("/review")
            .header("cookie", "session=renewed-fixture");
        then.json_body(json!({"data":[{"id":"approved-fixture"}]}));
    });
    let source = reviewer.cookie_path.clone();
    let marker = dir.path().join("renewal.request.json");
    let host = tokio::spawn(async move {
        for _ in 0..100 {
            if marker.exists() {
                renewed_session(&source);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    let response = reviewer
        .request(
            Method::GET,
            reviewer.url("/review", &[]).unwrap(),
            None,
            None,
        )
        .await;
    host.await.unwrap();
    assert!(
        response.is_ok(),
        "expired read must recover through the host: {response:?}"
    );
    assert_eq!(response.unwrap().0["data"][0]["id"], "approved-fixture");
    expired.assert_calls(1);
    ready.assert_calls(1);
}

#[tokio::test]
async fn expired_post_is_never_replayed_or_used_to_request_authentication() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let mut reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    reviewer.recovery_wait = Duration::from_millis(20);
    let rejected = server.mock(|when, then| {
        when.method(POST).path("/send");
        then.status(401);
    });
    let err = reviewer
        .request(
            Method::POST,
            reviewer.url("/send", &[]).unwrap(),
            Some(json!({"message":"fixture"})),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("reviewer_auth_required"));
    assert!(!dir.path().join("renewal.request.json").exists());
    rejected.assert_calls(1);
}

#[tokio::test]
async fn forbidden_get_never_starts_renewal() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let mut reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    reviewer.recovery_wait = Duration::from_millis(20);
    let forbidden = server.mock(|when, then| {
        when.method(GET);
        then.status(403);
    });
    let err = reviewer
        .request(
            Method::GET,
            reviewer.url("/review", &[]).unwrap(),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("reviewer_forbidden"));
    assert!(!dir.path().join("renewal.request.json").exists());
    forbidden.assert_calls(1);
}

#[tokio::test]
async fn failed_host_recovery_is_bounded_and_does_not_loop() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let mut reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    reviewer.recovery_wait = Duration::from_millis(20);
    let expired = server.mock(|when, then| {
        when.method(GET);
        then.status(401);
    });
    let started = std::time::Instant::now();
    let err = reviewer
        .request(
            Method::GET,
            reviewer.url("/review", &[]).unwrap(),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("reviewer_auth_required"));
    assert!(started.elapsed() < Duration::from_secs(1));
    expired.assert_calls(1);
}

#[tokio::test]
async fn failed_replay_does_not_start_a_second_renewal() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let mut reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    reviewer.recovery_wait = Duration::from_millis(500);
    let expired = server.mock(|when, then| {
        when.method(GET).header("cookie", "session=secret-sentinel");
        then.status(401);
    });
    let still_rejected = server.mock(|when, then| {
        when.method(GET).header("cookie", "session=renewed-fixture");
        then.status(401);
    });
    let source = reviewer.cookie_path.clone();
    let marker = dir.path().join("renewal.request.json");
    let host = tokio::spawn(async move {
        for _ in 0..100 {
            if marker.exists() {
                renewed_session(&source);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    let result = reviewer
        .request(
            Method::GET,
            reviewer.url("/review", &[]).unwrap(),
            None,
            None,
        )
        .await;
    host.await.unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("reviewer_auth_required")
    );
    expired.assert_calls(1);
    still_rejected.assert_calls(1);
}

#[tokio::test]
async fn concurrent_expired_reads_publish_only_one_host_request() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let mut reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    reviewer.recovery_wait = Duration::from_secs(2);
    let reviewer = std::sync::Arc::new(reviewer);
    let expired = server.mock(|when, then| {
        when.method(GET).header("cookie", "session=secret-sentinel");
        then.status(401);
    });
    let ready = server.mock(|when, then| {
        when.method(GET).header("cookie", "session=renewed-fixture");
        then.json_body(json!({"data":[]}));
    });
    let mut readers = Vec::new();
    for _ in 0..10 {
        let reviewer = reviewer.clone();
        readers.push(tokio::spawn(async move {
            reviewer
                .request(
                    Method::GET,
                    reviewer.url("/review", &[]).unwrap(),
                    None,
                    None,
                )
                .await
                .unwrap();
        }));
    }
    let marker = dir.path().join("renewal.request.json");
    for _ in 0..100 {
        if marker.exists() && expired.calls_async().await == 10 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    expired.assert_calls(10);
    let first_stamp = fs::metadata(&marker).unwrap().modified().unwrap();
    renewed_session(&reviewer.cookie_path);
    for reader in readers {
        reader.await.unwrap();
    }
    assert_eq!(
        fs::metadata(&marker).unwrap().modified().unwrap(),
        first_stamp
    );
    ready.assert_calls(10);
}

#[tokio::test]
async fn delayed_replay_refusal_does_not_latch_a_concurrently_replaced_session() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let mut reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    reviewer.recovery_wait = Duration::from_secs(2);
    let expired = server.mock(|when, then| {
        when.method(GET)
            .path("/review")
            .header("cookie", "session=secret-sentinel");
        then.status(401);
    });
    let delayed_replay = server.mock(|when, then| {
        when.method(GET)
            .path("/review")
            .header("cookie", "session=renewed-fixture");
        then.status(401).delay(Duration::from_millis(250));
    });
    let newer = server.mock(|when, then| {
        when.method(GET)
            .path("/review")
            .header("cookie", "session=newer-fixture");
        then.json_body(json!({"data":[{"id":"newer-session-approved"}]}));
    });
    let source = reviewer.cookie_path.clone();
    let marker = dir.path().join("renewal.request.json");
    let replace_during_replay = async {
        for _ in 0..200 {
            if marker.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(marker.exists(), "initial expiry must request renewal");
        renewed_session(&source);
        for _ in 0..200 {
            if delayed_replay.calls_async().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        delayed_replay.assert_calls(1);
        atomic_json(&source, &json!({"cookies":[{"name":"session","value":"newer-fixture","domain":"apple.com","path":"/"}]})).unwrap();
    };
    let url = reviewer.url("/review", &[]).unwrap();
    let (result, ()) = tokio::join!(
        reviewer.request(Method::GET, url, None, None),
        replace_during_replay
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .starts_with("reviewer_auth_required")
    );
    expired.assert_calls(1);
    delayed_replay.assert_calls(1);
    newer.assert_calls(0);
    assert!(
        !dir.path().join("renewal.read-refused.json").exists(),
        "a delayed failure must not reject the newer session"
    );
    let current_generation = crate::renewal::source_generation(&source).unwrap();
    if let Some((cached_generation, _, false)) = reviewer.recovery_lock.lock().await.as_ref() {
        assert_ne!(
            cached_generation, &current_generation,
            "the newer source must not inherit an old refusal cache"
        );
    }
    let next = reviewer
        .request(
            Method::GET,
            reviewer.url("/review", &[]).unwrap(),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(next.0["data"][0]["id"], "newer-session-approved");
    newer.assert_calls(1);
}

#[tokio::test]
async fn local_missing_replay_session_does_not_record_an_apple_refusal() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    atomic_json(&reviewer.cookie_path, &json!({"cookies":[]})).unwrap();
    let network = server.mock(|when, then| {
        when.method(GET);
        then.status(401);
    });
    let error = reviewer
        .replay_once(
            Method::GET,
            reviewer.url("/review", &[]).unwrap(),
            None,
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("reviewer_session_required"), "{error}");
    assert!(!dir.path().join("renewal.read-refused.json").exists());
    assert!(reviewer.recovery_lock.lock().await.is_none());
    network.assert_calls(0);
}

#[tokio::test]
async fn refusal_marker_write_failure_keeps_the_actionable_apple_auth_error() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    fs::create_dir(dir.path().join("renewal.read-refused.json")).unwrap();
    let rejected = server.mock(|when, then| {
        when.method(GET);
        then.status(401);
    });
    let error = reviewer
        .replay_once(
            Method::GET,
            reviewer.url("/review", &[]).unwrap(),
            None,
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("reviewer_auth_required"),
        "marker I/O must not replace Apple authentication evidence: {error}"
    );
    assert!(error.contains("reviewer-login.sh stack --fresh"), "{error}");
    assert!(!error.contains("secret-sentinel"));
    rejected.assert_calls(1);
}

#[test]
fn stale_observed_generation_cannot_mark_a_replacement_as_read_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("credentials/session.cookie");
    let state = dir.path().join("state");
    atomic_json(
        &source,
        &json!({"cookies":[{"name":"session","value":"old","domain":"apple.com","path":"/"}]}),
    )
    .unwrap();
    let observed = crate::renewal::source_generation(&source).unwrap();
    renewed_session(&source);
    crate::renewal::mark_read_refused(&source, &state, &observed).unwrap();
    assert!(!state.join("renewal.read-refused.json").exists());
    assert!(!state.join("renewal.status.json").exists());
    assert!(!state.join("renewal.request.json").exists());
}
