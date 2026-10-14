# App Privacy collection questionnaire

These tools use the existing Apple ID session and private Iris routes, not an
App Store Connect Team Key. They do not submit an app for App Review. The existing
No Data tools still refuse collection disclosures and never delete records.

`get_app_privacy_details` retains its previous output and adds:

- `rows`: safe resource IDs, category, purpose, grouping, and protection for every
  bounded page of the questionnaire. Invalid resource types and foreign owners
  fail closed. No cookies, raw response bodies, or private account details leave
  this reader.
- `disclosures`: normalized category, unique purposes, `linked_to_user`, and
  `used_for_tracking`. Unsupported or inconsistent shapes return null instead
  of silently omitting rows.
- `snapshot_digest`: SHA-256 over the app, exact rows, publication resource, and
  publication state. Obtain this from a fresh read before each mutation.

## Save a collection draft

`set_app_privacy_collection` requires exactly these fields:

```json
{
  "app_id": "123",
  "expected_snapshot_digest": "<64 lowercase hexadecimal characters from GET>",
  "confirmation": "REPLACE_APP_PRIVACY_DISCLOSURES",
  "exclusive_editing_confirmed": true,
  "disclosures": [
    {
      "category": "DEVICE_ID",
      "purposes": ["ANALYTICS", "APP_FUNCTIONALITY"],
      "linked_to_user": true,
      "used_for_tracking": false
    }
  ]
}
```

This explicitly replaces the entire questionnaire. Omitted existing disclosures
are removed only when the complete current snapshot matches the supplied digest.
The tool preserves already matching rows, deletes only obsolete exact resource
IDs, and creates only missing category/purpose/protection combinations. Both
booleans are required. Unknown fields, categories, purposes, duplicate categories,
duplicate purposes, missing values, nulls, and stale digests are refused.

The supported category and purpose enums are in the MCP tool schema. A tracking
answer adds a separate `DATA_USED_TO_TRACK_YOU` row for every purpose alongside
the linked or not-linked row. Grouping is read from Apple and is not invented in
creation payloads.

## Publish the exact verified draft

`publish_app_privacy_collection` requires the same complete disclosure matrix and
a new digest from the verified draft, plus:

```json
{
  "confirmation": "PUBLISH_APP_PRIVACY_DISCLOSURES_ACCURATE",
  "exclusive_editing_confirmed": true,
  "answers_accurate": true,
  "complies_with_app_store_guidelines_and_law": true,
  "will_update_if_changed": true
}
```

All three attestations must be true. These express the publisher's authorization
and declarations; the MCP does not decide legal compliance. Fresh GETs must match
the exact intended matrix and snapshot before one publish PATCH. Success requires
the same publication resource, unchanged questionnaire, and published=true in
the readback. Already matching published data is idempotent.

## Non-atomic writes and uncertainty

Iris does not provide a transaction for questionnaire replacement. A private
durable intent precedes every DELETE, POST, or PATCH. It stores the initial safe
snapshot, desired tuples, authentication generation, and current step/total.
Every step has a fresh full preflight and an exact readback before the next step.
An observed external change, source replacement, ambiguous response, or readback mismatch
stops the batch. There is no automatic retry or rollback.

Both mutation tools require `exclusive_editing_confirmed: true`: the caller must
ensure no other App Store Connect questionnaire editor or mutation caller operates
for the entire batch. This is a caller attestation, not a server-enforced lock.
The digest is a client preflight check, not server compare-and-swap. There is no
verified conditional Iris write contract, and the tools send no guessed If-Match
header. For example, another editor could change an existing row with the same ID
after the last GET and before DELETE; deletion and its readback could match the
old plan while the intermediate change remains invisible. Preflight and readback
detect observed differences; they cannot make that interval atomic or guarantee
continuous exclusive access. Do not use these tools concurrently with another
questionnaire editor.

Uncertain or partial batches block all App Privacy mutation tools across service
restarts. GET can confirm the final desired matrix only with the same unchanged
authentication source; publication also requires the same published resource.
A draft batch that has observed `published: false` never accepts a later
published=true as draft completion: it stops and retains its pending intent even
if the final matrix matches. The checkpoint is saved before each write when the
preflight is unpublished, and immediately after an unpublished readback before
step validation or final confirmation, including a batch with only one DELETE.
Both collection intent types require the original publication resource, so GET
cannot silently reconcile an unexpected publication after a service restart.
A partial matrix or absence alone cannot establish final completion. Do not
delete the private intent file, replay a write, or claim success to clear it.
This version deliberately provides no automatic partial-batch recovery.

## Schema evidence and verification boundary

The source is Fastlane 2.239.0's
[Tunes client](https://github.com/fastlane/fastlane/blob/2.239.0/spaceship/lib/spaceship/connect_api/tunes/tunes.rb),
AppDataUsage category, purpose and protection models, and
[upload action](https://github.com/fastlane/fastlane/blob/2.239.0/fastlane/lib/fastlane/actions/upload_app_privacy_details_to_app_store.rb).
The client defines POST `/iris/v1/appDataUsages` with to-one category, purpose,
and protection relationships; DELETE uses the existing usage resource ID;
publication PATCH sets published=true. The action creates one row for each
purpose/protection combination. No arbitrary API payloads or guessed routes are
accepted by these tools.

The automated suite verifies real client calls against controlled Iris HTTP
fixtures, including No Data replacement, explicit linkage/tracking, publication,
stale snapshots, strict arguments, and uncertain partial writes. It does not prove
production Apple accepted a particular publisher's questionnaire. Record live
readback evidence separately after an authorized use. Private Iris routes have
no Apple public API compatibility guarantee.
