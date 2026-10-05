# Setup

Install Rust 1.88 or newer, then:

```sh
cargo build --release --locked
./target/release/asc-mcp version
```

For official App Store Connect tools, configure `ASC_ISSUER_ID`, `ASC_KEY_ID`,
and `ASC_PRIVATE_KEY_PATH` pointing to a local `.p8` file. Do not put keys in Git.
Run `asc-mcp validate` to check local configuration; it does not prove Apple
accepts the credentials. Run `asc-mcp serve` for stdio MCP or `serve-http` for
Streamable HTTP. Public listeners require bearer tokens. Native TLS is available
with `--tls-cert` and `--tls-key`; browser origins require `--allowed-origins`.

For reviewer conversations, see [native login](docs/rust-login.md) and
[reviewer monitoring](docs/rust-reviewer.md). Reviewer-only mode needs an Apple ID
session rather than an API key. Monitor runs never start interactive login.

Use `cargo test --all-targets --locked` and
`cargo clippy --all-targets --locked -- -D warnings` before publishing changes.
There is no Go or Ruby runtime dependency. Offline migration fixtures can be
regenerated in a separate checkout of the original Go revision; see
[migration evidence](migration/public-api-coverage.md).
