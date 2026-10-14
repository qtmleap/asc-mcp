# asc-mcp — App Store Connect MCP

A general-purpose MCP server for Apple App Store Connect: manage app metadata,
builds, submissions, TestFlight and the other official API areas, and communicate
with App Review from the same MCP client.

This fork adds App Review conversations to
[antisynthesis/asc-mcp](https://github.com/antisynthesis/asc-mcp). Sign in once to
save an App Store Connect web session, then reuse it across calls and server
restarts to check review status, read Apple's messages and explicitly send a
reply. Cookie updates are persisted; optional macOS Keychain renewal can refresh
expired sessions. Apple may still require verification or a new login. These are
App Review conversations, separate from public App Store customer reviews.

The shipped server, uploads, monitor and CLI login run natively in Rust. No Go,
Ruby, Python or fastlane subprocess is required at runtime.

## Differences from upstream

| Area | Upstream `antisynthesis/asc-mcp` | This fork |
| --- | --- | --- |
| Official App Store Connect API | 356 tools covering metadata, builds, submissions, TestFlight, commerce, Game Center, reports and more | Retains those tool names and capability areas; see the compatibility notes below |
| App Review conversations | Official API submission/review-information tools and public customer-review responses | Adds session-authenticated review status, rejection details, Apple/developer messages, update monitoring and explicit App Review replies |
| Reviewer session | Official API authentication uses a `.p8` key, Key ID and Issuer ID | Adds native Apple Account login/import, private saved cookies and persisted cookie rotation; optional macOS Keychain renewal, with manual login when Apple requires verification |
| Reply confirmation and recovery | No App Review conversation reply flow | Draft-then-send with context checks, durable attempt records, GET-only delivery confirmation and explicit unsent-draft recovery; no automatic reply or POST replay |
| Implementation and distribution | Go server, Docker image, stdio and Streamable HTTP | Rust server with the same transports, a nonroot distroless HTTP sidecar, standalone/shared Compose helpers and self-hosted multi-platform image publishing |

Retained tool names do not imply identical arguments or response formatting.
This fork repairs several Apple-schema mappings, reports formerly ignored
unsupported arguments as errors, and returns ordinary API results as JSON text
with structured content. See the
[public API compatibility ledger](migration/public-api-coverage.md) for the
specific changes and offline verification.

Official API tools still require team API-key credentials. Reviewer tools use a
separate Apple Account web session and see only its current provider/team and
accessible apps. The two credential types do not replace each other. Reviewer
operations use undocumented Iris endpoints; reply attachments are not supported.

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

## Docker deployment

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

# Codex stdio MCP for the running shared LocalGPT stack.
codex mcp add asc-review -- "$PWD/script/docker/mcp.sh"
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
fourteen reviewer tools with a session alone (five status tools, five
conversation/monitor tools and four reply/recovery tools). It adds the 356 public API tools when
team API credentials are configured; actual access depends on Apple's role
permissions. Having a key does not grant every tool permission.

| Task | Credentials / environment | Supported behavior and limits |
| --- | --- | --- |
| List App Review conversations | Apple ID session with access to the app | `list_reviewer_threads`; implemented |
| Read Apple/developer messages, dates, rejection and attachment references | Same session | `get_reviewer_messages`; implemented; attachment file download is not implemented |
| Check new or changed Apple replies | Same session | `check_reviewer_updates`; compares messages with the saved local snapshot |
| Record that a checked snapshot has been reviewed | Local monitor state and exact `snapshot_token` | `acknowledge_reviewer_updates`; changes local state only, not Apple's read status |
| List visible apps; read versions and review submissions with raw Apple states; read the current rejection and Apple messages; check the session | Same session | Five read-only status tools; current provider only, no monitor state changes |
| Inspect old reply context | Same session | `get_legacy_reviewer_context`; experimental legacy endpoint; not live verified |
| Send a reply to App Review | Same session | `reply_app_review_message` creates and sends a draft; `get_app_review_reply_status` confirms delivery by reading the new developer message. Explicit reset/resume tools handle limited unsent-draft cases. No reply attachments or automatic sending |
| Read/update app metadata, versions and review information; upload screenshots/previews | Team `.p8`, Key ID, Issuer ID and sufficient role | Public API tools implemented and tested offline; live access pending |
| Upload an already-built artifact, select a build, submit for review | Same team API credentials; App Manager for submission | Implemented and tested offline; live upload/submission pending |
| Manage TestFlight, provisioning certificates/profiles, commerce, Game Center, reports and Xcode Cloud | Same team API credentials with permissions for the requested endpoint | Implemented public tools; some tasks require roles beyond App Manager; live access pending |
| Build/archive an Xcode project, sign locally and export an IPA | A Mac with Xcode, signing certificate **and its private key**, provisioning profile and project | Not implemented as MCP tools; putting these credentials in the container does not enable these steps |

An API `.p8` authorizes official API calls; it is not an app-signing private key.
The current implementation uses team-key JWTs with Issuer ID. Individual API keys
are not supported. App Manager is the intended role for app metadata, build
selection and resubmission; finance/account administration may require different
roles. Team keys cannot be restricted to a single app. Apple's role sources
are linked in the Docker setup above.

The reviewer tools do not expose all App Store Connect features just because an
Apple ID session can access them in the website. In particular, this session-only
client does not implement general metadata edits, build upload or submission.
App Review conversations are also distinct from public App Store ratings/reviews.
The `.p8` does not replace reviewer-session authentication.

### Session setup and renewal

Run these commands from the repository root in an interactive terminal.
On macOS, install the native login/renewal helper once:

```sh
cargo build --release --locked
./script/macos/install-renewal-agent.sh --binary "$PWD/target/release/asc-mcp"
```

Then log in on macOS or Linux:

```sh
./script/docker/reviewer-login.sh standalone --fresh
```

Use `stack` instead of `standalone` for the shared LocalGPT stack. The helper
configures private session storage and reuses your saved account and provider.
On macOS it uses the native helper and Keychain; on Linux it runs a disposable
login container. Later logins can omit `--fresh`; use it again to replace a stale
password. Passwords and verification codes are entered only in the terminal.

The macOS agent checks the session every 15 minutes. To let the Docker service
wait for renewal after a failed GET, add this to private
`~/.config/asc-mcp/runtime.env`:

```dotenv
ASC_REVIEW_RENEWAL_WAIT_SECS=30
```

Then recreate the service with the same deployment mode:

```sh
./script/docker/compose.sh standalone up -d --force-recreate asc-mcp
```

Apple may still require interactive verification. Linux uses manual login;
POSTs are never replayed. Set `ASC_REVIEW_APP_ID` only when using message
monitoring; per-app status and reply tools take their own app IDs.

See [Docker login and renewal](docs/docker-sidecar.md#log-in-or-renew-the-reviewer-session)
for custom paths, private storage, provider changes and how to stop the agent.
The scripts handle existing legacy storage automatically; there is no need to
assemble the Docker mounts or state-path selection by hand.

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
remain unverified. Reply delivery was confirmed by a live read-back on 2026-10-08. See [login implementation limits](docs/rust-login.md).

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
| `reply_app_review_message` | Explicitly create and send a reply after validating the expected Apple message and thread context |
| `get_app_review_reply_status` | Confirm delivery with GET-only read-back; inspect pending or uncertain attempts |
| `reset_rejected_app_review_reply` | Explicitly reset a definitively rejected draft-creation record after remote checks; no send |
| `resume_unsent_app_review_reply` | Explicitly send an unchanged, owned, attachment-free draft with no previous send attempt |

The five status tools are read-only: no login, POST, team switch, or monitor
state/lock write. Limits: they see only the provider of the current session;
older rejection messages are historical and labelled so; a `COMPLETE` review
submission is not approval; when the latest submission cannot be identified the
result is `unknown`/`complete: false`, never "no issues". See
[reviewer monitoring](docs/rust-reviewer.md#review-status-tools).

`reply_app_review_message` replies through the two-step flow the App Store Connect
web frontend uses (request shapes taken from its published JavaScript bundle
<https://appstoreconnect.apple.com/access/m-appstore/static/js/main.a52d4a9ea996a295dcaa.js>,
not from Apple documentation): `POST
/iris/v1/resolutionCenterDraftMessages`, then `POST /iris/v1/resolutionCenterMessages`
with `createFromDraftMessage`. It first verifies app → submission → thread membership,
`canDeveloperAddNote`, that `expected_apple_message_id` is still Apple's newest message,
and that no draft or identical developer message exists. It re-checks context and
then reads the draft again immediately before sending. External edits cannot be
locked atomically across Apple's GET and POST requests. The whole operation is bound to one session source
generation; a replaced source stops it before any further request
(`reviewer_session_changed`). POSTs are never retried or replayed. A durable
private per-thread record is written before each POST; any unfinished or uncertain
attempt (`delivery_unknown`) blocks all later sends on that thread, including restarts and
different bodies, until a person inspects the thread in App Store Connect. The record is
never removed automatically. `get_app_review_reply_status` uses GET requests only and
may mark an attempt delivered when a new developer message with the same body appears.
Nothing is claimed as delivered without that read-back. On 2026-10-08, an explicitly
resumed draft was delivered and confirmed through the status tool. Apple returned a
non-JSON send response; the client did not retry the POST and confirmed delivery with GET.

The legacy `reply_reviewer_message` remains **experimental, disabled by default and unverified (its route returned 404)**;
`ASC_REVIEW_ENABLE_LEGACY_REPLY=true` exposes the old reply route for explicit
validation. A request accepted by Apple is not a read-back delivery confirmation.
Reply attachments are not implemented. Neither checks nor acknowledgements send
messages or submit an app.

By default on native runs, private cookies, snapshots and state live under
`~/Library/Application Support/ASC MCP/`, outside Git. Files use mode
600 and the directory mode 700. An OS lock excludes concurrent monitors and
releases on process exit. Acknowledgements require a snapshot token so a newer,
unread message cannot be marked seen accidentally. Details:
[reviewer monitoring](docs/rust-reviewer.md).

Reply status distinguishes `not_attempted`, `not_sent` (no message-send POST was
made), `delivery_unknown`, and `delivered`. `reset_rejected_app_review_reply`
accepts the same `app_id`, `review_submission_id`, and `thread_id` as the status
tool. It only resets a local `draft_rejected` attempt after GET checks confirm
there is no remote draft or matching developer message. It preserves the local
record as `reset`, makes no Apple mutation, and does not resend. It cannot reset
an interrupted draft creation or an unknown send. After a reset, a separate,
explicitly requested reply call performs all preflight checks again. Existing
remote drafts are never modified or deleted.

`resume_unsent_app_review_reply` adds `draft_id` to the original reply's arguments.
It is an explicit first-send operation for a verified draft when create-response
validation was interrupted, or for a draft left unsent. It cannot resume any
`send_attempted`, `accepted`, or `delivered` record. The original target, expected
Apple message and body must be unchanged. The supplied draft ID, creator (current
signed-in USER), body, empty attachments and creation time are checked before the
shared send path runs. Status returns a safe pending-draft summary with `draft_id`
and `resumable`, without its body or creator email. No draft is recreated and an
unknown send is never retried.

## CI and deployment

- `.github/workflows/integration.yaml` validates development branches and pull
  requests: formatting, strict Clippy, tests, release builds and CLI smoke
  on self-hosted Linux X64 Docker and `macos-latest` runners by default, plus a
  distroless Docker build and Compose validation. Manual runs accept `test-os`,
  for example `["Linux","macos-latest","Windows"]`; Windows checks require a
  matching self-hosted Windows runner.
- `.github/workflows/deployment.yaml` runs on `master` pushes, `v*` tags
  and manual dispatch. It reuses Linux integration checks before publishing.
  The self-hosted Linux X64 Docker fleet builds the amd64 image natively and the
  arm64 image under QEMU emulation (slower, 90-minute limit), smokes each image,
  then publishes one multi-platform
  manifest to `ghcr.io/qtmleap/asc-mcp` with package-version and commit tags;
  default-branch builds also update `latest`. Other manual branch runs cannot
  publish release tags. Release tags must match the version in `Cargo.toml`.
- `.github/workflows/release.yml` packages native OS binaries for version tags;
  manual snapshot runs only upload workflow artifacts. Its Linux, macOS and
  Windows jobs target native self-hosted runners; macOS selects `macos-latest`,
  and Windows requires a matching Windows runner.

GHCR authentication uses GitHub Actions' `GITHUB_TOKEN` with `packages: write`;
no Apple API keys, Apple ID sessions or signing credentials are needed for CI/CD.
This deployment distributes the MCP image; it does not deploy your app to
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

For a native setup, build the binary above and run from the repository root:

```sh
./target/release/asc-mcp reviewer-login
codex mcp add asc-review -- "$PWD/target/release/asc-mcp" serve
```

This uses the native binary's own saved session; the Docker renewal agent does
not renew that default native session. For the shared LocalGPT stack, use the
Docker MCP registration shown above. Standalone Docker clients connect to
`http://127.0.0.1:8878/mcp` with the bearer token from private `runtime.env`.

For a custom native session location, configure `ASC_REVIEW_COOKIE_PATH`.
For the official API tools, also configure
the three API credential variables.
See [SETUP.md](SETUP.md) and `config/config.sample.env`. Never put private keys,
cookies or passwords in MCP tool arguments or Git.

## License

MIT. The original tool definitions and behaviour are derived from the upstream
Go project; its licence is retained in [LICENSE](LICENSE).
