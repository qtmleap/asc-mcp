# Docker sidecar

`asc-review` is the MCP connection name. The Rust binary and Compose service
are named `asc-mcp`. The image starts a long-running Streamable HTTP server on
port 8080 by default; it does not need an open stdin. Pass `serve` explicitly
when using stdio instead.

The multi-stage Dockerfile builds the Rust executable with BuildKit Cargo caches
and copies it into a nonroot distroless runtime with CA certificates. Apple
cookies and API keys are runtime mounts, never image build inputs. The release
Dockerfile uses the same HTTP default. The
[LocalMCP Dockerfile](https://github.com/tkgstrator/local-mcp/blob/master/Dockerfile)
also separates the Rust build and runtime; `asc-mcp` needs only its native binary
and certificates at runtime.

## Run beside LocalGPT and LocalMCP

From the `asc-mcp` checkout:

```sh
./script/docker/setup.sh
./script/docker/compose.sh stack up -d --no-deps --build asc-mcp
```

The helper merges this repository's Compose service into the existing
`localgpt` project. `LOCALGPT_DIR` can select an existing LocalGPT checkout when
it is not the sibling `../local-gpt`. `--no-deps` updates only `asc-mcp`.

Clients on the shared Compose network connect to `http://asc-mcp:8080/mcp`.
Clients on the host connect to `http://127.0.0.1:8878/mcp` (or `ASC_MCP_PORT`).
Both require `Authorization: Bearer <token>` using the private token from
`~/.config/asc-mcp/runtime.env`. Keep the token in the client's private
configuration; do not place it in source code or chat messages.

For a standalone Compose project:

```sh
./script/docker/compose.sh standalone up -d --build asc-mcp
```

For another application stack, merge `compose.yaml` into that stack and put
the client and `asc-mcp` on the same network. Use an absolute
`ASC_MCP_SOURCE_DIR` and `ASC_MCP_CONFIG_DIR` if its working directory differs
from this checkout.

## Authentication and persistent state

The setup helper generates the local HTTP bearer token; it does not log in to
Apple. Reviewer tools use an existing private
`~/.config/asc-mcp/credentials/session.cookie`. Official API tools additionally
need the team key and credential variables described in the main README.

Compose mounts `credentials/` read-only at `/run/asc-mcp` and mounts a separate
persistent `state/` directory at `/state`. Updated response cookies persist as a
private `session.state.json` overlay in that directory; neither the source cookies
nor p8 files are written. The source fingerprint is checked on each read, so a
renewed session file takes effect without rebuilding the image. Apple can still
reject a session and require an explicit login. Changes to
`runtime.env` require recreating the service. Neither the Docker socket nor a
source/workspace mount is required by this sidecar.

## Log in or renew the reviewer session

On macOS, install the native host helper and enroll once:

```sh
cargo build --release --locked
./script/macos/install-renewal-agent.sh --binary "$PWD/target/release/asc-mcp"
./script/docker/reviewer-login.sh standalone --fresh
ASC_REVIEW_RENEWAL_WAIT_SECS=30 ./script/docker/compose.sh standalone up -d --build asc-mcp
```

Use `stack` instead of `standalone` for the shared LocalGPT deployment.

The helper remembers the confirmed Apple ID and team, saves the confirmed password in macOS Keychain and reuses trusted-device cookies. Its LaunchAgent checks periodically and wakes on the advisory renewal marker. Persist `ASC_REVIEW_RENEWAL_WAIT_SECS=30` in private `runtime.env` across later service recreation. Docker credentials stay read-only; Keychain and login trust stay on the host.

Required verification, rejected credentials, unavailable Keychain access and team changes produce a sanitized user-action state and stop automatic password attempts. Re-run interactive login when required; `--fresh` enrolls a replacement password. Never pass passwords/codes through chat, MCP arguments, argv or environment. The service waits at most 30 seconds and replays GET once, never POST.

Linux retains `./script/docker/reviewer-login.sh stack` (or `standalone`) in a disposable interactive login container. It remembers the account but does not store passwords or renew unattended. Confirmed login writes the configured cookie source, reloaded without restart. Failed authentication does not replace the source.

Pass `--accept-provider-change` to the login helper only when intentionally
changing the confirmed Apple team. Unattended renewal never changes the pinned
provider.

### Stop or upgrade the macOS helper

To stop automatic authentication, unload `work.asc-mcp.reviewer-renewal` with `launchctl bootout gui/$(id -u)/work.asc-mcp.reviewer-renewal` before removing the session source. Removing the source alone does not revoke an enrolled host helper: it can create a new session from Keychain. The Mac wrapper is the supported enrollment path on Mac; do not run a Linux login container concurrently against the host trust store.

If the native helper binary changes, macOS may require Keychain authorization for its new code identity. Re-run the Mac login wrapper with `--fresh` after an upgrade; unattended access never opens UI. Missing provider pins require this initial enrollment but do not invalidate an already healthy reviewer session. Network, Keychain-access and storage interruptions use bounded retry; identity mismatches, refused credentials and Apple verification requirements stop repeated authentication.

### Session paths

`ASC_MCP_CONFIG_DIR` selects the host configuration directory (default
`~/.config/asc-mcp`). The helpers use `credentials/session.cookie` as the session
source, `credentials-login/` for login-only trusted-device cookies, and
`state/Library/Application Support/ASC MCP/` for the shared overlay and locks.
The service never receives passwords or the login-only trust directory.

Set `ASC_REVIEW_STATE_ROOT` in private `runtime.env` to override the container
state path beneath `/state/`; the macOS helper maps it to the corresponding host
path. Native runs outside these Docker helpers use their own private directory
under `~/Library/Application Support/ASC MCP/`, unless explicitly configured.

If the default state directory is absent but the legacy `InterceptorReview`
directory exists, the helpers keep using the whole legacy directory. No state or
credentials are merged automatically. To migrate, stop service, login and check
processes, then move the whole legacy directory to `ASC MCP`.

A bare `docker run` with the image's default command must supply
`ASC_MCP_AUTH_TOKENS`; without it the non-loopback HTTP listener refuses to
start. Browser clients also require explicitly configured allowed origins.

## Verify

The opt-in App Group tools can reuse the configured reviewer session on the
existing mounts. For an optional dedicated account, use the interactive
[`developer-login.sh` helper](developer-portal.md#docker). On both macOS and Linux it publishes to the service's shared
Developer state under `/state`. It does not enroll Keychain credentials or
start/restart the server. Existing raw `runtime.env` and `/state` mounts supply
the configuration; no new credential mount is needed.

```sh
curl --fail http://127.0.0.1:8878/healthz
./script/docker/compose.sh stack ps asc-mcp
```

`/healthz` confirms the server is running, not that the Apple session is valid.
Use `get_review_connection_status` over authenticated MCP to check the session
and current provider, then `get_all_app_review_statuses` to inspect visible apps.
The tools operate on the current provider session; they do not switch teams.

The Docker CI smoke test starts the image without stdin, checks `/healthz`,
checks that unauthenticated MCP is denied, and performs an authenticated MCP
initialize request. The distroless runtime has no shell or curl; these probes
run from the host or client container.
