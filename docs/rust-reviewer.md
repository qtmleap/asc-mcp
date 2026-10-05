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

The tools are `list_reviewer_threads`, `get_reviewer_messages`,
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
