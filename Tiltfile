# Local container development uses the Rust Dockerfile.
docker_build('asc-mcp', '.', dockerfile='Dockerfile')
local_resource('rust-check', cmd='cargo test --locked && cargo clippy --all-targets --locked -- -D warnings', deps=['src', 'Cargo.toml', 'Cargo.lock', 'data'])
