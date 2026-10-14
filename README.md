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
seventeen session-backed tools with a session alone (five status tools, five conversation/monitor tools, four reply/recovery tools and three App Privacy tools). It adds 365 public API tools (356 legacy and nine native additions) when
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
| Send a reply to App Review | Same session | `reply_app_review_message` (modern two-step Iris draft then send) and `get_app_review_reply_status`; tested offline and **live verified on 2026-10-08**: one existing draft was explicitly resumed and delivery confirmed by reading the new developer message. The legacy `reply_reviewer_message` route returned 404 and stays experimental, disabled by default and unverified (`ASC_REVIEW_ENABLE_LEGACY_REPLY=true`); no reply attachments |
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


Use your primary Apple Account email. Interactive login remembers the confirmed account and App Store Connect provider in private host-only storage. On macOS it saves the confirmed password in Keychain through native APIs and later reuses it. Passwords and codes never appear in argv, environment variables, files or MCP output. `--fresh` bypasses preflight and requests a replacement password; use it for initial enrollment or a stale password. `--accept-provider-change` explicitly permits a different confirmed Apple team; unattended renewal never changes the pinned team.

For macOS Docker installations, install the stable native helper once:

```sh
cargo build --release --locked
./script/macos/install-renewal-agent.sh --binary "$PWD/target/release/asc-mcp"
./script/docker/reviewer-login.sh stack --fresh
ASC_REVIEW_RENEWAL_WAIT_SECS=30 ./script/docker/compose.sh stack up -d --build asc-mcp
```

The first interactive login enrolls the account/password and handles any Apple verification. The host LaunchAgent checks the effective session every 15 minutes and wakes when a reviewer GET encounters authentication failure. It owns Keychain and the separate `credentials-login` trust directory. The container receives only confirmed ASC cookies and sanitized renewal status. Verification requirements, rejected credentials and team changes stop repeated attempts until successful human login. Locked/unavailable Keychain access also returns a user-action state without opening unattended UI.

The service retries GET once, waiting at most 30 seconds; POST is never replayed. Enable recovery persistently by adding `ASC_REVIEW_RENEWAL_WAIT_SECS=30` to private `runtime.env` and recreating only `asc-mcp`. Recovery is disabled by default without a host agent. Linux retains manual Docker login and remembered account, without password storage or unattended renewal.

`ASC_REVIEW_COOKIE_PATH` is both the session source and confirmed-login destination. `ASC_REVIEW_TRUST_ROOT` selects the separate login-only directory. The Mac helper maps existing Docker paths to their host counterparts, preserving legacy state-directory selection. A changed source generation invalidates older cookie overlays. Passwords and login trust are never mounted into the running service.

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

A session can expire or be revoked by Apple. With Mac renewal enabled, expired
sessions are renewed using Keychain and trusted-device cookies. Run interactive
login when Apple requires verification or refuses stored credentials. Scheduled
checks never prompt or request verification codes themselves. The configured 30-minute monitor only
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

Nine additions bring the public catalogue to **365 tools**:

- `get_app_store_version_build` reads the related selected build.
- `set_app_store_version_build` selects a processed, eligible build by resource ID. A successful 204 response returns `{"status":"success"}`; read back the selection before retrying an uncertain write.
- `create_screenshot_set` creates a display-specific set for an App Store version localization. Check `list_screenshot_sets` first, then use `upload_app_screenshot` to upload the image.
- `set_app_content_rights` records the confirmed publisher declaration using AppUpdateRequest.
- `get_app_info` reads one app info with optional `include` and `fields` (for example `primaryCategory`, `ageRatingDeclaration`).
- `update_app_info_categories` sets `primary_category_id` and optionally `secondary_category_id` (IDs from `list_app_categories`).
- `list_app_price_schedule_prices` lists `MANUAL` or `AUTOMATIC` prices of a schedule (`include=appPricePoint,territory`) to read back actual customer prices, such as a zero price.

Repairs to existing tools: `set_app_price_schedule` now sends Apple `${price-N}` inline IDs and requires a non-empty `prices` array (a free app uses the zero-price point; an empty array is rejected).
`list_app_store_review_attachments` takes `review_detail_id`; the legacy `version_id` remains an alias of that review-detail ID (not a version ID) and conflicting values are rejected.
`update_age_rating_declaration` additionally accepts Apple's `advertising`, `guns_or_other_weapons`, `health_or_wellness_topics`, `kids_age_band`, `loot_box`, `messaging_and_chat`, `parental_controls`, `age_assurance`, `social_media`, `social_media_age_restricted`, `user_generated_content`, `age_rating_override_v2`, `korea_age_rating_override` and `developer_age_rating_info_url`; unknown arguments, invalid enum values and nulls are rejected.

Public API failures preserve the HTTP status and bounded, sanitized Apple error
codes, titles, details and source pointers in both text and structured content.
Response bodies and headers are never echoed. Error text is untrusted source
material, not instructions.

