# Setup and development

Docker onboarding is in [README.md](README.md). This page covers native builds,
CI, deployment and migration verification.

## Native build

Requires Rust 1.88 or newer.

```sh
cargo build --release --locked
./target/release/asc-mcp version
./target/release/asc-mcp tools
./target/release/asc-mcp serve
```

`serve` speaks newline-delimited JSON-RPC on stdin/stdout; diagnostics go to
stderr. `serve-http` provides Streamable HTTP at `/mcp`, readiness at `/healthz`
and metrics at `/metrics`. It supports legacy sessions and the stateless
2026-07-28 revision alongside 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05.
Legacy initialize negotiates a legacy revision, not the stateless one.
Examples using `asc-mcp` assume the binary is on your PATH; otherwise use
`./target/release/asc-mcp`. Set `ASC_MCP_AUTH_TOKENS` in the server environment
before using a non-loopback listener.

```sh
asc-mcp serve-http --addr 127.0.0.1:8080
asc-mcp serve-http --addr :8080 \
  --allowed-origins https://your-client.example
```

Non-loopback listeners require bearer tokens; browser origins must be explicitly
allowed. Optional TLS uses paired `--tls-cert` / `--tls-key`.

## Native credentials

```sh
export ASC_ISSUER_ID='your-issuer-id'
export ASC_KEY_ID='your-key-id'
export ASC_PRIVATE_KEY_PATH='/private/path/AuthKey.p8'
asc-mcp validate
```

Validation checks local configuration, not Apple access. ES256 JWTs are cached
for 15 minutes. Pagination stays on the API origin, redirects are disabled and
writes are not retried. Uploads use Apple's storage requests without forwarding
the JWT or cookies. See `config/config.sample.env`.

## Native reviewer CLI

```sh
asc-mcp reviewer-login --apple-id 'your-apple-id'      # interactive terminal only
asc-mcp reviewer-import-session /private/path/session.cookie
asc-mcp reviewer-check                                  # read-only
asc-mcp reviewer-check --ack '<snapshot_token>'         # after reviewing that check
codex mcp add asc-review -- "$PWD/target/release/asc-mcp" serve
```

Native state defaults to `~/Library/Application Support/ASC MCP/` (files 600,
directory 700); override the session with `ASC_REVIEW_COOKIE_PATH`. The Docker
renewal agent does not renew this native session. Reviewer-only mode needs no API
key. Set `ASC_REVIEW_APP_ID` only for message monitoring. The legacy
`reply_reviewer_message` is experimental, disabled and unverified; see
[native login](docs/rust-login.md) and [reviewer monitoring](docs/rust-reviewer.md).

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
```

## CI and deployment

- `.github/workflows/integration.yaml` runs formatting, strict Clippy, tests,
  release builds, CLI smoke, a distroless Docker build and Compose validation on
  self-hosted Linux X64 Docker and `macos-latest` runners. Manual runs accept
  `test-os`, e.g. `["Linux","macos-latest","Windows"]`; Windows needs a matching
  self-hosted runner.
- `.github/workflows/deployment.yaml` runs on `master` pushes, `v*` tags and manual
  dispatch, reusing the Linux checks. It builds amd64 natively and arm64 under QEMU
  (90-minute limit), smokes each image and publishes a multi-platform manifest to
  `ghcr.io/qtmleap/asc-mcp` with version and commit tags; default-branch builds
  also update `latest`. Release tags must match `Cargo.toml`.
- `.github/workflows/release.yml` packages native binaries for version tags; manual
  runs only upload artifacts.

GHCR uses `GITHUB_TOKEN` with `packages: write`; CI needs no Apple credentials. It
publishes the MCP image and does not deploy your app or replace local containers.

## Migration verification

- 3,924 offline HTTP request comparisons span all 356 original tools.
- Compound submission, uploads, reports, protocol/auth, reviewer state and SRP have
  Rust fixtures and local HTTP tests. These do not prove Apple accepts live writes.
- Intentional schema repairs and remaining argument limits:
  [migration evidence](migration/public-api-coverage.md).

The original Go revision and export provenance are in Git history and
`data/compatibility_provenance.json`. The Go/Python exporters run in a separate
checkout of that revision and are not part of the build or runtime; `doc/adr` is
historical reference only. There is no Go or Ruby runtime dependency.
