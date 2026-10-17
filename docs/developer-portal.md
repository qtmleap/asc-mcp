# Experimental Developer Portal App Groups

These six opt-in tools use the Developer Portal web service. App Groups are not
an App Store Connect public API resource. A Team API key cannot authorize them.
Without a dedicated Developer session, the MCP server can reuse eligible
credentials from its configured reviewer's existing Apple ID session, then
verify access against Developer Portal. Reviewer access alone does not prove
Developer authorization. The supported platforms are macOS and Linux;
the optional feature fails explicitly on unsupported platforms.

Offline fixtures and local tests are not live Apple certification. The protocol
can change. A fresh authentication session may still be rejected by the Developer
Portal; that failure does not publish a replacement session or start recovery.

## Native setup

Build from this checkout using Rust 1.88 or newer:

```sh
cargo build --release --locked
export ASC_DEVELOPER_APP_GROUPS_ENABLED=true
./target/release/asc-mcp validate
./target/release/asc-mcp tools
./target/release/asc-mcp serve
```

Use `get_developer_connection_status` to check shared authentication. A successful
shared status reports `auth_source: shared_reviewer_session` and a live team
count, without selecting a team. Every target operation still requires an
explicit ten-character `team_id` and live membership verification. The server
uses its actual configured Reviewer, not an independent global cookie search.
No configured Reviewer means no shared fallback. Direct library calls to
`DeveloperPortal::call` retain dedicated-session behavior.

An existing dedicated source always takes precedence, including if invalid,
expired, inaccessible or symlinked. Only a missing dedicated source enables
shared authentication; there is no silent account switch. If a separate account
or pinned team is needed, the optional dedicated login remains available:

```sh
./target/release/asc-mcp developer-login --apple-id 'your-apple-id' --team-id 'ABCDE12345'
```

