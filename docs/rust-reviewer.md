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
`~/Library/Application Support/ASC MCP/session.cookie`, mode 600,
inside a mode 700 directory. Import converts a session; it does not verify that
Apple will accept it. Cookies reload before every request.

Responses can rotate or delete cookies through `Set-Cookie`. Accepted root-path
Apple cookies are merged into `session.state.json` in the private directory,
with mode 600. The imported source stays unchanged, including Docker's read-only
credentials mount. The source's SHA-256 fingerprint identifies the login: importing
a different source resets the overlay, while deleting the source disables access.
Deleting all cookies in an overlay does not restore the original cookies.
Malformed or insecure overlay files fail closed instead of reviving the original
cookies; explicit local recovery is required if such a file is damaged.
An independent OS lock protects read/merge/write across processes. Unrelated
concurrent updates are preserved; a stale response cannot overwrite a new imported
login or delete a newer cookie value. Concurrent replacements of the same cookie
use the last completed save; use one shared state directory per imported session.
Refresh is opportunistic, not an Apple token-refresh API or a guaranteed session
extension. A session already rejected by Apple may require login and 2FA again.
Save failures print a fixed warning without secrets; they do not retry requests
or change the result of an uncertain POST.

Reviewer tools activate when the private session exists or
`ASC_REVIEW_COOKIE_PATH`, `ASC_REVIEW_APP_ID`, or `ASC_REVIEWER_ONLY` is set.
`ASC_REVIEW_APP_ID` explicitly selects the monitored app; there is no default.
Without it, monitoring returns `ASC_REVIEW_APP_ID_required` before any request. Monitor files remain `state.json` and `messages.json`
in the same private directory.

The conversation and monitor tools are `list_reviewer_threads`, `get_reviewer_messages`,
`get_legacy_reviewer_context`, `check_reviewer_updates`, and
`acknowledge_reviewer_updates`. Iris reads follow up to 20 pages, bounded to
8 MiB, with pagination restricted to the same origin and exact endpoint path.
HTTP 401 and known Apple login redirects report `reviewer_auth_required`.
HTTP 403 reports `reviewer_forbidden`, without asserting expiry; other redirects
report `reviewer_unexpected_redirect` and are never followed. Response bodies,
redirect targets and request errors are not included in errors. Credential fields in returned JSON
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

