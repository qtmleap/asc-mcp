# asc-mcp — App Store Connect MCP

A general-purpose MCP server for Apple App Store Connect: app metadata, builds,
submissions, TestFlight and the other official API areas, plus **App Review
conversations** from the same MCP client.

This fork extends [antisynthesis/asc-mcp](https://github.com/antisynthesis/asc-mcp).
Its differentiator is a persisted reviewer session: sign in once, then read App
Review status and Apple's messages, and send an explicit, read-back-verified
reply across server restarts. These are App Review conversations, not public App
Store customer reviews. The server is a Rust binary built inside the Docker image.

## Quick start (Docker)

Prerequisites: Docker with Linux containers, Compose 2.30+ with BuildKit, Git,
curl, a POSIX shell and OpenSSL for the setup script. No host Rust toolchain is needed
to run the server. The Compose file builds the checked-out source, so you run
what you cloned (the published `latest` image may lag).

```sh
git clone https://github.com/qtmleap/asc-mcp.git && cd asc-mcp
./script/docker/setup.sh
```

`setup.sh` creates private directories under `~/.config/asc-mcp` and a
`runtime.env` holding a random MCP bearer token and your host UID/GID. It never
overwrites an existing file; do not replace those generated values. These examples
use that default directory. For another location, set `ASC_MCP_CONFIG_DIR` before
each helper command and adjust the credential paths below.

### 1. Get a Team API Key

Skip this for reviewer-only use ([below](#reviewer-session-optional)).

1. **Request access.** The Account Holder opens *Users and Access > Integrations >
   App Store Connect API* and selects *Request Access*. Approval may be pending.
2. **Generate a Team Key.** The Account Holder or an Admin opens *Team Keys*,
   selects *Generate API Key* (or **+**), enters a name and an *Access* role, then
   selects *Generate*.
3. **Download the `.p8` once** with *Download API Key* beside the new key. Apple
   will not offer it again. Record the **Key ID** and the **Issuer ID** (not your
   Developer Team ID).

Choosing the role:

- Team Key roles apply to all apps; a key cannot be limited to one app. Name and
  role cannot be edited, so revoke and regenerate to change them.
- **App Manager** covers metadata, build selection and review submission.
  Developer can upload builds but cannot submit for review.
- App Manager does not cover all 356 tools. Finance, user and provisioning areas
  follow Apple's role table. For financial-report-only use, choose Finance access;
  Admin also permits it but grants broader permissions. Apple's web-report roles
  are Account Holder, Admin and Finance;
  see [financial-report permissions](https://developer.apple.com/help/app-store-connect/getting-paid/download-financial-reports/).
- Only roles of the Apple Account that signs in matter for the separate reviewer
  session; see below.
- This server signs issuer-based **Team Key** JWTs. Apple also has Individual
  Keys, which this implementation does not support.

Sources: [request and generate keys](https://developer.apple.com/help/app-store-connect/get-started/app-store-connect-api/),
[key download and role scope](https://developer.apple.com/documentation/appstoreconnectapi/creating-api-keys-for-app-store-connect-api),
[Key ID and Issuer ID](https://developer.apple.com/documentation/appstoreconnectapi/generating-tokens-for-api-requests),
[submitting an app](https://developer.apple.com/help/app-store-connect/manage-submissions-to-app-review/submit-an-app/),
[uploading builds](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds/),
[replying to App Review](https://developer.apple.com/help/app-store-connect/manage-submissions-to-app-review/reply-to-app-review-messages/),
[role permissions](https://developer.apple.com/help/app-store-connect/reference/account-management/role-permissions/).

### 2. Configure credentials

Copy the key (owned by you, mode 600) and add all three variables to the private
`runtime.env`. Configure all three together, or none for reviewer-only use.
`/run/asc-mcp/AuthKey.p8` is the path **inside the container**, not on your host.

```sh
cp ~/Downloads/AuthKey_XXXXXXXXXX.p8 ~/.config/asc-mcp/credentials/AuthKey.p8
chmod 600 ~/.config/asc-mcp/credentials/AuthKey.p8
```

Edit the existing `~/.config/asc-mcp/runtime.env`, keeping its generated entries,
and set these values with your actual IDs. Update existing lines if already
configured, instead of appending duplicates:

```dotenv
ASC_ISSUER_ID=your-issuer-id
ASC_KEY_ID=your-key-id
ASC_PRIVATE_KEY_PATH=/run/asc-mcp/AuthKey.p8
```

### 3. Start and check

```sh
./script/docker/compose.sh standalone
curl -fsS http://127.0.0.1:8878/healthz
./script/docker/compose.sh standalone run --rm --no-deps asc-mcp validate
```

`healthz` only shows the server is up. `validate` checks local configuration, not
Apple access. If a Team Key is configured, call `list_apps` from your MCP client
to verify an actual API read. For reviewer-only use, complete login below and
call `get_review_connection_status` instead.

The helper handles all mounts. The image is distroless and nonroot with a
read-only root filesystem; credentials are mounted read-only and never baked in.
Details: [docs/docker-sidecar.md](docs/docker-sidecar.md).

After changing credentials or `runtime.env`, recreate the service with
`./script/docker/compose.sh standalone up -d --force-recreate asc-mcp`.

## Connect a client

The endpoint is `http://127.0.0.1:8878/mcp` (loopback only). Clients must send
the bearer token from `runtime.env`; it protects the MCP endpoint and is **not**
an Apple JWT.
If you set `ASC_MCP_PORT` in `runtime.env`, use that port instead of 8878 in the
client URL and health check.

| Setting | Value |
| --- | --- |
| URL | `http://127.0.0.1:8878/mcp` |
| Header | `Authorization: Bearer <ASC_MCP_AUTH_TOKENS value>` |

For Codex, set `ASC_MCP_AUTH_TOKENS` to the matching private token in the
environment that launches Codex, then register the endpoint:

```sh
codex mcp add asc-mcp --url http://127.0.0.1:8878/mcp \
  --bearer-token-env-var ASC_MCP_AUTH_TOKENS
```

This flag names the client's environment variable; it does not load `runtime.env`
or update an already-running Codex process. Other MCP clients can set the
authorization header directly in their private connection settings.

## Reviewer session (optional)

App Review conversations use a separate Apple Account web session, saved
privately and persisted across restarts.

- The Apple Account needs current provider/app access. Replying requires the
  Account Holder, Admin or App Manager role.
- A Team Key alone cannot read or send review conversations. A session alone
  cannot edit metadata, upload builds or submit.
- Cookie updates are persisted and rotated, but the session expires. Apple may
  demand verification again; optional macOS Keychain renewal cannot bypass that.

**macOS prerequisite:** `reviewer-login.sh` on Darwin needs the stable native
helper. With Rust 1.88+ and Python 3 on the host, install it once before login:

```sh
cargo build --release --locked
./script/macos/install-renewal-agent.sh --binary "$PWD/target/release/asc-mcp"
```

This host helper handles macOS login and Keychain renewal; the MCP server stays
in Docker. Linux runs a disposable login container and can skip those two steps.
See [login and renewal details](docs/docker-sidecar.md#log-in-or-renew-the-reviewer-session).

```sh
./script/docker/reviewer-login.sh standalone --fresh
```

Run it in an interactive terminal; passwords and codes are typed only there.
macOS stores the confirmed password in Keychain for renewal; Linux does not store
passwords, and verification codes are not persisted. `--fresh` skips the
saved-session preflight for first enrollment or recovery; later logins can omit
it. Pass `standalone` explicitly. For optional
renewal waiting, set `ASC_REVIEW_RENEWAL_WAIT_SECS` in `runtime.env` and recreate
the service with `./script/docker/compose.sh standalone up -d --force-recreate asc-mcp`
([details](docs/docker-sidecar.md#log-in-or-renew-the-reviewer-session)).

## Optional: shared LocalGPT stack

To run beside an existing LocalGPT checkout (sibling `../local-gpt` or
`LOCALGPT_DIR`), use the `stack` mode. Both modes share the same private
credentials.

```sh
./script/docker/compose.sh stack
codex mcp add asc-review -- "$PWD/script/docker/mcp.sh"
```

`mcp.sh` is a stdio bridge for the **stack only**: it uses the already-running
stack container. Standalone users connect over HTTP as above. Containers on the
stack reach the service at `http://asc-mcp:8080/mcp`.

## What you get

| Area | Needs | Notes |
| --- | --- | --- |
| 356 official API tools: metadata, versions, review submission, screenshots, TestFlight, provisioning, commerce, Game Center, reports, Xcode Cloud | Team Key with a role that permits the endpoint | Same tool names as upstream; see compatibility below |
| 14 reviewer tools: list apps, read versions/submissions/rejections, read and monitor Apple messages, send a reply, confirm or recover it | Reviewer session | Reviewer-only mode needs no API key |

Not implemented: building/archiving/signing an Xcode project (an API `.p8` is not
a signing key), reply attachments, attachment download, automatic replies.

### Reply safety

Replies use draft, then send, then GET read-back:

- Context checks first: app, submission, thread and the newest Apple message.
- A durable private record is written before each POST. An unfinished or unknown
  send (`delivery_unknown`) blocks every later send on that thread, even after a
  restart, until a person checks it in App Store Connect.
- No automatic reply, no POST replay. Delivery is claimed only after GET read-back.
- Status, reset and resume tools handle limited unsent-draft cases.

Tool-by-tool behaviour and recovery: [docs/rust-reviewer.md](docs/rust-reviewer.md),
[docs/rust-login.md](docs/rust-login.md).

## Differences from upstream

| Area | Upstream `antisynthesis/asc-mcp` | This fork |
| --- | --- | --- |
| Official API | 356 tools | Same tool names; several Apple-schema repairs, unsupported arguments reported as errors, JSON text plus structured results |
| App Review conversations | Official submission/review-information tools only | Session-based status, rejection details, Apple messages, update monitoring, verified replies |
| Reviewer session | Not applicable (`.p8`/Key ID/Issuer ID only) | Native Apple login/import, private persisted cookies, optional Keychain renewal |
| Reply recovery | None | Durable attempt records, GET-only confirmation, explicit unsent-draft recovery |
| Implementation | Go; Docker, stdio, Streamable HTTP | Rust; same transports, nonroot distroless HTTP service, Compose helpers |

Retained names do not imply identical arguments or output; see the
[public API compatibility ledger](migration/public-api-coverage.md).

## Limits

- Reviewer tools use undocumented Apple Iris endpoints and may break without notice.
- Native login and reviewer reads were confirmed for this project on 2026-10-05, and
  one reply delivery on 2026-10-08. Other authentication paths remain unverified.
  Live official-API access is pending; offline tests do not prove Apple accepts writes.
- Reviewer tools see only the current session's provider and apps.
- Tool calls change production App Store Connect data when credentials permit.
- Never put keys, cookies or passwords in tool arguments or Git.

## Native development, CI and migration

Building without Docker, native `serve`/`serve-http` and CLI usage, CI,
deployment and migration verification are in [SETUP.md](SETUP.md).

## License

MIT. Original tool definitions and behaviour derive from the upstream Go project;
its licence is retained in [LICENSE](LICENSE).
