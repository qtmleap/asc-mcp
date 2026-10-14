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
  publication state including the validated `lastPublished` observation. Obtain
  this from a fresh read before each mutation.
- `intent_fingerprint`, `intent_state`, `partial_confirmed`, `collection_complete`,
  and `last_error`: safe recovery metadata. The fingerprint binds exact private
  record bytes without exposing the authentication generation or credentials.
  Failure diagnostics contain only a fixed error kind and optional HTTP status,
  never Apple response bodies, headers, or account details.
- `last_published` and `last_published_status`: a validated RFC 3339 timestamp, or
  the fixed status `null`, `missing`, or `invalid`. The publisher email in
  `lastPublishedBy` is never returned.
- `request_accepted`, `current_published`, `fresh_publication_verified`, and
  `publication_basis`: distinguish a durably observed successful HTTP request,
  the current publication flag, evidence of a new publication, and an explicit
  acknowledgment of an already published goal. `request_accepted` is null when
  no HTTP acceptance receipt was saved; it is never inferred from the flag.
  A true receipt is historical evidence for the recorded request, even if the
  current snapshot later changes. A fresh-publication claim additionally requires
  the exact saved confirmed post-snapshot and publication checkpoint, so later
  external timestamp changes are never attributed to the earlier request.

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
the exact intended matrix and snapshot before one publish PATCH. The intent saves
a receipt-protocol version before PATCH. After a successful bounded response, it
records the internal HTTP 2xx status before readback; a JSON body with concrete
errors, a different resource identity, or published=false cannot create a receipt.
An empty 204 response is supported. Receipt-save failure never retries PATCH.

A saved receipt can confirm an idempotent goal when the readback has the same
authentication source, exact row IDs and attributes, intended matrix, publication
resource, published=true, and a valid non-regressing `lastPublished`. An equal
timestamp returns `request_accepted: true`, `current_published: true`, and
`fresh_publication_verified: false`. A receipt followed by interrupted GET can
reconcile these same conditions after restart without another PATCH.

A new publication is verified only by an advancing timestamp or a false-to-true
transition with a valid non-regressing timestamp. An explicit null is allowed
only for the initial unpublished baseline; missing or invalid baseline attributes
are refused before PATCH. Transport failures, 5xx, malformed JSON, or contradictory
responses have no receipt. Their unchanged old true flag never clears the pending
intent; reconciliation requires the stronger new-publication evidence and the
same exact rows, resource and source.

An authorized publish sends one PATCH even when the flag was already true. It
skips only an exact confirmed post-snapshot with unchanged source/resource/matrix
and a saved acceptance receipt, new-publication proof, or explicit goal
acknowledgment below. An already published goal does not prove a new publication.

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
Partial recovery is explicit and private-state-only as described below; it never
automatically retries an uncertain write.

## Acknowledge a verified applied prefix

`acknowledge_app_privacy_partial` sends only GETs to Apple. It requires:

```json
{
  "app_id": "123",
  "disclosures": [
    {
      "category": "DEVICE_ID",
      "purposes": ["ANALYTICS", "APP_FUNCTIONALITY"],
      "linked_to_user": true,
      "used_for_tracking": false
    }
  ],
  "expected_intent_fingerprint": "<fresh exact private-record fingerprint from GET>",
  "expected_snapshot_digest": "<fresh exact observed snapshot digest from GET>",
  "confirmation": "ACKNOWLEDGE_CONFIRMED_APP_PRIVACY_PARTIAL",
  "exclusive_editing_confirmed": true
}
```

The `disclosures` array must be the complete original matrix accepted by the set
tool. Recovery requires the same authentication source and original
matrix, the original publication resource, both exact digests, and an observed
completed prefix at the recorded attempted step. An absent uncertain POST is
never acknowledged. Original states are limited to a single No Data record or
a normalized subset of the same intended matrix, with no obsolete collection
rows to delete. Retained row IDs and attributes must be unchanged; added tuples
and their resource IDs must be unique and exactly match the applied prefix.

Before changing local state, the MCP preserves the original raw intent bytes in
a private archive and verifies them. Archive conflict or failure keeps the
original pending record. Success records `partial_confirmed` and an exact
recovery checkpoint; it reports `collection_complete: false` and
`remote_writes: 0`. It does not publish, delete disclosures, recreate an existing
tuple, or claim the full batch completed.

The next set must match that exact checkpoint, original source, original matrix,
and publication resource. It preserves existing rows and creates only missing
tuples. Any changed checkpoint is refused. No Data mutations remain blocked, and
publication is refused until the full original matrix is observed. If another
POST commits but its response fails, the newly recorded applied prefix can be
acknowledged the same way, retaining the archive chain across service restarts.

A recorded prefix can temporarily split tracking rows across purposes and thus
cannot normalize to complete disclosure answers. Only an authenticated exact
partial checkpoint permits continuing that shape. The resumed intent retains
its initial checkpoint digest and the archived original intent fingerprint.
Recovery verifies every referenced archive's raw SHA-256, app, authentication
source, full desired matrix, publication resource, initial snapshot, and exact
recorded applied prefix. Retained IDs and attributes must match; new IDs are
bound by the saved full snapshot digest, since a pre-POST intent cannot predict
Apple's generated resource IDs. An identical external incomplete shape without
this checkpoint is refused. Archive chains are bounded to 32 ancestor reads;
corrupt, cyclic, foreign-context, or longer chains stop before further writes.

## Acknowledge an unchanged legacy published goal

`acknowledge_app_privacy_published_goal` sends only GETs to Apple. It requires the
publish matrix, fresh `expected_snapshot_digest`, exact private
`expected_intent_fingerprint`, `exclusive_editing_confirmed: true`, and:

```json
{
  "confirmation": "ACKNOWLEDGE_CURRENT_PUBLISHED_APP_PRIVACY_GOAL",
  "current_published_goal_acknowledged": true,
  "answers_accurate": true,
  "complies_with_app_store_guidelines_and_law": true,
  "will_update_if_changed": true
}
```

It supports only a versionless legacy pending collection publication at step 1
of 1, with no recorded receipt or error. The original matrix, authentication
source, exact original rows, publication resource, and full original publication
checkpoint must match the current already-published goal. New receipt-protocol
intents are excluded even if a crash or receipt-save failure left no receipt.

The MCP archives and verifies the exact original bytes in a separate publication
goal archive, rereads the active intent, checks another fresh snapshot, then
atomically saves the basis `current_published_goal_acknowledged`. Archive conflict
or failure retains the original pending intent. It returns `remote_writes: 0`,
`request_accepted: null`, and `fresh_publication_verified: false`. This records the
caller's acknowledgment of the current goal; it never reconstructs historical
HTTP acceptance from an old error message, a supplied digest, or last_error=null.
Restart and repeated publish calls can recognize only this exact confirmed goal;
no additional PATCH is sent.

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
