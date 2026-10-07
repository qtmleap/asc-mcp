#![cfg(unix)]
use std::{fs, process::Command};

fn configured(temp: &std::path::Path, state: &str) -> std::process::Output {
    fs::write(
        temp.join("runtime.env"),
        format!("ASC_MCP_AUTH_TOKENS=fixture-must-not-load\nASC_REVIEW_STATE_ROOT={state}\n"),
    )
    .unwrap();
    Command::new("sh")
        .args(["-c", ". \"$ASC_TEST_SCRIPT\"; printf '%s\\n' \"$ASC_REVIEW_STATE_ROOT\" \"$ASC_REVIEW_TRUST_ROOT\" \"${ASC_MCP_AUTH_TOKENS-unset}\""])
        .env("ASC_TEST_SCRIPT", format!("{}/script/macos/reviewer-env.sh", env!("CARGO_MANIFEST_DIR")))
        .env("ASC_MCP_CONFIG_DIR", temp)
        .env_remove("ASC_MCP_AUTH_TOKENS")
        .output().unwrap()
}

#[test]
fn native_host_paths_preserve_spaces_and_do_not_load_service_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let output = configured(
        temp.path(),
        "/state/Library/Application Support/InterceptorReview",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout).unwrap();
    let host_root = temp.path().canonicalize().unwrap();
    assert_eq!(
        lines,
        format!(
            "{}/state/Library/Application Support/InterceptorReview\n{}/credentials-login\nunset\n",
            host_root.display(),
            host_root.display()
        )
    );
}

#[test]
fn native_host_paths_reject_state_traversal() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        !configured(temp.path(), "/state/../host-credentials")
            .status
            .success()
    );
}

#[test]
fn native_host_paths_reject_container_symlink_escape() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("state")).unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join("state/Library")).unwrap();
    assert!(
        !configured(temp.path(), "/state/Library/Application Support/ASC MCP")
            .status
            .success()
    );
}
