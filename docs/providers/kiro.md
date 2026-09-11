# Kiro CLI agent

Evidence and installed behavior were revalidated on 11 September 2026 against the official Kiro CLI documentation and the vendor-signed Windows Kiro CLI 2.21.3 MSI. The downloaded MSI hash matched the vendor manifest and both the installer and installed executable had a valid Amazon Web Services Authenticode signature.

| Capability | Classification | AgentDeck status |
|---|---|---|
| Detect executable/version | VERIFIED | Real Windows executable detected as `kiro-cli-chat 2.21.3` |
| Authentication status | VERIFIED | Bounded `whoami --format json`; errors and identifiers are redacted |
| Login | PARTIAL | Provider-owned interactive login is supported |
| Logout/profile isolation | UNSUPPORTED ON WINDOWS | `KIRO_HOME` isolates settings and sessions, but the tested OS credential was shared; AgentDeck does not expose logout or claim isolated identities |
| Session listing | VERIFIED | Official JSON session list is parsed and synchronized into SQLite |
| Native continuation | VERIFIED, SAME IDENTITY | A real session was created, synchronized, resumed directly, and resumed interactively through AgentDeck |
| Model listing | VERIFIED | Official JSON model list returned nine models and is exposed dynamically |
| Model selection | VERIFIED | `--model` construction and dynamic validation are implemented |
| Provider/backend identity | PARTIAL | The CLI catalog does not expose normalized backend identifiers, so AgentDeck labels the managed backend `kiro` without inferring Bedrock/credits/pricing |
| Provider quota usage | UNSUPPORTED | No quota or credit value is displayed |
| Portable AWHF | VERIFIED BY CONTRACT | Kiro is launched with the handoff prompt and no trusted tools; cross-agent authenticated content QA remains separate |

`KIRO_HOME` is still used for profile-scoped settings and sessions. It must not be interpreted as credential isolation on Windows: a real `whoami` under a fresh isolated home still observed the globally authorized identity. The adapter advertises multiple profiles as unsupported and avoids a provider logout operation that could unexpectedly sign out another profile.

Provider operations use fixed argument arrays and bounded subprocesses. Session/model JSON discovery receives a 20-second cold-start allowance; the fast version/auth probes remain at 10 seconds. Kiro interactive handoff and native resume use agent engine v2, pass no `--trust-tools` grant, and never invoke a shell.

## Local QA evidence

- Installed version: 2.21.3.
- `whoami --format json`: successful under an isolated `KIRO_HOME`, proving the shared Windows credential boundary.
- `chat --list-sessions --format json`: successful.
- `chat --list-models --format json`: nine models returned.
- Safe stream-JSON prompt through v2: returned the requested deterministic token.
- Session synchronization: one real provider session persisted with a sanitized title and corrected chronological timestamps.
- Native resume: prior conversation content was recovered both via the direct provider command and `agentdeck session resume`; the interactive process exited normally.

## Official references

- [Kiro CLI](https://kiro.dev/docs/cli/)
- [CLI command reference](https://kiro.dev/docs/cli/reference/cli-commands/)
- [Session management](https://kiro.dev/docs/cli/chat/session-management/)
- [Authentication](https://kiro.dev/docs/cli/authentication/)
- [Settings and `KIRO_HOME`](https://kiro.dev/docs/cli/reference/settings/)
