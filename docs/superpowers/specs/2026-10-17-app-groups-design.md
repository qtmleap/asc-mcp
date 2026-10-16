# App Group registration through ASC MCP

## Intent and delivery

Add experimental App Group listing, registration and additive iOS App ID association to `qtmleap/asc-mcp`, with tests and a pull request against `master`. The human approved this scope and a dedicated worktree. This work does not deploy the MCP, perform live Apple mutations, change certificates/profiles, delete groups or alter the iFakerX app. An existing ASC API key or reviewer connection is not proof of Developer Portal authorization.

Workspace: `/Users/devonly/Developer/asc-mcp-app-groups`, branch `codex/app-group-registration`, baseline `5fa6dad6c7e3282a9e87992468eb744993017688`. The desktop worktree tool is bound to the unrelated iFakerX task and has no repository-selection parameter, so Git created a sibling worktree following this repository's existing worktree layout. The primary checkout and other active branches remain unchanged.

## Protocol boundary

The official ASC OpenAPI4.5.1 has no App Group resource. Use a separate Developer Portal client, not an invented public JWT endpoint and not a wider `Reviewer::fetch` origin allowlist. The fixed production origin is `https://developer.apple.com`; the fixed endpoint family is `/services-account/QH65B2/`. A test-only constructor may use loopback and is unavailable through CLI/environment/MCP arguments.

Use the primary fastlane Portal implementation as protocol evidence: `account/listTeams.action`, `account/ios/identifiers/listApplicationGroups.action`, `addApplicationGroup.action`, `assignApplicationGroupToAppId.action`, `listAppIds.action` and `getAppIdDetail.action`. These are form-encoded POST endpoints, including reads. Encode repeated association values as `applicationGroups[]`. Interpret operation semantics explicitly, not HTTP method alone. Never follow redirects or automatically replay a request. Require bounded JSON responses, successful HTTP status, `resultCode == 0` and the expected typed body. Unknown formats fail closed; HTTP200 alone is not success. No arbitrary paths or URLs are accepted.

The endpoints are undocumented, may change and are not certified by offline tests. Documentation and tool descriptions disclose this. Required Apple role is Account Holder or Admin; verified Team membership is not a promise of sufficient mutation permission.

## Dedicated authentication and state

Add an interactive `developer-login --apple-id ACCOUNT --team-id TEAM` CLI. Reuse existing SRP/challenge primitives and native secret prompting, not reviewer-login finalization, provider pinning or session export. No passwords, verification codes, cookies or CSRF values are accepted by MCP tools. No login or unattended recovery is initiated inside MCP calls.

The login flow verifies the authenticated account using the existing identity check, selects only secure/unexpired parent-domain cookies actually observed during that authentication with retained `origin_host`, `host_only`, domain and path metadata, then performs a read-only Developer Team check. Publish the dedicated session only after the requested exact Team is verified. Do not change the reviewer session, its provider pin or its exports. Never import old provenance-free ASC snapshots implicitly. If the authentication flow does not produce a usable Developer session, fail explicitly rather than changing cookie scope or bypassing verification.

Use a separate `ASC_DEVELOPER_STATE_ROOT` (default private `ASC MCP/Developer Portal`) and `ASC_DEVELOPER_COOKIE_PATH` (default its `developer-session.cookie`). Directories are owner-only and files mode600, bounded and symlink-safe. Retain rich cookie metadata in the source. Select cookies per actual request host/path/expiry; host-only ASC/IDMSA cookies never reach Developer Portal and JWTs never enter this client. Require usable Developer `myacinfo`. Preserve the existing reviewer guards unchanged.

Developer-only response-cookie rotation uses its own bounded overlay bound to the immutable login-source generation, under the same Developer operation lock. It cannot update or read the reviewer overlay. Apply strict domain/path/secure/prefix checks to responses. Reject duplicate cookie keys and ambiguous authentication-cookie selection; a low-trust response cannot overwrite a differently scoped cookie. Removing/replacing the source revokes the old overlay. A newly authenticated source invalidates existing confirmation plans, not unresolved write receipts. CSRF values remain memory-only, come from the appropriate Developer read on the same session and are not sourced from ASC headers.

## Tools and configuration

Tools are exposed only with explicit `ASC_DEVELOPER_APP_GROUPS_ENABLED=true`; malformed configuration fails closed. Listing the ordinary catalogue without opt-in does not expose them. Add strict module-owned argument validation; generic protocol validation does not enforce every JSON Schema constraint.

- `get_developer_connection_status`: reports configuration and read-only Team access without account identifiers or secrets.
- `list_app_groups(team_id)`: returns complete bounded pages for the explicitly requested Team.
- `get_bundle_id_app_groups(team_id, bundle_id)`: resolves an exact iOS bundle identifier in that Team and returns its complete current group associations.
- `create_app_group(team_id, identifier, name, dry_run=true, operation_id?, confirmation_token?)`: dry-run produces a short-lived exact registration plan; execution requires the same arguments, operation ID and confirmation token.
- `associate_app_group(team_id, bundle_id, identifier, dry_run=true, operation_id?, confirmation_token?)`: plans or executes addition of one existing group, never removal.
- `get_app_group_write_status(operation_id)`: reports the private receipt and may perform only read-only reconciliation. It never resumes a write.

