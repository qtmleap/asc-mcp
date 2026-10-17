# Reuse the existing Apple ID session

## Outcome

The user requested reuse of the Apple ID authentication already used for App
Store Connect reviewer operations, without an unnecessary second login, followed
by tests, independent review and a pull request. They explicitly requested
continuous execution rather than repeated approval pauses. This supersedes the
mandatory authentication separation in the earlier App Groups design; it does
not authorize merging this follow-up or deploying it.

## Authentication and transport

- A present dedicated Developer source retains precedence, including on malformed,
  expired, rejected, inaccessible or symlinked sources. Only an absent source may
  reuse the existing effective reviewer source and rotation overlay.
- Derive eligible root-path, unexpired, syntactically valid `apple.com` parent
  credentials, excluding ASC-host cookies and `__Host-` cookies. Require exactly
  one `myacinfo`. Never send an API JWT or an ASC Cookie header to Developer.
- Legacy reviewer credentials lack complete browser provenance. Keep them a
  distinct in-memory credential type, not fabricated native Portal cookies.
  Admit this deliberate trust boundary only to the existing fixed HTTPS
  Developer origin. Do not add redirect/login/challenge automation.
- Verify Developer access live. All target operations still require an explicit
  ten-character Team ID and live membership. Teamless connection status may
  report shared authentication and a validated team count, never select a team.
- Reuse does not prove Developer acceptance. Documentation and the PR must
  distinguish fixtures from live acceptance; missing or rejected credentials
  fail closed without silently selecting a different account.

## Shared credential lifecycle

- Never persist an inherited credential copy as a new authentication source.
  Source removal, replacement, account change, dedicated-source appearance or
  loss of the effective shared authentication revokes derived access.
- Namespaced source identity binds plans and confirmations. Routine cookie
  rotation must not invalidate a valid same-source confirmation. Revalidate the
  effective shared snapshot immediately before dispatch and persistence.
- Keep Developer response cookies, CSRF and negative/revocation deltas in a
  separate shared-mode overlay, bound to source identity and the shared seed
  fingerprint. A stale overlay must not overwrite newer shared authentication.
  Each shared-key delta also binds its expected base value; a rotation of a
  different shared key must not allow stale per-key data to overwrite it.
- To avoid breaking reviewer operations when Apple rotates a global credential,
  propagate only valid non-deleting response updates to already-shared exact
  parent-domain keys into the reviewer rotation overlay. Under its existing
  lock, compare source generation and expected sent cookie values first. New
  keys, Developer-host cookies, deletions and CSRF never flow back. Never write
  the reviewer source, renew credentials or launch login from an MCP call.
  The propagation helper reports a real CAS mismatch instead of the existing
  reviewer's silent generation-change no-op. Never append a previously absent
  key or truncate an oversized overlay.
- Existing Reviewer response updates also compare the expected sent value per
  key, preventing a delayed response from overwriting a newer shared CAS. Only
  credentials actually sent by Developer may propagate back; a suppressed-key
  response stays a Developer-local, base-bound delta.
- A local Developer revocation must not be reseeded from the same stale shared
  credential. CAS/storage failures fail closed. Lock order is Developer then
  reviewer, never the reverse; no reviewer lock spans network operations.
  Authentication tombstones bind the source, SSO value and cookie key, surviving
  restarts and unrelated shared-key rotations until that authentication changes.
  Propagation failure after mutation dispatch follows the existing conservative
  unknown-delivery receipt path, never a retryable unsent classification. Apply
  the reviewer CAS before persisting Developer deltas so a crash cannot restore
  older shared authentication.
- Persist newly observed negative deltas while holding the Reviewer lock after
  rechecking source identity and SSO value. Unrelated-key rotation must not lose
  a logout; preserve the tombstone but reject the stale in-flight operation.
- Dedicated and shared overlays remain separate. Existing one-shot dispatch,
  receipt persistence, unknown-delivery blocking and redaction remain intact.
  Switching authentication mode must not clear an unresolved target block.

## Verification

Use synthetic credentials and fixed-origin mocks. Cover shared-only success,
dedicated precedence and invalid-source refusal, cookie filtering/legacy scope,
expiry and duplicates, live Team rejection, teamless status, source and overlay
revocation, rotation CAS/races, stale confirmation, dedicated-source appearance,
secret echo, distinct overlays, and unknown-delivery blocking across modes.
Run all Rust targets, formatting, strict Clippy and native protocol smoke tests.
No live Apple mutations, new certificate, service deployment or credential
inspection belongs to this PR task.
