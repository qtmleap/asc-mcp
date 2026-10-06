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

## Docker and shared LocalGPT stack

See [docs/docker-sidecar.md](docs/docker-sidecar.md) for the Docker sidecar details.

The Docker build uses BuildKit read-only source bind mounts and Cargo cache
mounts. The runtime is nonroot `distroless/cc-debian12` with no shell or build
toolchain; private keys/cookies are never copied into the image. The build context
contains only Cargo manifests, Rust source and embedded runtime data.

```sh
# Standalone server, loopback HTTP endpoint at http://127.0.0.1:8878/mcp
./script/docker/compose.sh standalone

# LocalGPT + LocalMCP + asc-mcp in the existing LocalGPT Compose project
# LOCALGPT_DIR defaults to the sibling ../local-gpt checkout.
./script/docker/compose.sh stack

# Start/rebuild only this service without restarting other development services
./script/docker/compose.sh stack up -d --build asc-mcp

# Codex stdio MCP uses the same running container, credentials and state.
codex mcp add asc-review -- /absolute/path/asc-mcp/script/docker/mcp.sh
```

The helper creates `~/.config/asc-mcp/runtime.env` with a random HTTP bearer token
and your host UID/GID. It never overwrites an existing config. The credentials
bind is read-only; monitor state uses a separate writable bind. The root
filesystem is read-only. Set `ASC_MCP_CONFIG_DIR` for a different private config
location. Docker Compose 2.30+ and BuildKit are required.

Place the team key at `~/.config/asc-mcp/credentials/AuthKey.p8` (mode 600), then
append the following to the private `runtime.env`:

```dotenv
ASC_ISSUER_ID=your-issuer-id
ASC_KEY_ID=your-key-id
ASC_PRIVATE_KEY_PATH=/run/asc-mcp/AuthKey.p8
```