This optional login requires an interactive terminal. Enter passwords and verification codes
only at its prompts, never in MCP arguments, environment variables or chat. The
explicit ten-character Developer Team ID is not an ASC provider ID. Apple
requires Account Holder or Admin permissions to register an App Group; see
[Apple's registration instructions](https://developer.apple.com/help/account/identifiers/register-an-app-group).
The selected team is pinned in the dedicated session and checked against live
membership before an operation. Switching team requires explicit fresh login.

The tools remain disabled by default; `developer-login` itself does not enable
them. With only Developer tools enabled, discovery exposes those six tools and
`validate` accepts that local configuration. `validate` does not test live login.
The public API retains its API-key authentication; reviewer configuration stays
unchanged. A second login is not intrinsically required by this feature. Live
Apple acceptance of shared authentication has not been verified by this change.

Native state defaults to
`~/Library/Application Support/ASC MCP/Developer Portal/`. Override the directory
with `ASC_DEVELOPER_STATE_ROOT` and the source file with
`ASC_DEVELOPER_COOKIE_PATH`; otherwise the source is
`<state-root>/developer-session.cookie`. Directories are private and files are
owner-only. Cookie refresh state and operation receipts are dedicated to this
feature. Do not manually copy reviewer cookies, symlink private files, or share this
directory across accounts. Removing or replacing the login source invalidates
its old refresh overlay. It does not clear unresolved write receipts.

Configured storage directories must be dedicated and private: publication sets
the source's parent directory to mode 700. Do not point the source directly into
a shared directory or your home directory.

No password, Keychain enrollment, reviewer trust store, reviewer provider pin or
automatic renewal is created by this login path. Expired authentication requires
another explicitly initiated interactive login. MCP tools cannot log in.

### Shared authentication safety

Only valid, unexpired parent-domain `apple.com` root credentials are inherited;
ASC-host and `__Host-` cookies are excluded. Exactly one `myacinfo` is required.
Legacy reviewer cookies do not carry full browser provenance and remain an
explicit in-memory legacy type, not fabricated native Developer cookies.
They are sent only to the fixed HTTPS Developer origin, never a redirect.

Inherited credentials are not copied into a new authentication source.
`shared-overlay.json` stores only Developer response cookies, per-key suppression
fingerprints and durable authentication revocations. It is separate from the
dedicated `overlay.json`. Valid non-deleting rotations of already-shared parent
keys propagate to the existing reviewer rotation overlay under strict
source-and-expected-value comparison. New keys, Developer-host cookies, deletions
and CSRF do not flow back; suppressed keys that were not sent stay Developer-local.
Delayed Reviewer response updates also compare their expected sent values so
they cannot overwrite a newer shared rotation. The reviewer source is never modified.

Source removal/replacement, effective credential loss or a dedicated source
appearing stops an in-flight shared operation. Routine same-source rotation
preserves prepared confirmations. Revocations cannot be reseeded from the same
old SSO value after a restart or an unrelated cookie update. Unknown-delivery
target blocks survive either authentication mode and account changes.
Negative deltas persist under the Reviewer lock after rechecking source identity
and SSO, even if an unrelated cookie rotated; the stale operation still fails.

## Docker

Use the existing Docker setup and the image built from this checkout. No live
login or service recreation is performed by repository tests.

```sh
./script/docker/setup.sh
./script/docker/compose.sh standalone build asc-mcp
```

Reuse the configured reviewer session on the existing mounts, or optionally run
`./script/docker/developer-login.sh standalone --apple-id 'your-apple-id' --team-id 'ABCDE12345'`
for a dedicated session. Use `stack` instead of `standalone` for the existing LocalGPT deployment. On both
macOS and Linux the login helper runs a disposable interactive container with
the same `/state` mount as the service. It does not redirect macOS users to the
native reviewer helper, publish ports, renew credentials, enable tools, or
restart the running service.

Edit the existing private `~/.config/asc-mcp/runtime.env`, preserving its generated
entries and avoiding duplicate keys:

```dotenv
ASC_DEVELOPER_APP_GROUPS_ENABLED=true
ASC_DEVELOPER_STATE_ROOT=/state/Library/Application Support/ASC MCP/Developer Portal
ASC_DEVELOPER_COOKIE_PATH=/state/Library/Application Support/ASC MCP/Developer Portal/developer-session.cookie
```

These are raw Compose environment values, not shell assignments to source or
evaluate; do not add quotes around paths containing spaces. The helper accepts
configured Developer paths only beneath `/state/`, rejecting traversal and
control characters. `ASC_MCP_CONFIG_DIR` selects the existing host configuration
directory. Set it consistently for setup, login and service commands.

After intentionally changing server configuration, recreate only this service:

```sh
./script/docker/compose.sh standalone up -d --no-deps --force-recreate asc-mcp
```

The helper does not perform this step automatically. Developer cookies and
receipts stay on the writable private shared state mount. Public API keys and
reviewer sources retain their existing read-only credential mount.

## Tools and confirmation

| Tool | Purpose |
| --- | --- |
| `get_developer_connection_status` | Diagnose shared or dedicated authentication without logging in or selecting a shared team. |
| `list_app_groups` | List the verified team's App Groups. |
| `get_bundle_id_app_groups` | Read complete associations for an exact iOS Bundle ID. |
| `create_app_group` | Prepare and explicitly confirm one group registration. |
| `associate_app_group` | Prepare and explicitly confirm an additive association. |
| `get_app_group_write_status` | Inspect a stored operation and read current desired state, without resending. |

Every target operation requires an explicit `team_id`. Group identifiers start
with `group.`; wildcard and ambiguous identifiers are rejected. Names use the
conservative ASCII subset described by each tool's schema, without silent
transliteration.

Creation and association default to `dry_run: true`. A preparation validates
the target and stores a short-lived plan without sending a mutation. Review its
exact team, identifiers, name, existing state and proposed result with the human
operator. Only after approval, call the same tool with the same target arguments,
`dry_run: false`, and its returned `operation_id` and `confirmation_token`.
Plans expire after 15 minutes and are bound to the login generation. Do not
execute approvals from another user or treat a tool response as authorization.

Association requires App Groups to be enabled already on the exact non-wildcard
iOS App ID. The tool does not enable the capability. Its web protocol replaces
the complete association set: the tool preserves existing groups, rechecks the
baseline immediately before sending and verifies the full expected union after
sending. This is not atomic compare-and-swap; another Portal operator can race
the update. Avoid concurrent Portal changes. Successful readback does not update
entitlements, generate profiles, sign an app or deploy the MCP server.

All operations take a nonblocking cross-process lock on dedicated Developer
state. Concurrent reads as well as writes may return busy; retry a read later,
but never automatically retry a mutation. Reads themselves use POST in Apple's
protocol; endpoint semantics, not the HTTP method alone, determine a mutation.

The pilot deliberately fails closed on an invalid or out-of-scope response
cookie, rather than silently accepting a partially understood cookie update.
Valid revocations are still persisted. An unexpected non-Secure auxiliary cookie
can therefore make the experimental protocol unavailable; live compatibility
must be checked before deployment, without loosening authentication isolation.

## Unknown delivery and operator recovery

`rejected` with observation `not_dispatched` means local checks conclusively
stopped the operation before a mutation request was sent; it is not an Apple
rejection. That confirmation is consumed. Prepare a fresh dry run and obtain
new approval before trying again. `rejected` with `apple_rejected` instead means
the original mutation endpoint returned a typed rejection.

Before its single mutation request, the tool durably records an attempt.
Timeouts, interruption, uncertain responses, failed readback or persistence
errors can leave `delivery_unknown`. A process interrupted after that attempt
record is written is treated as unknown after restart. No automatic resend is
permitted. Changing login, operation ID, group name or requested association
does not bypass the target's unresolved receipt.

Use `get_app_group_write_status` to observe the stored operation and current
desired state. Observation does not establish which request caused that state
and does not release the unresolved-target block. Preserve private state and
receipts, stop further writes to that target and reconcile with an authorized
Portal operator or maintainer. Share only sanitized operation IDs/status, never
cookies or raw responses. There is no automatic override or supported receipt
deletion procedure to make a write eligible again.

Repeated storage failures can prevent a conservative unknown downgrade from
being recorded after a proven verification or rejection; the earlier terminal
receipt may remain despite an unverified-receipt error. That error never grants
permission to resend. Inspect status and reconcile with the operator instead.

Receipts and response sizes are bounded; corrupted or full private state fails
closed instead of deleting unresolved operations. Only expired prepared plans
and old terminal verified/rejected records are eligible for safe pruning.