Every read and write verifies exact Developer Team membership through the Developer endpoint. Numeric ASC provider IDs are never interpreted as Team IDs. Team IDs are ten uppercase ASCII alphanumeric characters; resource IDs are validated independently. Identifiers are bounded, explicit reverse-DNS identifiers with no wildcard; group identifiers start with `group.`. Names are bounded and validated without silently rewriting the caller's intended text. Unknown/extra arguments, wrong types, incomplete pages, ambiguous identities and inconsistent duplicate records are rejected.

Association resolves the exact App ID and group inside the same Team and verifies their identities from detail/list responses. It does not assume an old App ID prefix equals Team ID. The App Groups capability must already be enabled; enabling capabilities is not a hidden second write. Missing or malformed association data is unknown, never an empty set. The reported `associatedApplicationGroupsCount` must equal the unique complete association count; inconsistent or unavailable completeness metadata prevents a write.

## Confirmation, persistence and unknown delivery

A dry-run returns the exact Team, identifiers, requested name or existing/proposed group sets, replacement-semantics warning, operation UUID, expiry and an opaque confirmation token. A canonical digest binds operation type, complete request, source generation and association baseline. Plans expire after15 minutes. Tokens are not authentication credentials and do not substitute for explicit human authorization.

The dedicated cross-process lock protects source/overlay reads, planning, conflict checks and receipt transitions. Persist a private `attempting` receipt atomically before dispatching a mutation, then send it at most once. If recording intent fails, do not send. State transitions are `prepared -> attempting -> verified | rejected | delivery_unknown`. A process interruption while attempting is treated as unknown after restart. Unresolved-intent blocking keys are registration `(team, identifier)` and association `(team, bundle_id)`, independent of operation UUID, requested name/group and authentication generation. Repeated execution, another operation ID, another requested group, re-login or restart cannot bypass that block. Do not log or persist raw responses, passwords, cookies or CSRF headers in plans/receipts.

Only an explicit, typed Apple rejection can be `rejected`. Transport errors, redirects,5xx, malformed responses, unexpected successful bodies, failed readback and failed final receipt persistence remain unknown. A read-only status check may report the desired state is now observed, but must not claim that an ambiguous request created it. Neither observed presence nor absence proves a timed-out request cannot apply later; reconciliation does not consume an unknown receipt or release its write block. This first version has no automated unknown-intent override. No automatic rollback or compensating mutation is attempted.

Registration first checks for an existing exact identifier. An already-existing exact desired group is a no-op, not a new creation; conflicting identity/name does not overwrite it. Successful creation requires exact resource identity and a fresh complete readback.

Association uses `current union requested`, preserving every existing group. Re-read complete detail immediately before dispatch and reject a changed baseline; send no empty or partial array. Verify the complete expected set afterward and report any mismatch without retry. The private endpoint provides no atomic external CAS, so another Portal/Xcode writer can still race between check and write. Disclose this residual risk in the confirmation and docs; do not claim atomic additive behavior. Local concurrent operations remain serialized.

## Integration and verification

Keep new transport/session/receipt code in a dedicated Developer module with small submodules. Integrate tool discovery/dispatch without altering public API authentication, existing reviewer recovery or protocol-version behavior. Extend CLI help, native/Docker setup documentation and sample environment configuration. Do not change the running container, plugin package, saved credentials or production MCP endpoint in this PR.

Use fixture-driven tests before production changes. Cover strict cookie provenance/path/expiry/prefix selection; JWT and ASC-cookie isolation; no redirects or replay; CSRF requirements; exact Team checks; typed resultCode handling; complete bounded pagination; identity ambiguity; capability prerequisite; association baseline changes and union preservation; explicit confirmation/expiry/session changes; concurrent requests; crash/timeout/restart unknown delivery; storage/readback faults; redacted outputs/receipts; CLI noninteractive refusal; opt-in catalogue and stdio/HTTP regressions. Real Apple writes are outside this task and must be labeled unverified.

Baseline on studio: `cargo test --all-targets --locked` passes225 tests with2 pre-existing ignored tests (`/tmp/asc-appgroups-baseline-20261017.log`). Final gates: `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --all-targets --locked`, native CLI smoke, independent actual-diff review and the mandatory native Git identity/message/outgoing-commit checks before committing and pushing. Open and attach the PR only after those gates; do not claim live Developer acceptance from mocks.

## Sources

- Apple registration and required role: https://developer.apple.com/help/account/identifiers/register-an-app-group
- Primary protocol implementation: https://github.com/fastlane/fastlane/blob/master/spaceship/lib/spaceship/portal/portal_client.rb
- Primary group and App ID mappings: https://github.com/fastlane/fastlane/blob/master/spaceship/lib/spaceship/portal/app_group.rb and https://github.com/fastlane/fastlane/blob/master/spaceship/lib/spaceship/portal/app.rb
- Current official API specification: https://developer.apple.com/sample-code/app-store-connect/app-store-connect-openapi-specification.zip