Five session-only, read-only tools sit beside the five above (fourteen reviewer tools with the four reply/recovery tools below
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
- **Connection**: `authentication` is `ok`, `missing`, `rejected`, `forbidden`,
  `redirected` or `unreachable`. A per-app 403 does not stop the remaining batch.
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

## Modern reply tools

`reply_app_review_message` (`app_id`, `review_submission_id`, `thread_id`,
`expected_apple_message_id`, `message`) and `get_app_review_reply_status` (`app_id`,
`review_submission_id`, `thread_id`) are always listed, so a session-only server exposes
fourteen reviewer tools (the legacy `reply_reviewer_message` is extra and off by default).

Flow: take the per-thread private lock (`.reply-<thread>.lock`, non-blocking; a second
caller gets `reviewer_reply_busy`) → read the durable record (`reply-<thread>.json`, mode
600, body hash only) → verify app→submission and submission→thread membership,
`canDeveloperAddNote` and an ACTIVE thread or UNRESOLVED_ISSUES submission, known
`APPLE`/`USER`/`API_KEY`/`XCODE_CLOUD` actors and valid dates, newest Apple message equals the expected id,
no duplicate developer body (CRLF/outer whitespace folded) and no existing draft (only
`data: null` proves absence; errors are not absence) → write the `intent` record → `POST`
draft (`messageBody` raw string, `resolutionCenterThread` relationship) → record
`draft_created` → GET the draft and re-check context, leaving the draft unsent on any
difference → record `send_attempted` → `POST` message with `createFromDraftMessage` →
read back. Headers for these writes: `X-Csrf-Itc: [asc-ui]`, `Accept`/`Content-Type:
application/vnd.api+json` (legacy transport unchanged). POSTs are never replayed.

Session binding: each reply, resume, reset or status call pins the authentication source
generation once. The `/olympus/v1/session` ownership proof, every preflight GET, the draft
POST and the message POST use cookies from that generation only; cookie rotation through
the private overlay within the same generation is allowed. If the source is replaced (for
example account A → B), the next request fails locally with `reviewer_session_changed`
before anything is sent, and the generation is re-checked immediately before each POST.
These pinned requests do not use the 401 renewal wait/replay; the regular read tools keep it.

Integrity: response bodies used for draft/message body hashes, draft identity and creator
email checks are compared unredacted, in memory only. Public tool results contain only
fixed codes and IDs; an ID that equals or contains a session cookie value is refused.
Pagination keeps the original `filter[...]` scope: a `links.next` that changes, duplicates
or adds a filter is refused (`reviewer_unsafe_pagination_url`), and a missing filter is
re-added with its original value, so a page cannot widen thread membership.

Records in any state other than `delivered` or explicitly `reset` block every later send on the thread, even a
different body, until inspected by a person; nothing deletes or patches drafts or
records automatically. Messages are limited to 4000 UTF-16 units (matching the observed
web reply box maxLength, not a public API specification),
non-blank, no control characters. Errors expose fixed codes only, never response bodies,
message text or cookies.

Live verification on 2026-10-08: one existing draft was explicitly resumed once;
the status tool confirmed a new developer message with the matching raw body. The send
response was non-JSON, so the send call conservatively returned `delivery_unknown`;
a GET-only status check established delivery without retrying the POST.

Limitations: the request shapes come from the web frontend bundle
<https://appstoreconnect.apple.com/access/m-appstore/static/js/main.a52d4a9ea996a295dcaa.js>
(a versioned asset that Apple may replace) and may change. If Apple normalises message HTML the read-back can stay `delivery_unknown` even
though the message was sent; check App Store Connect before anything else. No attachments.


### Recovery without an automatic resend

`get_app_review_reply_status` reports `not_attempted` when no attempt exists or it was
explicitly reset, `not_sent` for `intent`/`draft_created`/`draft_pending_validation`/`draft_rejected`, and
`delivery_unknown` for an unconfirmed message-send attempt. Both unfinished states remain
blocked. A draft POST receiving a definite HTTP 4xx (excluding 408) is saved as
`draft_rejected`; transport errors and 5xx remain uncertain. POSTs are never retried.

`reset_rejected_app_review_reply` takes `app_id`, `review_submission_id`, and `thread_id`.
This explicit call can only reset a `draft_rejected` record without draft/message IDs.
It checks membership, replyable state, the absence of a remote draft and the absence of
a matching developer message with GETs, then atomically changes the local phase to
`reset`. It never sends, patches or deletes anything on Apple. A subsequent reply is a
separate explicit call and repeats the complete preflight. Unknown sends and interrupted
draft creations cannot be reset this way.

For unknown outcomes, first read messages and the status; never start another send or
remove the local record just because a request timed out. If a matching developer message
is read back, status records confirmed delivery. For a draft left unsent, inspect it in
App Store Connect and decide whether to keep or discard it. Any manual recovery of a
local record requires stopping only the asc-mcp service (not the entire LocalGPT stack),
confirming no reply is running, and preserving a backup of `reply-<thread>.json` under
the configured private state root. The default root is `Library/Application Support/ASC MCP`
under the service HOME; an existing legacy root or `ASC_REVIEW_STATE_ROOT` may override it.
Do not edit or remove authentication cookies, trust files or the monitor acknowledgement
state. There is no automatic unblock for an uncertain send.

Confirmed delivery requires a new, known developer-authored message and matching body
(after CRLF/outer-whitespace normalization), and the returned message ID when available.
Server timestamps are used to choose the newest Apple message, but delivery does not
compare Apple's clock with the host's clock. A returned ID alone does not verify that the
body is the one requested; a mismatch remains unconfirmed and is not resent.


### Explicitly resume a verified unsent draft

`resume_unsent_app_review_reply` takes the original five reply arguments plus an explicit
`draft_id`. It accepts only `intent`, `draft_created`, or `draft_pending_validation`
records with no message-send attempt. It validates the unchanged app/submission/thread,
expected Apple message, body hash, and message-ID set; a known draft ID must match.
It reads the thread's draft (never lists or selects a newest draft), verifies its resource
ID/type/body and empty attachments, and compares its USER creator email with the current
session email in memory. Emails are neither returned nor saved. The draft must be created
within the original intent's window (5-second clock tolerance, maximum 24-hour age), and
not in the future beyond that tolerance. Missing creator/time information fails closed.
An identical draft made by the same account in the web UI cannot be distinguished from
this intent; the explicit caller-supplied ID confirms which verified draft to send.

After proof, the ID is saved and the same draft/context checks and sender used by the
normal reply tool run. `send_attempted` is persisted before the one message POST, so a
crash or unknown outcome cannot be resumed. Existing drafts are never patched or deleted.
A pending status returns only an ID and verification flag, not draft text or email.
Successful Iris POSTs with empty bodies preserve status headers; a create without an ID
stays `draft_pending_validation` until explicit resume. Non-empty invalid JSON and HTTP
failures remain errors. Optional local diagnostics contain only a numeric HTTP status
and fixed error kind, never response snippets or header values.

On 2026-10-08, a live draft was created and read back with the requested body, USER creator,
and empty attachments. Its create response could not be confirmed by the caller. This is
not proof of message delivery; that requires a separate verified read-back after send.


A pending intent older than 24 hours cannot use the resume tool. It remains blocked for
manual inspection/recovery; this age limit never automatically clears a record or sends
anything. The original caller must retain the message body to supply the unchanged
request when resuming; local records deliberately retain only its hash.
