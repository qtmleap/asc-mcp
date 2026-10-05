# Reviewer launcher

`check.sh` is a compatibility launcher for the native Rust release binary; it
never invokes fastlane, Ruby or Go. Build first with `cargo build --release --locked`.

- `check.sh` → `asc-mcp reviewer-check`
- `check.sh --login [Apple ID]` → `asc-mcp reviewer-login`
- `check.sh --ack <snapshot_token>` → `asc-mcp reviewer-check --ack <snapshot_token>`

See [native reviewer operation](../../docs/rust-reviewer.md) and
[authentication](../../docs/rust-login.md). Live Apple access requires a valid
session and remains unverified until the user completes CLI authentication.
