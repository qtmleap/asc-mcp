#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{
    fs::{self, File},
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    helper: PathBuf,
    trace: PathBuf,
}

fn executable(path: &Path, source: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let helper = root.join("script/docker/developer-login.sh");
    let source = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/script/docker/developer-login.sh"
    ))
    .expect("Developer Docker login helper must exist");
    executable(&helper, &source);
    executable(
        &root.join("script/docker/compose.sh"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ASC_TEST_TRACE\"\nprintf 'token=%s\\n' \"${ASC_MCP_AUTH_TOKENS-unset}\"\n",
    );
    executable(
        &root.join("script/macos/developer-login.sh"),
        "#!/bin/sh\nprintf 'unexpected-native-login' > \"$ASC_TEST_TRACE\"\nexit 93\n",
    );
    executable(
        &root.join("fake-bin/uname"),
        "#!/bin/sh\nprintf '%s\\n' \"$ASC_TEST_OS\"\n",
    );
    let config = root.join("private config");
    fs::create_dir_all(config.join("state")).unwrap();
    fs::write(
        config.join("runtime.env"),
        "ASC_MCP_AUTH_TOKENS=must-not-load\nASC_DEVELOPER_APP_GROUPS_ENABLED=false\n",
    )
    .unwrap();
    Fixture {
        trace: root.join("compose-arguments.txt"),
        root,
        helper,
        config,
        _directory: directory,
    }
}

fn terminal_pair() -> (File, File) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0);
    unsafe { (File::from_raw_fd(master_fd), File::from_raw_fd(slave_fd)) }
}

fn run(fixture: &Fixture, platform: &str, interactive: bool, arguments: &[&str]) -> Output {
    let mut command = Command::new("sh");
    command
        .arg(&fixture.helper)
        .args(arguments)
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                fixture.root.join("fake-bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("HOME", &fixture.root)
        .env("ASC_MCP_CONFIG_DIR", &fixture.config)
        .env("ASC_TEST_TRACE", &fixture.trace)
        .env("ASC_TEST_OS", platform);
    let terminal = interactive.then(terminal_pair);
    if let Some((_, slave)) = &terminal {
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
    }
    command.output().unwrap()
}

#[test]
fn developer_helper_requires_tty_before_compose() {
    let fixture = fixture();
    let output = run(&fixture, "Linux", false, &["standalone"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(!fixture.trace.exists());
}

#[test]
fn developer_helper_uses_shared_docker_state_on_darwin_and_linux() {
    for platform in ["Darwin", "Linux"] {
        let fixture = fixture();
        let output = run(
            &fixture,
            platform,
            true,
            &[
                "standalone",
                "--apple-id",
                "operator@example.test",
                "--team-id",
                "TEAM123456",
            ],
        );
        assert!(output.status.success(), "{platform}: {:?}", output.status);
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "token=unset\n");
        let arguments = fs::read_to_string(&fixture.trace).unwrap();
        assert!(arguments.starts_with("standalone\nrun\n--rm\n--no-deps\n"));
        assert!(arguments.contains("--env\nHOME=/state\n"));
        assert!(arguments.contains(
            "ASC_DEVELOPER_STATE_ROOT=/state/Library/Application Support/ASC MCP/Developer Portal\n"
        ));
        assert!(arguments.contains("ASC_DEVELOPER_COOKIE_PATH=/state/Library/Application Support/ASC MCP/Developer Portal/developer-session.cookie\n"));
        assert!(arguments.ends_with(
            "asc-mcp\ndeveloper-login\n--apple-id\noperator@example.test\n--team-id\nTEAM123456\n"
        ));
        assert!(!arguments.contains("reviewer-login"));
        assert!(!arguments.contains("ASC_DEVELOPER_APP_GROUPS_ENABLED="));
        assert!(!arguments.contains("--service-ports"));
    }
}

#[test]
fn developer_helper_preserves_raw_paths_without_evaluating_runtime_env() {
    let fixture = fixture();
    let canary = fixture.root.join("must-not-create");
    fs::write(
        fixture.config.join("runtime.env"),
        format!(
            "ASC_DEVELOPER_STATE_ROOT=/state/Private Area/Developer\nASC_DEVELOPER_COOKIE_PATH=/state/Private Area/session.cookie\nASC_TEST_INJECTION=$(touch {})\nASC_MCP_AUTH_TOKENS=must-not-load\n",
            canary.display()
        ),
    )
    .unwrap();
    let output = run(&fixture, "Darwin", true, &["stack"]);
    assert!(output.status.success());
    assert!(!canary.exists());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "token=unset\n");
    let arguments = fs::read_to_string(&fixture.trace).unwrap();
    assert!(arguments.starts_with("stack\nrun\n"));
    assert!(arguments.contains("ASC_DEVELOPER_STATE_ROOT=/state/Private Area/Developer\n"));
    assert!(arguments.contains("ASC_DEVELOPER_COOKIE_PATH=/state/Private Area/session.cookie\n"));
}

#[test]
fn developer_helper_rejects_unsafe_or_unmapped_container_paths() {
    for (key, value) in [
        ("ASC_DEVELOPER_STATE_ROOT", "/state/../outside"),
        ("ASC_DEVELOPER_STATE_ROOT", "/private/developer"),
        ("ASC_DEVELOPER_COOKIE_PATH", "/state/./session.cookie"),
        ("ASC_DEVELOPER_COOKIE_PATH", "/run/asc-mcp/session.cookie"),
    ] {
        let fixture = fixture();
        fs::write(
            fixture.config.join("runtime.env"),
            format!("{key}={value}\n"),
        )
        .unwrap();
        let output = run(&fixture, "Linux", true, &["standalone"]);
        assert!(!output.status.success(), "{key}={value}");
        assert!(!fixture.trace.exists());
    }
}

#[test]
fn developer_helper_help_and_invalid_modes_never_start_compose() {
    let fixture = fixture();
    assert!(run(&fixture, "Darwin", false, &["--help"]).status.success());
    assert!(!run(&fixture, "Linux", false, &["other"]).status.success());
    assert!(!fixture.trace.exists());
}

#[test]
fn developer_helper_requires_existing_private_setup() {
    let fixture = fixture();
    fs::remove_file(fixture.config.join("runtime.env")).unwrap();
    let output = run(&fixture, "Darwin", true, &["standalone"]);
    assert!(!output.status.success());
    assert!(!fixture.trace.exists());
}
