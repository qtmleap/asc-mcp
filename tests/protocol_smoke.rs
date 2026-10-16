use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};
#[test]
fn native_binary_stdio_catalogue_and_error_contract() {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_asc-mcp"));
    cmd.arg("serve")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path());
    for key in [
        "ASC_ISSUER_ID",
        "ASC_KEY_ID",
        "ASC_PRIVATE_KEY_PATH",
        "ASC_REVIEW_COOKIE_PATH",
        "ASC_REVIEW_APP_ID",
        "ASC_REVIEWER_ONLY",
    ] {
        cmd.env_remove(key);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"missing_tool","arguments":{}}}),
    ];
    {
        let mut stdin = child.stdin.take().unwrap();
        for request in input {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(values.len(), 3);
    let catalogue = values.iter().find(|v| v["id"] == 2).unwrap();
    assert_eq!(catalogue["result"]["tools"].as_array().unwrap().len(), 366);
    assert_eq!(
        values.iter().find(|v| v["id"] == 3).unwrap()["error"]["code"],
        -32601
    );
}
#[test]
fn native_monitor_reports_missing_auth_without_prompting() {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_asc-mcp"));
    cmd.arg("reviewer-check")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path());
    for key in [
        "ASC_REVIEW_COOKIE_PATH",
        "ASC_REVIEW_APP_ID",
        "ASC_REVIEWER_ONLY",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("ASC_REVIEW_APP_ID", "123");
    let output = cmd.stdin(Stdio::null()).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "blocked");
    assert!(
        value["reason"]
            .as_str()
            .unwrap()
            .contains("session_required")
    );
}
