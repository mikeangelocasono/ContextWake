# Data classification

| Data | Classification | Persistence/export rule |
|---|---|---|
| Agent ID, adapter version, declared capabilities | PUBLIC | SQLite/UI/docs; no secrets |
| Detected executable path/version | LOCAL PRIVATE | Runtime/diagnostics; path is never put in AWHF |
| Model-provider ID and model preference | LOCAL PRIVATE | SQLite/checkpoint/AWHF only when user creates an artifact |
| Profile UUID/name/agent-home path/auth state | LOCAL PRIVATE | SQLite only; AWHF may include UUID but never home path |
| Provider credential reference/file/keychain item | SENSITIVE | Agent-owned; AgentDeck does not read or persist its value |
| Password, API/access/refresh token, private key | NEVER PERSIST | Redact/reject from logs and continuity artifacts |
| Workspace ID/name/absolute path | LOCAL PRIVATE | SQLite; exported AWHF uses name plus one-way path fingerprint, not absolute path |
| Repository branch/commit/filenames/diff counts | LOCAL PRIVATE | Checkpoint/AWHF only after explicit creation/export |
| Repository file contents | SENSITIVE | Not collected, except explicitly configured bounded project instructions after trust |
| Provider session ID/title | LOCAL PRIVATE | SQLite; ID may enter checkpoint/AWHF source metadata, title remains local; neither is assumed portable or authorized |
| Provider transcript/internal state | SENSITIVE | Not collected by current adapters |
| Hidden reasoning/chain-of-thought | NEVER PERSIST | Not accessible, requested, or claimed |
| Checkpoint objective/task/decisions/issues | LOCAL PRIVATE | Local JSON; explicit export only; secret scanned |
| Handoff Markdown/JSON | LOCAL PRIVATE | Local, integrity hashed, secret scanned; user-controlled export/import |
| Validation executable/args/status | LOCAL PRIVATE | Config/checkpoint; output and environment are not persisted |
| Local activity counters | LOCAL PRIVATE | SQLite only; telemetry is off |
| Provider quota/balance | SENSITIVE/UNKNOWN | Not persisted or displayed because current adapters do not expose a reliable source |

The operating-system user account is the local trust boundary. File/directory permissions are restricted where supported, but AgentDeck cannot protect data after the host account is compromised.
