# App Group Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans in this session. A verified worker implements the Rust tasks with test-first evidence; Codex owns integration, host verification and the PR. No worker commits, pushes, logs in, deploys or mutates Apple resources. Execution ruling: Claude's mounted Linux environment has no Rust toolchain, so a verified native studio worker implements Rust while Claude performs independent read-only review. The workspace and approved scope are unchanged.

**Goal:** Open a tested PR adding experimental, explicitly confirmed App Group registration and additive iOS App ID association to ASC MCP.

**Architecture:** Separate fixed-origin Developer Portal transport, rich-cookie session and durable operation receipts. Interactive developer-login reuses existing SRP primitives without reviewer finalization or credential persistence. Existing public JWT and reviewer clients retain their origin/auth boundaries.

**Tech Stack:** Existing Rust1.88+/Tokio/reqwest/serde/fs2/sha2/rand/uuid/clap, httpmock/tempfile, existing shell/Docker configuration. No new production dependency unless a concrete requirement cannot be met with existing crates.

**Spec:** `docs/superpowers/specs/2026-10-17-app-groups-design.md` (human approved2026-10-17).

## Global Constraints

- Work only in `/Users/devonly/Developer/asc-mcp-app-groups`, branch `codex/app-group-registration`, base `5fa6dad6c7e3282a9e87992468eb744993017688`; preserve other checkouts and existing PR branches.
- PR targets `qtmleap/asc-mcp:master`; no production deployment/live Apple operations/certificates/profiles/delete/capability-enabling side effects.
- Fixed Developer HTTPS origin and QH65B2 path; `Policy::none()`, `reqwest::retry::never()`,30-second request timeout,120-second operation deadline,8MiB streamed response cap.
- Explicit `ASC_DEVELOPER_APP_GROUPS_ENABLED=true`; malformed config stops startup. Default existing tool catalogue and reviewer-only behavior remain unchanged.
- Separate owner-only rich-cookie source/overlay/receipt storage; never use a rendered ASC Cookie header or JWT and never widen existing AuthClient/Reviewer URL guards.
- Team membership and pinned login Team must match every operation; default mode is dry-run. Host-client human approval is the consent boundary; a confirmation token alone does not attest a human.
- One-time server-stored15-minute plans. Durable attempting receipt before a single mutation; never retry or automatically release an unknown operation, including across new UUID/group/name/session/process.
- Registration conflict key `(team, case-folded identifier)`; association key `(team, case-folded verified bundle identifier)`, independent of source generation. Existing group arrays/counts must be complete before union/recheck/readback.
-100 rows/page,100 pages,1000 groups,10000 App IDs. Records64KiB maximum,2048-record capacity. Only expired prepared records and verified/rejected records older than30days may be pruned; never unknown/attempting.
- First Developer pilot supports macOS/Linux owner-only filesystem semantics. On non-Unix, this optional feature fails explicitly as unsupported without changing existing API/reviewer support.
- No new inline comments/license headers, unrelated refactoring or unsafe fixture access to real credentials. Keep source/test changes focused. Only Codex creates/checks Git commits and pushes through the existing mandatory native guard.

## Review Focus

- Browser/Portal responses that rotate or ambiguously scope authentication cookies: no leakage to another host, no resurrection after source revocation (Task1 cookie/overlay tests).
- Old or incomplete Portal fixtures reporting group-count0 with one associated group: reject, never discard existing associations (Task2 completeness test).
- Dropped mutation connection followed by re-login/new requested group/restart: unresolved target remains blocked (Task2 restart/conflict-key tests).
- Transport cancellation or persistence failure after Apple may have processed the request: no second POST and no falsely verified receipt (Task2 cancellation/IO tests).
- Developer-only configuration and noninteractive login on Mac/Linux versus disabled feature/Windows: useful discovery without breaking old protocols or prompting inside MCP (Tasks3–4).

## Shared Interfaces And File Ownership

Rust worker owns `src/developer.rs`, new `src/developer/{cookies,fs_safe,state,groups,tests}.rs`, focused `src/apple_login.rs` additions, `src/lib.rs`, `src/main.rs`, `src/protocol.rs`, impacted Server-literal test files and new Rust fixtures/tests. Do not edit reviewer transport/auth modules or public API tool data. Codex owns docs/config/Docker shell helper, verification logs and Git/PR operations.

Public integration surface:

