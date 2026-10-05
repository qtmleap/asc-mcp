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

Run the native Rust login in a disposable Docker container from an interactive
terminal:

```sh
./script/docker/reviewer-login.sh stack
# For a standalone installation:
./script/docker/reviewer-login.sh standalone
```

Enter the Apple ID, password, and any required 2FA code in that terminal.
The helper can also be launched by a local agent with an interactive terminal;
passwords and codes must not be passed in MCP arguments, chat, or command lines.
It refuses noninteractive execution, so scheduled checks cannot start login.

Only this one-off container mounts the private credentials directory writable
at the login's save path. The running service keeps its read-only credentials
mount. A successfully confirmed login atomically replaces `session.cookie`
in the existing host credentials directory; the monitoring service uses it on
the next request without restarting. A failed login leaves the existing file
intact. The one-off container is removed when login ends and publishes no ports.
No host Rust installation or manual Cookie copying is required. Build the image
with the startup commands above before using the helper.

A bare `docker run` with the image's default command must supply
`ASC_MCP_AUTH_TOKENS`; without it the non-loopback HTTP listener refuses to
start. Browser clients also require explicitly configured allowed origins.

## Verify

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
