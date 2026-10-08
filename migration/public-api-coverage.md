# Rust public API compatibility ledger

All 356 original public tool names, descriptions, annotations and input definitions
are retained in `data/public_tools.json`, exported directly from the Go registry.
The runtime catalogue adds fields already accepted by Go but missing from its
schema, and the subscription-offer fields described below. Original names remain
available; runtime validation reports legacy arguments which had no implementation.

## Offline compatibility evidence

The 356-tool Go snapshot and request probes remain unchanged. Three native
additions are maintained separately in `data/native_tools.json` and
`data/native_request_recipes.json`: `get_app_store_version_build`,
`set_app_store_version_build`, and `create_screenshot_set`. They use the bundled
Apple OpenAPI build relationship and screenshot-set schemas, with Rust HTTP and
protocol regression tests. The runtime public catalogue therefore has 359 tools;
the original Go evidence still covers exactly 356.

`migration/export.sh`, run in a separate checkout of the recorded Go revision, installs a temporary, build-tagged capture client and runs
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

Original Go source revision: `d47bf2c9dd2ce5639dba8b397c22302f149c6d78`. To regenerate after removing Go source from the Rust tree, check out that revision in a separate directory and copy the `migration` scripts into it. The hashes in `data/compatibility_provenance.json` identify the exact files captured.

356 tools; 3924 captured initial requests. 345 single-request tools, one three-step review workflow, eight upload workflows, and two report tools.

