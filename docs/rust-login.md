# Native Apple ID login

The Rust CLI implements browser-free Apple ID authentication:

```sh
asc-mcp reviewer-login --apple-id you@example.com
```

Run this yourself in an interactive terminal. The password and verification codes
are hidden as you type. Passwords are not taken from command-line arguments,
environment variables, cached credential stores, or browser state. MCP requests
never invoke this flow or prompt for login. Neither Ruby, fastlane, Go, nor a
browser is used at runtime.

Supported protocol paths are Apple's SRP-6a 2048-bit/SHA256 `s2k` and `s2k_fo`,
Apple hashcash, trusted-device two-factor verification, SMS/voice verification,
and legacy four-digit two-step verification. SMS users select a masked trusted
phone number returned by Apple. A rejected verification code ends the attempt;
there are no automatic credential/code retries. Accounts requiring acceptance
of terms, privacy acknowledgement, account recovery, or an authentication upgrade
must complete those actions on Apple's website before retrying.

After authentication, the CLI confirms access through the App Store Connect
Olympus session endpoint. It then writes only applicable cookies to
`~/Library/Application Support/ASC MCP/session.cookie`, using the
reviewer's canonical JSON format and atomic private-file writer. The directory
has mode 700 and the file has mode 600 on Unix. An existing session is replaced
only after the new session is confirmed. The password is never persisted.

The login HTTP client has an empty cookie jar, a request timeout, disabled
redirects and retries, and fixed HTTPS origins `idmsa.apple.com` and
`appstoreconnect.apple.com`. Apple authentication headers are sent only to IDMSA.
The public widget key is discovered from a fresh HEAD `/logout` redirect without
following it, with the Olympus configuration endpoint as fallback. Authentication
responses, password-derived values, cookies, and verification codes are never
included in error messages or logs. Hashcash accepts at most 24 difficulty bits,
a bounded challenge, 100 million counters, and 30 seconds of computation.

## Validation and limits

On 2026-10-05, a user completed native CLI login with a real account and the
Rust Docker client successfully read Interceptor's App Review messages. Local
snapshot acknowledgement also succeeded. Passwords and verification codes were
entered by the user in the terminal and were not exposed to the assistant.
This confirms one account/session workflow; the other supported 2FA/legacy
paths have not all been exercised live. Apple's undocumented flow may change,
and sessions can expire or be revoked. Legacy reply delivery and official API
key access remain unverified. Existing private sessions can still be converted
with `reviewer-import-session` without logging in again.

Synthetic deterministic fixtures independently calculated with Python's hashlib
and integer arithmetic verify both password derivations and the SRP client
proofs against a server-derived shared secret. Tests also cover invalid SRP public
values, salt/iteration/protocol bounds, hashcash proof/bounds, origin filtering,
verification-code formats, and persisted cookie scope/replacement/deletion.
There are no live-network tests. Existing private cookie sessions can also be
converted with `reviewer-import-session` without running login.

The implementation uses `num-bigint` for SRP modular arithmetic; this is not a
constant-time cryptographic backend. Password buffers and intermediate byte
buffers use `zeroize`, but big integers and temporary JSON/request buffers do not
guarantee complete memory erasure. Protect the local process and session file
as credentials. Authentication transport is protected by normal HTTPS certificate
validation.

## Protocol references

The mathematical group and SRP safety checks follow
[RFC 5054](https://www.rfc-editor.org/rfc/rfc5054).
Apple-specific password derivation, minimal integer proof encoding, endpoints,
and two-step/two-factor sequence were checked against installed fastlane 2.240.1
and fastlane-sirp 1.1.0 source, and the public
[fastlane authentication client](https://github.com/fastlane/fastlane/blob/master/spaceship/lib/spaceship/client.rb)
and [two-factor client](https://github.com/fastlane/fastlane/blob/master/spaceship/lib/spaceship/two_step_or_factor_client.rb).
These are interoperability references rather than an Apple-supported login API.

## Session reuse and trusted-device cookies

Before prompting, login loads the effective existing session (source plus the shared rotation
overlay) and calls Olympus `/session`. The session is reused only on HTTP 200 whose
`user.emailAddress` equals the requested Apple ID (trimmed, case-insensitive); rotated
cookies are saved atomically and no password is requested. A provider object alone is never
accepted. 401, a different account, or no identity falls through to one interactive login;
network errors, malformed bodies, 403 and other statuses fail closed. A new login is saved only
after Olympus confirms the same identity, and a failed login leaves the old session untouched.

IDMSA trusted-device cookies (including valid broad `apple.com` cookies from IDMSA) are kept
with full scope metadata in `trust.json` (mode 600) inside `ASC_REVIEW_TRUST_ROOT` (mode 700;
default `<private root>/login-trust`; Docker helper: `credentials-login`). The file stores a
SHA-256 account fingerprint, not the Apple ID. Only matching-account trust is loaded; a missing
file is normal, while insecure, symlinked or malformed storage fails closed. Trust is written
after identity confirmation, before the ASC session, and replaced on account change only then.
Widget discovery uses a separate empty client; trust is seeded only into the authentication
client, as one URL-selected `Cookie` header on IDMSA requests (DES values quoted). It is never
sent to ASC. `rememberMe` stays false. The reviewer service never reads the trust directory.
