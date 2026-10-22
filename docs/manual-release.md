# Manual App Store release

`release_app_store_version` publishes an approved iOS version whose release option
is `MANUAL` and whose state is `PENDING_DEVELOPER_RELEASE`. Changing a version's
`release_type` configures its release option; it does not publish a version that
is already waiting for the developer.

The tool uses Apple's [manual release endpoint](https://developer.apple.com/documentation/appstoreconnectapi/post-v1-appstoreversionreleaserequests)
and the bundled `AppStoreVersionReleaseRequestCreateRequest` schema. Apple says
that a release request cannot be canceled. Obtain explicit approval for the exact
app, version and build before confirming.

## Preview, then confirm

The tool is available when the public API client is configured on macOS or Linux.
Use a Team Key whose role permits releasing the app. No Apple ID reviewer session
is required.

First call with the target's opaque IDs and expected version string:

```json
{
  "app_id": "1234567890",
  "version_id": "VERSION_ID",
  "expected_version_string": "1.0.0",
  "expected_build_id": "BUILD_ID"
}
```

`dry_run` defaults to `true`. The preview only reads Apple's API and saves a local
receipt. It verifies the returned version ID, iOS platform, manual release option,
app relationship, attached build ID and version string. The version must be
pending developer release. Missing or inconsistent data stops the operation.

Inspect the returned `target` and warning. A successful preview returns
`status: prepared` and a `confirmation_token`, valid for 15 minutes. The token
binds the exact target; it does not attest that a person approved it. A later
preview replaces the earlier token for that version.

After approval, repeat the same target arguments with `dry_run: false` and the
returned token:

```json
{
  "app_id": "1234567890",
  "version_id": "VERSION_ID",
  "expected_version_string": "1.0.0",
  "expected_build_id": "BUILD_ID",
  "dry_run": false,
  "confirmation_token": "TOKEN_FROM_PREVIEW"
}
```

Confirmation reads the version again and repeats every check. It then records
the attempt durably before sending exactly one `POST
/v1/appStoreVersionReleaseRequests`. Its body contains only the target version
relationship. It does not alter release options, pricing, availability territories,
build selection, review submission or phased release settings.

The preflight and POST are separate Apple requests. The API provides no atomic
conditional release, so another App Store Connect operator can change the version
between them. Avoid concurrent manual changes during confirmation.

## Acceptance and recovery

- `release_request_created` with `request_id` means Apple returned a valid
  HTTP 201 release-request resource. It does not mean distribution is complete.
- `outcome_unknown` means the POST response could not establish acceptance.
  HTTP errors, transport failures and malformed success bodies are handled
  conservatively. No automatic write retry occurs. If Apple returned a valid
  acceptance but saving its outcome failed, `acceptance_observed: true` and
  `request_id` retain that evidence while the durable attempt still blocks replay.
- Once an attempt is recorded, subsequent confirmations for that version are
  blocked, including after process cancellation or restart. This also applies
  to successful requests and failed HTTP responses.
- A dry run after an attempt performs a read-only inspection and returns
  `release_attempt_recorded`, the observed version state and no new token.
  When the read fails, it still reports the attempt guard with
  `observation_error: read_failed`.

Use the ordinary version-read tools or App Store Connect to check processing and
distribution. Do not retry an uncertain release or delete its guard record to
force another send. Resolving a recorded attempt requires a person to inspect
Apple's state; this tool has no automatic unblock or reset operation.

## Persistent state

Receipts and per-version locks default to
`$HOME/Library/Application Support/ASC MCP/Manual Releases`. Set
`ASC_RELEASE_STATE_ROOT` to override the directory. The directory must be private,
writable and persistent. Private filesystem operations reject symlinked paths.
Use a dedicated directory: the private-state helper sets its permissions to 0700;
do not point the override at a shared directory or your home directory itself.

The supplied Docker image sets `HOME=/state`, and Compose persists that mount.
All processes using this tool for the same Apple account must share the same
state directory. Separate roots cannot prevent each other from sending a release.
Retain attempted receipts and include them in private state backups.

The old 356-tool compatibility catalogue is unchanged. The native release tool
adds one tool for an API-configured server; reviewer-only servers do not expose it.
