# Native Rust reviewer access

Reviewer calls use Rust HTTP and an existing Apple ID session. They do not run
Go, Ruby, fastlane, browser automation, or an interactive login. The separate
`reviewer-login` CLI provides an explicit native login. Public App Store
Connect JWT credentials cannot authenticate these undocumented endpoints.

Set `ASC_REVIEW_COOKIE_PATH` to an owner-only session file, or import one with the
Rust credential import command. `import_session` accepts a fastlane cookie YAML
sequence (including Ruby object tags as inert data) or a JSON cookie array / an
object containing `cookies`. The canonical imported format is:

```json
{
  "cookies": [
    {
      "name": "SESSION_COOKIE_NAME",
      "value": "SESSION_COOKIE_VALUE",
      "domain": ".apple.com",
      "path": "/",
      "expires": "2030-01-01T00:00:00Z"
    }
  ]
}
```

`expires` can be absent/null for a session cookie. Invalid or expired dates,
unsafe cookie characters, unrelated domains, and non-root paths are excluded.
Input files must be regular files with no group/other permissions; symlinks are
rejected. Import stores JSON at
`~/Library/Application Support/InterceptorReview/session.cookie`, mode 600,
inside a mode 700 directory. Import converts a session; it does not verify that
Apple will accept it. Cookies reload before every request.

Reviewer tools activate when the private session exists or
`ASC_REVIEW_COOKIE_PATH`, `ASC_REVIEW_APP_ID`, or `ASC_REVIEWER_ONLY` is set.
`ASC_REVIEW_APP_ID` selects the monitored app; the previous monitor's default,
`6749347474`, is retained. Monitor files remain `state.json` and `messages.json`
in the same private directory.

The conversation and monitor tools are `list_reviewer_threads`, `get_reviewer_messages`,
`get_legacy_reviewer_context`, `check_reviewer_updates`, and
`acknowledge_reviewer_updates`. Iris reads follow up to 20 pages, bounded to
8 MiB, with pagination restricted to the same origin and exact endpoint path.
Redirects and HTTP 401/403 require renewed authentication; response bodies and
request errors are not included in errors. Credential fields in returned JSON
are redacted. Message HTML is untrusted material.

`reviewer-check` stores a fresh private snapshot and returns its SHA-256
`snapshot_token` plus new/changed messages from positively identified `APPLE`
actors. Developer and unknown actors do not generate reviewer notifications.
A successful check does not update seen state: repeating a check repeats pending
messages until the user reviews and acknowledges them. Use
`reviewer-check --ack TOKEN` or the acknowledgement tool with the token from the
exact analysed check. A newer check invalidates an older token. Acknowledgement
only marks messages from that snapshot locally; it sends no request to Apple.
Concurrent checks/acknowledgements use an OS advisory lock. The lock is released
when the process exits, including after forced termination; its empty file may remain. Old Ruby fingerprints can differ once due to JSON canonicalization;
reviewing and acknowledging the Rust snapshot establishes the new baseline.

Set `ASC_REVIEW_ENABLE_LEGACY_REPLY=1` (or `true`) to expose the experimental
`reply_reviewer_message`. It uses the legacy context's own thread identifiers,
checks active/replyable state, rejects an identical existing message, requires
both CSRF headers, and sends one POST. It never automatically retries. A legacy
`SUCCESS` response is `accepted_unverified`; read back the conversation before
claiming delivery or retrying an uncertain result. This route is not live
validated.

## Review status tools

Five session-only, read-only tools sit beside the five above (ten reviewer tools
in total). They reuse the same HTTP client, redaction, same-origin/exact-path
pagination (max 20 pages, 8 MiB). They never write `state.json`/`messages.json`,
take the monitor lock, log in, switch teams or send a POST. Live verification
for the current provider is described below.

