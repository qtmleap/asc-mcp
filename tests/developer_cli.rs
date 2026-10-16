#![cfg(unix)]
use std::process::{Command, Stdio};

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_asc-mcp"));
    command
        .env_clear()
        .env("HOME", root)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("ASC_DEVELOPER_STATE_ROOT", root.join("developer"))
        .env(
            "ASC_DEVELOPER_COOKIE_PATH",
            root.join("developer/session.cookie"),
        )
        .stdin(Stdio::null());
    command
}

#[test]
fn developer_login_requires_interactive_terminal() {
    let root = tempfile::tempdir().unwrap();
    let result = command(root.path())
        .args([
            "developer-login",
            "--apple-id",
            "synthetic@example.com",
            "--team-id",
            "ABCDEFGHIJ",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("developer_login_interactive_terminal_required")
    );
    assert!(!root.path().join("developer/session.cookie").exists());
}

#[test]
fn developer_only_cli_catalogue_and_validate() {
    let root = tempfile::tempdir().unwrap();
    let result = command(root.path())
        .env("ASC_DEVELOPER_APP_GROUPS_ENABLED", "true")
        .arg("tools")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let tools: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(tools.as_array().unwrap().len(), 6);
    assert!(
        tools
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false)
    );
    let validate = command(root.path())
        .env("ASC_DEVELOPER_APP_GROUPS_ENABLED", "true")
        .arg("validate")
        .output()
        .unwrap();
    assert!(validate.status.success());
    let malformed = command(root.path())
        .env("ASC_DEVELOPER_APP_GROUPS_ENABLED", "yes")
        .arg("validate")
        .output()
        .unwrap();
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("developer_configuration_invalid"));
}

#[test]
fn developer_disabled_keeps_legacy_catalogue() {
    let root = tempfile::tempdir().unwrap();
    let result = command(root.path()).arg("tools").output().unwrap();
    assert!(result.status.success());
    let tools: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        tools.as_array().unwrap().len(),
        asc_mcp::catalog::tools().len()
    );
    assert!(
        !tools
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "create_app_group")
    );
    let partial = command(root.path())
        .env("ASC_KEY_ID", "malformed-partial")
        .arg("tools")
        .output()
        .unwrap();
    assert!(
        partial.status.success(),
        "{}",
        String::from_utf8_lossy(&partial.stderr)
    );
}