If `submit_app_for_review` fails after creating a draft, its error includes
`submission_id`, `completed_steps`, `failed_step`, `cause` and `outcome_unknown`.
Read the existing submission and use `add_review_submission_item` or
`submit_review_submission` to resume; do not create another draft blindly.
An unknown write outcome requires a read-back check. Writes are never retried
automatically. App Privacy questionnaire answers have no supported public API
in the bundled Apple schema. With the existing Apple ID reviewer session enabled,
`get_app_privacy_details`, `declare_app_privacy_no_data`, and
`publish_app_privacy_no_data`, `set_app_privacy_collection`, and
`publish_app_privacy_collection` use the private Iris routes implemented by fastlane
2.239.0. The No Data write tools require `confirmation: "DATA_NOT_COLLECTED"`, refuse
existing collection disclosures, and never delete them. A durable uncertain-write
record blocks retries until a GET conclusively confirms the desired state.
These routes are not covered by Apple's public API compatibility guarantees.
The collection tools replace or publish an explicit complete disclosure matrix,
with a fresh exact snapshot digest and separate confirmation. See
[collection questionnaire editing](docs/privacy-collection.md) for the schemas,
publish attestations, and conservative handling of partial draft writes.

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

The native additions also include `get_app_usage_report` and `get_app_crash_report`,
described below.

### Apple crash and usage reports

Apple collects App Store diagnostics and usage data without an analytics SDK in
your app. The native `get_app_usage_report` and `get_app_crash_report` tools read
Apple's Analytics Reports API, download its report segments, and return parsed
metrics with the reporting date, processing date, granularity and source IDs.
They supplement the original public catalogue; they do not replace its tools.

Both tools require `app_id`. Optional `report_date` is the **event date**, not
the processing date; omitting it selects the latest available event date.
`granularity` accepts `DAILY` (default), `WEEKLY` or `MONTHLY`. Weekly dates refer
to Monday and monthly dates to the first day of the month. For example, use
`get_app_usage_report` with `{"app_id":"6749347474","granularity":"DAILY"}`
to inspect Interceptor's latest App Store usage report, or use
`get_app_crash_report` with the same arguments for crash counts.

Usage rows include sessions and Apple's unique-device count for each reported
combination of date, app version, device and other dimensions. Crash rows include
crashes and the number of devices that crashed. Unique-device counts are **not
additive** across rows or dates, so `unique_devices_total` is `null` rather than
an inflated estimate. These are opt-in device metrics, not a count of all users
or installations. No device identifiers or capture contents are collected by
this server or added to the app.

Before the first use:

1. Configure a **team API key with Admin access** using the existing
   `ASC_ISSUER_ID`, `ASC_KEY_ID` and `ASC_PRIVATE_KEY_PATH` settings. Keep the
   `.p8` in the private credentials bind, never in Git or a chat message.
2. Call `list_analytics_report_requests` for the app. If there is no active
   `ONGOING` request, explicitly call `create_analytics_report_request` with
   `{"app_id":"6749347474","access_type":"ONGOING"}`. Reading a report never
   creates a request or changes Apple-side settings.
3. Allow approximately 24–48 hours for Apple to generate the first reports.
   Once requests exist, Admin, Sales and Reports, or Finance keys can read
   generated reports. App Manager alone does not grant Analytics Reports access.

Results distinguish available data, generation/setup pending, missing data,
unavailable data and permission failures. Missing reports are not reported as
zero usage or zero crashes. App Sessions and App Crashes include only people
who agreed to share diagnostics, have privacy thresholds, and may take five
days to finalize daily data. Weekly and monthly reports are complete when
published; later corrections can still replace previously published data.
For a reporting date, newer processing-date data replaces older data instead
of being added to it.

TestFlight uses separate existing tools: `get_beta_build_usage_metrics` returns
build-level installs, sessions and crashes; `list_beta_feedback_crashes` and
`get_beta_feedback_crash_log` read submitted crash feedback. An empty feedback
list is not proof that a build has never crashed. For symbolicated production
stack traces, use Xcode's Crashes Organizer with the matching archive/dSYMs;
the App Crashes analytics report contains aggregate counts, not stack traces.

Reads are bounded to 25,000 parsed rows and 2,000 returned rows, with 32 MiB
compressed and 64 MiB expanded data per call. Results expose the limits and flag
truncated rows; complete additive totals still include all rows for the date.
Exceeding a processing limit returns `unavailable`, not a partial total.

Instance metadata is read in full (up to 2,000 instances, at most 10 pages of 200)
before sorting newest `processingDate` first; a larger list is `unavailable`.
Segments are then downloaded only until the result is settled: the default search
stops once the next `processingDate` is earlier than the greatest event date
already seen, or equals it but is older than the winning processing date
(an instance cannot hold event dates after its own processing date),
and an explicit `report_date` search stops once the next `processingDate` is
earlier than that date or than the processing date of the partition already
resolved. Only the winning date partition is retained, and it is replaced as a
whole. A `report_date` later than every `processingDate` is `no_data` without
downloading segments; rows dated after their own instance's `processingDate` make
the result `unavailable`. Two instances with the same `processingDate` holding the
selected event date are `unavailable` rather than arbitrarily chosen, but such an
overlap on a date that a different instance supersedes is ignored.

