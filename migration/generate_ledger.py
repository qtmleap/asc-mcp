"""Publish a per-tool ledger; initial-request fixtures are not upload proof."""
import json,re,hashlib,subprocess
from pathlib import Path
recipes=json.loads(Path('data/request_recipes.json').read_text());probes=json.loads(Path('data/go_request_probes.json').read_text())
sources={}
for source in Path('internal/asc/tools').glob('*.go'):
 for name in re.findall(r'Name:\s*"([a-z][a-z0-9_]+)"',source.read_text()):sources[name]=str(source)
revision=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
original_files=sorted(list(Path('internal/asc/tools').glob('*.go'))+list(Path('internal/asc/api').glob('*.go')))
provenance={'source_revision':revision,'source_sha256':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in original_files if not p.name.endswith('_test.go')},'capture_transport':'in-memory, terminal HTTP 400, no network'}
Path('data/compatibility_provenance.json').write_text(json.dumps(provenance,indent=2)+'\n')
records=[]
for name,recipe in recipes.items():
 kind='review workflow' if name=='submit_app_for_review' else 'report' if name in ('get_sales_report','get_finance_report') else 'upload workflow' if recipe['kind']=='compound' else 'single request'
 records.append({'name':name,'source':sources.get(name),'implementation':kind,'go_initial_request_probes':sum(bool(p['requests']) for p in probes[name].values()),'cursor_supported':recipe['supports_cursor'],'legacy_ignored_arguments':recipe['legacy_ignored']})
Path('data/public_coverage.json').write_text(json.dumps(records,indent=2)+'\n')
summary='''# Rust public API compatibility ledger

All 356 original public tool names, descriptions, annotations and input definitions
are retained in `data/public_tools.json`, exported directly from the Go registry.
The runtime catalogue adds fields already accepted by Go but missing from its
schema, and the subscription-offer fields described below. Original names remain
available; runtime validation reports legacy arguments which had no implementation.

## Offline compatibility evidence

`migration/export.sh` installs a temporary, build-tagged capture client and runs
the original Go handlers through an in-memory HTTP transport. That transport
always returns HTTP 400 and never opens a socket. The temporary Go API file is
removed even if export fails. No production Rust code executes Go or Python.

The fixtures cover required-only arguments, every optional omission, empty
strings/arrays/maps, false/null booleans, zero and large limits, arbitrary query
maps (including collisions with dedicated filters), opaque cursor requests,
endpoint selector branches, JSON:API to-many relationships, and variable-length
inline price arrays. Rust's `all_captured_requests_match_go` test reconstructs
every captured initial request and compares method, path, query and JSON body.
The independent Python checker verifies the same recipe language.

Request replay proves request construction, not success against a live Apple
account. Single-request handlers preserve Go response data; MCP presentation now
uses JSON text with structured content rather than the old hand-formatted prose.
Upload rows below verify reservations only in this fixture set. Separate Rust
upload tests verify transfer/commit. Reports require gzip/text handling in the
report dispatcher. `submit_app_for_review` also has HTTP mock tests covering all
three requests and a failure after submission creation, without retrying writes.

## Deliberate repairs backed by Apple's bundled OpenAPI

The primary specification is `doc/references/apple-asc/openapi.oas.json`:

- `SubscriptionOfferCodeCreateRequest` requires `customerEligibilities`,
  `offerEligibility`, `duration`, `offerMode`, `numberOfPeriods` and a `prices`
  relationship. The legacy singular `customer_eligibility` now becomes a
  one-element `customerEligibilities` array. `offer_eligibility_type` aliases
  `offer_eligibility`. The runtime schema adds `customer_eligibilities`,
  `offer_eligibility`, `duration`, `price_ids`, and inline `prices` objects with
  `territory_id` and `price_point_id`. Existing prices and inline prices are
  mutually exclusive. Inline resources use `SubscriptionOfferCodePriceInlineCreate`
  and its `territory`/`subscriptionPricePoint` relationships. Creation rejects
  missing Apple-required fields before making an HTTP request.
- `SubscriptionOfferCodeUpdateRequest` defines nullable boolean `active`.
  Legacy `is_active` aliases `active`, preserving explicit false.
- `PromotedPurchaseCreateRequest` and `PromotedPurchaseUpdateRequest` define
  boolean `visibleForAllUsers`. `visibility_type=SHOW_FOR_ALL_USERS` maps to
  true and `APP_STORE_CONNECT_ONLY` to false; unknown values are rejected.
  False on creation is serialized even though the Go struct omitted it.
- `AgeRatingDeclarationUpdateRequest` explicitly defines `horrorOrFearThemes`.
  The legacy `horror_or_fear_themes` argument, decoded but never assigned by Go,
  is now mapped with the enum values in Apple's schema.

Alias conflicts are errors. The unchanged Go fixtures exercise baseline
`build_request`; separate regression tests exercise runtime `prepare_request`
and actual mock HTTP dispatch of the repaired offer body. Resource IDs are
encoded as path segments; slash, backslash and dot-segment IDs are rejected.
Authenticated cursor origin/redirect restrictions are enforced by `ApiClient`.

## Remaining legacy limitations, reported as errors

- `list_app_store_versions` advertised `cursor`, `filter`, `sort`, `fields` and
  `include`, but its Go handler read none of them. Nonempty values now return an
  explicit error rather than silently producing the wrong result. The distinct
  `get_app_versions` contract and ordinary version listing still work.
- `create_subscription_offer_code` advertised `duration_type` with offer-mode
  values, `total_number_of_codes`, and `is_active`; none is a creation attribute
  in the applicable Apple request schema. Nonempty supplied values produce an
  explicit error. Use the additive `duration` and existing `offer_mode` fields.
  Code quantity belongs to a separate code-generation operation, not offer
  creation. The old handler could send an Apple-invalid create body; the Rust
  implementation requires the complete, valid fields described above.

These are documented exceptions, not unsupported generic handlers. Live writes
have not been tested or performed by this migration. Apple may reject a request
for account state, permissions, or later API changes; offline tests do not prove
account-specific success.

## Every public tool

'''
count=sum(r['go_initial_request_probes'] for r in records)
summary+=f'Original Go source revision: `{revision}`. To regenerate after removing Go source from the Rust tree, check out that revision in a separate directory and copy the `migration` scripts into it. The hashes in `data/compatibility_provenance.json` identify the exact files captured.\n\n'
summary+=f'{len(records)} tools; {count} captured initial requests. '
summary+='345 single-request tools, one three-step review workflow, eight upload workflows, and two report tools.\n\n'
summary+='| Tool | Implementation | Initial request probes | Original source |\n|---|---|---:|---|\n'
for r in records:summary+=f"| `{r['name']}` | {r['implementation']} | {r['go_initial_request_probes']} | `{r['source']}` |\n"
Path('migration/public-api-coverage.md').write_text(summary)
print('Coverage ledger:',len(records),'tools,',count,'captured requests')
