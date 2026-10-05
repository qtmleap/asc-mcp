# asc-mcp — Rust

A native Rust MCP server for App Store Connect, including browser-free reviewer
conversation monitoring. This repository derives from
[antisynthesis/asc-mcp](https://github.com/antisynthesis/asc-mcp), whose original
implementation was Go. The shipped server, uploads, monitor and CLI login now
run in Rust: no Go, Ruby, Python or fastlane subprocess is required at runtime.

## Build and run

Requires Rust 1.88 or newer.

```sh
cargo build --release --locked
./target/release/asc-mcp version
./target/release/asc-mcp tools
./target/release/asc-mcp serve
```

`serve` speaks newline-delimited JSON-RPC on stdin/stdout. Diagnostics stay on
stderr. `serve-http` provides Streamable HTTP at `/mcp`, readiness at `/healthz`,
and metrics at `/metrics`. It supports legacy sessions and the stateless
2026-07-28 revision alongside 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05.
Legacy initialize negotiates a legacy revision, not the stateless one.

```sh
asc-mcp serve-http --addr 127.0.0.1:8080
asc-mcp serve-http --addr :8080 --auth-tokens "$ASC_MCP_AUTH_TOKENS" \
  --allowed-origins https://your-client.example
```

Non-loopback listeners require bearer tokens. Browser origins must be explicitly
allowed. Optional native TLS uses paired `--tls-cert` / `--tls-key` flags.

## Official API tools

All **356 original public tool names** are retained. Areas include app metadata,
versions, review submission, screenshots/previews, TestFlight, provisioning,
commerce, Game Center, reporting, analytics, Xcode Cloud, webhooks and assets.
The complete runtime catalogue is available with `asc-mcp tools`.

Set the existing credential variables:

```sh
export ASC_ISSUER_ID='your-issuer-id'
export ASC_KEY_ID='your-key-id'
export ASC_PRIVATE_KEY_PATH='/private/path/AuthKey.p8'
asc-mcp validate
```

The `.p8` file remains local. ES256 JWTs are cached for 15 minutes and refreshed
before expiry. Authenticated pagination stays on the API origin, redirects are
disabled, and writes are not automatically retried. Uploads use Apple's supplied
storage requests without forwarding the API JWT/cookies.

Validation checks local configuration; it is not proof of live Apple access.
Tool calls still affect production App Store Connect data when supplied with
valid credentials and permissions.

## Reviewer tools and CLI

Reviewer messages use the undocumented Iris HTTP API and a separate Apple ID
session. The public API's `.p8` key does not replace this session.

```sh
# Interactive terminal only: password, SRP and verification codes stay local.
asc-mcp reviewer-login --apple-id 'your-apple-id'

# Or import an existing private session, without logging in.
asc-mcp reviewer-import-session /private/path/session.cookie

# Read new Apple messages; no interactive login, sending or acknowledgement.
asc-mcp reviewer-check

# Only after reviewing that exact check's messages:
asc-mcp reviewer-check --ack '<snapshot_token>'
```

The CLI login implements native Apple SRP, bounded hashcash, modern two-factor
and legacy two-step verification, and verifies an App Store Connect web session
before saving private canonical cookies. **Live Apple authentication and reviewer
access remain unverified** until a real account completes authentication. See
[login implementation limits](docs/rust-login.md).

Reviewer-only MCP requires `ASC_REVIEW_COOKIE_PATH` (or the default private
session). It does not require official API-key credentials. Optional tools:

| Tool | Purpose |
| --- | --- |
| `list_reviewer_threads` | Read submission conversations |
| `get_reviewer_messages` | Read messages, actors and attachment references |
| `get_legacy_reviewer_context` | Inspect the old reply context |
| `check_reviewer_updates` | Read unseen/changed Apple messages |
| `acknowledge_reviewer_updates` | Acknowledge the exact analysed snapshot locally |

`reply_reviewer_message` remains **experimental and disabled by default**;
`ASC_REVIEW_ENABLE_LEGACY_REPLY=true` exposes the old reply route for explicit
validation. A request accepted by Apple is not a read-back delivery confirmation.
Reply attachments are not implemented. Neither checks nor acknowledgements send
messages or submit an app.

On this project's Mac setup, private cookies, snapshots and state live under
`~/Library/Application Support/InterceptorReview/`, outside Git. Files use mode
600 and the directory mode 700. An OS lock excludes concurrent monitors and
releases on process exit. Acknowledgements require a snapshot token so a newer,
unread message cannot be marked seen accidentally. Details:
[reviewer monitoring](docs/rust-reviewer.md).

## Migration verification

- 3,924 offline HTTP request comparisons span all 356 original tools.
- Compound submission, reserve/chunk/commit uploads, reports, protocol/auth,
  reviewer state and SRP have independent Rust fixtures/local HTTP tests.
- Intentional Apple-schema repairs and remaining old argument limitations are
  documented in [the per-tool coverage ledger](migration/public-api-coverage.md).
- Request construction tests do not establish that Apple accepts live writes.
- Ordinary API results use JSON text and structured content; compound assets and
  reports retain their text/structured tool-result envelope.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
```

The original Go revision and export provenance are retained in Git history and
`data/compatibility_provenance.json`. Migration-only Go/Python exporters can be
used in a separate checkout of that historical revision; they are not part of
the build or runtime. Historical Go architecture decisions under `doc/adr` remain
reference material, not current implementation instructions.

## MCP setup

```sh
codex mcp add asc-review \
  --env 'ASC_REVIEW_COOKIE_PATH=/private/path/session.cookie' \
  -- /absolute/path/asc-mcp serve
```

For the official API tools, also configure the three API credential variables.
See [SETUP.md](SETUP.md) and `config/config.sample.env`. Never put private keys,
cookies or passwords in MCP tool arguments or Git.

## License

MIT. The original tool definitions and behaviour are derived from the upstream
Go project; its licence is retained in [LICENSE](LICENSE).