- `pub struct developer::DeveloperPortal`; `pub fn from_env() -> anyhow::Result<Option<Self>>`, ungated `pub fn new_default() -> Result<Self>`, `pub fn tools(&self) -> Vec<serde_json::Value>`, `pub async fn call(&self, name: &str, args: &Value) -> Result<Value>`. Explicit CLI login uses new_default, not the MCP feature gate.
- `Server` adds `pub developer: Option<DeveloperPortal>`. Developer dispatch precedes public-client-required fallback. Library module exported from `src/lib.rs`.
- `pub async fn apple_login::developer_login(email: &str, team_id: &str) -> Result<PathBuf>`; CLI requires both strings. Validate before TTY/password checks, then perform fresh interactive authentication. Never call reviewer finalization or credential/trust save/load functions.
- `pub(crate) struct developer::PortalCookie`: strict serde fields `name,value,origin_host,domain,path,host_only,secure,expires`. Source schema1 additionally pins `team_id`; generation derives privately from immutable source bytes. No account email is saved in the Developer source.
- `pub(crate) async fn DeveloperPortal::publish_login(&self, team_id: &str, cookies: Vec<PortalCookie>) -> Result<PathBuf>`: separate fixed Developer bootstrap, exact Team check, publication under the Developer lock. Only the explicit login orchestration calls it.
- Test-only `DeveloperPortal::for_test(endpoint: &str, root: &Path, source: &Path) -> Result<Self>` accepts loopback only, behind cfg(test), never exposed as operator endpoint configuration. Unit fixtures share this constructor; CLI smoke uses fake private roots but never a live cookie file.
- Tools exactly: `get_developer_connection_status`, `list_app_groups`, `get_bundle_id_app_groups`, `create_app_group`, `associate_app_group`, `get_app_group_write_status`; strict requests and safe typed response summaries per spec, not raw Apple JSON.

### Task1 — Dedicated Cookie, Transport And State Foundations

**Files:** New Developer module/cookies/fs_safe/state and unit tests; export module in `src/lib.rs` only when needed to compile the contract.

**Produces:** Verified Team/session transaction and typed bounded transport used by Task2; strict source publication used by Task3.

- [ ] Write failing tests named `developer_cookie_scope_is_not_asc_scope`, `developer_overlay_is_generation_bound`, `developer_team_is_explicit`, `developer_transport_never_redirects_retries_or_leaks`, `developer_private_state_is_bounded_and_symlink_safe`. Assert Secure/domain/path/expiry/host-only/prefix/provenance constraints, no ASC-only cookies or Authorization header at the mock, missing myacinfo rejected, exact pinned Team required, and mismatched generation overlay ignored without recreating revoked state.
- [ ] Run `cargo test --locked developer_ -- --test-threads=1`; retain the expected missing-module/behavior RED output in `target/verification/app-groups/task1-red.log`.
- [ ] Adapt existing audited owner-only filesystem primitives into the Developer helper with redacted `developer_*` errors. Production uses real storage; add a private IO seam for deterministic failure tests, not a runtime unsafe/private-store fallback. Source max1MiB,150 cookies, immutable-source generation plus a dedicated bounded overlay. Cookie storage keys retain sufficient scope/provenance; reject ambiguous auth selection.
- [ ] Implement fixed enum-selected paths, form bodies, media/status/resultCode parsing, CSRF response pair handling, bounded streaming and a source-pinned transaction holding one nonblocking process lock across the entire operation. Document that parallel reads also return busy. All network errors are fixed codes; never return reqwest/raw JSON/credential-bearing errors. A free-lock load treats leftover attempting receipts as unknown. Reserve at least30seconds of the120-second operation budget for mutation/readback: if preflight consumes more than90seconds, do not dispatch. Any cancellation/deadline after attempting leaves an unknown receipt, never an eligible prepared plan.
- [ ] Add response fixtures derived from pinned fastlane2.240.1 source and fixtures, clearly synthetic/historical rather than live proof. List arrays use `teams`, `applicationGroupList`, `appIds`; App detail uses `appId`. Ignore unrelated response metadata, but required fields/types/completeness must validate. For list paging, use bounded short-page termination; validate non-null reported totalRecords consistently, reject duplicates/changed totals and disagreement with the complete result.
- [ ] Run focused tests GREEN, then `cargo test --all-targets --locked`; expect all tests pass and only existing platform/ignored limitations. Record actual host and commands. Do not commit.

### Task2 — Confirmed Registration And Association With Durable Receipts

**Files:** `src/developer/{state,groups,tests}.rs`, `src/developer.rs` tool schemas/dispatch; synthetic fixtures.

**Consumes:** Task1 source-pinned locked transport, safe files, complete Team/group/App ID reads.

**Produces:** Six fully validated tool contracts consumed by Task3 integration. Create/associate use strict `team_id,identifier`, create additionally `name`, associate `bundle_id`, plus `dry_run` defaulttrue and optional `operation_id,confirmation_token` only when executing. Unknown keys/wrong types fail before network.