| Tool | Implementation | Initial request probes | Original source |
|---|---|---:|---|
| `list_apps` | single request | 22 | `internal/asc/tools/apps.go` |
| `get_app` | single request | 3 | `internal/asc/tools/apps.go` |
| `get_app_versions` | single request | 8 | `internal/asc/tools/apps.go` |
| `list_builds` | single request | 25 | `internal/asc/tools/builds.go` |
| `get_build` | single request | 3 | `internal/asc/tools/builds.go` |
| `list_beta_groups` | single request | 25 | `internal/asc/tools/testflight.go` |
| `create_beta_group` | single request | 12 | `internal/asc/tools/testflight.go` |
| `delete_beta_group` | single request | 3 | `internal/asc/tools/testflight.go` |
| `list_beta_testers` | single request | 25 | `internal/asc/tools/testflight.go` |
| `invite_beta_tester` | single request | 12 | `internal/asc/tools/testflight.go` |
| `remove_beta_tester` | single request | 3 | `internal/asc/tools/testflight.go` |
| `add_tester_to_group` | single request | 4 | `internal/asc/tools/testflight.go` |
| `list_bundle_ids` | single request | 22 | `internal/asc/tools/provisioning.go` |
| `get_bundle_id` | single request | 3 | `internal/asc/tools/provisioning.go` |
| `list_certificates` | single request | 22 | `internal/asc/tools/provisioning.go` |
| `list_profiles` | single request | 22 | `internal/asc/tools/provisioning.go` |
| `list_devices` | single request | 22 | `internal/asc/tools/provisioning.go` |
| `register_device` | single request | 7 | `internal/asc/tools/provisioning.go` |
| `list_app_info_localizations` | single request | 18 | `internal/asc/tools/localizations.go` |
| `get_app_info_localization` | single request | 3 | `internal/asc/tools/localizations.go` |
| `create_app_info_localization` | single request | 17 | `internal/asc/tools/localizations.go` |
| `update_app_info_localization` | single request | 18 | `internal/asc/tools/localizations.go` |
| `delete_app_info_localization` | single request | 3 | `internal/asc/tools/localizations.go` |
| `get_app_infos` | single request | 3 | `internal/asc/tools/localizations.go` |
| `list_version_localizations` | single request | 18 | `internal/asc/tools/localizations.go` |
| `get_version_localization` | single request | 3 | `internal/asc/tools/localizations.go` |
| `create_version_localization` | single request | 22 | `internal/asc/tools/localizations.go` |
| `update_version_localization` | single request | 21 | `internal/asc/tools/localizations.go` |
| `delete_version_localization` | single request | 3 | `internal/asc/tools/localizations.go` |
| `list_customer_reviews` | single request | 23 | `internal/asc/tools/reviews.go` |
| `get_customer_review` | single request | 3 | `internal/asc/tools/reviews.go` |
| `create_customer_review_response` | single request | 4 | `internal/asc/tools/reviews.go` |
| `delete_customer_review_response` | single request | 3 | `internal/asc/tools/reviews.go` |
| `list_customer_review_summarizations` | single request | 20 | `internal/asc/tools/reviews.go` |
| `list_in_app_purchases` | single request | 23 | `internal/asc/tools/iap.go` |
| `get_in_app_purchase` | single request | 3 | `internal/asc/tools/iap.go` |
| `create_in_app_purchase` | single request | 13 | `internal/asc/tools/iap.go` |
| `update_in_app_purchase` | single request | 13 | `internal/asc/tools/iap.go` |
| `delete_in_app_purchase` | single request | 3 | `internal/asc/tools/iap.go` |
| `list_subscription_groups` | single request | 23 | `internal/asc/tools/subscriptions.go` |
| `get_subscription_group` | single request | 3 | `internal/asc/tools/subscriptions.go` |
| `list_subscriptions` | single request | 23 | `internal/asc/tools/subscriptions.go` |
| `get_subscription` | single request | 3 | `internal/asc/tools/subscriptions.go` |
| `create_in_app_purchase_version` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `get_in_app_purchase_version` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `list_in_app_purchase_versions` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `list_in_app_purchase_version_localizations` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `create_in_app_purchase_localization` | single request | 8 | `internal/asc/tools/commerce_versions.go` |
| `update_in_app_purchase_localization` | single request | 9 | `internal/asc/tools/commerce_versions.go` |
| `delete_in_app_purchase_localization` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `list_in_app_purchase_version_images` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `create_in_app_purchase_image` | single request | 5 | `internal/asc/tools/commerce_versions.go` |
| `update_in_app_purchase_image` | single request | 7 | `internal/asc/tools/commerce_versions.go` |
| `delete_in_app_purchase_image` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `create_subscription_version` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `get_subscription_version` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `list_subscription_versions` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `list_subscription_version_localizations` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `create_subscription_localization` | single request | 8 | `internal/asc/tools/commerce_versions.go` |
| `update_subscription_localization` | single request | 9 | `internal/asc/tools/commerce_versions.go` |
| `delete_subscription_localization` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `list_subscription_version_images` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `create_subscription_image` | single request | 5 | `internal/asc/tools/commerce_versions.go` |
| `update_subscription_image` | single request | 7 | `internal/asc/tools/commerce_versions.go` |
| `delete_subscription_image` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `create_subscription_group_version` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `get_subscription_group_version` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `list_subscription_group_versions` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `list_subscription_group_version_localizations` | single request | 20 | `internal/asc/tools/commerce_versions.go` |
| `create_subscription_group_localization` | single request | 8 | `internal/asc/tools/commerce_versions.go` |
| `update_subscription_group_localization` | single request | 9 | `internal/asc/tools/commerce_versions.go` |
| `delete_subscription_group_localization` | single request | 3 | `internal/asc/tools/commerce_versions.go` |
| `list_in_app_purchase_offer_codes` | single request | 20 | `internal/asc/tools/iap_offer_codes.go` |
| `get_in_app_purchase_offer_code` | single request | 3 | `internal/asc/tools/iap_offer_codes.go` |
| `create_in_app_purchase_offer_code` | single request | 11 | `internal/asc/tools/iap_offer_codes.go` |
| `update_in_app_purchase_offer_code` | single request | 4 | `internal/asc/tools/iap_offer_codes.go` |
| `list_in_app_purchase_offer_code_prices` | single request | 20 | `internal/asc/tools/iap_offer_codes.go` |
| `list_in_app_purchase_offer_code_custom_codes` | single request | 20 | `internal/asc/tools/iap_offer_codes.go` |
| `create_in_app_purchase_offer_code_custom_code` | single request | 9 | `internal/asc/tools/iap_offer_codes.go` |
| `update_in_app_purchase_offer_code_custom_code` | single request | 4 | `internal/asc/tools/iap_offer_codes.go` |
| `list_in_app_purchase_offer_code_one_time_use_codes` | single request | 20 | `internal/asc/tools/iap_offer_codes.go` |
| `create_in_app_purchase_offer_code_one_time_use_code` | single request | 9 | `internal/asc/tools/iap_offer_codes.go` |
| `update_in_app_purchase_offer_code_one_time_use_code` | single request | 4 | `internal/asc/tools/iap_offer_codes.go` |
| `get_in_app_purchase_offer_code_one_time_use_code_values` | single request | 3 | `internal/asc/tools/iap_offer_codes.go` |
| `list_in_app_purchase_price_points` | single request | 20 | `internal/asc/tools/commerce_pricing.go` |
| `get_in_app_purchase_price_schedule` | single request | 3 | `internal/asc/tools/commerce_pricing.go` |
| `set_in_app_purchase_price_schedule` | single request | 11 | `internal/asc/tools/commerce_pricing.go` |
| `list_in_app_purchase_price_schedule_prices` | single request | 24 | `internal/asc/tools/commerce_pricing.go` |
| `get_in_app_purchase_availability` | single request | 3 | `internal/asc/tools/commerce_pricing.go` |
| `set_in_app_purchase_availability` | single request | 8 | `internal/asc/tools/commerce_pricing.go` |
| `list_in_app_purchase_available_territories` | single request | 20 | `internal/asc/tools/commerce_pricing.go` |
| `list_subscription_plan_availabilities` | single request | 20 | `internal/asc/tools/commerce_pricing.go` |
| `get_subscription_plan_availability` | single request | 3 | `internal/asc/tools/commerce_pricing.go` |
| `create_subscription_plan_availability` | single request | 9 | `internal/asc/tools/commerce_pricing.go` |
| `update_subscription_plan_availability` | single request | 9 | `internal/asc/tools/commerce_pricing.go` |
| `list_subscription_plan_available_territories` | single request | 20 | `internal/asc/tools/commerce_pricing.go` |
| `get_subscription_price_point` | single request | 3 | `internal/asc/tools/commerce_pricing.go` |
| `list_subscription_price_point_equalizations` | single request | 20 | `internal/asc/tools/commerce_pricing.go` |
| `list_subscription_price_point_adjusted_equalizations` | single request | 26 | `internal/asc/tools/commerce_pricing.go` |
| `set_app_price_schedule` | single request | 13 | `internal/asc/tools/commerce_pricing.go` |
| `get_app_price_point` | single request | 3 | `internal/asc/tools/commerce_pricing.go` |
| `list_app_price_point_equalizations` | single request | 20 | `internal/asc/tools/commerce_pricing.go` |
| `list_app_store_versions` | single request | 23 | `internal/asc/tools/versions.go` |
| `get_app_store_version` | single request | 3 | `internal/asc/tools/versions.go` |
| `create_app_store_version` | single request | 11 | `internal/asc/tools/versions.go` |
| `update_app_store_version` | single request | 12 | `internal/asc/tools/versions.go` |
| `delete_app_store_version` | single request | 3 | `internal/asc/tools/versions.go` |
| `submit_app_for_review` | review workflow | 7 | `internal/asc/tools/versions.go` |
| `get_app_store_review_detail` | single request | 3 | `internal/asc/tools/versions.go` |
| `create_app_store_review_detail` | single request | 28 | `internal/asc/tools/versions.go` |
| `update_app_store_review_detail` | single request | 28 | `internal/asc/tools/versions.go` |
| `list_review_submissions` | single request | 23 | `internal/asc/tools/review_submissions.go` |
| `get_review_submission` | single request | 3 | `internal/asc/tools/review_submissions.go` |
| `create_review_submission` | single request | 6 | `internal/asc/tools/review_submissions.go` |
| `add_review_submission_item` | single request | 36 | `internal/asc/tools/review_submissions.go` |
| `submit_review_submission` | single request | 3 | `internal/asc/tools/review_submissions.go` |
| `cancel_review_submission` | single request | 3 | `internal/asc/tools/review_submissions.go` |
| `get_phased_release` | single request | 3 | `internal/asc/tools/phased_release.go` |
| `create_phased_release` | single request | 6 | `internal/asc/tools/phased_release.go` |
| `update_phased_release` | single request | 4 | `internal/asc/tools/phased_release.go` |
| `delete_phased_release` | single request | 3 | `internal/asc/tools/phased_release.go` |
| `list_screenshot_sets` | single request | 23 | `internal/asc/tools/screenshots.go` |
| `list_screenshots` | single request | 23 | `internal/asc/tools/screenshots.go` |
| `get_screenshot` | single request | 3 | `internal/asc/tools/screenshots.go` |
| `delete_screenshot` | single request | 3 | `internal/asc/tools/screenshots.go` |
| `list_preview_sets` | single request | 23 | `internal/asc/tools/screenshots.go` |
| `list_previews` | single request | 23 | `internal/asc/tools/screenshots.go` |
| `get_preview` | single request | 3 | `internal/asc/tools/screenshots.go` |
| `delete_preview` | single request | 3 | `internal/asc/tools/screenshots.go` |
| `end_app_availability_pre_order` | single request | 3 | `internal/asc/tools/preorders.go` |
| `list_app_events` | single request | 23 | `internal/asc/tools/events.go` |
| `get_app_event` | single request | 3 | `internal/asc/tools/events.go` |
| `create_app_event` | single request | 22 | `internal/asc/tools/events.go` |
| `update_app_event` | single request | 21 | `internal/asc/tools/events.go` |
| `delete_app_event` | single request | 3 | `internal/asc/tools/events.go` |
| `list_analytics_report_requests` | single request | 23 | `internal/asc/tools/analytics.go` |
| `get_analytics_report_request` | single request | 3 | `internal/asc/tools/analytics.go` |
| `create_analytics_report_request` | single request | 4 | `internal/asc/tools/analytics.go` |
| `delete_analytics_report_request` | single request | 3 | `internal/asc/tools/analytics.go` |
| `list_analytics_reports` | single request | 23 | `internal/asc/tools/analytics.go` |
| `list_analytics_report_instances` | single request | 23 | `internal/asc/tools/analytics.go` |
| `list_analytics_report_segments` | single request | 23 | `internal/asc/tools/analytics.go` |
| `list_app_clips` | single request | 23 | `internal/asc/tools/appclips.go` |
| `get_app_clip` | single request | 3 | `internal/asc/tools/appclips.go` |
| `list_app_clip_default_experiences` | single request | 23 | `internal/asc/tools/appclips.go` |
| `get_app_clip_default_experience` | single request | 3 | `internal/asc/tools/appclips.go` |
| `list_app_clip_advanced_experiences` | single request | 23 | `internal/asc/tools/appclips.go` |
| `get_app_clip_advanced_experience` | single request | 3 | `internal/asc/tools/appclips.go` |
| `get_game_center_detail` | single request | 3 | `internal/asc/tools/gamecenter.go` |
| `list_game_center_achievements` | single request | 23 | `internal/asc/tools/gamecenter.go` |
| `get_game_center_achievement` | single request | 3 | `internal/asc/tools/gamecenter.go` |
| `create_game_center_achievement` | single request | 18 | `internal/asc/tools/gamecenter.go` |
| `update_game_center_achievement` | single request | 23 | `internal/asc/tools/gamecenter.go` |
| `delete_game_center_achievement` | single request | 3 | `internal/asc/tools/gamecenter.go` |
| `list_game_center_leaderboards` | single request | 23 | `internal/asc/tools/gamecenter.go` |
| `get_game_center_leaderboard` | single request | 3 | `internal/asc/tools/gamecenter.go` |
| `create_game_center_leaderboard` | single request | 16 | `internal/asc/tools/gamecenter.go` |
| `update_game_center_leaderboard` | single request | 16 | `internal/asc/tools/gamecenter.go` |
| `delete_game_center_leaderboard` | single request | 3 | `internal/asc/tools/gamecenter.go` |
| `list_game_center_leaderboard_sets` | single request | 20 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `get_game_center_leaderboard_set` | single request | 3 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `create_game_center_leaderboard_set` | single request | 7 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `update_game_center_leaderboard_set` | single request | 4 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `delete_game_center_leaderboard_set` | single request | 3 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `list_game_center_leaderboard_set_members` | single request | 20 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `add_game_center_leaderboard_set_members` | single request | 4 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `remove_game_center_leaderboard_set_members` | single request | 4 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `list_game_center_leaderboard_set_versions` | single request | 20 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `get_game_center_leaderboard_set_version` | single request | 3 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `create_game_center_leaderboard_set_version` | single request | 3 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `list_game_center_leaderboard_set_localizations` | single request | 20 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `create_game_center_leaderboard_set_localization` | single request | 5 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `update_game_center_leaderboard_set_localization` | single request | 4 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `delete_game_center_leaderboard_set_localization` | single request | 3 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `upload_game_center_leaderboard_set_image` | upload workflow | 4 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `list_game_center_leaderboard_set_member_localizations` | single request | 15 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `create_game_center_leaderboard_set_member_localization` | single request | 6 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `update_game_center_leaderboard_set_member_localization` | single request | 4 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `delete_game_center_leaderboard_set_member_localization` | single request | 3 | `internal/asc/tools/gamecenter_leaderboard_sets.go` |
| `list_game_center_activities` | single request | 20 | `internal/asc/tools/gamecenter_activities.go` |
| `get_game_center_activity` | single request | 3 | `internal/asc/tools/gamecenter_activities.go` |
| `create_game_center_activity` | single request | 29 | `internal/asc/tools/gamecenter_activities.go` |
| `update_game_center_activity` | single request | 29 | `internal/asc/tools/gamecenter_activities.go` |
| `delete_game_center_activity` | single request | 3 | `internal/asc/tools/gamecenter_activities.go` |
| `list_game_center_activity_versions` | single request | 20 | `internal/asc/tools/gamecenter_activities.go` |
| `get_game_center_activity_version` | single request | 3 | `internal/asc/tools/gamecenter_activities.go` |
| `create_game_center_activity_version` | single request | 6 | `internal/asc/tools/gamecenter_activities.go` |
| `update_game_center_activity_version` | single request | 4 | `internal/asc/tools/gamecenter_activities.go` |
| `list_game_center_activity_localizations` | single request | 20 | `internal/asc/tools/gamecenter_activities.go` |
| `create_game_center_activity_localization` | single request | 8 | `internal/asc/tools/gamecenter_activities.go` |
| `update_game_center_activity_localization` | single request | 8 | `internal/asc/tools/gamecenter_activities.go` |
| `delete_game_center_activity_localization` | single request | 3 | `internal/asc/tools/gamecenter_activities.go` |
| `upload_game_center_activity_image` | upload workflow | 7 | `internal/asc/tools/gamecenter_activities.go` |
| `list_game_center_challenges` | single request | 20 | `internal/asc/tools/gamecenter_challenges.go` |
| `get_game_center_challenge` | single request | 3 | `internal/asc/tools/gamecenter_challenges.go` |
| `create_game_center_challenge` | single request | 17 | `internal/asc/tools/gamecenter_challenges.go` |
| `update_game_center_challenge` | single request | 17 | `internal/asc/tools/gamecenter_challenges.go` |
| `delete_game_center_challenge` | single request | 3 | `internal/asc/tools/gamecenter_challenges.go` |
| `list_game_center_challenge_versions` | single request | 20 | `internal/asc/tools/gamecenter_challenges.go` |
| `get_game_center_challenge_version` | single request | 3 | `internal/asc/tools/gamecenter_challenges.go` |
| `create_game_center_challenge_version` | single request | 3 | `internal/asc/tools/gamecenter_challenges.go` |
| `list_game_center_challenge_localizations` | single request | 20 | `internal/asc/tools/gamecenter_challenges.go` |
| `create_game_center_challenge_localization` | single request | 8 | `internal/asc/tools/gamecenter_challenges.go` |
| `update_game_center_challenge_localization` | single request | 8 | `internal/asc/tools/gamecenter_challenges.go` |
| `delete_game_center_challenge_localization` | single request | 3 | `internal/asc/tools/gamecenter_challenges.go` |
| `upload_game_center_challenge_image` | upload workflow | 7 | `internal/asc/tools/gamecenter_challenges.go` |
| `submit_game_center_player_achievement` | single request | 17 | `internal/asc/tools/gamecenter_submissions.go` |
| `submit_game_center_leaderboard_entry` | single request | 19 | `internal/asc/tools/gamecenter_submissions.go` |
| `list_ci_products` | single request | 25 | `internal/asc/tools/xcode_cloud.go` |
| `get_ci_product` | single request | 3 | `internal/asc/tools/xcode_cloud.go` |
| `list_ci_workflows` | single request | 23 | `internal/asc/tools/xcode_cloud.go` |
| `get_ci_workflow` | single request | 3 | `internal/asc/tools/xcode_cloud.go` |
| `list_ci_build_runs` | single request | 23 | `internal/asc/tools/xcode_cloud.go` |
| `get_ci_build_run` | single request | 3 | `internal/asc/tools/xcode_cloud.go` |
| `start_ci_build_run` | single request | 3 | `internal/asc/tools/xcode_cloud.go` |
| `get_sales_report` | report | 7 | `internal/asc/tools/reports.go` |
| `get_finance_report` | report | 6 | `internal/asc/tools/reports.go` |
| `list_encryption_declarations` | single request | 25 | `internal/asc/tools/encryption.go` |
| `get_encryption_declaration` | single request | 3 | `internal/asc/tools/encryption.go` |
| `create_encryption_declaration` | single request | 29 | `internal/asc/tools/encryption.go` |
| `assign_build_to_encryption_declaration` | single request | 4 | `internal/asc/tools/encryption.go` |
| `list_users` | single request | 22 | `internal/asc/tools/users.go` |
| `get_user` | single request | 3 | `internal/asc/tools/users.go` |
| `update_user` | single request | 10 | `internal/asc/tools/users.go` |
| `delete_user` | single request | 3 | `internal/asc/tools/users.go` |
| `list_user_invitations` | single request | 22 | `internal/asc/tools/users.go` |
| `get_user_invitation` | single request | 3 | `internal/asc/tools/users.go` |
| `create_user_invitation` | single request | 10 | `internal/asc/tools/users.go` |
| `delete_user_invitation` | single request | 3 | `internal/asc/tools/users.go` |
| `get_app_price_schedule` | single request | 3 | `internal/asc/tools/pricing.go` |
| `list_app_price_points` | single request | 23 | `internal/asc/tools/pricing.go` |
| `list_territories` | single request | 22 | `internal/asc/tools/pricing.go` |
| `list_subscription_price_points` | single request | 23 | `internal/asc/tools/pricing.go` |
| `get_app_availability` | single request | 3 | `internal/asc/tools/availability.go` |
| `create_app_availability` | single request | 10 | `internal/asc/tools/availability.go` |
| `list_territory_availabilities` | single request | 23 | `internal/asc/tools/availability.go` |
| `get_age_rating_declaration` | single request | 3 | `internal/asc/tools/age_rating.go` |
| `update_age_rating_declaration` | single request | 51 | `internal/asc/tools/age_rating.go` |
| `list_territory_age_ratings` | single request | 17 | `internal/asc/tools/age_rating.go` |
| `list_beta_app_review_submissions` | single request | 22 | `internal/asc/tools/beta_review.go` |
| `get_beta_app_review_submission` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `create_beta_app_review_submission` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `get_beta_license_agreement` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `update_beta_license_agreement` | single request | 4 | `internal/asc/tools/beta_review.go` |
| `list_beta_app_localizations` | single request | 23 | `internal/asc/tools/beta_review.go` |
| `get_beta_app_localization` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `create_beta_app_localization` | single request | 16 | `internal/asc/tools/beta_review.go` |
| `update_beta_app_localization` | single request | 15 | `internal/asc/tools/beta_review.go` |
| `delete_beta_app_localization` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `list_beta_build_localizations` | single request | 23 | `internal/asc/tools/beta_review.go` |
| `get_beta_build_localization` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `create_beta_build_localization` | single request | 7 | `internal/asc/tools/beta_review.go` |
| `update_beta_build_localization` | single request | 6 | `internal/asc/tools/beta_review.go` |
| `delete_beta_build_localization` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `get_build_beta_detail` | single request | 3 | `internal/asc/tools/beta_review.go` |
| `update_build_beta_detail` | single request | 7 | `internal/asc/tools/beta_review.go` |
| `list_sandbox_testers` | single request | 22 | `internal/asc/tools/sandbox.go` |
| `update_sandbox_tester` | single request | 13 | `internal/asc/tools/sandbox.go` |
| `clear_sandbox_tester_purchase_history` | single request | 3 | `internal/asc/tools/sandbox.go` |
| `list_promoted_purchases` | single request | 23 | `internal/asc/tools/promoted_purchases.go` |
| `get_promoted_purchase` | single request | 3 | `internal/asc/tools/promoted_purchases.go` |
| `create_promoted_purchase` | single request | 15 | `internal/asc/tools/promoted_purchases.go` |
| `update_promoted_purchase` | single request | 14 | `internal/asc/tools/promoted_purchases.go` |
| `delete_promoted_purchase` | single request | 3 | `internal/asc/tools/promoted_purchases.go` |
| `list_subscription_offer_codes` | single request | 23 | `internal/asc/tools/promoted_purchases.go` |
| `get_subscription_offer_code` | single request | 3 | `internal/asc/tools/promoted_purchases.go` |
| `create_subscription_offer_code` | single request | 39 | `internal/asc/tools/promoted_purchases.go` |
| `update_subscription_offer_code` | single request | 11 | `internal/asc/tools/promoted_purchases.go` |
| `list_win_back_offers` | single request | 23 | `internal/asc/tools/promoted_purchases.go` |
| `get_win_back_offer` | single request | 3 | `internal/asc/tools/promoted_purchases.go` |
| `create_win_back_offer` | single request | 25 | `internal/asc/tools/promoted_purchases.go` |
| `update_win_back_offer` | single request | 9 | `internal/asc/tools/promoted_purchases.go` |
| `delete_win_back_offer` | single request | 3 | `internal/asc/tools/promoted_purchases.go` |
| `list_app_custom_product_pages` | single request | 23 | `internal/asc/tools/product_pages.go` |
| `get_app_custom_product_page` | single request | 3 | `internal/asc/tools/product_pages.go` |
| `create_app_custom_product_page` | single request | 4 | `internal/asc/tools/product_pages.go` |
| `update_app_custom_product_page` | single request | 10 | `internal/asc/tools/product_pages.go` |
| `delete_app_custom_product_page` | single request | 3 | `internal/asc/tools/product_pages.go` |
| `list_app_store_version_experiments` | single request | 27 | `internal/asc/tools/product_pages.go` |
| `get_app_store_version_experiment` | single request | 3 | `internal/asc/tools/product_pages.go` |
| `create_app_store_version_experiment` | single request | 10 | `internal/asc/tools/product_pages.go` |
| `update_app_store_version_experiment` | single request | 15 | `internal/asc/tools/product_pages.go` |
| `delete_app_store_version_experiment` | single request | 3 | `internal/asc/tools/product_pages.go` |
| `list_perf_power_metrics` | single request | 23 | `internal/asc/tools/diagnostics.go` |
| `list_diagnostic_signatures` | single request | 23 | `internal/asc/tools/diagnostics.go` |
| `list_diagnostic_logs` | single request | 23 | `internal/asc/tools/diagnostics.go` |
| `list_app_store_review_attachments` | single request | 23 | `internal/asc/tools/diagnostics.go` |
| `get_app_store_review_attachment` | single request | 3 | `internal/asc/tools/diagnostics.go` |
| `create_app_store_review_attachment` | single request | 5 | `internal/asc/tools/diagnostics.go` |
| `delete_app_store_review_attachment` | single request | 3 | `internal/asc/tools/diagnostics.go` |
| `get_routing_app_coverage` | single request | 3 | `internal/asc/tools/diagnostics.go` |
| `create_routing_app_coverage` | single request | 5 | `internal/asc/tools/diagnostics.go` |
| `delete_routing_app_coverage` | single request | 3 | `internal/asc/tools/diagnostics.go` |
| `get_end_user_license_agreement` | single request | 3 | `internal/asc/tools/misc.go` |
| `create_end_user_license_agreement` | single request | 7 | `internal/asc/tools/misc.go` |
| `update_end_user_license_agreement` | single request | 4 | `internal/asc/tools/misc.go` |
| `delete_end_user_license_agreement` | single request | 3 | `internal/asc/tools/misc.go` |
| `list_app_categories` | single request | 22 | `internal/asc/tools/misc.go` |
| `get_app_category` | single request | 3 | `internal/asc/tools/misc.go` |
| `list_alternative_distribution_keys` | single request | 22 | `internal/asc/tools/misc.go` |
| `get_alternative_distribution_key` | single request | 3 | `internal/asc/tools/misc.go` |
| `create_alternative_distribution_key` | single request | 3 | `internal/asc/tools/misc.go` |
| `delete_alternative_distribution_key` | single request | 3 | `internal/asc/tools/misc.go` |
| `get_alternative_distribution_package` | single request | 3 | `internal/asc/tools/misc.go` |
| `get_marketplace_search_detail` | single request | 3 | `internal/asc/tools/misc.go` |
| `create_marketplace_search_detail` | single request | 4 | `internal/asc/tools/misc.go` |
| `update_marketplace_search_detail` | single request | 4 | `internal/asc/tools/misc.go` |
| `delete_marketplace_search_detail` | single request | 3 | `internal/asc/tools/misc.go` |
| `list_webhooks` | single request | 14 | `internal/asc/tools/webhooks.go` |
| `get_webhook` | single request | 3 | `internal/asc/tools/webhooks.go` |
| `create_webhook` | single request | 11 | `internal/asc/tools/webhooks.go` |
| `update_webhook` | single request | 19 | `internal/asc/tools/webhooks.go` |
| `delete_webhook` | single request | 3 | `internal/asc/tools/webhooks.go` |
| `list_webhook_deliveries` | single request | 20 | `internal/asc/tools/webhooks.go` |
| `ping_webhook` | single request | 3 | `internal/asc/tools/webhooks.go` |
| `redeliver_webhook_delivery` | single request | 3 | `internal/asc/tools/webhooks.go` |
| `list_accessibility_declarations` | single request | 17 | `internal/asc/tools/accessibility.go` |
| `get_accessibility_declaration` | single request | 3 | `internal/asc/tools/accessibility.go` |
| `create_accessibility_declaration` | single request | 40 | `internal/asc/tools/accessibility.go` |
| `update_accessibility_declaration` | single request | 43 | `internal/asc/tools/accessibility.go` |
| `delete_accessibility_declaration` | single request | 3 | `internal/asc/tools/accessibility.go` |
| `list_app_tags` | single request | 23 | `internal/asc/tools/app_tags.go` |
| `update_app_tag` | single request | 4 | `internal/asc/tools/app_tags.go` |
| `list_app_tag_territories` | single request | 11 | `internal/asc/tools/app_tags.go` |
| `list_android_to_ios_app_mappings` | single request | 14 | `internal/asc/tools/android_mapping.go` |
| `get_android_to_ios_app_mapping` | single request | 3 | `internal/asc/tools/android_mapping.go` |
| `create_android_to_ios_app_mapping` | single request | 5 | `internal/asc/tools/android_mapping.go` |
| `update_android_to_ios_app_mapping` | single request | 9 | `internal/asc/tools/android_mapping.go` |
| `delete_android_to_ios_app_mapping` | single request | 3 | `internal/asc/tools/android_mapping.go` |
| `upload_app_screenshot` | upload workflow | 4 | `internal/asc/tools/uploads.go` |
| `upload_app_preview` | upload workflow | 7 | `internal/asc/tools/uploads.go` |
| `upload_review_attachment` | upload workflow | 4 | `internal/asc/tools/uploads.go` |
| `start_build_upload` | single request | 10 | `internal/asc/tools/build_uploads.go` |
| `upload_build_file` | upload workflow | 18 | `internal/asc/tools/build_uploads.go` |
| `get_build_upload` | single request | 3 | `internal/asc/tools/build_uploads.go` |
| `list_build_uploads` | single request | 17 | `internal/asc/tools/build_uploads.go` |
| `list_build_upload_files` | single request | 11 | `internal/asc/tools/build_uploads.go` |
| `delete_build_upload` | single request | 3 | `internal/asc/tools/build_uploads.go` |
| `list_beta_feedback_crashes` | single request | 17 | `internal/asc/tools/beta_feedback.go` |
| `get_beta_feedback_crash` | single request | 3 | `internal/asc/tools/beta_feedback.go` |
| `get_beta_feedback_crash_log` | single request | 3 | `internal/asc/tools/beta_feedback.go` |
| `delete_beta_feedback_crash` | single request | 3 | `internal/asc/tools/beta_feedback.go` |
| `list_beta_feedback_screenshots` | single request | 17 | `internal/asc/tools/beta_feedback.go` |
| `get_beta_feedback_screenshot` | single request | 3 | `internal/asc/tools/beta_feedback.go` |
| `delete_beta_feedback_screenshot` | single request | 3 | `internal/asc/tools/beta_feedback.go` |
| `get_beta_build_usage_metrics` | single request | 8 | `internal/asc/tools/beta_feedback.go` |
| `list_background_assets` | single request | 20 | `internal/asc/tools/background_assets.go` |
| `get_background_asset` | single request | 6 | `internal/asc/tools/background_assets.go` |
| `create_background_asset` | single request | 4 | `internal/asc/tools/background_assets.go` |
| `update_background_asset` | single request | 4 | `internal/asc/tools/background_assets.go` |
| `list_background_asset_versions` | single request | 20 | `internal/asc/tools/background_assets.go` |
| `get_background_asset_version` | single request | 6 | `internal/asc/tools/background_assets.go` |
| `create_background_asset_version` | single request | 3 | `internal/asc/tools/background_assets.go` |
| `upload_background_asset_file` | upload workflow | 9 | `internal/asc/tools/background_assets.go` |
| `list_background_asset_upload_files` | single request | 11 | `internal/asc/tools/background_assets.go` |
| `get_background_asset_version_release` | single request | 6 | `internal/asc/tools/background_assets.go` |
