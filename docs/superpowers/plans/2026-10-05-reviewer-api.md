# Reviewer API Implementation Plan

**Goal:** Read reviewer conversations and provide an explicitly experimental reply path through HTTP APIs, without browser control.

**Architecture:** Keep the official API/JWT client unchanged. Add a separate reviewer client authenticated by a fastlane CLI-created Apple ID cookie jar; reload the jar on every call. Register reviewer tools when configured and support reviewer-only startup without an API key. Use the known Iris GET endpoints; legacy reply is opt-in until validated against a real account.

**Tech Stack:** Go standard HTTP client, existing YAML dependency, fastlane CLI for interactive Apple ID/2FA authentication only.

**Spec:** User requested API access and accepted CLI Apple ID authentication; no browser operation. Keys/sessions never appear in responses, logs, or Git.

- [x] Add tests for cookie loading, auth errors, redirect protection, thread/message retrieval and reply guardrails.
- [x] Implement separate client and optional MCP registration / reviewer-only configuration.
- [x] Document CLI authentication and clearly distinguish tested fixtures from live verification.
- [ ] Run tests/build and validate against the user's authenticated account when available.

Reply constraints: derive version identifiers from a fresh thread read, require an active replyable thread, reject empty/overlong text and repeated body, no automatic POST retries. An uncertain result must not be reported as sent.

## Monitoring follow-up

Added a fastlane Spaceship read-only monitor and CLI login helper, exposed check/ack tools through MCP, and registered `asc-review` locally. Created the 30-minute thread heartbeat `interceptor-apple`. Snapshot tokens bind acknowledgements to the exact analysed messages. Go tests, Ruby monitor fixtures, build and MCP protocol smoke passed. Both existing Apple ID sessions were rejected; live message reading and all reply paths remain unverified pending CLI authentication. The existing Interceptor/Mudmouth periodic-save automation is unchanged.