- [ ] Add RED tests `developer_prepare_never_mutates`, `developer_create_requires_exact_stored_plan`, `developer_association_preserves_complete_union`, `developer_incomplete_association_is_not_empty`, `developer_unknown_blocks_target_across_restart_and_relogin`, `developer_attempt_record_failure_sends_nothing`, `developer_cancellation_never_resends`. Count mutation endpoint calls explicitly, not all POSTs (reads are POST too).
- [ ] Run focused tests and save the expected behavioral failures, not unrelated fixture/import errors.
- [ ] Implement bounded strict identifiers/names, uppercase10-character Team IDs and canonical conflict keys. Resolve exact App IDs/groups inside the current verified Team; reject wildcard/non-iOS/ambiguous targets. Do not compare App ID prefix to Team ID. Require `features.APG3427HIY == true` with compatible `enabledFeatures`; unknown/contradictory capability metadata denies association. Never enable it implicitly. All six schemas have additionalProperties:false; read tools declare readOnlyHint:true, mutations readOnlyHint:false/openWorldHint:true/idempotentHint:false, create destructiveHint:false and association destructiveHint:true because its remote protocol replaces a set. Pin these annotations and omitted dry_run=true in catalogue tests.
- [ ] Persist a prepared operation with random256-bit token hash, canonical request/internal IDs, expiry, source generation, baseline and expected sets. Execute only an exact unexpired stored plan, after live Team/session/precondition revalidation. Existing exact desired group/association is an explicit no-op; case-variant ambiguity and conflicting names never cause a create/overwrite.
- [ ] Under the shared operation lock, scan the fully validated bounded receipt store for conflicts. Persist attempting atomically and verify persistence before sending once. A target remains blocked by unknown/attempting regardless of new operation ID, name, requested group or login generation. No secondary index write gap and no public unknown-override route.
- [ ] Prime csrf/csrf_ts from the same session's Developer group read, re-read App detail immediately before association and compare complete count-consistent baseline, then send the full union using `applicationGroups[]`. Compare complete readback with expected set; persist mismatch/IO failure as unknown. Store only typed/redacted observations. Registration similarly requires exact response identity and complete list readback.
- [ ] Implement read-only write-status reconciliation: report desired-state observations without proving causation, releasing unknown blocks or resubmitting.30-day safe pruning never removes unknown/attempting; capacity/full/invalid receipt state denies new mutation.
- [ ] Add IO fault/concurrency/timeout/5xx/non-JSON/typed rejection/cookie-source-replacement/old-plan/repeated-token tests, including simultaneous distinct requested groups for the same App ID. Assert zero extra mutation calls after restart and source changes. Include the primary fixture's count0/list1 inconsistency as a negative test rather than accepting it.
- [ ] Run focused suite GREEN and all-target tests; record output. Do not commit.

### Task3 — Separate Interactive Login And MCP/CLI Integration

**Files:** `src/apple_login.rs`, `src/{lib,main,protocol}.rs`, all impacted Server-literal test sites; `tests/{protocol_smoke,host_config}.rs` as relevant.

**Consumes:** `PortalCookie`, `publish_login`, `DeveloperPortal` tool surface. Existing `AuthClient` SRP/challenge arithmetic and identity matcher remain single maintained implementations.

- [ ] Add RED tests `developer_login_does_not_finalize_reviewer`, `developer_login_rejects_wrong_identity_and_team`, `developer_login_never_publishes_before_portal_bootstrap`, `developer_only_protocol_catalogue_and_dispatch`, `developer_disabled_keeps_legacy_catalogue`, `developer_login_requires_interactive_terminal`. Synthetic authentication responses must contain provenance-preserving parent-domain cookies; absent/host-only/expired seeds fail before any source publication.
- [ ] Add the dedicated orchestration inside apple_login (no new login-host allowlist). Only reuse widget discovery via AuthClient::new/widget_key, then a fresh AuthClient::with_trust(widget,vec![]), authenticate_with_password with ChallengePolicy::Interactive, session.success and identity_matches, followed by a new AuthClient::developer_seed. Export only secure, unexpired, non-host-only `apple.com` cookies actually observed and not seeded, preserving metadata. Instantiate DeveloperPortal::new_default and pass the seed to its separate bootstrap. Never call login_with_password/authenticate_and_finalize/finalize_with_publication. No Keychain/password/trust/provider/reviewer snapshot writes or reads. Tests assert all those boundaries remain untouched. Any unusable Developer bootstrap leaves prior dedicated source and reviewer state unchanged and returns a fixed error.
- [ ] Add required `developer-login --apple-id ACCOUNT --team-id TEAM` CLI and help. Non-Unix optional Developer support returns explicit unsupported without weakening file privacy. Never invoke this CLI from a tool.
- [ ] Wire optional Developer configuration and tools into Server, CLI tools and validate; malformed gate/config rejects startup, absent session still permits enabled diagnostic/discovery. Keep developer-only results limited to its six tools and prevent accidental public-client fallback. With Developer disabled, preserve both existing Server reviewer-only discovery and the CLI's distinct full-public-catalogue-plus-reviewer introspection behavior; do not blindly replace CLI Tools with Server::tools and change that legacy contract.
- [ ] Run unit auth fixtures, process-level CLI smoke and full stdio/HTTP regression suite, preserving all protocol versions. Tests use temporary/canonicalized private roots and scrub inherited auth environment; do not prompt, discover real private cookies, connect to Apple or touch running services.
- [ ] Run all-target tests and formatting/Clippy on the verified worker host. Report exact changed files, test logs, unverified live behavior and hand ownership back to Codex. Do not commit/push/deploy.