For metadata/build selection/resubmission use a **Team Key with App Manager**
access. Team keys apply across all apps; the current JWT implementation requires
an Issuer ID and does not support individual keys. See Apple's
[key creation](https://developer.apple.com/help/app-store-connect/get-started/app-store-connect-api)
and [submission roles](https://developer.apple.com/help/app-store-connect/manage-submissions-to-app-review/submit-an-app).

Reviewer conversations still require a separately authenticated session. After
native CLI login, place the private `session.cookie` in that credentials directory
(mode 600). Cookies are reloaded on each reviewer call. No monitor starts interactive login or
sends replies automatically. The endpoint requires the bearer token stored in
`runtime.env`. From containers on the shared stack use `http://asc-mcp:8080/mcp`;
from this Mac use `http://127.0.0.1:8878/mcp`.

## Capabilities and required credentials

The Apple ID session below means an authenticated **App Store Connect web
session**, not this chat or an official API-key JWT. The current server exposes
ten reviewer tools with a session alone (five status tools plus the five conversation/monitor tools). It adds the 356 public API tools when
team API credentials are configured; actual access depends on Apple's role
permissions. Having a key does not grant every tool permission.

| Task | Credentials / environment | Current implementation and verification |
| --- | --- | --- |
| List App Review conversations | Apple ID session with access to the app | `list_reviewer_threads`; implemented |
| Read Apple/developer messages, dates, rejection and attachment references | Same session | `get_reviewer_messages`; implemented; attachment file download is not implemented |
| Check new or changed Apple replies | Same session | `check_reviewer_updates`; live read confirmed for Interceptor on 2026-10-05 |
| Record that a checked snapshot has been reviewed | Local monitor state and exact `snapshot_token` | `acknowledge_reviewer_updates`; live workflow confirmed; changes local state only, not Apple's read status |
| List visible apps; read versions and review submissions with raw Apple states; read the current rejection and Apple messages; check the session | Same session | Five status tools implemented and tested offline; live HTTP MCP verified on 2026-10-05 for the current provider's 10 apps, with rejection details for Interceptor and Kotatsu. Read-only, no monitor state |
| Inspect old reply context | Same session | `get_legacy_reviewer_context`; experimental legacy endpoint; not live verified |
| Send a reply to App Review | Same session plus `ASC_REVIEW_ENABLE_LEGACY_REPLY=true` | `reply_reviewer_message`; experimental, disabled by default, not live verified; no reply attachments |
| Read/update app metadata, versions and review information; upload screenshots/previews | Team `.p8`, Key ID, Issuer ID and sufficient role | Public API tools implemented and tested offline; live access pending |
| Upload an already-built artifact, select a build, submit for review | Same team API credentials; App Manager for submission | Implemented and tested offline; live upload/submission pending |
| Manage TestFlight, provisioning certificates/profiles, commerce, Game Center, reports and Xcode Cloud | Same team API credentials with permissions for the requested endpoint | Implemented public tools; some tasks require roles beyond App Manager; live access pending |
| Build/archive an Xcode project, sign locally and export an IPA | A Mac with Xcode, signing certificate **and its private key**, provisioning profile and project | Not implemented as MCP tools; putting these credentials in the container does not enable these steps |

An API `.p8` authorizes official API calls; it is not an app-signing private key.
The current implementation uses team-key JWTs with Issuer ID. Individual API keys
are not supported. App Manager is the intended role for app metadata, build
selection and resubmission; finance/account administration may require different
roles. Team keys cannot be restricted to only Interceptor. Apple's role sources
are linked in the Docker setup above.

The reviewer tools do not expose all App Store Connect features just because an
Apple ID session can access them in the website. In particular, this session-only
client does not implement general metadata edits, build upload or submission.
App Review conversations are also distinct from public App Store ratings/reviews.
The `.p8` does not replace reviewer-session authentication.

### Session setup and renewal

The generic private directory is `Library/Application Support/ASC MCP` beneath
HOME. If it is absent and the old `InterceptorReview` directory exists, the
whole legacy directory remains in use; no credentials or state are copied or
mixed automatically. To migrate, stop all service/login/check processes and move
the whole directory to `ASC MCP` (never merge two directories). In Docker this
is beneath `~/.config/asc-mcp/state`; native runs use HOME. The host credentials
and login-trust bind directories keep their existing generic `asc-mcp` names.
Set `ASC_REVIEW_APP_ID` explicitly for reply monitoring; no app is selected by
default. Session/status and per-app tools remain available without a monitor ID.


Use your primary Apple Account email (not a phone number or email alias). If the saved ASC session or overlay is malformed, or preflight is refused, `reviewer-login --fresh` skips preflight for recovery while still reusing valid login cookies. Malformed or insecure login-cookie storage remains an error: fix its permissions or remove the private `credentials-login/trust.json` before logging in again. The login-only store can contain full Apple session credentials as well as device-trust cookies.

Run `reviewer-login` yourself in a terminal; supply the Apple ID, password and
2FA code there. Successful native login saves a private session, never the
password. For Docker, run `./script/docker/reviewer-login.sh stack` (or
`standalone`) in an interactive terminal. The disposable container saves the
confirmed session directly to `~/.config/asc-mcp/credentials/session.cookie`
with mode 600; no manual copying is needed. The running reviewer reloads cookies
on each call, so replacing the file does not require a rebuild.
When changing API credential environment variables, recreate the container with
`./script/docker/compose.sh stack up -d asc-mcp`.

**Trusted-device cookies and session reuse.** Login first checks the existing session
(source plus rotation overlay) against Apple's Olympus endpoint. It is reused, with rotated
cookies saved and no password prompt, only when Apple reports the same `user.emailAddress`
(case-insensitive, trimmed) as the requested Apple ID. A 401, a different account or a missing
identity starts one fresh interactive login; a network error, malformed reply, 403 or other
status stops without prompting. Trusted-device cookies from a confirmed login are kept in a
private, login-only directory, bound to a hash of the Apple ID (never the identity, password
or codes), and the reviewer service never mounts or reads it. The Docker helper uses
`~/.config/asc-mcp/credentials-login` (mounted at `/login-trust`, override with
`ASC_REVIEW_TRUST_ROOT`); a native run defaults to `login-trust` beneath the effective private root (`<HOME>/Library/Application Support/ASC MCP` for new installations).
`ASC_REVIEW_COOKIE_PATH` selects the session read by the service and login preflight. Native new logins save to `session.cookie` beneath the effective private root; update a custom native source with that confirmed file before resuming checks. The Docker command below maps both paths to the same host file, so no copying is needed.

`ASC_REVIEW_STATE_ROOT` selects the overlay/lock directory shared with the service
(default: the private root). To run the container yourself from this repository (run `./script/docker/setup.sh` once to create the private directories):

```sh
asc_state_name='ASC MCP'
asc_state_base="$HOME/.config/asc-mcp/state/Library/Application Support"
if [ ! -e "$asc_state_base/ASC MCP" ] && [ ! -L "$asc_state_base/ASC MCP" ] &&
   { [ -e "$asc_state_base/InterceptorReview" ] || [ -L "$asc_state_base/InterceptorReview" ]; }; then
  asc_state_name='InterceptorReview'
fi
asc_state_root="/state/Library/Application Support/$asc_state_name"
asc_configured_root=$(sed -n 's/^[[:space:]]*ASC_REVIEW_STATE_ROOT=//p' "$HOME/.config/asc-mcp/runtime.env" | tail -n 1)
if [ -n "$asc_configured_root" ]; then asc_state_root=$asc_configured_root; fi
docker compose --env-file ~/.config/asc-mcp/runtime.env -p asc-mcp -f compose.yaml run --rm --no-deps \
  --env HOME=/login-home \
  --env "ASC_REVIEW_STATE_ROOT=$asc_state_root" \
  --env ASC_REVIEW_TRUST_ROOT=/login-trust \
  --volume ~/.config/asc-mcp/credentials:'/login-home/Library/Application Support/ASC MCP':rw \
  --volume ~/.config/asc-mcp/credentials-login:/login-trust:rw \
  asc-mcp reviewer-login
```

A session can expire or be revoked by Apple. Monitoring reports authentication
failure and does not automatically prompt for credentials; rerun terminal login
when required. The configured 30-minute monitor only
checks, analyses and acknowledges reviewed snapshots. It never automatically
replies, uploads a build or submits an app.

On 2026-10-05, user-completed native login and authenticated reviewer reads through
the Rust Docker client were confirmed, including the latest Apple reply and local
snapshot acknowledgement. This validates that account/session at that time, not
every authentication branch, every Apple ID or future session validity. The
public `.p8` API calls and legacy reply delivery have not been live verified.

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
before saving private canonical cookies. Native login and authenticated reviewer
reads were confirmed for this project on 2026-10-05; other authentication paths
and reply delivery remain unverified. See [login implementation limits](docs/rust-login.md).

Reviewer-only MCP requires `ASC_REVIEW_COOKIE_PATH` (or the default private
session). It does not require official API-key credentials. Optional tools:

| Tool | Purpose |
| --- | --- |
| `list_reviewer_threads` | Read submission conversations |
| `get_reviewer_messages` | Read messages, actors and attachment references |
| `get_legacy_reviewer_context` | Inspect the old reply context |
| `check_reviewer_updates` | Read unseen/changed Apple messages |
| `acknowledge_reviewer_updates` | Acknowledge the exact analysed snapshot locally |
| `list_review_apps` | List apps visible to the session (all bounded pages) |
| `get_app_review_status` | Versions and review submissions for one numeric `app_id`, raw states |
| `get_all_app_review_statuses` | The same for every visible app; partial results say `complete: false` |
| `get_app_rejection_details` | Current rejection and its latest linked submission's Apple messages |
| `get_review_connection_status` | Session check for the current provider (no identity) |

The five status tools are read-only: no login, POST, team switch, or monitor
state/lock write. Limits: they see only the provider of the current session;
older rejection messages are historical and labelled so; a `COMPLETE` review
submission is not approval; when the latest submission cannot be identified the
result is `unknown`/`complete: false`, never "no issues". See
[reviewer monitoring](docs/rust-reviewer.md#review-status-tools).

`reply_reviewer_message` remains **experimental and disabled by default**;
`ASC_REVIEW_ENABLE_LEGACY_REPLY=true` exposes the old reply route for explicit
validation. A request accepted by Apple is not a read-back delivery confirmation.
Reply attachments are not implemented. Neither checks nor acknowledgements send
messages or submit an app.

On this project's Mac setup, private cookies, snapshots and state live under
`~/Library/Application Support/ASC MCP/`, outside Git. Files use mode
600 and the directory mode 700. An OS lock excludes concurrent monitors and
releases on process exit. Acknowledgements require a snapshot token so a newer,
unread message cannot be marked seen accidentally. Details:
[reviewer monitoring](docs/rust-reviewer.md).

## CI and deployment

- `.github/workflows/integration.yaml` validates development branches and pull
  requests: formatting, strict Clippy, tests, release builds and CLI smoke on
  Linux/macOS/Windows, plus a distroless Docker build and Compose validation.
- `.github/workflows/deployment.yaml` runs on `master` pushes, `v*` tags
  and manual dispatch. It reuses Linux integration checks before publishing;
  three-OS validation remains in the PR workflow. Native
  amd64/arm64 runners build and smoke each image, then publish one multi-platform
  manifest to `ghcr.io/qtmleap/asc-mcp` with package-version and commit tags;
  default-branch builds also update `latest`. Other manual branch runs cannot
  publish release tags. Release tags must match the version in `Cargo.toml`.
- `.github/workflows/release.yml` packages native OS binaries for version tags;
  manual snapshot runs only upload workflow artifacts.

GHCR authentication uses GitHub Actions' `GITHUB_TOKEN` with `packages: write`;
no Apple API keys, Apple ID sessions or signing credentials are needed for CI/CD.
This deployment distributes the MCP image; it does not deploy Interceptor to
App Store Connect or replace running local containers automatically.

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