| Tool | Arguments | Endpoint(s) |
| --- | --- | --- |
| `list_review_apps` | none | `/iris/v1/apps?limit=200` |
| `get_app_review_status` | `app_id` (digits only) | `/iris/v1/apps/{id}/appStoreVersions?limit=200`, `/iris/v1/apps/{id}/reviewSubmissions?limit=200&include=appStoreVersionForReview` |
| `get_all_app_review_statuses` | none | the two above for each listed app |
| `get_app_rejection_details` | `app_id` (digits only) | the two above, then the existing threads/messages endpoints |
| `get_review_connection_status` | none | `GET /olympus/v1/session` |

Extra or invalid arguments fail before any HTTP request. There is no app-count
cap: all pages are followed, and failure to complete is explicit.

- **Status**: version `id/platform/versionString/appStoreState/appVersionState/createdDate`
  and submission `id/platform/state/submittedDate/version_id` are preserved as Apple's
  raw strings (unknown values included). A `COMPLETE` submission is **not** approval.
  Either resource failing fails the call.
- **Batch**: item deadline 55 s, batch deadline 150 s (below the 180 s MCP timeout).
  A 500 on one app is recorded in `errors` and the batch continues; an auth failure
  stops the rest (`not_checked`). Partial results have `complete: false`. Output is
  bounded to 8 MiB; trimmed results set `truncated: true` and `omitted_count`.
- **Rejection**: versions whose `appVersionState` (fallback `appStoreState`) is
  `REJECTED`/`METADATA_REJECTED`. Only submissions linked to that version are used; the
  latest is the one with the greatest valid `submittedDate`. Missing links, bad dates or
  ties give `unknown_*` and `complete: false`, not "no issues". Messages include
  `body_html`, rejection details and attachment references (no download). Older linked
  submissions appear under a labelled `older_message_history`. A sender that is not
  positively `APPLE`/`DEVELOPER` is never treated as Apple.
- **Connection**: `authentication` is `ok`, `missing`, `rejected` or `unreachable`.
  Only `provider {providerId, name}` is returned (else `"unknown"`); no user, email or
  cookies. `public_api_configured` reflects whether the server has a public API client,
  not environment guessing. Scope is the current provider session only.

On 2026-10-05 all five status tools were verified through the Docker sidecar's
authenticated HTTP MCP endpoint. The current provider's ten apps were retrieved
without per-app errors or truncation; Kotatsu's iOS rejection and tvOS release
pending state were distinct. Interceptor and Kotatsu rejection messages and
reason fields were read successfully. Monitor snapshots/state were unchanged.
This confirms that session and provider at the time, not other teams or future
session validity. Missing evidence, unknown version states and ambiguous history
are reported as incomplete, rather than as no rejection.

## Authentication status

The separate `reviewer-login` command implements native Apple ID SRP password
authentication, trusted-device or trusted-phone verification, and an Olympus
web-session check before saving canonical private cookies. Password entry is
interactive and hidden; legacy two-step authentication is also supported. Monitor
checks and MCP calls never start login or request verification codes.

On 2026-10-05, user-completed native login and authenticated App Review reads
for Interceptor were confirmed through the Rust Docker client, followed by exact
snapshot acknowledgement. Earlier cached fastlane sessions were rejected; a fresh
login resolved the authentication block. This does not verify legacy reply
sending, every login path or future session validity. A session can expire and
require renewed human authentication; monitoring never starts login itself.

Rust [ipakeep](https://github.com/seifreed/ipakeep/) provides GrandSlam SRP and
2FA for App Store purchases/downloads with Anisette support. Its documented
authentication output does not establish an App Store Connect web session.
Adopting it would require proving the web-session exchange and reviewing its
Anisette dependency; an unverified GrandSlam token is not treated as an ASC
cookie. The native login instead targets Apple's web authentication flow.

Unit and local HTTP tests cover safe YAML, expiry and permissions, redirects and
auth errors, response redaction, pagination safety/cycles, session reload,
positive actor detection, repeated pending updates, stale snapshot rejection,
private file modes, concurrency exclusion, and one-attempt uncertain POSTs.
