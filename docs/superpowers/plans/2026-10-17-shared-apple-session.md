# Shared Apple ID session implementation

Spec: `docs/superpowers/specs/2026-10-17-shared-apple-session-design.md`.
Baseline: `114c51ee4c2ed93e54127c57919499d0c1a81de9`, 271 passing tests and
two pre-existing ignored tests. Worktree: `asc-mcp-app-groups`, branch
`codex/reuse-apple-id-session`. Claude implements; Codex integrates and verifies.
Independent actual-diff review precedes the authorized commit, push and PR.
Independent Claude plan reviews informed this plan. After restoring LocalMCP,
LocalGPT verified repository access and independently reviewed it with Thinking;
Pro was unavailable. Codex resolves material findings before integration.

## Tasks

1. Add a narrow shared-session snapshot API beside `reviewer::Reusable` and the
   locked rotation overlay. Keep legacy credentials in memory, validate the
   eligible scope, expose no raw ASC header, and provide strict expected-value
   rotation CAS and current-snapshot validation. Derive this capability from the
   actual configured Server Reviewer, never a separate global path lookup. First add regression tests
   proving missing support fails; record RED before implementing GREEN.
2. Add dedicated/shared authentication variants to Developer transactions.
   Dedicated files keep their schema and precedence. Shared state derives from
   the effective reviewer snapshot, uses a separate response-delta overlay and
   generation binding, and preserves revocation, one-shot receipts and secret
   redaction. No fabricated dedicated Source or native-cookie provenance.
   Keep the existing 64-hex receipt generation schema, namespacing the digest
   input instead of changing its serialized representation.
3. Connect fixed-origin transport, controlled shared-key rotations, explicit
   per-operation Team verification and teamless connection status. Update the
   three request/response references currently using `source.team_id`. Add
   end-to-end mock tests through actual tool dispatch and preserve CLI behavior.
4. Update current setup and Developer documentation and tool descriptions; mark
   earlier authentication-separation documentation as superseded. Run targeted
   tests, then `cargo fmt --all -- --check`,
   `cargo clippy --all-targets --locked -- -D warnings` and
   `cargo test --all-targets --locked -- --test-threads=1`. Record real evidence.
5. Review the actual diff independently, resolve material findings with tests,
   rerun affected gates, validate canonical Git identity/message/outgoing commits
   using the native guard, and create/attach the PR against `master`. Do not merge
   or deploy this follow-up.

## Review focus

- Legacy apex-domain ambiguity must not become fabricated browser provenance.
- Routine rotations preserve plans; replacement/removal and revoked effective
  authentication do not preserve access or confirmations.
- Shared rotation cannot clobber a concurrent reviewer update or log it out.
- A stale Developer delta cannot resurrect or override shared authentication.
- Unknown mutation delivery blocks a target across source and mode changes.
- Existing dedicated-mode receipts remain readable; unknown blocks survive
  transitions in either direction. Post-dispatch CAS/storage errors never become
  retryable not-dispatched results.