Segment gzip CRC and exact size are always checked, and a present checksum must
match. If any selected segment has no checksum, the result is still returned with
`provenance.checksum_verified: false` and a warning; otherwise it is `true`. A
blank optional `Total Session Duration` cell is omitted; a malformed non-empty one
makes the result `unavailable`.

Omit `limit` when calling `get_beta_build_usage_metrics`. Apple rejects it
unless grouping or filtering is used, and those options are not
exposed by this server.

Signed segment downloads require direct HTTPS access. The isolated downloader
does not use proxy environment variables, cookies, redirects or API credentials;
networks requiring a mandatory proxy cannot retrieve these segments.

References: [requesting and downloading reports](https://developer.apple.com/documentation/appstoreconnectapi/downloading-analytics-reports),
[App Sessions](https://developer.apple.com/documentation/analytics-reports/app-sessions),
[App Crashes](https://developer.apple.com/documentation/analytics-reports/app-crashes),
[data corrections](https://developer.apple.com/documentation/analytics-reports/data-completeness-corrections)
and [Apple crash reports](https://developer.apple.com/documentation/xcode/acquiring-crash-reports-and-diagnostic-logs).

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
and that no draft or identical developer message exists, then re-checks the draft and
context immediately before sending. The whole operation is bound to one session source
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

On this project's Mac setup, private cookies, snapshots and state live under
`~/Library/Application Support/ASC MCP/`, outside Git. Files use mode
600 and the directory mode 700. An OS lock excludes concurrent monitors and
releases on process exit. Acknowledgements require a snapshot token so a newer,
unread message cannot be marked seen accidentally. Details:
[reviewer monitoring](docs/rust-reviewer.md).

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

To stop automatic authentication, unload `work.asc-mcp.reviewer-renewal` with `launchctl bootout gui/$(id -u)/work.asc-mcp.reviewer-renewal` before removing the session source. Removing the source alone does not revoke an enrolled host helper: it can create a new session from Keychain. The Mac wrapper is the supported enrollment path on Mac; do not run a Linux login container concurrently against the host trust store.

If the native helper binary changes, macOS may require Keychain authorization for its new code identity. Re-run the Mac login wrapper with `--fresh` after an upgrade; unattended access never opens UI. Missing provider pins require this initial enrollment but do not invalidate an already healthy reviewer session. Network, Keychain-access and storage interruptions use bounded retry; identity mismatches, refused credentials and Apple verification requirements stop repeated authentication.


Reply status distinguishes `not_attempted`, `not_sent` (no message-send POST was made),
`delivery_unknown`, and `delivered`. `reset_rejected_app_review_reply` accepts the same
`app_id`, `review_submission_id`, and `thread_id` as the status tool. It only resets a
local `draft_rejected` attempt after GET checks confirm there is no remote draft or
matching developer message. It preserves the local record as `reset`, makes no Apple
mutation, and does not resend. It cannot reset an interrupted draft creation or an
unknown send. After a reset, a separate, explicitly requested reply call performs all
preflight checks again. Existing remote drafts are never modified or deleted.


`resume_unsent_app_review_reply` adds `draft_id` to the original reply's arguments.
It is an explicit first-send operation for a verified draft when create-response validation
was interrupted, or for a draft left unsent. It cannot resume any `send_attempted`,
`accepted`, or `delivered` record. The original target, expected Apple message and body
must be unchanged. The supplied draft ID, creator (current signed-in USER), body, empty
attachments and creation time are checked before the shared send path runs. Status
returns a safe pending-draft summary with `draft_id` and `resumable`, without its body or
creator email. No draft is recreated and an unknown send is never retried.

## Session-backed App Privacy tools

`get_app_privacy_details` reads the app's current disclosure and publication state
through the same Apple ID session as the reviewer tools. `declare_app_privacy_no_data`
and `publish_app_privacy_no_data` require `app_id` and the explicit string
`confirmation: "DATA_NOT_COLLECTED"`. They only support no-collection declarations,
refuse existing collection disclosures or ambiguous schemas, and never delete Apple
records. Each mutation is sent once, followed by GET verification.

A private per-app intent is saved before each write. Interrupted or unconfirmed writes
remain `outcome_unknown` and block further mutations, including after a service restart.
Use `get_app_privacy_details` to inspect them: reconciliation requires the same
unchanged authentication source and a conclusively visible single no-collection
record; publishing also requires the same publication resource with `published: true`.
An empty or conflicting readback does not clear the intent. After the authentication
source changes, status is still readable and reports
`authentication_source_matches_intent: false`, but the old intent stays blocked.
Do not delete a pending intent file or retry its mutation to resolve uncertainty.
The same private lock and intent record protect collection edits and publication:
an uncertain collection write also blocks the No Data tools. The read tool retains
its existing fields and adds `rows`, normalized `disclosures`, and `snapshot_digest`.
Unsupported or mixed collection shapes remain visible as safe rows but cannot be
edited or published by the collection tools. Full details are in
[collection questionnaire editing](docs/privacy-collection.md).
If saving a definitive rejection fails, the durable pending intent conservatively
remains blocked until conclusive verification is possible.