### Task4 — Operator Setup And Safe Docker Login Helper

**Owner:** Codex, after Tasks1–3 interface handoff; independent documentation drafting may proceed sooner but no execution before plan approval.

**Files:** New `script/docker/developer-login.sh`, new `docs/developer-portal.md`, `config/config.sample.env`, `SETUP.md`, `docs/docker-sidecar.md`, small README link; `compose.yaml` only if its existing raw env/shared state are insufficient. Add a helper regression in an existing Rust host-config/shell-test pattern, coordinating ownership after Rust worker completion.

- [ ] Write/run a failing helper test using fake compose/docker: no TTY denies without starting a container; valid forwarding pins shared Developer state, validates mode/arguments, and never invokes reviewer-login or restarts a service. Exercise both Darwin and Linux, spaces in configured paths and non-evaluation of runtime.env text.
- [ ] Add POSIX helper following existing compose wrapper conventions, but do not copy the reviewer's Darwin-to-native-login redirection. On both Darwin and Linux, use the existing private shared `/state` mount for the dedicated Developer source/overlay, so a one-off login publishes where the Docker service reads. Keep it interactive with no secret CLI flags, service recreation, ports or additional long-lived container. Do not auto-enable the experimental tools or run a real login.
- [ ] Document exact native/Docker commands, required role, opt-in/root paths, new developer-only validate semantics, first-pilot platform scope, no credential import/reuse assumption, dry-run/human-confirmation flow, capability prerequisite, readback versus signing/profile completion, non-atomic replacement risk, unknown-operation freeze/support procedure and lack of live-Apple certification. Never instruct deleting receipts to retry.
- [ ] Run helper fake tests, `sh -n script/docker/developer-login.sh`, full host-config tests and any configuration-only Compose validation with disposable nonsecret inputs. No production Docker command or login.

### Task5 — Native Verification, Independent Diff Review And PR

**Owner:** Codex; independent reviewers are not the implementation worker.

- [ ] Inspect actual diff against baseline, generated tool schemas, request fixtures, durable records and no-auth native CLI output. Verify edits are only in the intended worktree and no real credentials or iFakerX files enter its diff. Preserve useful review conversations/logs under ignored `target/verification/app-groups`.
- [ ] Run on studio: `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --all-targets --locked`, CLI help/version/disabled and enabled fake-root catalogue/noninteractive-login smoke, and shell/config tests. Read exit codes and exact pass/skip counts; do not infer success from a worker summary.
- [ ] Request independent Claude actual-diff review with base/current refs, spec and this plan. For this security-impacting cross-origin/auth change, request an independent native file review as fallback if LocalGPT's own file access remains unavailable. LocalGPT concept review already completed; do not call it a repository review. Resolve material findings with RED/GREEN evidence and rerun the relevant/full gates; re-review substantial changes.
- [ ] Read/apply the opted-in gh-commit-identity skill, check account/identity/message, commit source through guarded Git with signing disabled, verify every actual new commit. Do not rewrite inherited history or add unverified attribution.
- [ ] Run the mandatory online native outgoing check for the exact `origin` branch ref, then push normally. Confirm live remote HEAD equals checked local HEAD; if outcome is uncertain inspect remote instead of retrying blindly.
- [ ] Create PR against `qtmleap/asc-mcp:master`, describing experimental/private protocol, auth setup, tests and unverified live writes. Immediately attach its verified URL with the Codex artifact tool. No merge or deployment.
- [ ] Final response: concise Japanese PR link and material limitation only. No claim that the running ASC MCP or iFakerX registration/profile blocker is already fixed by an unmerged PR.

## Review And Execution Ledger

- Approved spec contains the independent LocalGPT concept and Claude/native actual-file findings; LocalGPT worker's LocalMCP connection currently fails, although LocalGPT generation and its direct gateway are healthy. Do not silently substitute file contents.
- Baseline on studio: formatting and strict Clippy pass;225 tests pass,2 existing ignored. Re-run after implementation rather than counting baseline as feature verification.
- Intent/auth changes make the Rust tasks tightly coupled; use one Claude implementation context with Codex integration and one fresh whole-diff review, not a fresh reviewer for every small task. Human reviews this plan before any product/test implementation.
