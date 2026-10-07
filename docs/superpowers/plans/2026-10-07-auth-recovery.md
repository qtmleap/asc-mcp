# Reviewer Authentication Recovery Implementation Plan

> For agentic workers: execute task by task with native Claude Code tools and independent review; do not create commits until Codex verifies the final diff.

**Goal:** Remember the Apple account and use macOS Keychain plus existing trust cookies to renew expired reviewer sessions automatically.

**Architecture:** Share interactive and unattended SRP authentication. A native host helper owns Keychain/login trust and writes confirmed session cookies; the Docker service requests bounded recovery through fixed shared state and retries reads once.

**Tech Stack:** Rust, macOS Security framework, launchd, existing Docker Compose stack.

**Spec:** ../specs/2026-10-07-auth-recovery-design.md

## Global Constraints

- Passwords never enter argv, environment, files, logs, or MCP responses.
- Docker never reads the password or login-trust store; its existing credentials mount remains read-only.
- GET replay is bounded to one; POST is never replayed; unattended 2FA never invokes challenge endpoints.
- Renewal wait is bounded to 30 seconds. Failed credentials/account action stop repeated authentication until interactive success.
- Preserve existing configured paths and legacy state-directory selection; publish to the configured cookie source.

## Review Focus

- A stale 401 arrives after another worker renewed the session: reload generation before new renewal.
- Apple returns 409/401/403/412 during SRP: avoid challenge spam and account lockouts.
- Keychain is locked or requests UI: unattended code returns an actionable state without UI.
- Container-writable files are malformed/symlinked: never turn them into host commands or account selection.
- User selects another Apple ID: never reuse the first account's password/trust/session as that account.

## Task 1: Account memory and shared authentication

Files: src/main.rs, src/apple_login.rs, new src/apple_login/credentials.rs (or focused equivalent), Cargo.toml/Cargo.lock, authentication tests.

- [x] Write failing behavior tests for remembered account/explicit override, success-only credential saving, noninteractive 409 without challenge, account mismatch, and configured source publication.
- [x] Run the new tests and record expected failures before product code.
- [x] Add a secret-store interface and macOS implementation using Security APIs with UI disabled during unattended access; retain an explicit unsupported Linux fallback.
- [x] Split reusable SRP flow from TTY prompts; the caller supplies interactive vs unattended challenge policy. Keep secret buffers zeroized.
- [x] Run targeted tests and the complete Cargo suite.

## Task 2: Host renewal and Docker integration

Files: new focused renewal module, src/reviewer.rs and tests, src/main.rs commands, script/macos or equivalent host scripts, script/docker/reviewer-login.sh, compose.yaml, README.md/docs/rust-login.md/docs/docker-sidecar.md.

- [x] Write failing tests for single-flight/cooldown/user-action latch, generation swap racing persistence, fixed marker validation, missing helper timeout, GET replay once, and POST non-replay.
- [x] Run tests and record failures before product code.
- [x] Add host-only renewal lock/config and shared sanitized status/marker. Recheck effective session under lock, use Keychain only when session needs authentication, and latch Apple-required user actions.
- [x] Add bounded opt-in service recovery and read-only retry. Treat shared state as untrusted; no subprocess from reviewer.
- [x] Add native host launcher/launchd installer with periodic renewal and marker wake, matching the configured/legacy host paths; use native enrollment on Mac while retaining Linux interactive helper.
- [x] Run full tests, formatting, linting, Mac build and Linux Docker build.
- [x] Complete independent final diff review and focused follow-up review.
- [x] Verify deployment commands, configured paths, private mounts, daemon state and Docker-to-host marker wake.
- [ ] Enroll once with human password/verification input and verify live reviewer reads.

## Final integration

- [x] Independent Claude review of final diff and relevant files; resolve material findings and re-review affected changes.
- Publish the verified changes through the existing review branch/PR, using native Git identity/message/commit/push checks and accurate live enrollment limitations.
- [x] Update the ASC runtime and renewal agent; verify MCP initialization and preserve the unfinished Kotatsu review-response task.

## Verified deployment state

Mac: 173 ordinary tests passed, two ignored; the disposable native Keychain fixture also passed when explicitly enabled. Linux: 172 tests passed, one ignored. Formatting, strict Clippy, native release build and Docker release build passed. The independent final reviews have no remaining material findings. LocalGPT plan review could not be recovered, so independent Claude file reviews were used; Claude's Linux host lacked Rust, and Codex ran the checks natively.

The deployed service runs the verified image with a 30-second recovery wait, read-only credentials and no login-trust mount. The native LaunchAgent exits successfully and a Docker-written advisory marker woke it within 0.25 seconds. The existing session remains rejected; host renewal reports `account_missing` until initial human enrollment. No real password or verification code was supplied during deployment, and eight-hour endurance has not been observed.
