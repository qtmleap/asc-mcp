# Full Rust Migration Implementation Plan

**Goal:** Replace asc-mcp's Go/Ruby execution paths with Rust, retaining the public MCP tool catalogue and reviewer monitoring.

**Architecture:** A Rust executable owns stdio/HTTP MCP transports, JWT authentication, public API requests, asset transfers, and reviewer session/monitor state. Export the existing tool definitions and compare HTTP request construction with the Go implementation offline; the final runtime never executes Go or Ruby. A browser-free Apple ID session remains a distinct credential from the public API key. Do not promise live reviewer access while authentication is pending.

**Tech Stack:** Rust, tokio, serde/serde_json, reqwest, clap, JWT/ES256 support, secure local cookie/state files.

**Spec:** User explicitly selected migration of the entire MCP, not only monitoring.

## Constraints

- Preserve 356 public tool names and argument contracts; optional reviewer tools retain their names.
- No Go/Ruby subprocesses in the shipped runtime or recurring monitor.
- Retain browser-free reviewer reads, exact-snapshot acknowledgement, and no automatic replies/submission.
- Keep secrets out of logs, Git and MCP outputs. Restrict authenticated redirects/pagination; do not retry side-effecting requests blindly.
- Keep existing automation/local MCP functional until the Rust replacement is verified, then update their executable paths.
- Full Rust code/build/test verification is independent of live Apple authentication, which remains pending user login.

## Tasks

- [x] Export tool schemas and a public API request compatibility inventory; implement Rust public tool dispatch with offline request tests.
- [x] Implement Rust JWT/HTTP and MCP transports/CLI, preserving protocol compatibility and tool annotations.
- [x] Port reviewer reads, monitoring/acknowledgement and credential import to Rust; remove Ruby execution.
- [x] Port upload/report helpers and test multi-step transfers without live credentials.
- [x] Integrate, run tests/build and protocol smoke; review tool coverage and defects.
- [x] Switch local MCP/automation to Rust, update README/build/CI, commit and push the migration to qtmleap/asc-mcp.

## Validation ledger

Every exported public tool must have a dispatch implementation and coverage evidence. Catalogue enumeration alone is not proof that handlers work. Live Apple writes remain untested until credentials and explicitly authorized real operations exist.

## Verified integration

Local macOS: `cargo fmt --all -- --check`, strict all-target Clippy, `cargo test --locked` (50 passed; one subprocess-only helper intentionally ignored), and `cargo build --release --locked` passed. The request comparison includes 3,924 captured Go cases across 356 tools. Release-binary stdio enumeration and a noninteractive missing-session monitor smoke passed. The local `asc-review` MCP launch command and 30-minute `interceptor-apple` automation now use Rust; live Apple credentials/authentication remain pending. GitHub CI runs separately on Linux, macOS and Windows.
