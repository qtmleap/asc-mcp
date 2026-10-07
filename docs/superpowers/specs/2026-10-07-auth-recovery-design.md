# Reviewer authentication recovery

The user requires review monitoring to survive session expiry without repeatedly entering the Apple ID and password. Apple-required verification remains interactive. This change completes the earlier cookie persistence work.

## Behavior

- Remember the confirmed account in private host-only storage; explicit `--apple-id` overrides it. Never infer an account from untrusted shared state.
- On macOS, reuse and save the confirmed password in Keychain through native APIs, with no password in files, environment, argv, logs, or MCP output. Save only after Apple confirms the requested account. Unattended reads must never open Keychain UI.
- Refactor SRP authentication so interactive CLI and a native host renewal helper share the same implementation. Unattended 409 stops before any 2FA challenge endpoint; rejected credentials and account-action-required also stop automatic attempts.
- Keep Docker's credential directory read-only and login trust/password unavailable to the container. The host helper publishes only confirmed ASC cookies to the configured session source.
- A launchd host job performs periodic preflight/renewal and wakes on a fixed shared-state request marker. The marker carries no account, paths, commands, or secret. Concurrent helpers share a host-only lock and recheck the effective session before authenticating. Permanent user-action errors remain latched until successful interactive login; transient failures use a bounded cooldown.
- Opt-in Docker recovery on authentication failure waits at most 30 seconds for a new session generation or a terminal result. Reload a newer generation first. Replay GET at most once; never replay POST or interpret 403 as expiry. Surface actionable errors when the host helper is missing, locked, or requires user interaction.
- Continue supporting Linux interactive login without Keychain; no claim of unattended recovery there without a supported secret store.

## Deployment

Provide a Mac host launcher/installer with explicit host paths matching the existing Docker credentials, state, and separate trust directories, including legacy InterceptorReview state. Honor configured session source on both read and write. Install only the ASC renewal agent, preserving other schedules. Use the native Mac binary for interactive enrollment so Keychain saving is available. Build/update the ASC container alone.

## Evidence required

Regression tests must exercise remembered account precedence, no secret leaks, no unattended 2FA requests, account mismatch, credential refusal and latch behavior, session replacement racing cookie rotation, concurrent renewal, GET replay once and POST non-replay, absent-helper timeout, malformed/symlinked state, and Mac/Linux compilation. Verify deployed mounts and daemon state. Live enrollment may require one human password/2FA entry; report that accurately and do not claim eight-hour endurance without observation.
